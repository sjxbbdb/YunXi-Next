//! Synchronous plugin lifecycle orchestration above `yunxi-cordis-core`.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use yunxi_cordis_core::{Context, CordisError, Fiber, FiberId, FiberSnapshot, FiberState, Plugin};

use crate::error::RuntimeError;
use crate::events::{RuntimeEventJournal, RuntimeEventKind};
use crate::manifest::PluginManifest;
use crate::registry::PluginRegistry;
use crate::snapshot::{
    FailureInfo, FailurePhase, PluginRuntimeState, PluginSnapshot, RuntimeSnapshot, StartupFailure,
    StartupReport,
};

const MAX_FAILURE_MESSAGE_BYTES: usize = 1024;
const MAX_MOUNT_ATTEMPTS: u32 = u8::MAX as u32;

struct PluginRecord {
    enabled: bool,
    lifecycle: PluginRuntimeState,
    fiber: Option<Fiber>,
    fiber_state: Option<FiberState>,
    failure: Option<FailureInfo>,
    mount_attempts: u32,
}

impl PluginRecord {
    fn new(manifest: PluginManifest) -> Self {
        let enabled = manifest.default_enabled();
        Self {
            enabled,
            lifecycle: if enabled {
                PluginRuntimeState::Pending
            } else {
                PluginRuntimeState::Disabled
            },
            fiber: None,
            fiber_state: None,
            failure: None,
            mount_attempts: 0,
        }
    }

    fn snapshot(
        &self,
        manifest: PluginManifest,
        override_value: Option<bool>,
    ) -> Result<PluginSnapshot, RuntimeError> {
        let fiber = self
            .fiber
            .as_ref()
            .map(Fiber::snapshot)
            .transpose()
            .map_err(RuntimeError::RootContextFailed)?;
        let fiber_state = fiber
            .as_ref()
            .map(FiberSnapshot::state)
            .or(self.fiber_state);
        Ok(PluginSnapshot::new(
            manifest,
            override_value,
            self.enabled,
            self.lifecycle,
            fiber,
            fiber_state,
            self.failure.clone(),
        ))
    }
}

/// The smallest useful Cordis meta-runtime for statically linked plugins.
pub struct CordisRuntime {
    registry: PluginRegistry,
    root: Context,
    records: BTreeMap<String, PluginRecord>,
    overrides: BTreeMap<String, bool>,
    events: RuntimeEventJournal,
    started: bool,
    closed: bool,
}

impl CordisRuntime {
    /// Build a runtime from a static definition slice and run the default
    /// startup policy immediately.
    pub fn from_static(
        definitions: &'static [crate::PluginDefinition],
    ) -> Result<Self, RuntimeError> {
        let registry = PluginRegistry::new(definitions)?;
        let mut runtime = Self::new(registry);
        runtime.start_default()?;
        Ok(runtime)
    }

    pub fn new(registry: PluginRegistry) -> Self {
        let records = registry
            .definitions()
            .iter()
            .map(|definition| {
                (
                    definition.id().to_owned(),
                    PluginRecord::new(definition.manifest()),
                )
            })
            .collect();
        Self {
            registry,
            root: Context::new(),
            records,
            overrides: BTreeMap::new(),
            events: RuntimeEventJournal::new(),
            started: false,
            closed: false,
        }
    }

    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    /// Return a clone of the root scope for service/event access. Plugin
    /// lifecycle changes should go through this runtime's static registry.
    pub fn root_context(&self) -> Context {
        self.root.clone()
    }

    pub const fn is_started(&self) -> bool {
        self.started
    }

    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Return lifecycle events after an opaque monotonically increasing
    /// cursor.  The page is bounded and reports when the cursor fell behind
    /// the retained window, allowing Web clients to fall back to a snapshot.
    pub fn events_since(&self, after_sequence: u64, limit: usize) -> crate::RuntimeEventPage {
        self.events.page(after_sequence, limit)
    }

    /// Mount all entries according to their manifest defaults and overrides.
    /// Optional failures are retained and do not stop later entries. A Core
    /// or AgentSpine failure is returned after the remaining entries have been
    /// attempted.
    pub fn start_default(&mut self) -> Result<StartupReport, RuntimeError> {
        self.ensure_open("start")?;
        self.record_event(RuntimeEventKind::StartupStarted, None, None, None);
        self.started = true;
        let ids = self
            .registry
            .definitions()
            .iter()
            .map(|definition| definition.id().to_owned())
            .collect::<Vec<_>>();
        let mut report = StartupReport::default();
        let mut required_error = None;
        let mut pending = Vec::new();

        for plugin_id in ids {
            let index = self.registry.index_of(&plugin_id)?;
            let definition = *self.registry.definition_at(index);
            let desired = self.effective_enabled(definition.manifest());
            if !desired {
                if self.has_fiber(index) {
                    if let Err(error) = self.unmount_index(index, false) {
                        let failure = self
                            .records
                            .get(definition.id())
                            .and_then(|record| record.failure.clone())
                            .unwrap_or_else(|| {
                                FailureInfo::new(
                                    FailurePhase::Unmount,
                                    bounded_message(error.to_string()),
                                    1,
                                )
                            });
                        report.failures.push(StartupFailure::new(
                            plugin_id,
                            definition.manifest().role().is_required(),
                            failure,
                        ));
                        continue;
                    }
                }
                self.deactivate_without_mount(index);
                self.record_event(
                    RuntimeEventKind::PluginSkipped,
                    Some(&plugin_id),
                    Some(PluginRuntimeState::Disabled),
                    None,
                );
                report.skipped.push(plugin_id);
                continue;
            }

            if self.has_fiber(index) {
                report.activated.push(plugin_id);
                continue;
            }

            pending.push(plugin_id);
        }

        let mut deferred_errors = BTreeMap::new();
        while !pending.is_empty() {
            let mut next = Vec::new();
            let mut mounted_this_pass = false;

            for plugin_id in pending {
                let index = self.registry.index_of(&plugin_id)?;
                let definition = *self.registry.definition_at(index);
                match self.mount_index(index) {
                    Ok(_) => {
                        mounted_this_pass = true;
                        deferred_errors.remove(&plugin_id);
                        report.activated.push(plugin_id);
                    }
                    Err(error) if is_missing_dependency(&error) => {
                        self.defer_missing_dependency(index);
                        deferred_errors.insert(plugin_id.clone(), error);
                        next.push(plugin_id);
                    }
                    Err(error) => {
                        let required = definition.manifest().role().is_required();
                        if required && required_error.is_none() {
                            required_error = Some(error.clone());
                        }
                        let failure = self
                            .records
                            .get(definition.id())
                            .and_then(|record| record.failure.clone())
                            .unwrap_or_else(|| {
                                FailureInfo::new(
                                    FailurePhase::Mount,
                                    bounded_message(error.to_string()),
                                    1,
                                )
                            });
                        report
                            .failures
                            .push(StartupFailure::new(plugin_id, required, failure));
                    }
                }
            }

            if next.is_empty() {
                break;
            }
            if mounted_this_pass {
                pending = next;
                continue;
            }

            // Every remaining plugin is waiting on a service that no pending
            // plugin could provide. This covers absent dependencies and cycles
            // without making registry order observable.
            for plugin_id in next {
                let index = self.registry.index_of(&plugin_id)?;
                let definition = *self.registry.definition_at(index);
                let error = deferred_errors
                    .remove(&plugin_id)
                    .expect("deferred plugin retains its dependency error");
                self.record_failure(index, FailurePhase::Mount, error.to_string());
                let required = definition.manifest().role().is_required();
                if required && required_error.is_none() {
                    required_error = Some(error.clone());
                }
                let failure = self
                    .records
                    .get(definition.id())
                    .and_then(|record| record.failure.clone())
                    .expect("dependency failure was recorded");
                report
                    .failures
                    .push(StartupFailure::new(plugin_id, required, failure));
            }
            break;
        }

        required_error.map_or(Ok(report), Err)
    }

    /// Explicitly mount one plugin and persist an enable override for optional
    /// entries. Core and AgentSpine entries may be mounted, but not disabled.
    pub fn mount(&mut self, plugin_id: &str) -> Result<FiberId, RuntimeError> {
        self.ensure_open("mount")?;
        let index = self.registry.index_of(plugin_id)?;
        if !self
            .registry
            .definition_at(index)
            .manifest()
            .role()
            .is_required()
        {
            self.overrides.insert(plugin_id.to_owned(), true);
        }
        if !self.has_fiber(index) {
            self.reset_for_manual_mount(index);
        }
        self.mount_index(index)
    }

    /// Disable an optional plugin. Its Fiber is unmounted before it is
    /// removed from the runtime's index.
    pub fn disable(&mut self, plugin_id: &str) -> Result<(), RuntimeError> {
        self.set_enabled(plugin_id, false)
    }

    /// Enable an optional plugin and mount it immediately once startup has
    /// begun. Before startup, the override is applied on the next start.
    pub fn enable(&mut self, plugin_id: &str) -> Result<(), RuntimeError> {
        self.set_enabled(plugin_id, true)
    }

    pub fn set_enabled(&mut self, plugin_id: &str, enabled: bool) -> Result<(), RuntimeError> {
        self.ensure_open("change plugin state")?;
        let index = self.registry.index_of(plugin_id)?;
        let manifest = self.registry.definition_at(index).manifest();
        if !enabled && manifest.role().is_required() {
            return Err(RuntimeError::CorePluginCannotDisable {
                plugin_id: plugin_id.to_owned(),
                role: manifest.role(),
            });
        }

        self.overrides.insert(plugin_id.to_owned(), enabled);
        if !self.started {
            if enabled {
                let record = self
                    .records
                    .get_mut(plugin_id)
                    .expect("registry record exists");
                record.enabled = true;
                record.failure = None;
                record.mount_attempts = 0;
                record.lifecycle = if record.fiber.is_some() {
                    PluginRuntimeState::Mounted
                } else {
                    record.fiber_state = None;
                    PluginRuntimeState::Pending
                };
            } else {
                if self.has_fiber(index) {
                    self.unmount_index(index, false)?;
                } else {
                    self.deactivate_without_mount(index);
                }
            }
            return Ok(());
        }

        if enabled {
            if self.has_fiber(index) {
                return Ok(());
            }
            self.reset_for_manual_mount(index);
            self.mount_index(index).map(|_| ())
        } else {
            self.unmount_index(index, false)
        }
    }

    /// Remove a user-toggleable plugin's Fiber and persist the disabled state.
    pub fn unmount(&mut self, plugin_id: &str) -> Result<(), RuntimeError> {
        self.disable(plugin_id)
    }

    /// Return to the manifest default for an optional plugin.
    pub fn clear_override(&mut self, plugin_id: &str) -> Result<(), RuntimeError> {
        self.ensure_open("clear plugin override")?;
        let index = self.registry.index_of(plugin_id)?;
        let manifest = self.registry.definition_at(index).manifest();
        self.overrides.remove(plugin_id);
        if manifest.role().is_required() {
            return Ok(());
        }
        let desired = manifest.default_enabled();
        if !self.started {
            if desired {
                let record = self
                    .records
                    .get_mut(plugin_id)
                    .expect("registry record exists");
                record.enabled = true;
                record.failure = None;
                record.mount_attempts = 0;
                record.lifecycle = if record.fiber.is_some() {
                    PluginRuntimeState::Mounted
                } else {
                    record.fiber_state = None;
                    PluginRuntimeState::Pending
                };
            } else {
                if self.has_fiber(index) {
                    self.unmount_index(index, false)?;
                } else {
                    self.deactivate_without_mount(index);
                }
            }
            return Ok(());
        }
        if desired {
            if self.has_fiber(index) {
                Ok(())
            } else {
                self.mount_index(index).map(|_| ())
            }
        } else {
            self.unmount_index(index, false)
        }
    }

    pub fn plugin(&self, plugin_id: &str) -> Result<PluginSnapshot, RuntimeError> {
        let index = self.registry.index_of(plugin_id)?;
        let definition = self.registry.definition_at(index);
        self.records
            .get(plugin_id)
            .expect("registry record exists")
            .snapshot(
                definition.manifest(),
                self.overrides.get(plugin_id).copied(),
            )
    }

    /// Query a read-only snapshot of the core Fiber owned by a plugin.
    ///
    /// A live `Fiber` handle is intentionally not exposed: its public
    /// `unmount` method would otherwise bypass the runtime's Core/AgentSpine
    /// switch invariant.
    pub fn fiber(&self, plugin_id: &str) -> Result<Option<FiberSnapshot>, RuntimeError> {
        self.registry.index_of(plugin_id)?;
        self.records
            .get(plugin_id)
            .expect("registry record exists")
            .fiber
            .as_ref()
            .map(Fiber::snapshot)
            .transpose()
            .map_err(RuntimeError::RootContextFailed)
    }

    /// Query a read-only Fiber snapshot by its core-generated id.
    pub fn fiber_by_id(&self, fiber_id: FiberId) -> Result<Option<FiberSnapshot>, RuntimeError> {
        self.records
            .values()
            .find_map(|record| record.fiber.as_ref().filter(|fiber| fiber.id() == fiber_id))
            .map(Fiber::snapshot)
            .transpose()
            .map_err(RuntimeError::RootContextFailed)
    }

    pub fn snapshot(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        let plugins = self
            .registry
            .definitions()
            .iter()
            .map(|definition| {
                self.records
                    .get(definition.id())
                    .expect("registry record exists")
                    .snapshot(
                        definition.manifest(),
                        self.overrides.get(definition.id()).copied(),
                    )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RuntimeSnapshot::new(
            self.root.id(),
            self.root.state().map_err(RuntimeError::RootContextFailed)?,
            self.started,
            self.closed,
            plugins,
        ))
    }

    /// Stop every plugin, including required entries, and dispose the root
    /// context. This is a runtime shutdown operation, not a user disable.
    pub fn shutdown(&mut self) -> Result<(), RuntimeError> {
        if self.closed {
            return Ok(());
        }
        self.record_event(RuntimeEventKind::ShutdownStarted, None, None, None);
        let indexes = (0..self.registry.len()).rev().collect::<Vec<_>>();
        let mut first_error = None;
        for index in indexes {
            if self.has_fiber(index) {
                if let Err(error) = self.unmount_index(index, true) {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            } else {
                self.deactivate_without_mount(index);
            }
        }
        if let Err(error) = self.root.dispose().map_err(RuntimeError::RootContextFailed) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        self.started = false;
        self.closed = true;
        self.record_event(RuntimeEventKind::ShutdownCompleted, None, None, None);
        first_error.map_or(Ok(()), Err)
    }

    fn ensure_open(&self, operation: &'static str) -> Result<(), RuntimeError> {
        if self.closed {
            Err(RuntimeError::RuntimeClosed { operation })
        } else {
            Ok(())
        }
    }

    fn effective_enabled(&self, manifest: PluginManifest) -> bool {
        if manifest.role().is_required() {
            true
        } else {
            self.overrides
                .get(manifest.id())
                .copied()
                .unwrap_or_else(|| manifest.default_enabled())
        }
    }

    fn has_fiber(&self, index: usize) -> bool {
        let id = self.registry.definition_at(index).id();
        self.records
            .get(id)
            .expect("registry record exists")
            .fiber
            .is_some()
    }

    fn deactivate_without_mount(&mut self, index: usize) {
        let id = self.registry.definition_at(index).id();
        let record = self.records.get_mut(id).expect("registry record exists");
        record.enabled = false;
        record.lifecycle = PluginRuntimeState::Disabled;
        record.failure = None;
        record.mount_attempts = 0;
        if record.fiber.is_none() {
            record.fiber_state = None;
        }
        self.record_event(
            RuntimeEventKind::PluginDisabled,
            Some(id),
            Some(PluginRuntimeState::Disabled),
            None,
        );
    }

    fn reset_for_manual_mount(&mut self, index: usize) {
        let id = self.registry.definition_at(index).id();
        let record = self.records.get_mut(id).expect("registry record exists");
        record.failure = None;
        record.mount_attempts = 0;
        record.fiber_state = None;
        record.lifecycle = PluginRuntimeState::Pending;
    }

    fn defer_missing_dependency(&mut self, index: usize) {
        let id = self.registry.definition_at(index).id();
        let record = self.records.get_mut(id).expect("registry record exists");
        record.lifecycle = PluginRuntimeState::Pending;
        record.fiber_state = None;
        record.failure = None;
        record.mount_attempts = 0;
    }

    fn mount_index(&mut self, index: usize) -> Result<FiberId, RuntimeError> {
        let definition = *self.registry.definition_at(index);
        let manifest = definition.manifest();
        let id = manifest.id();
        {
            let record = self.records.get_mut(id).expect("registry record exists");
            if record.fiber.is_some() {
                return Err(RuntimeError::InvalidPluginState {
                    plugin_id: id.to_owned(),
                    state: record.lifecycle,
                    operation: "mount",
                });
            }
            record.enabled = true;
            record.lifecycle = PluginRuntimeState::Mounting;
            record.fiber_state = Some(FiberState::Mounting);
            record.failure = None;
            record.mount_attempts = record
                .mount_attempts
                .saturating_add(1)
                .min(MAX_MOUNT_ATTEMPTS);
        }
        self.record_event(
            RuntimeEventKind::PluginMounting,
            Some(id),
            Some(PluginRuntimeState::Mounting),
            None,
        );

        let plugin = match catch_unwind(AssertUnwindSafe(|| definition.factory().instantiate())) {
            Ok(plugin) => plugin,
            Err(_) => {
                let error = RuntimeError::PluginFactoryPanicked {
                    plugin_id: id.to_owned(),
                };
                self.record_failure(index, FailurePhase::Factory, error.to_string());
                return Err(error);
            }
        };

        let actual_id = match catch_unwind(AssertUnwindSafe(|| plugin.id().to_owned())) {
            Ok(actual_id) => actual_id,
            Err(_) => {
                let error = RuntimeError::PluginIdentityPanicked {
                    plugin_id: id.to_owned(),
                };
                self.record_failure(index, FailurePhase::Identity, error.to_string());
                return Err(error);
            }
        };
        if actual_id != id {
            let error = RuntimeError::PluginIdentityMismatch {
                expected: id.to_owned(),
                actual: bounded_message(actual_id),
            };
            self.record_failure(index, FailurePhase::Identity, error.to_string());
            return Err(error);
        }

        let registered_plugin = RegisteredPlugin {
            expected_id: id,
            inner: plugin,
        };
        match self.root.mount(registered_plugin) {
            Ok(fiber) => {
                let fiber_id = fiber.id();
                let record = self.records.get_mut(id).expect("registry record exists");
                record.lifecycle = PluginRuntimeState::Mounted;
                record.fiber_state = Some(FiberState::Mounted);
                record.failure = None;
                record.mount_attempts = 0;
                record.fiber = Some(fiber);
                self.record_event(
                    RuntimeEventKind::PluginMounted,
                    Some(id),
                    Some(PluginRuntimeState::Mounted),
                    None,
                );
                Ok(fiber_id)
            }
            Err(cause) => {
                let error = RuntimeError::PluginMountFailed {
                    plugin_id: id.to_owned(),
                    cause,
                };
                self.record_failure(index, FailurePhase::Mount, error.to_string());
                Err(error)
            }
        }
    }

    fn unmount_index(&mut self, index: usize, allow_required: bool) -> Result<(), RuntimeError> {
        let definition = *self.registry.definition_at(index);
        let manifest = definition.manifest();
        let id = manifest.id();
        if !allow_required && manifest.role().is_required() {
            return Err(RuntimeError::CorePluginCannotDisable {
                plugin_id: id.to_owned(),
                role: manifest.role(),
            });
        }

        // Emit the transition before borrowing the record.  The journal is
        // owned by this runtime, so keeping the record borrow short avoids a
        // mutable-borrow overlap while the Fiber performs its disposer.
        self.record_event(
            RuntimeEventKind::PluginUnmounting,
            Some(id),
            Some(PluginRuntimeState::Unmounting),
            None,
        );
        let result = {
            let record = self.records.get_mut(id).expect("registry record exists");
            let Some(fiber) = record.fiber.as_ref() else {
                record.enabled = false;
                record.lifecycle = PluginRuntimeState::Disabled;
                record.failure = None;
                record.mount_attempts = 0;
                record.fiber_state = None;
                return Ok(());
            };
            record.lifecycle = PluginRuntimeState::Unmounting;
            record.fiber_state = Some(FiberState::Unmounting);
            fiber.unmount()
        };

        let failure_message = {
            let record = self.records.get_mut(id).expect("registry record exists");
            let _removed_fiber = record.fiber.take();
            record.enabled = false;
            record.lifecycle = PluginRuntimeState::Disabled;
            record.fiber_state = Some(FiberState::Unmounted);
            record.mount_attempts = 0;
            match result {
                Ok(()) => {
                    record.failure = None;
                    None
                }
                Err(cause) => {
                    let error = RuntimeError::PluginUnmountFailed {
                        plugin_id: id.to_owned(),
                        cause,
                    };
                    let message = bounded_message(error.to_string());
                    record.failure =
                        Some(FailureInfo::new(FailurePhase::Unmount, message.clone(), 1));
                    Some((message, error))
                }
            }
        };
        match failure_message {
            None => {
                self.record_event(
                    RuntimeEventKind::PluginDisabled,
                    Some(id),
                    Some(PluginRuntimeState::Disabled),
                    None,
                );
                Ok(())
            }
            Some((message, error)) => {
                self.record_event(
                    RuntimeEventKind::PluginFailed,
                    Some(id),
                    Some(PluginRuntimeState::Failed),
                    Some(&message),
                );
                Err(error)
            }
        }
    }

    fn record_failure(&mut self, index: usize, phase: FailurePhase, message: String) {
        let id = self.registry.definition_at(index).id();
        let failure_message = {
            let record = self.records.get_mut(id).expect("registry record exists");
            record.lifecycle = PluginRuntimeState::Failed;
            record.fiber_state = match phase {
                FailurePhase::Factory | FailurePhase::Identity => None,
                FailurePhase::Mount => Some(FiberState::Failed),
                FailurePhase::Unmount => Some(FiberState::Unmounted),
            };
            let message = bounded_message(message);
            record.failure = Some(FailureInfo::new(
                phase,
                message.clone(),
                record.mount_attempts.max(1),
            ));
            message
        };
        self.record_event(
            RuntimeEventKind::PluginFailed,
            Some(id),
            Some(PluginRuntimeState::Failed),
            Some(&failure_message),
        );
    }

    fn record_event(
        &mut self,
        kind: RuntimeEventKind,
        plugin_id: Option<&str>,
        state: Option<PluginRuntimeState>,
        message: Option<&str>,
    ) {
        self.events.record(kind, plugin_id, state, message);
    }
}

impl Drop for CordisRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct RegisteredPlugin {
    expected_id: &'static str,
    inner: Box<dyn Plugin>,
}

impl Plugin for RegisteredPlugin {
    fn id(&self) -> &str {
        self.expected_id
    }

    fn dependencies(&self) -> Vec<yunxi_cordis_core::ServiceDependency> {
        self.inner.dependencies()
    }

    fn mount(&self, context: &Context) -> Result<(), CordisError> {
        self.inner.mount(context)
    }
}

fn bounded_message(message: impl Into<String>) -> String {
    let message = message.into();
    if message.len() <= MAX_FAILURE_MESSAGE_BYTES {
        return message;
    }
    let mut end = MAX_FAILURE_MESSAGE_BYTES.saturating_sub(3);
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &message[..end])
}

fn is_missing_dependency(error: &RuntimeError) -> bool {
    matches!(
        error,
        RuntimeError::PluginMountFailed {
            cause: CordisError::MissingDependency { .. },
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DefaultEnablement, PluginDefinition, PluginFactory, PluginManifest, PluginRegistry,
        PluginRisk, PluginRole,
    };
    use yunxi_cordis_core::ScopeState;

    struct LifecyclePlugin {
        id: &'static str,
    }

    impl Plugin for LifecyclePlugin {
        fn id(&self) -> &str {
            self.id
        }

        fn mount(&self, _context: &Context) -> Result<(), CordisError> {
            Ok(())
        }
    }

    fn safe_factory() -> Box<dyn Plugin> {
        Box::new(LifecyclePlugin { id: "runtime.safe" })
    }

    fn external_factory() -> Box<dyn Plugin> {
        Box::new(LifecyclePlugin {
            id: "runtime.external",
        })
    }

    fn failing_factory() -> Box<dyn Plugin> {
        Box::new(FailingPlugin)
    }

    struct FailingPlugin;

    impl Plugin for FailingPlugin {
        fn id(&self) -> &str {
            "runtime.failing"
        }

        fn mount(&self, _context: &Context) -> Result<(), CordisError> {
            Err(CordisError::PluginMountFailed {
                plugin: yunxi_cordis_core::PluginId::new("runtime.failing").unwrap(),
                message: "intentional test failure".to_owned(),
            })
        }
    }

    fn core_factory() -> Box<dyn Plugin> {
        Box::new(LifecyclePlugin { id: "runtime.core" })
    }

    static DEFAULTS: [PluginDefinition; 2] = [
        PluginDefinition::new(
            PluginManifest::safe_optional("runtime.safe", "Safe"),
            PluginFactory::new(safe_factory),
        ),
        PluginDefinition::new(
            PluginManifest::external_optional("runtime.external", "External"),
            PluginFactory::new(external_factory),
        ),
    ];

    static WITH_FAILURE: [PluginDefinition; 2] = [
        PluginDefinition::new(
            PluginManifest::safe_optional("runtime.failing", "Failing"),
            PluginFactory::new(failing_factory),
        ),
        PluginDefinition::new(
            PluginManifest::safe_optional("runtime.safe", "Safe"),
            PluginFactory::new(safe_factory),
        ),
    ];

    static WITH_CORE: [PluginDefinition; 1] = [PluginDefinition::new(
        PluginManifest::new(
            "runtime.core",
            "Core",
            PluginRole::Core,
            PluginRisk::Safe,
            DefaultEnablement::Always,
        ),
        PluginFactory::new(core_factory),
    )];

    #[test]
    fn defaults_start_safe_optional_and_skip_external_optional() {
        let registry = PluginRegistry::new(&DEFAULTS).unwrap();
        let mut runtime = CordisRuntime::new(registry);
        let report = runtime.start_default().unwrap();
        assert_eq!(report.activated(), &["runtime.safe"]);
        assert_eq!(report.skipped(), &["runtime.external"]);
        assert_eq!(
            runtime.plugin("runtime.safe").unwrap().state(),
            PluginRuntimeState::Mounted
        );
        assert_eq!(
            runtime.plugin("runtime.external").unwrap().state(),
            PluginRuntimeState::Disabled
        );
    }

    #[test]
    fn user_override_wins_and_disable_removes_the_fiber() {
        let registry = PluginRegistry::new(&DEFAULTS).unwrap();
        let mut runtime = CordisRuntime::new(registry);
        runtime.enable("runtime.external").unwrap();
        runtime.start_default().unwrap();
        let fiber_id = runtime
            .plugin("runtime.external")
            .unwrap()
            .fiber()
            .unwrap()
            .id();
        assert!(runtime.fiber_by_id(fiber_id).unwrap().is_some());
        runtime.disable("runtime.external").unwrap();
        let snapshot = runtime.plugin("runtime.external").unwrap();
        assert!(!snapshot.enabled());
        assert_eq!(snapshot.state(), PluginRuntimeState::Disabled);
        assert_eq!(snapshot.fiber(), None);
        assert_eq!(snapshot.fiber_state(), Some(FiberState::Unmounted));
        assert!(runtime.fiber_by_id(fiber_id).unwrap().is_none());
    }

    #[test]
    fn optional_mount_failure_is_isolated_from_other_plugins() {
        let registry = PluginRegistry::new(&WITH_FAILURE).unwrap();
        let mut runtime = CordisRuntime::new(registry);
        let report = runtime.start_default().unwrap();
        assert_eq!(report.activated(), &["runtime.safe"]);
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].plugin_id(), "runtime.failing");
        let failed = runtime.plugin("runtime.failing").unwrap();
        assert_eq!(failed.state(), PluginRuntimeState::Failed);
        assert_eq!(failed.fiber_state(), Some(FiberState::Failed));
        assert_eq!(failed.failure().unwrap().phase(), FailurePhase::Mount);
        assert_eq!(
            runtime.plugin("runtime.safe").unwrap().state(),
            PluginRuntimeState::Mounted
        );
    }

    #[test]
    fn core_and_agent_spine_cannot_be_disabled() {
        let registry = PluginRegistry::new(&WITH_CORE).unwrap();
        let mut runtime = CordisRuntime::new(registry);
        assert!(matches!(
            runtime.disable("runtime.core"),
            Err(RuntimeError::CorePluginCannotDisable {
                role: PluginRole::Core,
                ..
            })
        ));
        runtime.start_default().unwrap();
        assert!(matches!(
            runtime.set_enabled("runtime.core", false),
            Err(RuntimeError::CorePluginCannotDisable { .. })
        ));
        assert_eq!(
            runtime.plugin("runtime.core").unwrap().state(),
            PluginRuntimeState::Mounted
        );
    }

    #[test]
    fn duplicate_unknown_and_repeated_mounts_are_structured_errors() {
        let registry = PluginRegistry::new(&DEFAULTS).unwrap();
        let mut runtime = CordisRuntime::new(registry);
        assert!(matches!(
            runtime.plugin("missing"),
            Err(RuntimeError::UnknownPlugin { .. })
        ));
        runtime.mount("runtime.external").unwrap();
        assert!(matches!(
            runtime.mount("runtime.external"),
            Err(RuntimeError::InvalidPluginState {
                operation: "mount",
                ..
            })
        ));
    }

    #[test]
    fn shutdown_can_stop_required_plugins_and_closes_runtime() {
        let registry = PluginRegistry::new(&WITH_CORE).unwrap();
        let mut runtime = CordisRuntime::new(registry);
        runtime.start_default().unwrap();
        runtime.shutdown().unwrap();
        assert!(runtime.is_closed());
        assert!(matches!(
            runtime.start_default(),
            Err(RuntimeError::RuntimeClosed { operation: "start" })
        ));
        assert_eq!(
            runtime.snapshot().unwrap().root_state(),
            ScopeState::Disposed
        );
    }
}
