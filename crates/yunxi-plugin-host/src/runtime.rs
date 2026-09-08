//! Process lifecycle and typed invocation over the capability catalog.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use yunxi_kernel::{
    KernelError, KernelSnapshot, KernelState, PluginCommand, PluginFailure, PluginId,
    PluginSnapshot, PluginSpec, YunxiKernel,
};
use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, CapabilityDescriptor, GrantKind, HostMessage,
    HostPluginSession, InvocationCodecError, InvocationRequest, ModelStreamEvent, PluginAcceptor,
    PluginConnectionInfo, PluginMessage, ProtocolError,
};

use crate::resource::PluginResourcePolicy;
use crate::{
    CapabilityCatalog, CatalogError, HostSecretBroker, MAX_AUTOMATIC_RESTARTS, RetryAction,
    RetryController, RetrySnapshot,
};

const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STREAM_READ_POLL: Duration = Duration::from_millis(50);
const INVOCATION_CANCEL_GRACE: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginLaunch {
    id: PluginId,
    display_name: String,
    command: PluginCommand,
    handshake_timeout: Duration,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    required_grants: Vec<GrantKind>,
    expected_capabilities: Option<Vec<CapabilityDescriptor>>,
    expected_plugin_version: Option<String>,
    resource_policy: PluginResourcePolicy,
    executable_fingerprint: Option<crate::discovery::ExecutableFingerprint>,
}

/// Launch state retained for one registered plugin.
///
/// The kernel owns the effective [`PluginSpec`] and its process supervisor.
/// The host retains the user-provided launch settings and the loopback
/// acceptor because a later kernel generation must perform a fresh handshake
/// before its capabilities are routed again.
struct PluginSlot {
    launch: PluginLaunch,
    acceptor: PluginAcceptor,
    retry: RetryController,
}

impl PluginSlot {
    fn new(launch: PluginLaunch, acceptor: PluginAcceptor) -> Self {
        Self {
            launch,
            acceptor,
            retry: RetryController::default(),
        }
    }
}

/// A transport is valid only for the kernel generation that created it.
/// Keeping this fence beside the session prevents a late error from an old
/// socket from being applied to a replacement process.
struct ActiveConnection {
    generation: u64,
    session: HostPluginSession,
}

struct InvocationLease {
    plugin_id: PluginId,
    request_id: u64,
    request: InvocationRequest,
    connection: ActiveConnection,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    resource_policy: PluginResourcePolicy,
    started_at: Instant,
}

enum ConnectionDisposition {
    Reusable,
    Failed(String),
}

struct InvocationExecution<Response> {
    result: Result<Response, PluginCallError>,
    disposition: ConnectionDisposition,
}

impl<Response> InvocationExecution<Response> {
    fn reusable(result: Result<Response, PluginCallError>) -> Self {
        Self {
            result,
            disposition: ConnectionDisposition::Reusable,
        }
    }

    fn failed(result: PluginCallError, message: String) -> Self {
        Self {
            result: Err(result),
            disposition: ConnectionDisposition::Failed(message),
        }
    }
}

impl PluginLaunch {
    pub fn new(id: PluginId, command: PluginCommand) -> Self {
        let display_name = id.to_string();
        Self {
            id,
            display_name,
            command,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            read_timeout: None,
            write_timeout: Some(DEFAULT_WRITE_TIMEOUT),
            required_grants: Vec::new(),
            expected_capabilities: None,
            expected_plugin_version: None,
            resource_policy: PluginResourcePolicy::default(),
            executable_fingerprint: None,
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        let display_name = display_name.into();
        if !display_name.trim().is_empty() {
            self.display_name = display_name;
        }
        self
    }

    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    pub fn with_io_timeouts(
        mut self,
        read_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
    ) -> Self {
        self.read_timeout = read_timeout;
        self.write_timeout = write_timeout;
        self
    }

    pub fn with_required_grants(mut self, grants: impl IntoIterator<Item = GrantKind>) -> Self {
        self.required_grants = grants.into_iter().collect();
        self.required_grants.sort_unstable();
        self.required_grants.dedup();
        self
    }

    /// Applies portable Host-side bounds to this plugin's protocol session.
    ///
    /// The policy limits protocol memory and invocation time. It does not
    /// pretend to be an operating-system CPU or memory sandbox.
    pub fn with_resource_policy(mut self, policy: PluginResourcePolicy) -> Self {
        self.resource_policy = policy;
        self
    }

    pub const fn resource_policy(&self) -> PluginResourcePolicy {
        self.resource_policy
    }

    pub(crate) fn with_executable_fingerprint(
        mut self,
        fingerprint: crate::discovery::ExecutableFingerprint,
    ) -> Self {
        self.executable_fingerprint = Some(fingerprint);
        self
    }

    /// Requires the launched process to announce exactly this capability set
    /// during its readiness handshake.
    ///
    /// The list is normalized by capability id and version so callers do not
    /// need to care about declaration order. Leaving this unset preserves the
    /// low-level host's backwards-compatible "plugin chooses its catalog"
    /// behavior for generic integrations.
    pub fn with_expected_capabilities(
        mut self,
        capabilities: impl IntoIterator<Item = CapabilityDescriptor>,
    ) -> Self {
        let mut capabilities = capabilities.into_iter().collect::<Vec<_>>();
        capabilities.sort_by(|left, right| {
            left.id()
                .as_str()
                .cmp(right.id().as_str())
                .then_with(|| left.version().cmp(&right.version()))
        });
        capabilities
            .dedup_by(|left, right| left.id() == right.id() && left.version() == right.version());
        self.expected_capabilities = Some(capabilities);
        self
    }

    /// Requires the child to announce this exact version during readiness.
    ///
    /// The value is intentionally optional for backwards compatibility with
    /// low-level callers that let a plugin choose its own catalog metadata.
    pub fn with_expected_plugin_version(mut self, version: impl Into<String>) -> Self {
        let version = version.into();
        self.expected_plugin_version = (!version.trim().is_empty()).then_some(version);
        self
    }

    pub fn expected_plugin_version(&self) -> Option<&str> {
        self.expected_plugin_version.as_deref()
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn command(&self) -> &PluginCommand {
        &self.command
    }
}

pub struct ProcessPluginHost {
    kernel: YunxiKernel,
    catalog: CapabilityCatalog,
    secret_broker: HostSecretBroker,
    connections: BTreeMap<PluginId, ActiveConnection>,
    leased_connections: BTreeSet<(PluginId, u64)>,
    slots: BTreeMap<PluginId, PluginSlot>,
    next_request_id: u64,
}

impl ProcessPluginHost {
    pub fn new() -> Self {
        Self::new_with_secret_broker(HostSecretBroker::new())
    }

    /// Builds a process Host around an explicitly selected secret backend.
    /// This keeps deterministic tests on the default memory store while
    /// allowing production callers to inject an encrypted durable broker.
    pub fn new_with_secret_broker(secret_broker: HostSecretBroker) -> Self {
        Self {
            kernel: YunxiKernel::new(),
            catalog: CapabilityCatalog::new(),
            secret_broker,
            connections: BTreeMap::new(),
            leased_connections: BTreeSet::new(),
            slots: BTreeMap::new(),
            next_request_id: 1,
        }
    }

    pub fn launch(&mut self, launch: PluginLaunch) -> Result<PluginId, PluginHostError> {
        let id = launch.id.clone();
        if self.slots.contains_key(&id) {
            return Err(PluginHostError::Kernel(KernelError::DuplicatePlugin { id }));
        }
        self.secret_broker.set_plugin_enabled(&id, false)?;
        let acceptor = PluginAcceptor::bind()?;
        let command = launch
            .command
            .clone()
            .env(CONNECT_ADDRESS_ENV, acceptor.address()?.to_string())
            .env(CONNECT_TOKEN_ENV, acceptor.connection_token());
        let spec =
            PluginSpec::new(id.clone(), command).with_display_name(launch.display_name.clone());
        self.kernel.register(spec)?;
        self.slots
            .insert(id.clone(), PluginSlot::new(launch, acceptor));

        if let Err(error) = self.start_generation(&id) {
            // An initial handshake failure is kept available for a later
            // explicit enable, but must not start a hidden retry loop.
            self.disable_retry(&id);
            return Err(error);
        }
        Ok(id)
    }

    pub fn invoke<Request, Response>(
        &mut self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, PluginCallError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        let mut lease = self.prepare_invocation(capability, operation, payload)?;
        let plugin_id = lease.plugin_id.clone();
        let execution = catch_unwind(AssertUnwindSafe(|| execute_unary_invocation(&mut lease)))
            .unwrap_or_else(|_| invocation_panic_execution(plugin_id));
        self.finish_invocation(lease, execution)
    }

    /// Invokes a model-capable route while forwarding bounded progress frames.
    ///
    /// The regular [`Self::invoke`] path remains unchanged for legacy plugins.
    /// A streaming call temporarily uses a short read timeout so a caller can
    /// cancel an in-flight request even when the child has not emitted a new
    /// frame. Cancellation closes and fails only the affected plugin
    /// generation; the normal recovery policy can start a fresh generation on
    /// the next refresh.
    pub fn invoke_streaming<Request, Response, IsCancelled, OnEvent>(
        &mut self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
        is_cancelled: IsCancelled,
        mut on_event: OnEvent,
    ) -> Result<Response, PluginCallError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
        IsCancelled: Fn() -> bool,
        OnEvent: FnMut(ModelStreamEvent) -> Result<(), String>,
    {
        let mut lease = self.prepare_invocation(capability, operation, payload)?;
        let plugin_id = lease.plugin_id.clone();
        let execution = catch_unwind(AssertUnwindSafe(|| {
            execute_streaming_invocation(&mut lease, is_cancelled, &mut on_event)
        }))
        .unwrap_or_else(|_| invocation_panic_execution(plugin_id));
        self.finish_invocation(lease, execution)
    }

    fn prepare_invocation<Request>(
        &mut self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
    ) -> Result<InvocationLease, PluginCallError>
    where
        Request: Serialize,
    {
        let plugin_id = self
            .catalog
            .resolve_unique(capability.id().as_str(), capability.version())
            .map_err(PluginCallError::Route)?
            .id()
            .clone();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).unwrap_or(1);
        let request = InvocationRequest::encode(request_id, capability.clone(), operation, payload)
            .map_err(PluginCallError::Codec)?;
        let resource_policy = self
            .slots
            .get(&plugin_id)
            .map(|slot| slot.launch.resource_policy)
            .unwrap_or_default();
        let encoded_request = serde_json::to_vec(&HostMessage::Invoke {
            request: request.clone(),
        })
        .map_err(|error| PluginCallError::Codec(InvocationCodecError::Serialize(error)))?;
        if encoded_request.len() > resource_policy.max_invocation_bytes() {
            return Err(PluginCallError::ResourceLimit {
                plugin_id,
                resource: "invocation bytes",
                limit: resource_policy.max_invocation_bytes(),
            });
        }
        let Some(connection) = self.connections.remove(&plugin_id) else {
            let message = if self
                .leased_connections
                .iter()
                .any(|(leased_id, _)| leased_id == &plugin_id)
            {
                "plugin already has an invocation in flight"
            } else {
                "plugin connection is not available"
            }
            .to_string();
            return Err(PluginCallError::Unavailable { plugin_id, message });
        };
        let (read_timeout, write_timeout) = self
            .slots
            .get(&plugin_id)
            .map(|slot| (slot.launch.read_timeout, slot.launch.write_timeout))
            .unwrap_or((None, None));
        self.leased_connections
            .insert((plugin_id.clone(), connection.generation));
        Ok(InvocationLease {
            plugin_id,
            request_id,
            request,
            connection,
            read_timeout,
            write_timeout,
            resource_policy,
            started_at: Instant::now(),
        })
    }

    fn finish_invocation<Response>(
        &mut self,
        lease: InvocationLease,
        execution: InvocationExecution<Response>,
    ) -> Result<Response, PluginCallError> {
        let generation = lease.connection.generation;
        self.leased_connections
            .remove(&(lease.plugin_id.clone(), generation));

        if let ConnectionDisposition::Failed(message) = execution.disposition {
            self.fail_connection(&lease.plugin_id, Some(generation), message);
            return execution.result;
        }

        if let Err(error) = lease
            .connection
            .session
            .set_timeouts(lease.read_timeout, lease.write_timeout)
        {
            let message = error.to_string();
            self.fail_connection(&lease.plugin_id, Some(generation), message.clone());
            return Err(PluginCallError::Unavailable {
                plugin_id: lease.plugin_id,
                message,
            });
        }

        self.kernel.refresh();
        let generation_is_current_and_active =
            self.kernel
                .plugin(&lease.plugin_id)
                .is_some_and(|snapshot| {
                    snapshot.generation() == generation && snapshot.state().is_active()
                });
        let can_return = generation_is_current_and_active
            && self.slots.contains_key(&lease.plugin_id)
            && self.catalog.plugin(&lease.plugin_id).is_some()
            && !self.connections.contains_key(&lease.plugin_id);
        if can_return {
            self.connections
                .insert(lease.plugin_id.clone(), lease.connection);
        } else if !self.connections.contains_key(&lease.plugin_id)
            && self
                .kernel
                .plugin(&lease.plugin_id)
                .is_none_or(|snapshot| snapshot.generation() == generation)
        {
            self.catalog.unregister(&lease.plugin_id);
        }
        execution.result
    }

    pub fn stop(&mut self, id: &PluginId) {
        let _ignored = self.disable(id);
    }

    /// Stops routing and the managed process until an explicit `enable` call.
    pub fn disable(&mut self, id: &PluginId) -> Result<(), PluginHostError> {
        self.ensure_slot(id)?;
        self.disable_retry(id);
        self.secret_broker.set_plugin_enabled(id, false)?;
        self.stop_current(id)
    }

    /// Fully remove a quiescent plugin and all of its capability routes.
    ///
    /// This is the Host-side half of hot reload.  `disable` deliberately
    /// keeps a slot for a later user enable; `unregister` releases the slot,
    /// acceptor, connection, catalog record, and kernel supervisor together.
    pub fn unregister(&mut self, id: &PluginId) -> Result<(), PluginHostError> {
        self.ensure_slot(id)?;
        self.disable_retry(id);
        self.secret_broker.set_plugin_enabled(id, false)?;
        self.stop_current(id)?;
        self.kernel.refresh();
        self.kernel.unregister(id)?;
        self.detach_connection(id);
        self.slots.remove(id);
        Ok(())
    }

    pub fn is_registered(&self, id: &PluginId) -> bool {
        self.slots.contains_key(id)
    }

    /// Starts a fresh enable cycle for a registered plugin.
    ///
    /// The retry budget is reset only here (or through [`Self::enable`]).
    pub fn restart(&mut self, id: &PluginId) -> Result<PluginId, PluginHostError> {
        self.manual_start(id)
    }

    /// Enables a plugin and resets its automatic-restart budget.
    pub fn enable(&mut self, id: &PluginId) -> Result<PluginId, PluginHostError> {
        self.manual_start(id)
    }

    /// Explicitly named compatibility alias for a manual restart operation.
    pub fn manual_restart(&mut self, id: &PluginId) -> Result<PluginId, PluginHostError> {
        self.manual_start(id)
    }

    pub fn refresh(&mut self) {
        self.kernel.refresh();
        self.recover_failed_plugins();
        self.kernel.refresh();
        self.prune_inactive_connections();
    }

    pub fn kernel_state(&self) -> KernelState {
        self.kernel.state()
    }

    pub fn plugin(&mut self, id: &PluginId) -> Option<PluginSnapshot> {
        self.refresh();
        self.kernel.plugin(id)
    }

    pub fn snapshot(&mut self) -> KernelSnapshot {
        self.refresh();
        self.kernel.snapshot()
    }

    pub fn catalog(&self) -> &CapabilityCatalog {
        &self.catalog
    }

    /// Returns a cloneable handle to the Host-owned secret broker. Secret
    /// references are revoked when the corresponding plugin is disabled.
    pub fn secret_broker(&self) -> HostSecretBroker {
        self.secret_broker.clone()
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len() + self.leased_connections.len()
    }

    /// Returns the bounded recovery state for a registered plugin.
    pub fn retry_snapshot(&mut self, id: &PluginId) -> Option<RetrySnapshot> {
        self.refresh();
        self.slots.get(id).map(|slot| slot.retry.snapshot())
    }

    pub fn shutdown(&mut self) {
        for slot in self.slots.values_mut() {
            slot.retry.disable();
        }
        for id in self.slots.keys() {
            let _ignored = self.secret_broker.set_plugin_enabled(id, false);
        }
        let ids = self.connections.keys().cloned().collect::<Vec<_>>();
        for id in &ids {
            if let Some(connection) = self.connections.get_mut(id) {
                let _ignored = connection.session.send(&HostMessage::Shutdown);
            }
        }
        self.connections.clear();
        self.leased_connections.clear();
        for id in ids {
            self.catalog.unregister(&id);
        }
        self.kernel.shutdown();
        self.slots.clear();
    }

    fn ensure_slot(&self, id: &PluginId) -> Result<(), PluginHostError> {
        if self.slots.contains_key(id) {
            Ok(())
        } else {
            Err(PluginHostError::Kernel(KernelError::UnknownPlugin {
                id: id.clone(),
            }))
        }
    }

    fn manual_start(&mut self, id: &PluginId) -> Result<PluginId, PluginHostError> {
        self.ensure_slot(id)?;
        self.stop_current(id)?;
        if let Some(slot) = self.slots.get_mut(id) {
            slot.retry.manual_restart();
        }
        match self.start_generation(id) {
            Ok(()) => Ok(id.clone()),
            Err(error) => {
                // A failed explicit start is quiescent until the user asks
                // for another enable; refresh must not turn it into a loop.
                self.disable_retry(id);
                Err(error)
            }
        }
    }

    fn start_generation(&mut self, id: &PluginId) -> Result<(), PluginHostError> {
        self.ensure_slot(id)?;
        self.detach_connection(id);
        self.kernel.refresh();

        let executable_check = self.slots.get(id).and_then(|slot| {
            slot.launch
                .executable_fingerprint
                .as_ref()
                .map(|fingerprint| {
                    (
                        fingerprint.clone(),
                        slot.launch.command.program().to_path_buf(),
                    )
                })
        });
        if let Some((fingerprint, path)) = executable_check
            && let Err(message) = fingerprint.verify(&path)
        {
            self.secret_broker.set_plugin_enabled(id, false)?;
            return Err(PluginHostError::Protocol(ProtocolError::Handshake(message)));
        }
        if let Err(error) = self.kernel.start(id) {
            self.secret_broker.set_plugin_enabled(id, false)?;
            return Err(error.into());
        }

        let generation = self
            .kernel
            .plugin(id)
            .map(|snapshot| snapshot.generation())
            .ok_or_else(|| {
                PluginHostError::Kernel(KernelError::UnknownPlugin { id: id.clone() })
            })?;
        let retry_enabled = self
            .slots
            .get_mut(id)
            .is_some_and(|slot| slot.retry.observe_generation(generation));
        if !retry_enabled {
            let message = "plugin enable cycle is disabled".to_string();
            self.fail_connection(id, Some(generation), message.clone());
            return Err(PluginHostError::Protocol(ProtocolError::Handshake(message)));
        }
        let (
            handshake_timeout,
            read_timeout,
            write_timeout,
            required_grants,
            expected_capabilities,
            expected_plugin_version,
            resource_policy,
        ) = {
            let slot = self
                .slots
                .get(id)
                .expect("plugin slot exists after ensure_slot");
            (
                slot.launch.handshake_timeout,
                slot.launch.read_timeout,
                slot.launch.write_timeout,
                slot.launch.required_grants.clone(),
                slot.launch.expected_capabilities.clone(),
                slot.launch.expected_plugin_version.clone(),
                slot.launch.resource_policy,
            )
        };

        let connection = {
            let slot = self
                .slots
                .get(id)
                .expect("plugin slot exists after ensure_slot");
            slot.acceptor.accept_with_max_frame_bytes(
                id.as_str(),
                handshake_timeout,
                resource_policy.max_frame_bytes(),
            )
        };
        let mut connection = match connection {
            Ok(connection) => connection,
            Err(error) => {
                self.fail_connection(id, Some(generation), error.to_string());
                return Err(PluginHostError::Protocol(error));
            }
        };
        if let Err(error) =
            validate_expected_capabilities(connection.info(), expected_capabilities.as_deref())
        {
            self.fail_connection(id, Some(generation), error.to_string());
            return Err(PluginHostError::Protocol(error));
        }
        if let Some(expected) = expected_plugin_version.as_deref()
            && connection.info().plugin_version() != expected
        {
            let error = ProtocolError::Handshake(format!(
                "plugin `{}` announced version `{}`; expected `{expected}`",
                connection.info().plugin_id(),
                connection.info().plugin_version(),
            ));
            self.fail_connection(id, Some(generation), error.to_string());
            return Err(PluginHostError::Protocol(error));
        }
        if let Err(error) = validate_required_grants(connection.info(), &required_grants) {
            self.fail_connection(id, Some(generation), error.to_string());
            return Err(PluginHostError::Protocol(error));
        }
        if let Err(error) = connection.set_timeouts(read_timeout, write_timeout) {
            self.fail_connection(id, Some(generation), error.to_string());
            return Err(PluginHostError::Protocol(error));
        }
        connection.set_max_frame_bytes(resource_policy.max_frame_bytes());
        if let Err(error) = self.catalog.register_connection(connection.info()) {
            self.fail_connection(id, Some(generation), error.to_string());
            return Err(PluginHostError::Catalog(error));
        }
        self.connections.insert(
            id.clone(),
            ActiveConnection {
                generation,
                session: connection,
            },
        );
        if let Err(error) = self.secret_broker.set_plugin_enabled(id, true) {
            self.fail_connection(id, Some(generation), error.to_string());
            return Err(error.into());
        }
        Ok(())
    }

    fn stop_current(&mut self, id: &PluginId) -> Result<(), PluginHostError> {
        self.ensure_slot(id)?;
        if let Some(connection) = self.connections.get_mut(id) {
            let _ignored = connection.session.send(&HostMessage::Shutdown);
        }
        self.detach_connection(id);
        self.kernel.refresh();

        let state = self
            .kernel
            .plugin(id)
            .ok_or_else(|| PluginHostError::Kernel(KernelError::UnknownPlugin { id: id.clone() }))?
            .state()
            .clone();
        if state.is_active() {
            self.kernel.stop(id)?;
        }

        let deadline = Instant::now() + STOP_WAIT_TIMEOUT;
        loop {
            self.kernel.refresh();
            let snapshot = self.kernel.plugin(id).ok_or_else(|| {
                PluginHostError::Kernel(KernelError::UnknownPlugin { id: id.clone() })
            })?;
            if !snapshot.state().is_active() {
                break;
            }
            if Instant::now() >= deadline {
                let _ignored = self.kernel.fail(
                    id,
                    PluginFailure::Monitor {
                        message: "timed out waiting for plugin supervisor to stop".to_string(),
                    },
                );
                self.kernel.refresh();
                break;
            }
            thread::sleep(STOP_POLL_INTERVAL);
        }
        Ok(())
    }

    fn recover_failed_plugins(&mut self) {
        // A single refresh may observe a fast-crashing replacement. Repeat a
        // bounded number of times so one call can settle the cycle, while the
        // controller remains the authoritative retry limit.
        for _ in 0..=MAX_AUTOMATIC_RESTARTS {
            self.kernel.refresh();
            let failures = self
                .kernel
                .snapshot()
                .plugins()
                .iter()
                .filter(|snapshot| snapshot.state().is_failed())
                .map(|snapshot| (snapshot.id().clone(), snapshot.generation()))
                .collect::<Vec<_>>();
            if failures.is_empty() {
                break;
            }

            let mut progressed = false;
            for (id, generation) in failures {
                progressed |= self.recover_failed_plugin(&id, generation);
            }
            if !progressed {
                break;
            }
        }
    }

    fn recover_failed_plugin(&mut self, id: &PluginId, generation: u64) -> bool {
        let _ignored = self.secret_broker.set_plugin_enabled(id, false);
        let action = {
            let Some(slot) = self.slots.get_mut(id) else {
                return false;
            };
            if !slot.retry.observe_generation(generation) {
                return false;
            }
            slot.retry.on_failure(generation)
        };

        match action {
            None => false,
            Some(RetryAction::Restart { .. }) => {
                self.detach_connection(id);
                match self.start_generation(id) {
                    Ok(()) => true,
                    Err(_error) => {
                        self.kernel.refresh();
                        let snapshot = self.kernel.plugin(id);
                        let has_new_failed_generation = snapshot.is_some_and(|snapshot| {
                            snapshot.generation() > generation && snapshot.state().is_failed()
                        });
                        if !has_new_failed_generation {
                            self.disable_failed_plugin(id);
                        }
                        true
                    }
                }
            }
            Some(RetryAction::Disable) => {
                self.disable_failed_plugin(id);
                true
            }
        }
    }

    fn disable_failed_plugin(&mut self, id: &PluginId) {
        self.disable_retry(id);
        let _ignored = self.secret_broker.set_plugin_enabled(id, false);
        let _ignored = self.stop_current(id);
        self.detach_connection(id);
    }

    fn disable_retry(&mut self, id: &PluginId) {
        if let Some(slot) = self.slots.get_mut(id) {
            slot.retry.disable();
        }
    }

    fn prune_inactive_connections(&mut self) {
        let disconnected = self
            .connections
            .keys()
            .filter(|id| {
                self.kernel
                    .plugin(id)
                    .is_none_or(|snapshot| !snapshot.state().is_active())
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in disconnected {
            self.detach_connection(&id);
        }
    }

    fn detach_connection(&mut self, id: &PluginId) {
        self.connections.remove(id);
        self.catalog.unregister(id);
    }

    fn fail_connection(
        &mut self,
        id: &PluginId,
        connection_generation: Option<u64>,
        message: String,
    ) {
        let should_detach = match connection_generation {
            Some(generation) => self
                .connections
                .get(id)
                .is_none_or(|connection| connection.generation == generation),
            None => true,
        };
        if should_detach {
            let _ignored = self.secret_broker.set_plugin_enabled(id, false);
            self.detach_connection(id);
        }
        self.kernel.refresh();
        let Some(snapshot) = self.kernel.plugin(id) else {
            return;
        };
        let failure_generation = connection_generation.unwrap_or(snapshot.generation());

        // A stale transport event must not fail a newer kernel generation.
        if snapshot.generation() != failure_generation {
            return;
        }
        if !snapshot.state().is_failed()
            && (snapshot.state().is_active()
                || matches!(snapshot.state(), yunxi_kernel::PluginState::Registered))
        {
            let _ignored = self.kernel.fail(id, PluginFailure::Protocol { message });
        }
        self.kernel.refresh();
    }
}

/// Cloneable facade that releases the global host lock while one plugin is
/// performing blocking I/O.
///
/// Each process connection still handles only one invocation at a time. The
/// connection is leased out of the routing state for the duration of a call,
/// so unrelated plugins and lifecycle/status operations remain responsive.
/// A generation fence prevents a late call result from being returned to, or
/// failing, a replacement process.
#[derive(Clone)]
pub struct SharedProcessPluginHost {
    inner: Arc<Mutex<ProcessPluginHost>>,
}

impl SharedProcessPluginHost {
    pub fn new(host: ProcessPluginHost) -> Self {
        Self {
            inner: Arc::new(Mutex::new(host)),
        }
    }

    pub fn from_arc(inner: Arc<Mutex<ProcessPluginHost>>) -> Self {
        Self { inner }
    }

    pub fn lock(&self) -> MutexGuard<'_, ProcessPluginHost> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn secret_broker(&self) -> HostSecretBroker {
        self.lock().secret_broker()
    }

    pub fn invoke<Request, Response>(
        &self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, PluginCallError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        let mut lease = {
            let mut host = self.lock();
            host.prepare_invocation(capability, operation, payload)?
        };
        let plugin_id = lease.plugin_id.clone();
        let execution = catch_unwind(AssertUnwindSafe(|| execute_unary_invocation(&mut lease)))
            .unwrap_or_else(|_| invocation_panic_execution(plugin_id));
        self.lock().finish_invocation(lease, execution)
    }

    pub fn invoke_streaming<Request, Response, IsCancelled, OnEvent>(
        &self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
        is_cancelled: IsCancelled,
        mut on_event: OnEvent,
    ) -> Result<Response, PluginCallError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
        IsCancelled: Fn() -> bool,
        OnEvent: FnMut(ModelStreamEvent) -> Result<(), String>,
    {
        let mut lease = {
            let mut host = self.lock();
            host.prepare_invocation(capability, operation, payload)?
        };
        let plugin_id = lease.plugin_id.clone();
        let execution = catch_unwind(AssertUnwindSafe(|| {
            execute_streaming_invocation(&mut lease, is_cancelled, &mut on_event)
        }))
        .unwrap_or_else(|_| invocation_panic_execution(plugin_id));
        self.lock().finish_invocation(lease, execution)
    }
}

fn execute_unary_invocation<Response>(lease: &mut InvocationLease) -> InvocationExecution<Response>
where
    Response: DeserializeOwned,
{
    let invocation_timeout = effective_invocation_timeout(lease);
    if let Err(error) = lease.connection.session.set_timeouts(
        Some(invocation_timeout),
        Some(effective_write_timeout(lease)),
    ) {
        let message = error.to_string();
        return InvocationExecution::failed(
            PluginCallError::Unavailable {
                plugin_id: lease.plugin_id.clone(),
                message: message.clone(),
            },
            message,
        );
    }
    if let Err(error) = lease.connection.session.send(&HostMessage::Invoke {
        request: lease.request.clone(),
    }) {
        let message = error.to_string();
        return InvocationExecution::failed(
            PluginCallError::Unavailable {
                plugin_id: lease.plugin_id.clone(),
                message: message.clone(),
            },
            message,
        );
    }

    let mut progress_frames = 0usize;
    let mut output_bytes = 0usize;
    loop {
        if lease.started_at.elapsed() >= lease.resource_policy.max_invocation_duration() {
            let error = invocation_duration_limit_error(lease);
            let message = error.to_string();
            return InvocationExecution::failed(error, message);
        }
        // A socket timeout is relative, so recompute the remaining wall-clock
        // budget before every receive instead of resetting it after progress.
        let remaining = lease
            .resource_policy
            .max_invocation_duration()
            .saturating_sub(lease.started_at.elapsed());
        let read_timeout = lease
            .read_timeout
            .map_or(remaining, |timeout| timeout.min(remaining));
        if let Err(error) = lease
            .connection
            .session
            .set_timeouts(Some(read_timeout), Some(effective_write_timeout(lease)))
        {
            let message = error.to_string();
            return InvocationExecution::failed(
                PluginCallError::Unavailable {
                    plugin_id: lease.plugin_id.clone(),
                    message: message.clone(),
                },
                message,
            );
        }
        let response = match lease.connection.session.receive() {
            Ok(response) => response,
            Err(error)
                if is_read_timeout(&error)
                    && lease.started_at.elapsed()
                        >= lease.resource_policy.max_invocation_duration() =>
            {
                let error = invocation_duration_limit_error(lease);
                let message = error.to_string();
                return InvocationExecution::failed(error, message);
            }
            Err(error) => {
                let message = error.to_string();
                return InvocationExecution::failed(
                    PluginCallError::Unavailable {
                        plugin_id: lease.plugin_id.clone(),
                        message: message.clone(),
                    },
                    message,
                );
            }
        };
        match response {
            PluginMessage::InvocationProgress {
                request_id: response_id,
                event,
            } if response_id == lease.request_id => {
                if let Err(error) = charge_output_bytes(lease, &mut output_bytes, &event) {
                    let message = error.to_string();
                    return InvocationExecution::failed(error, message);
                }
                progress_frames = progress_frames.saturating_add(1);
                if progress_frames > yunxi_protocol::MAX_STREAM_EVENTS_PER_TURN {
                    let message = format!(
                        "request {} exceeded the bounded progress frame count",
                        lease.request_id
                    );
                    return InvocationExecution::failed(
                        PluginCallError::ProtocolViolation {
                            plugin_id: lease.plugin_id.clone(),
                            message: message.clone(),
                        },
                        message,
                    );
                }
                if let Err(error) = event.validate() {
                    let message = error.to_string();
                    return InvocationExecution::failed(
                        PluginCallError::ProtocolViolation {
                            plugin_id: lease.plugin_id.clone(),
                            message: message.clone(),
                        },
                        message,
                    );
                }
            }
            PluginMessage::InvocationCompleted { response }
                if response.request_id() == lease.request_id =>
            {
                if let Err(error) = charge_output_bytes(lease, &mut output_bytes, &response) {
                    let message = error.to_string();
                    return InvocationExecution::failed(error, message);
                }
                return match response.decode_payload() {
                    Ok(response) => InvocationExecution::reusable(Ok(response)),
                    Err(error) => {
                        let message = error.to_string();
                        InvocationExecution::failed(
                            PluginCallError::ProtocolViolation {
                                plugin_id: lease.plugin_id.clone(),
                                message: message.clone(),
                            },
                            message,
                        )
                    }
                };
            }
            PluginMessage::InvocationFailed {
                request_id: response_id,
                code,
                message,
                retryable,
            } if response_id == lease.request_id => {
                return InvocationExecution::reusable(Err(PluginCallError::Rejected {
                    plugin_id: lease.plugin_id.clone(),
                    code,
                    message,
                    retryable,
                }));
            }
            message => {
                let detail = format!(
                    "expected response for request {}, received {message:?}",
                    lease.request_id
                );
                return InvocationExecution::failed(
                    PluginCallError::ProtocolViolation {
                        plugin_id: lease.plugin_id.clone(),
                        message: detail.clone(),
                    },
                    detail,
                );
            }
        }
    }
}

fn execute_streaming_invocation<Response, IsCancelled, OnEvent>(
    lease: &mut InvocationLease,
    is_cancelled: IsCancelled,
    on_event: &mut OnEvent,
) -> InvocationExecution<Response>
where
    Response: DeserializeOwned,
    IsCancelled: Fn() -> bool,
    OnEvent: FnMut(ModelStreamEvent) -> Result<(), String>,
{
    let poll_timeout = lease
        .read_timeout
        .map_or(STREAM_READ_POLL, |timeout| timeout.min(STREAM_READ_POLL));
    if let Err(error) = lease
        .connection
        .session
        .set_timeouts(Some(poll_timeout), Some(effective_write_timeout(lease)))
    {
        let message = error.to_string();
        return InvocationExecution::failed(
            PluginCallError::Unavailable {
                plugin_id: lease.plugin_id.clone(),
                message: message.clone(),
            },
            message,
        );
    }
    if let Err(error) = lease.connection.session.send(&HostMessage::Invoke {
        request: lease.request.clone(),
    }) {
        let message = error.to_string();
        return InvocationExecution::failed(
            PluginCallError::Unavailable {
                plugin_id: lease.plugin_id.clone(),
                message: message.clone(),
            },
            message,
        );
    }

    let mut progress_frames = 0usize;
    let mut output_bytes = 0usize;
    let mut last_activity = Instant::now();
    let mut cancellation_requested = false;
    let mut cancellation_deadline = None;
    loop {
        if lease.started_at.elapsed() >= lease.resource_policy.max_invocation_duration() {
            let error = invocation_duration_limit_error(lease);
            let message = error.to_string();
            return InvocationExecution::failed(error, message);
        }
        if !cancellation_requested && is_cancelled() {
            if let Err(error) = lease.connection.session.send(&HostMessage::Cancel {
                request_id: lease.request_id,
            }) {
                let message = format!("failed to send invocation cancellation: {error}");
                return InvocationExecution::failed(
                    PluginCallError::Unavailable {
                        plugin_id: lease.plugin_id.clone(),
                        message: message.clone(),
                    },
                    message,
                );
            }
            cancellation_requested = true;
            cancellation_deadline = Some(Instant::now() + INVOCATION_CANCEL_GRACE);
        }
        if cancellation_requested
            && cancellation_deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            let message = format!(
                "plugin did not acknowledge cancellation for request {} within {} milliseconds",
                lease.request_id,
                INVOCATION_CANCEL_GRACE.as_millis()
            );
            return InvocationExecution::failed(
                PluginCallError::Unavailable {
                    plugin_id: lease.plugin_id.clone(),
                    message: message.clone(),
                },
                message,
            );
        }
        let response = match lease.connection.session.receive() {
            Ok(response) => response,
            Err(error) if is_read_timeout(&error) => {
                if lease.started_at.elapsed() >= lease.resource_policy.max_invocation_duration() {
                    let error = invocation_duration_limit_error(lease);
                    let message = error.to_string();
                    return InvocationExecution::failed(error, message);
                }
                if lease
                    .read_timeout
                    .is_some_and(|timeout| last_activity.elapsed() >= timeout)
                {
                    let message = format!(
                        "stream invocation received no plugin frame for {} milliseconds",
                        lease
                            .read_timeout
                            .expect("stream read timeout is checked above")
                            .as_millis()
                    );
                    return InvocationExecution::failed(
                        PluginCallError::Unavailable {
                            plugin_id: lease.plugin_id.clone(),
                            message: message.clone(),
                        },
                        message,
                    );
                }
                continue;
            }
            Err(error) => {
                let message = error.to_string();
                return InvocationExecution::failed(
                    PluginCallError::Unavailable {
                        plugin_id: lease.plugin_id.clone(),
                        message: message.clone(),
                    },
                    message,
                );
            }
        };
        last_activity = Instant::now();

        match response {
            PluginMessage::InvocationProgress {
                request_id: response_id,
                event,
            } if response_id == lease.request_id => {
                if cancellation_requested {
                    continue;
                }
                if let Err(error) = charge_output_bytes(lease, &mut output_bytes, &event) {
                    let message = error.to_string();
                    return InvocationExecution::failed(error, message);
                }
                progress_frames = progress_frames.saturating_add(1);
                if progress_frames > yunxi_protocol::MAX_STREAM_EVENTS_PER_TURN {
                    let message = format!(
                        "request {} exceeded the bounded progress frame count",
                        lease.request_id
                    );
                    return InvocationExecution::failed(
                        PluginCallError::ProtocolViolation {
                            plugin_id: lease.plugin_id.clone(),
                            message: message.clone(),
                        },
                        message,
                    );
                }
                if let Err(error) = event.validate() {
                    let message = error.to_string();
                    return InvocationExecution::failed(
                        PluginCallError::ProtocolViolation {
                            plugin_id: lease.plugin_id.clone(),
                            message: message.clone(),
                        },
                        message,
                    );
                }
                if let Err(message) = on_event(event) {
                    return InvocationExecution::failed(
                        PluginCallError::ProtocolViolation {
                            plugin_id: lease.plugin_id.clone(),
                            message: message.clone(),
                        },
                        message,
                    );
                }
            }
            PluginMessage::InvocationCompleted { response }
                if response.request_id() == lease.request_id =>
            {
                if let Err(error) = charge_output_bytes(lease, &mut output_bytes, &response) {
                    let message = error.to_string();
                    return InvocationExecution::failed(error, message);
                }
                if cancellation_requested {
                    return InvocationExecution::reusable(Err(invocation_cancelled_error(
                        lease,
                        "plugin completed after cancellation was requested",
                    )));
                }
                return match response.decode_payload() {
                    Ok(response) => InvocationExecution::reusable(Ok(response)),
                    Err(error) => {
                        let message = error.to_string();
                        InvocationExecution::failed(
                            PluginCallError::ProtocolViolation {
                                plugin_id: lease.plugin_id.clone(),
                                message: message.clone(),
                            },
                            message,
                        )
                    }
                };
            }
            PluginMessage::InvocationFailed {
                request_id: response_id,
                code,
                message,
                retryable,
            } if response_id == lease.request_id => {
                if cancellation_requested {
                    return InvocationExecution::reusable(Err(invocation_cancelled_error(
                        lease,
                        format!("plugin acknowledged cancellation ({code}): {message}"),
                    )));
                }
                return InvocationExecution::reusable(Err(PluginCallError::Rejected {
                    plugin_id: lease.plugin_id.clone(),
                    code,
                    message,
                    retryable,
                }));
            }
            message => {
                let detail = format!(
                    "expected streaming response for request {}, received {message:?}",
                    lease.request_id
                );
                return InvocationExecution::failed(
                    PluginCallError::ProtocolViolation {
                        plugin_id: lease.plugin_id.clone(),
                        message: detail.clone(),
                    },
                    detail,
                );
            }
        }
    }
}

fn validate_required_grants(
    connection: &PluginConnectionInfo,
    required_grants: &[GrantKind],
) -> Result<(), ProtocolError> {
    if required_grants.is_empty() {
        return Ok(());
    }
    let Some(manifest) = connection.manifest() else {
        return Err(ProtocolError::Handshake(format!(
            "plugin `{}` must provide a manifest declaring required grants: {}",
            connection.plugin_id(),
            required_grants
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    for grant in required_grants {
        if !manifest.declares_required_grant(*grant) {
            return Err(ProtocolError::Handshake(format!(
                "plugin `{}` manifest does not declare required grant `{grant}`",
                connection.plugin_id()
            )));
        }
    }
    Ok(())
}

fn validate_expected_capabilities(
    connection: &PluginConnectionInfo,
    expected: Option<&[CapabilityDescriptor]>,
) -> Result<(), ProtocolError> {
    let Some(expected) = expected else {
        return Ok(());
    };

    let expected = expected.iter().map(capability_key).collect::<BTreeSet<_>>();
    let announced = connection
        .capabilities()
        .iter()
        .map(capability_key)
        .collect::<BTreeSet<_>>();
    if expected == announced {
        return Ok(());
    }

    Err(ProtocolError::Handshake(format!(
        "plugin `{}` announced capabilities outside its launch contract (expected: {}; received: {})",
        connection.plugin_id(),
        format_capability_keys(&expected),
        format_capability_keys(&announced),
    )))
}

fn capability_key(capability: &CapabilityDescriptor) -> (String, u32) {
    (capability.id().to_string(), capability.version())
}

fn format_capability_keys(capabilities: &BTreeSet<(String, u32)>) -> String {
    if capabilities.is_empty() {
        return "<none>".to_string();
    }
    capabilities
        .iter()
        .map(|(id, version)| format!("{id}@{version}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_read_timeout(error: &ProtocolError) -> bool {
    matches!(
        error,
        ProtocolError::Io(io_error)
            if matches!(
                io_error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            )
    )
}

fn effective_invocation_timeout(lease: &InvocationLease) -> Duration {
    lease
        .read_timeout
        .map_or(lease.resource_policy.max_invocation_duration(), |timeout| {
            timeout.min(lease.resource_policy.max_invocation_duration())
        })
}

fn effective_write_timeout(lease: &InvocationLease) -> Duration {
    lease.write_timeout.map_or_else(
        || lease.resource_policy.max_invocation_duration(),
        |timeout| timeout.min(lease.resource_policy.max_invocation_duration()),
    )
}

fn charge_output_bytes<T: Serialize>(
    lease: &InvocationLease,
    output_bytes: &mut usize,
    value: &T,
) -> Result<(), PluginCallError> {
    let serialized =
        serde_json::to_vec(value).map_err(|error| PluginCallError::ProtocolViolation {
            plugin_id: lease.plugin_id.clone(),
            message: format!("failed to measure plugin output: {error}"),
        })?;
    *output_bytes = output_bytes.saturating_add(serialized.len());
    if *output_bytes > lease.resource_policy.max_output_bytes() {
        return Err(PluginCallError::ResourceLimit {
            plugin_id: lease.plugin_id.clone(),
            resource: "invocation output bytes",
            limit: lease.resource_policy.max_output_bytes(),
        });
    }
    Ok(())
}

fn invocation_panic_execution<Response>(plugin_id: PluginId) -> InvocationExecution<Response> {
    let message = "plugin invocation boundary panicked".to_string();
    InvocationExecution::failed(
        PluginCallError::Unavailable {
            plugin_id,
            message: message.clone(),
        },
        message,
    )
}

fn invocation_duration_limit_error(lease: &InvocationLease) -> PluginCallError {
    PluginCallError::ResourceLimit {
        plugin_id: lease.plugin_id.clone(),
        resource: "invocation duration",
        limit: lease
            .resource_policy
            .max_invocation_duration()
            .as_millis()
            .try_into()
            .unwrap_or(usize::MAX),
    }
}

fn invocation_cancelled_error(
    lease: &InvocationLease,
    message: impl Into<String>,
) -> PluginCallError {
    PluginCallError::Cancelled {
        plugin_id: lease.plugin_id.clone(),
        message: message.into(),
    }
}

impl Default for ProcessPluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProcessPluginHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
pub enum PluginHostError {
    Kernel(KernelError),
    Catalog(CatalogError),
    Protocol(ProtocolError),
    Secret(yunxi_secret_broker::SecretError),
}

impl fmt::Display for PluginHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(error) => write!(formatter, "kernel operation failed: {error}"),
            Self::Catalog(error) => write!(formatter, "capability catalog failed: {error}"),
            Self::Protocol(error) => write!(formatter, "plugin protocol failed: {error}"),
            Self::Secret(error) => write!(formatter, "plugin secret broker failed: {error}"),
        }
    }
}

impl Error for PluginHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Kernel(error) => Some(error),
            Self::Catalog(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Secret(error) => Some(error),
        }
    }
}

impl From<KernelError> for PluginHostError {
    fn from(error: KernelError) -> Self {
        Self::Kernel(error)
    }
}

impl From<CatalogError> for PluginHostError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<ProtocolError> for PluginHostError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<yunxi_secret_broker::SecretError> for PluginHostError {
    fn from(error: yunxi_secret_broker::SecretError) -> Self {
        Self::Secret(error)
    }
}

#[derive(Debug)]
pub enum PluginCallError {
    Route(CatalogError),
    Codec(InvocationCodecError),
    Cancelled {
        plugin_id: PluginId,
        message: String,
    },
    Unavailable {
        plugin_id: PluginId,
        message: String,
    },
    ResourceLimit {
        plugin_id: PluginId,
        resource: &'static str,
        limit: usize,
    },
    Rejected {
        plugin_id: PluginId,
        code: String,
        message: String,
        retryable: bool,
    },
    ProtocolViolation {
        plugin_id: PluginId,
        message: String,
    },
}

impl fmt::Display for PluginCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Route(error) => error.fmt(formatter),
            Self::Codec(error) => error.fmt(formatter),
            Self::Cancelled { plugin_id, message } => {
                write!(
                    formatter,
                    "plugin {plugin_id} invocation was cancelled: {message}"
                )
            }
            Self::Unavailable { plugin_id, message } => {
                write!(formatter, "plugin `{plugin_id}` is unavailable: {message}")
            }
            Self::ResourceLimit {
                plugin_id,
                resource,
                limit,
            } => write!(
                formatter,
                "plugin `{plugin_id}` exceeded its {resource} limit ({limit})"
            ),
            Self::Rejected {
                plugin_id,
                code,
                message,
                retryable,
            } => {
                write!(
                    formatter,
                    "plugin `{plugin_id}` rejected the request ({code}): {message}"
                )?;
                if *retryable {
                    formatter.write_str(" [retryable]")?;
                }
                Ok(())
            }
            Self::ProtocolViolation { plugin_id, message } => {
                write!(
                    formatter,
                    "plugin `{plugin_id}` violated the protocol: {message}"
                )
            }
        }
    }
}

impl Error for PluginCallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Route(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::Cancelled { .. }
            | Self::Unavailable { .. }
            | Self::ResourceLimit { .. }
            | Self::Rejected { .. }
            | Self::ProtocolViolation { .. } => None,
        }
    }
}
