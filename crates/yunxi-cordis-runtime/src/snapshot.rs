//! Read-only runtime status projections and bounded failure details.

use std::fmt;

use yunxi_cordis_core::{FiberId, FiberSnapshot, FiberState, ScopeId, ScopeState};

use crate::manifest::{DefaultEnablement, PluginRisk, PluginRole};

/// Stable lifecycle state for a registered plugin slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginRuntimeState {
    Disabled,
    Pending,
    Mounting,
    Mounted,
    Failed,
    Unmounting,
}

impl fmt::Display for PluginRuntimeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Disabled => "disabled",
            Self::Pending => "pending",
            Self::Mounting => "mounting",
            Self::Mounted => "mounted",
            Self::Failed => "failed",
            Self::Unmounting => "unmounting",
        })
    }
}

/// Operation which produced a retained plugin failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailurePhase {
    Factory,
    Identity,
    Mount,
    Unmount,
}

impl fmt::Display for FailurePhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Factory => "factory",
            Self::Identity => "identity",
            Self::Mount => "mount",
            Self::Unmount => "unmount",
        })
    }
}

/// Bounded diagnostic information retained for one plugin slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureInfo {
    pub(crate) phase: FailurePhase,
    pub(crate) message: String,
    pub(crate) attempts: u32,
}

impl FailureInfo {
    pub(crate) fn new(phase: FailurePhase, message: String, attempts: u32) -> Self {
        Self {
            phase,
            message,
            attempts,
        }
    }

    pub fn phase(&self) -> FailurePhase {
        self.phase
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }
}

/// A stable status record for one registered plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSnapshot {
    id: String,
    display_name: String,
    role: PluginRole,
    risk: PluginRisk,
    default_enablement: DefaultEnablement,
    default_enabled: bool,
    override_value: Option<bool>,
    enabled: bool,
    state: PluginRuntimeState,
    fiber: Option<FiberSnapshot>,
    fiber_state: Option<FiberState>,
    failure: Option<FailureInfo>,
}

impl PluginSnapshot {
    pub(crate) fn new(
        manifest: crate::manifest::PluginManifest,
        override_value: Option<bool>,
        enabled: bool,
        state: PluginRuntimeState,
        fiber: Option<FiberSnapshot>,
        fiber_state: Option<FiberState>,
        failure: Option<FailureInfo>,
    ) -> Self {
        Self {
            id: manifest.id().to_owned(),
            display_name: manifest.display_name().to_owned(),
            role: manifest.role(),
            risk: manifest.risk(),
            default_enablement: manifest.default_enablement(),
            default_enabled: manifest.default_enabled(),
            override_value,
            enabled,
            state,
            fiber,
            fiber_state,
            failure,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub const fn role(&self) -> PluginRole {
        self.role
    }

    pub const fn risk(&self) -> PluginRisk {
        self.risk
    }

    pub const fn default_enablement(&self) -> DefaultEnablement {
        self.default_enablement
    }

    pub const fn default_enabled(&self) -> bool {
        self.default_enabled
    }

    pub const fn override_value(&self) -> Option<bool> {
        self.override_value
    }

    /// The resolved user intent, after applying an override.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub const fn state(&self) -> PluginRuntimeState {
        self.state
    }

    pub fn fiber(&self) -> Option<&FiberSnapshot> {
        self.fiber.as_ref()
    }

    pub fn fiber_id(&self) -> Option<FiberId> {
        self.fiber.as_ref().map(FiberSnapshot::id)
    }

    pub fn fiber_scope_id(&self) -> Option<ScopeId> {
        self.fiber.as_ref().map(FiberSnapshot::scope_id)
    }

    /// The current or last known core Fiber state. `None` means no fiber has
    /// been created for this entry yet.
    pub const fn fiber_state(&self) -> Option<FiberState> {
        self.fiber_state
    }

    pub fn failure(&self) -> Option<&FailureInfo> {
        self.failure.as_ref()
    }
}

/// A complete snapshot of the root context and all registered plugin slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    root_scope_id: ScopeId,
    root_state: ScopeState,
    started: bool,
    closed: bool,
    plugins: Vec<PluginSnapshot>,
}

impl RuntimeSnapshot {
    pub(crate) fn new(
        root_scope_id: ScopeId,
        root_state: ScopeState,
        started: bool,
        closed: bool,
        plugins: Vec<PluginSnapshot>,
    ) -> Self {
        Self {
            root_scope_id,
            root_state,
            started,
            closed,
            plugins,
        }
    }

    pub const fn root_scope_id(&self) -> ScopeId {
        self.root_scope_id
    }

    pub const fn root_state(&self) -> ScopeState {
        self.root_state
    }

    pub const fn started(&self) -> bool {
        self.started
    }

    pub const fn closed(&self) -> bool {
        self.closed
    }

    pub fn plugins(&self) -> &[PluginSnapshot] {
        &self.plugins
    }

    pub fn plugin(&self, plugin_id: &str) -> Option<&PluginSnapshot> {
        self.plugins.iter().find(|plugin| plugin.id() == plugin_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupFailure {
    plugin_id: String,
    required: bool,
    failure: FailureInfo,
}

impl StartupFailure {
    pub(crate) fn new(plugin_id: String, required: bool, failure: FailureInfo) -> Self {
        Self {
            plugin_id,
            required,
            failure,
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub const fn required(&self) -> bool {
        self.required
    }

    pub fn failure(&self) -> &FailureInfo {
        &self.failure
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct StartupReport {
    pub(crate) activated: Vec<String>,
    pub(crate) skipped: Vec<String>,
    pub(crate) failures: Vec<StartupFailure>,
}

impl StartupReport {
    pub fn activated(&self) -> &[String] {
        &self.activated
    }

    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    pub fn failures(&self) -> &[StartupFailure] {
        &self.failures
    }
}
