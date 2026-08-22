//! Explicit workspace access carried by stateful capability calls.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceGrant {
    root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state_root: Option<PathBuf>,
    legacy_read: bool,
    next_write: bool,
    #[serde(default)]
    workspace_write: bool,
}

impl WorkspaceGrant {
    pub fn read_only(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state_root: None,
            legacy_read: true,
            next_write: false,
            workspace_write: false,
        }
    }

    pub fn read_write(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state_root: None,
            legacy_read: true,
            next_write: true,
            workspace_write: false,
        }
    }

    pub fn with_state_root(mut self, state_root: impl Into<PathBuf>) -> Self {
        self.state_root = Some(state_root.into());
        self
    }

    /// Grants a tool permission to modify files below the workspace root.
    ///
    /// This is intentionally separate from `next_write`, which only permits
    /// Next-owned state such as sessions and memory. Action plugins must still
    /// require an approved `ActionGrant` before using this permission.
    pub fn with_workspace_write(mut self) -> Self {
        self.workspace_write = true;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state_root(&self) -> Option<&Path> {
        self.state_root.as_deref()
    }

    pub fn allows_legacy_read(&self) -> bool {
        self.legacy_read
    }

    pub fn allows_next_write(&self) -> bool {
        self.next_write
    }

    pub fn allows_workspace_write(&self) -> bool {
        self.workspace_write
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_round_trip_keeps_access_flags_explicit() {
        let grant = WorkspaceGrant::read_write(r"C:\workspace");
        let json = serde_json::to_string(&grant).expect("serialize grant");
        assert!(json.contains("\"legacy_read\":true"));
        assert!(json.contains("\"next_write\":true"));
        assert_eq!(
            serde_json::from_str::<WorkspaceGrant>(&json).expect("deserialize grant"),
            grant
        );
    }
}
