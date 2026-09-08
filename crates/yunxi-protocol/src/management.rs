//! Typed and bounded requests for process-isolated management capabilities.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::WorkspaceGrant;

pub const MEMORY_MANAGEMENT_STATUS_OPERATION: &str = "status";
pub const MEMORY_MANAGEMENT_LIST_OPERATION: &str = "list";
pub const MEMORY_MANAGEMENT_SHOW_OPERATION: &str = "show";
pub const MEMORY_MANAGEMENT_QUERY_OPERATION: &str = "query";
pub const MEMORY_MANAGEMENT_MUTATE_OPERATION: &str = "mutate";
pub const MEMORY_MANAGEMENT_CLEAR_OPERATION: &str = "clear";
pub const MEMORY_MANAGEMENT_SET_ENABLED_OPERATION: &str = "set_enabled";
pub const PERSONA_MANAGEMENT_STATUS_OPERATION: &str = "status";
pub const PERSONA_MANAGEMENT_LIST_OPERATION: &str = "list";
pub const PERSONA_MANAGEMENT_PROFILE_OPERATION: &str = "profile";
pub const PERSONA_MANAGEMENT_IMPORT_OPERATION: &str = "import";
pub const PERSONA_MANAGEMENT_SET_ACTIVE_OPERATION: &str = "set_active";
pub const PERSONA_MANAGEMENT_SET_ENABLED_OPERATION: &str = "set_enabled";
pub const PERSONA_MANAGEMENT_RESET_OPERATION: &str = "reset";
pub const COMPANION_MANAGEMENT_STATUS_OPERATION: &str = "status";
pub const COMPANION_MANAGEMENT_CHECK_OPERATION: &str = "check";
pub const COMPANION_MANAGEMENT_HISTORY_OPERATION: &str = "history";
pub const COMPANION_MANAGEMENT_CLEAR_OPERATION: &str = "clear";
pub const COMPANION_MANAGEMENT_SET_ENABLED_OPERATION: &str = "set_enabled";

pub const MAX_MANAGEMENT_RECORDS: usize = 512;
pub const MAX_MANAGEMENT_ID_CHARS: usize = 256;
pub const MAX_PERSONA_PROFILE_ID_BYTES: usize = 64;
pub const MAX_PERSONA_PROFILE_BYTES: usize = 512 * 1024;
pub const MAX_MANAGEMENT_QUERY_CHARS: usize = 16 * 1024;
pub const MAX_COMPANION_HISTORY_RECORDS: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryStatusRequest {
    grant: WorkspaceGrant,
}

impl MemoryStatusRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryListRequest {
    grant: WorkspaceGrant,
    limit: usize,
}

impl MemoryListRequest {
    pub fn new(grant: WorkspaceGrant, limit: usize) -> Self {
        Self { grant, limit }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        if !(1..=MAX_MANAGEMENT_RECORDS).contains(&self.limit) {
            return Err(ManagementRequestError::InvalidLimit {
                value: self.limit,
                maximum: MAX_MANAGEMENT_RECORDS,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryShowRequest {
    grant: WorkspaceGrant,
    id: String,
}

impl MemoryShowRequest {
    pub fn new(grant: WorkspaceGrant, id: impl Into<String>) -> Self {
        Self {
            grant,
            id: id.into(),
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        validate_memory_id(&self.id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryManagementScope {
    All,
    Global,
    Workspace,
    Relationship,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryQueryRequest {
    grant: WorkspaceGrant,
    scope: MemoryManagementScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    pending_only: bool,
    limit: usize,
}

impl MemoryQueryRequest {
    pub fn list(grant: WorkspaceGrant, scope: MemoryManagementScope, limit: usize) -> Self {
        Self {
            grant,
            scope,
            query: None,
            pending_only: false,
            limit,
        }
    }

    pub fn pending(grant: WorkspaceGrant, limit: usize) -> Self {
        Self {
            grant,
            scope: MemoryManagementScope::All,
            query: None,
            pending_only: true,
            limit,
        }
    }

    pub fn search(
        grant: WorkspaceGrant,
        scope: MemoryManagementScope,
        query: impl Into<String>,
        limit: usize,
    ) -> Self {
        Self {
            grant,
            scope,
            query: Some(query.into()),
            pending_only: false,
            limit,
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn scope(&self) -> MemoryManagementScope {
        self.scope
    }

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub fn pending_only(&self) -> bool {
        self.pending_only
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        validate_limit(self.limit)?;
        if let Some(query) = self.query() {
            if query.trim().is_empty() || query.chars().count() > MAX_MANAGEMENT_QUERY_CHARS {
                return Err(ManagementRequestError::InvalidQuery);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMutationAction {
    Approve,
    Reject,
    Archive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryMutationRequest {
    grant: WorkspaceGrant,
    id: String,
    action: MemoryMutationAction,
}

impl MemoryMutationRequest {
    pub fn new(grant: WorkspaceGrant, id: impl Into<String>, action: MemoryMutationAction) -> Self {
        Self {
            grant,
            id: id.into(),
            action,
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn action(&self) -> MemoryMutationAction {
        self.action
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        validate_memory_id(&self.id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryClearScope {
    Workspace,
    Relationship,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryClearRequest {
    grant: WorkspaceGrant,
    scope: MemoryClearScope,
}

impl MemoryClearRequest {
    pub fn new(grant: WorkspaceGrant, scope: MemoryClearScope) -> Self {
        Self { grant, scope }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn scope(&self) -> MemoryClearScope {
        self.scope
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemorySetEnabledRequest {
    grant: WorkspaceGrant,
    enabled: bool,
}

impl MemorySetEnabledRequest {
    pub fn new(grant: WorkspaceGrant, enabled: bool) -> Self {
        Self { grant, enabled }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaStatusRequest {
    grant: WorkspaceGrant,
}

impl PersonaStatusRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaListRequest {
    grant: WorkspaceGrant,
}

impl PersonaListRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaProfileRequest {
    grant: WorkspaceGrant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

impl PersonaProfileRequest {
    pub fn new(grant: WorkspaceGrant, id: Option<String>) -> Self {
        Self { grant, id }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        let Some(id) = self.id() else {
            return Ok(());
        };
        if id.is_empty()
            || id.len() > MAX_PERSONA_PROFILE_ID_BYTES
            || !id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(ManagementRequestError::InvalidPersonaProfileId);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaImportRequest {
    grant: WorkspaceGrant,
    profile_json: String,
}

impl PersonaImportRequest {
    pub fn new(grant: WorkspaceGrant, profile_json: impl Into<String>) -> Self {
        Self {
            grant,
            profile_json: profile_json.into(),
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn profile_json(&self) -> &str {
        &self.profile_json
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        if self.profile_json.is_empty() || self.profile_json.len() > MAX_PERSONA_PROFILE_BYTES {
            return Err(ManagementRequestError::InvalidPersonaProfile);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaSetActiveRequest {
    grant: WorkspaceGrant,
    id: String,
}

impl PersonaSetActiveRequest {
    pub fn new(grant: WorkspaceGrant, id: impl Into<String>) -> Self {
        Self {
            grant,
            id: id.into(),
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        validate_persona_profile_id(&self.id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaSetEnabledRequest {
    grant: WorkspaceGrant,
    enabled: bool,
}

impl PersonaSetEnabledRequest {
    pub fn new(grant: WorkspaceGrant, enabled: bool) -> Self {
        Self { grant, enabled }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaResetRequest {
    grant: WorkspaceGrant,
}

impl PersonaResetRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionStatusRequest {
    grant: WorkspaceGrant,
}

impl CompanionStatusRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionCheckRequest {
    grant: WorkspaceGrant,
    prompt: String,
}

impl CompanionCheckRequest {
    pub fn new(grant: WorkspaceGrant, prompt: impl Into<String>) -> Self {
        Self {
            grant,
            prompt: prompt.into(),
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        if self.prompt.trim().is_empty() || self.prompt.chars().count() > MAX_MANAGEMENT_QUERY_CHARS
        {
            return Err(ManagementRequestError::InvalidQuery);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionHistoryRequest {
    grant: WorkspaceGrant,
    limit: usize,
}

impl CompanionHistoryRequest {
    pub fn new(grant: WorkspaceGrant, limit: usize) -> Self {
        Self { grant, limit }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn validate(&self) -> Result<(), ManagementRequestError> {
        if !(1..=MAX_COMPANION_HISTORY_RECORDS).contains(&self.limit) {
            return Err(ManagementRequestError::InvalidLimit {
                value: self.limit,
                maximum: MAX_COMPANION_HISTORY_RECORDS,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionClearRequest {
    grant: WorkspaceGrant,
}

impl CompanionClearRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionSetEnabledRequest {
    grant: WorkspaceGrant,
    enabled: bool,
}

impl CompanionSetEnabledRequest {
    pub fn new(grant: WorkspaceGrant, enabled: bool) -> Self {
        Self { grant, enabled }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

fn validate_memory_id(id: &str) -> Result<(), ManagementRequestError> {
    if id.is_empty()
        || id.chars().count() > MAX_MANAGEMENT_ID_CHARS
        || id.chars().any(char::is_control)
    {
        return Err(ManagementRequestError::InvalidMemoryId);
    }
    Ok(())
}

fn validate_persona_profile_id(id: &str) -> Result<(), ManagementRequestError> {
    if id.is_empty()
        || id.len() > MAX_PERSONA_PROFILE_ID_BYTES
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ManagementRequestError::InvalidPersonaProfileId);
    }
    Ok(())
}

fn validate_limit(limit: usize) -> Result<(), ManagementRequestError> {
    if !(1..=MAX_MANAGEMENT_RECORDS).contains(&limit) {
        return Err(ManagementRequestError::InvalidLimit {
            value: limit,
            maximum: MAX_MANAGEMENT_RECORDS,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagementRequestError {
    InvalidLimit { value: usize, maximum: usize },
    InvalidMemoryId,
    InvalidPersonaProfileId,
    InvalidPersonaProfile,
    InvalidQuery,
}

impl fmt::Display for ManagementRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit { value, maximum } => write!(
                formatter,
                "management limit {value} is outside the supported range 1..={maximum}"
            ),
            Self::InvalidMemoryId => formatter.write_str("memory id is invalid"),
            Self::InvalidPersonaProfileId => formatter.write_str("persona profile id is invalid"),
            Self::InvalidPersonaProfile => formatter.write_str("persona profile JSON is invalid"),
            Self::InvalidQuery => formatter.write_str("management query is invalid"),
        }
    }
}

impl Error for ManagementRequestError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_requests_round_trip_with_explicit_authority() {
        let grant =
            WorkspaceGrant::read_only(r"C:\workspace").with_state_root(r"C:\state\yunxi-next");
        let request = MemoryListRequest::new(grant.clone(), 200);
        request.validate().expect("valid memory list request");
        let encoded = serde_json::to_string(&request).expect("serialize request");
        assert!(encoded.contains("state_root"));
        assert_eq!(
            serde_json::from_str::<MemoryListRequest>(&encoded).expect("decode request"),
            request
        );

        PersonaProfileRequest::new(grant, Some("yunxi_companion_strong".to_string()))
            .validate()
            .expect("valid persona profile request");
    }

    #[test]
    fn management_bounds_are_checked_after_wire_decode() {
        let invalid_limit = MemoryListRequest::new(WorkspaceGrant::read_only("."), 0);
        assert!(matches!(
            invalid_limit.validate(),
            Err(ManagementRequestError::InvalidLimit { .. })
        ));
        assert!(matches!(
            MemoryShowRequest::new(WorkspaceGrant::read_only("."), "\n").validate(),
            Err(ManagementRequestError::InvalidMemoryId)
        ));
        assert!(matches!(
            PersonaProfileRequest::new(
                WorkspaceGrant::read_only("."),
                Some("../escape".to_string())
            )
            .validate(),
            Err(ManagementRequestError::InvalidPersonaProfileId)
        ));
    }
}
