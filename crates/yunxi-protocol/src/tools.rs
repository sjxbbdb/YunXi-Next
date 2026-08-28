//! Versioned action requests for host-approved shell and patch plugins.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{NetworkGrant, SecretGrant, WorkspaceGrant};

pub const TOOL_SHELL_EXECUTE_OPERATION: &str = "execute";
pub const TOOL_PATCH_APPLY_OPERATION: &str = "apply";

const DEFAULT_ACTION_TIMEOUT_MILLIS: u64 = 120_000;
const DEFAULT_ACTION_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_ACTION_TIMEOUT_MILLIS: u64 = 600_000;
const MAX_ACTION_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionGrant {
    workspace: WorkspaceGrant,
    working_directory: PathBuf,
    approval: ActionApproval,
    allow_write: bool,
    allow_network: bool,
    #[serde(default, skip_serializing_if = "NetworkGrant::is_empty")]
    network: NetworkGrant,
    #[serde(default, skip_serializing_if = "SecretGrant::is_empty")]
    secrets: SecretGrant,
    timeout_millis: u64,
    max_output_bytes: usize,
}

impl ActionGrant {
    pub fn pending(workspace: WorkspaceGrant, working_directory: impl Into<PathBuf>) -> Self {
        Self {
            workspace,
            working_directory: working_directory.into(),
            approval: ActionApproval::Denied {
                reason: "explicit user approval is required".to_string(),
            },
            allow_write: false,
            allow_network: false,
            network: NetworkGrant::none(),
            secrets: SecretGrant::empty(),
            timeout_millis: DEFAULT_ACTION_TIMEOUT_MILLIS,
            max_output_bytes: DEFAULT_ACTION_OUTPUT_BYTES,
        }
    }

    pub fn approved(
        workspace: WorkspaceGrant,
        working_directory: impl Into<PathBuf>,
        ticket: impl Into<String>,
    ) -> Self {
        Self {
            workspace,
            working_directory: working_directory.into(),
            approval: ActionApproval::Approved {
                ticket: ticket.into(),
            },
            allow_write: false,
            allow_network: false,
            network: NetworkGrant::none(),
            secrets: SecretGrant::empty(),
            timeout_millis: DEFAULT_ACTION_TIMEOUT_MILLIS,
            max_output_bytes: DEFAULT_ACTION_OUTPUT_BYTES,
        }
    }

    pub fn with_write(mut self, allow: bool) -> Self {
        self.allow_write = allow;
        self
    }

    pub fn with_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        if !allow {
            self.network = NetworkGrant::none();
        }
        self
    }

    /// Adds an exact network authority to this action.
    ///
    /// The old `with_network(true)` API remains available for legacy tools,
    /// but MCP HTTP calls use this scoped form.
    pub fn with_network_grant(mut self, grant: NetworkGrant) -> Self {
        self.allow_network = !grant.is_empty();
        self.network = grant;
        self
    }

    /// Adds secret references without carrying any secret values.
    pub fn with_secret_grant(mut self, grant: SecretGrant) -> Self {
        self.secrets = grant;
        self
    }

    pub fn with_limits(mut self, timeout_millis: u64, max_output_bytes: usize) -> Self {
        self.timeout_millis = timeout_millis;
        self.max_output_bytes = max_output_bytes;
        self
    }

    pub fn workspace(&self) -> &WorkspaceGrant {
        &self.workspace
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub fn approval(&self) -> &ActionApproval {
        &self.approval
    }

    pub fn is_approved(&self) -> bool {
        matches!(self.approval, ActionApproval::Approved { .. })
    }

    pub fn approval_ticket(&self) -> Option<&str> {
        match &self.approval {
            ActionApproval::Approved { ticket } => Some(ticket),
            ActionApproval::Denied { .. } => None,
        }
    }

    pub fn allow_write(&self) -> bool {
        self.allow_write
    }

    pub fn allow_network(&self) -> bool {
        self.allow_network
    }

    pub fn network_grant(&self) -> &NetworkGrant {
        &self.network
    }

    pub fn secret_grant(&self) -> &SecretGrant {
        &self.secrets
    }

    pub fn allows_network_url(&self, url: &str) -> bool {
        self.allow_network && self.network.allows_url(url)
    }

    pub fn timeout_millis(&self) -> u64 {
        self.timeout_millis
    }

    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    pub fn validate(&self) -> Result<(), ActionGrantError> {
        if self.workspace.root().as_os_str().is_empty() {
            return Err(ActionGrantError::EmptyWorkspaceRoot);
        }
        if self.working_directory.as_os_str().is_empty() {
            return Err(ActionGrantError::EmptyWorkingDirectory);
        }
        if !self.is_approved() {
            return Err(ActionGrantError::ApprovalRequired);
        }
        if self
            .approval_ticket()
            .is_none_or(|ticket| ticket.trim().is_empty())
        {
            return Err(ActionGrantError::EmptyApprovalTicket);
        }
        if self.timeout_millis == 0 || self.timeout_millis > MAX_ACTION_TIMEOUT_MILLIS {
            return Err(ActionGrantError::InvalidTimeout {
                value: self.timeout_millis,
                maximum: MAX_ACTION_TIMEOUT_MILLIS,
            });
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > MAX_ACTION_OUTPUT_BYTES {
            return Err(ActionGrantError::InvalidOutputLimit {
                value: self.max_output_bytes,
                maximum: MAX_ACTION_OUTPUT_BYTES,
            });
        }
        if self.allow_write && !self.workspace.allows_workspace_write() {
            return Err(ActionGrantError::WorkspaceWriteNotGranted);
        }
        self.network
            .validate()
            .map_err(|error| ActionGrantError::InvalidNetworkGrant(error.to_string()))?;
        self.secrets
            .validate()
            .map_err(|error| ActionGrantError::InvalidSecretGrant(error.to_string()))?;
        if !self.allow_network && !self.network.is_empty() {
            return Err(ActionGrantError::NetworkGrantNotEnabled);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionGrantError {
    EmptyWorkspaceRoot,
    EmptyWorkingDirectory,
    ApprovalRequired,
    EmptyApprovalTicket,
    InvalidTimeout { value: u64, maximum: u64 },
    InvalidOutputLimit { value: usize, maximum: usize },
    WorkspaceWriteNotGranted,
    InvalidNetworkGrant(String),
    InvalidSecretGrant(String),
    NetworkGrantNotEnabled,
}

impl std::fmt::Display for ActionGrantError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyWorkspaceRoot => formatter.write_str("workspace root cannot be empty"),
            Self::EmptyWorkingDirectory => formatter.write_str("working directory cannot be empty"),
            Self::ApprovalRequired => formatter.write_str("explicit action approval is required"),
            Self::EmptyApprovalTicket => formatter.write_str("approval ticket cannot be empty"),
            Self::InvalidTimeout { value, maximum } => {
                write!(
                    formatter,
                    "action timeout {value} ms is outside 1..={maximum}"
                )
            }
            Self::InvalidOutputLimit { value, maximum } => write!(
                formatter,
                "action output limit {value} bytes is outside 1..={maximum}"
            ),
            Self::WorkspaceWriteNotGranted => {
                formatter.write_str("workspace write permission was not granted")
            }
            Self::InvalidNetworkGrant(message) => {
                write!(formatter, "network grant is invalid: {message}")
            }
            Self::InvalidSecretGrant(message) => {
                write!(formatter, "secret grant is invalid: {message}")
            }
            Self::NetworkGrantNotEnabled => {
                formatter.write_str("network scope was supplied without network permission")
            }
        }
    }
}

impl std::error::Error for ActionGrantError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ActionApproval {
    Denied { reason: String },
    Approved { ticket: String },
}

impl ActionApproval {
    pub fn denial_reason(&self) -> Option<&str> {
        match self {
            Self::Denied { reason } => Some(reason),
            Self::Approved { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellExecuteRequest {
    grant: ActionGrant,
    command: String,
}

impl ShellExecuteRequest {
    pub fn new(grant: ActionGrant, command: impl Into<String>) -> Self {
        Self {
            grant,
            command: command.into(),
        }
    }

    pub fn grant(&self) -> &ActionGrant {
        &self.grant
    }

    pub fn command(&self) -> &str {
        &self.command
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellExecuteResult {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    output_truncated: bool,
}

impl ShellExecuteResult {
    pub fn new(
        exit_code: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        timed_out: bool,
        output_truncated: bool,
    ) -> Self {
        Self {
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            timed_out,
            output_truncated,
        }
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    pub fn timed_out(&self) -> bool {
        self.timed_out
    }

    pub fn output_truncated(&self) -> bool {
        self.output_truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PatchApplyRequest {
    grant: ActionGrant,
    patch: String,
}

impl PatchApplyRequest {
    pub fn new(grant: ActionGrant, patch: impl Into<String>) -> Self {
        Self {
            grant,
            patch: patch.into(),
        }
    }

    pub fn grant(&self) -> &ActionGrant {
        &self.grant
    }

    pub fn patch(&self) -> &str {
        &self.patch
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchChangeKind {
    Added,
    Updated,
    Deleted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PatchFileChange {
    path: String,
    kind: PatchChangeKind,
    added_lines: usize,
    removed_lines: usize,
}

impl PatchFileChange {
    pub fn new(
        path: impl Into<String>,
        kind: PatchChangeKind,
        added_lines: usize,
        removed_lines: usize,
    ) -> Self {
        Self {
            path: path.into(),
            kind,
            added_lines,
            removed_lines,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn kind(&self) -> PatchChangeKind {
        self.kind
    }

    pub fn added_lines(&self) -> usize {
        self.added_lines
    }

    pub fn removed_lines(&self) -> usize {
        self.removed_lines
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PatchApplyResult {
    changes: Vec<PatchFileChange>,
}

impl PatchApplyResult {
    pub fn new(changes: Vec<PatchFileChange>) -> Self {
        Self { changes }
    }

    pub fn changes(&self) -> &[PatchFileChange] {
        &self.changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_action_grant_is_explicit_and_not_approved() {
        let grant = ActionGrant::pending(WorkspaceGrant::read_only("C:\\workspace"), ".");
        assert!(!grant.is_approved());
        assert_eq!(grant.approval_ticket(), None);
        let json = serde_json::to_string(&grant).expect("serialize grant");
        assert!(json.contains("explicit user approval is required"));
    }

    #[test]
    fn approved_action_grant_keeps_limits_and_scope() {
        let grant = ActionGrant::approved(
            WorkspaceGrant::read_write("C:\\workspace").with_workspace_write(),
            "C:\\workspace",
            "ticket-1",
        )
        .with_write(true)
        .with_limits(5000, 1024);
        assert!(grant.is_approved());
        assert_eq!(grant.approval_ticket(), Some("ticket-1"));
        assert!(grant.workspace().allows_workspace_write());
        assert_eq!(grant.timeout_millis(), 5000);
        assert_eq!(grant.max_output_bytes(), 1024);
    }

    #[test]
    fn action_grant_keeps_network_and_secret_authority_scoped() {
        let network = NetworkGrant::for_url("https://api.example.test/mcp").expect("network");
        let secrets = SecretGrant::one("mcp/provider-token").expect("secret");
        let grant = ActionGrant::approved(
            WorkspaceGrant::read_only("C:\\workspace"),
            "C:\\workspace",
            "ticket-1",
        )
        .with_network_grant(network.clone())
        .with_secret_grant(secrets.clone());

        grant.validate().expect("scoped grant is valid");
        assert!(grant.allows_network_url("https://api.example.test/mcp"));
        assert!(!grant.allows_network_url("https://other.example.test/mcp"));
        assert_eq!(grant.network_grant(), &network);
        assert_eq!(grant.secret_grant(), &secrets);
        let wire = serde_json::to_string(&grant).expect("serialize scoped grant");
        assert!(wire.contains("mcp/provider-token"));
        assert!(!wire.contains("secret-value"));
    }
}
