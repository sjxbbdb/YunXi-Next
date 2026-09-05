//! Browser-safe point-in-time projections used by the Gateway.

use serde::Serialize;
use yunxi_composition::PluginInventorySnapshot;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    status: String,
    kernel: String,
    plugin: String,
    protocol_ready: bool,
    plugins: usize,
    capabilities: usize,
    failed_plugins: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

impl GatewayStatus {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kernel: impl Into<String>,
        plugin: impl Into<String>,
        protocol_ready: bool,
        plugins: usize,
        capabilities: usize,
        failed_plugins: usize,
    ) -> Self {
        let kernel = kernel.into();
        let plugin = plugin.into();
        let status = if kernel == "running" && protocol_ready {
            "ok"
        } else {
            "degraded"
        };
        Self {
            status: status.to_string(),
            kernel,
            plugin,
            protocol_ready,
            plugins,
            capabilities,
            failed_plugins,
            provider: None,
            model: None,
        }
    }

    pub fn with_model(mut self, provider: impl Into<String>, model: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn kernel(&self) -> &str {
        &self.kernel
    }

    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    pub fn protocol_ready(&self) -> bool {
        self.protocol_ready
    }

    pub fn plugins(&self) -> usize {
        self.plugins
    }

    pub fn capabilities(&self) -> usize {
        self.capabilities
    }

    pub fn failed_plugins(&self) -> usize {
        self.failed_plugins
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySessionSummary {
    session_id: String,
    updated_at: u64,
    running: bool,
    blank: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

impl GatewaySessionSummary {
    pub fn new(session_id: impl Into<String>, updated_at: u64, running: bool, blank: bool) -> Self {
        Self {
            session_id: session_id.into(),
            updated_at,
            running,
            blank,
            parent_session_id: None,
            origin: None,
            cwd: None,
        }
    }

    pub fn with_parent_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.parent_session_id = Some(session_id.into());
        self
    }

    pub fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    pub fn with_subagent_origin(self) -> Self {
        self.with_origin("subagent")
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn updated_at(&self) -> u64 {
        self.updated_at
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn blank(&self) -> bool {
        self.blank
    }

    pub fn parent_session_id(&self) -> Option<&str> {
        self.parent_session_id.as_deref()
    }

    pub fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayProjection {
    status: GatewayStatus,
    inventory: PluginInventorySnapshot,
    sessions: Vec<GatewaySessionSummary>,
    cwd: String,
    home: String,
}

impl GatewayProjection {
    pub fn new(status: GatewayStatus, inventory: PluginInventorySnapshot) -> Self {
        Self {
            status,
            inventory,
            sessions: Vec::new(),
            cwd: ".".to_string(),
            home: ".".to_string(),
        }
    }

    pub fn with_sessions(
        mut self,
        sessions: impl IntoIterator<Item = GatewaySessionSummary>,
    ) -> Self {
        self.sessions = sessions.into_iter().collect();
        self
    }

    pub fn with_host_paths(mut self, cwd: impl Into<String>, home: impl Into<String>) -> Self {
        self.cwd = cwd.into();
        self.home = home.into();
        self
    }

    pub fn status(&self) -> &GatewayStatus {
        &self.status
    }

    pub fn inventory(&self) -> &PluginInventorySnapshot {
        &self.inventory
    }

    pub fn sessions(&self) -> &[GatewaySessionSummary] {
        &self.sessions
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn home(&self) -> &str {
        &self.home
    }
}
