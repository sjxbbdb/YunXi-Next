//! Process lifecycle and typed invocation over the capability catalog.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
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
    HostPluginSession, InvocationCodecError, InvocationRequest, PluginAcceptor,
    PluginConnectionInfo, PluginMessage, ProtocolError,
};

use crate::{
    CapabilityCatalog, CatalogError, MAX_AUTOMATIC_RESTARTS, RetryAction, RetryController,
    RetrySnapshot,
};

const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    connections: BTreeMap<PluginId, ActiveConnection>,
    slots: BTreeMap<PluginId, PluginSlot>,
    next_request_id: u64,
}

impl ProcessPluginHost {
    pub fn new() -> Self {
        Self {
            kernel: YunxiKernel::new(),
            catalog: CapabilityCatalog::new(),
            connections: BTreeMap::new(),
            slots: BTreeMap::new(),
            next_request_id: 1,
        }
    }

    pub fn launch(&mut self, launch: PluginLaunch) -> Result<PluginId, PluginHostError> {
        let id = launch.id.clone();
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

        let connection_generation = self
            .connections
            .get(&plugin_id)
            .map(|connection| connection.generation);
        let send_error = match self.connections.get_mut(&plugin_id) {
            Some(connection) => connection
                .session
                .send(&HostMessage::Invoke { request })
                .err()
                .map(|error| error.to_string()),
            None => Some("plugin connection is not available".to_string()),
        };
        if let Some(message) = send_error {
            self.fail_connection(&plugin_id, connection_generation, message.clone());
            return Err(PluginCallError::Unavailable { plugin_id, message });
        }

        let response = match self.connections.get_mut(&plugin_id) {
            Some(connection) => match connection.session.receive() {
                Ok(response) => response,
                Err(error) => {
                    let message = error.to_string();
                    self.fail_connection(&plugin_id, connection_generation, message.clone());
                    return Err(PluginCallError::Unavailable { plugin_id, message });
                }
            },
            None => {
                let message = "plugin connection closed after request send".to_string();
                self.fail_connection(&plugin_id, connection_generation, message.clone());
                return Err(PluginCallError::Unavailable { plugin_id, message });
            }
        };

        match response {
            PluginMessage::InvocationCompleted { response }
                if response.request_id() == request_id =>
            {
                match response.decode_payload() {
                    Ok(response) => Ok(response),
                    Err(error) => {
                        let message = error.to_string();
                        self.fail_connection(&plugin_id, connection_generation, message.clone());
                        Err(PluginCallError::ProtocolViolation { plugin_id, message })
                    }
                }
            }
            PluginMessage::InvocationFailed {
                request_id: response_id,
                code,
                message,
                retryable,
            } if response_id == request_id => Err(PluginCallError::Rejected {
                plugin_id,
                code,
                message,
                retryable,
            }),
            message => {
                let message =
                    format!("expected response for request {request_id}, received {message:?}");
                self.fail_connection(&plugin_id, connection_generation, message.clone());
                Err(PluginCallError::ProtocolViolation { plugin_id, message })
            }
        }
    }

    pub fn stop(&mut self, id: &PluginId) {
        let _ignored = self.disable(id);
    }

    /// Stops routing and the managed process until an explicit `enable` call.
    pub fn disable(&mut self, id: &PluginId) -> Result<(), PluginHostError> {
        self.ensure_slot(id)?;
        self.disable_retry(id);
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

    pub fn connection_count(&self) -> usize {
        self.connections.len()
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
        let ids = self.connections.keys().cloned().collect::<Vec<_>>();
        for id in &ids {
            if let Some(connection) = self.connections.get_mut(id) {
                let _ignored = connection.session.send(&HostMessage::Shutdown);
            }
        }
        self.connections.clear();
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
        self.kernel.start(id)?;

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
            )
        };

        let connection = {
            let slot = self
                .slots
                .get(id)
                .expect("plugin slot exists after ensure_slot");
            slot.acceptor.accept(id.as_str(), handshake_timeout)
        };
        let connection = match connection {
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
}

impl fmt::Display for PluginHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(error) => write!(formatter, "kernel operation failed: {error}"),
            Self::Catalog(error) => write!(formatter, "capability catalog failed: {error}"),
            Self::Protocol(error) => write!(formatter, "plugin protocol failed: {error}"),
        }
    }
}

impl Error for PluginHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Kernel(error) => Some(error),
            Self::Catalog(error) => Some(error),
            Self::Protocol(error) => Some(error),
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

#[derive(Debug)]
pub enum PluginCallError {
    Route(CatalogError),
    Codec(InvocationCodecError),
    Unavailable {
        plugin_id: PluginId,
        message: String,
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
            Self::Unavailable { plugin_id, message } => {
                write!(formatter, "plugin `{plugin_id}` is unavailable: {message}")
            }
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
            Self::Unavailable { .. } | Self::Rejected { .. } | Self::ProtocolViolation { .. } => {
                None
            }
        }
    }
}
