//! Bounded graph state and atomic persistence under Host-issued authority.

use std::collections::{BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use yunxi_protocol::{
    AgentBudget, AgentDelegationGrant, AgentEvent, AgentEventKind, AgentInterruptRequest,
    AgentListResult, AgentMutationResult, AgentProtocolError, AgentSnapshot, AgentSpawnRequest,
    AgentSpawnResult, AgentStatus, AgentTranscriptEntry, AgentTurnCompleteRequest,
    AgentTurnFailRequest, AgentTurnStartRequest, AgentTurnStartResult, GrantKind, MAX_AGENT_COUNT,
    MAX_AGENT_EVENTS, MAX_AGENT_TRANSCRIPT_ENTRIES, ROOT_AGENT_ID,
};

const DOCUMENT_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct CoordinatorStore {
    path: PathBuf,
    workspace_root: PathBuf,
    instance_id: String,
    authority: AgentDelegationGrant,
    writable: bool,
}

impl CoordinatorStore {
    pub fn from_grant(
        authority: &AgentDelegationGrant,
        instance_id: impl Into<String>,
    ) -> Result<Self, MultiAgentStoreError> {
        authority
            .validate()
            .map_err(MultiAgentStoreError::Protocol)?;
        let workspace_root = fs::canonicalize(authority.workspace().root()).map_err(|source| {
            MultiAgentStoreError::Workspace {
                path: authority.workspace().root().to_path_buf(),
                source,
            }
        })?;
        if !workspace_root.is_dir() {
            return Err(MultiAgentStoreError::NotDirectory(workspace_root));
        }
        let state_root = authority
            .workspace()
            .state_root()
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root.join(".yunxi-next"));
        let instance_id = instance_id.into();
        validate_instance_id(&instance_id)?;
        Ok(Self {
            path: state_root
                .join("multi-agent")
                .join(format!("{}.json", authority.session_id())),
            workspace_root,
            instance_id,
            authority: authority.clone(),
            writable: authority.workspace().allows_next_write(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn spawn(
        &self,
        request: &AgentSpawnRequest,
    ) -> Result<AgentSpawnResult, MultiAgentStoreError> {
        self.require_matching_authority(request.grant())?;
        request.validate().map_err(MultiAgentStoreError::Protocol)?;
        self.require_write()?;
        let mut document = self.load_or_new()?;
        if document.agents.len() >= usize::from(self.authority.budget().max_agents()) {
            return Err(MultiAgentStoreError::AgentLimitReached(
                self.authority.budget().max_agents(),
            ));
        }

        let (parent_depth, parent_grants) = if request.parent_id() == ROOT_AGENT_ID {
            (0, self.authority.allowed_child_grants())
        } else {
            let parent = document
                .agents
                .iter()
                .find(|agent| agent.id == request.parent_id())
                .ok_or_else(|| {
                    MultiAgentStoreError::ParentNotFound(request.parent_id().to_string())
                })?;
            if matches!(parent.status, AgentStatus::Interrupted) {
                return Err(MultiAgentStoreError::ParentUnavailable(
                    request.parent_id().to_string(),
                ));
            }
            (parent.depth, parent.child_grants.as_slice())
        };
        if !request
            .requested_child_grants()
            .iter()
            .all(|grant| parent_grants.contains(grant))
        {
            return Err(MultiAgentStoreError::GrantEscalation);
        }
        let depth = parent_depth.saturating_add(1);
        if depth > self.authority.budget().max_depth() {
            return Err(MultiAgentStoreError::DepthLimitReached(
                self.authority.budget().max_depth(),
            ));
        }

        let now = now_millis();
        let sequence = document.next_agent_sequence;
        document.next_agent_sequence = document.next_agent_sequence.saturating_add(1);
        let id = format!("agent-{sequence}");
        let name = request
            .name()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Agent {sequence}"));
        let stored = StoredAgent {
            id: id.clone(),
            parent_id: request.parent_id().to_string(),
            name,
            depth,
            status: AgentStatus::Pending,
            turns_used: 0,
            child_grants: request.requested_child_grants().to_vec(),
            transcript: Vec::new(),
            created_at_millis: now,
            updated_at_millis: now,
        };
        let event = document.push_event(
            &id,
            AgentEventKind::Spawned,
            format!("spawned under {}", request.parent_id()),
            now,
        )?;
        document.agents.push(stored.clone());
        self.save(&document)?;
        Ok(AgentSpawnResult::new(stored.snapshot()?, event))
    }

    pub fn list(&self) -> Result<AgentListResult, MultiAgentStoreError> {
        let document = self.load_or_new()?;
        let agents = document
            .agents
            .iter()
            .map(StoredAgent::snapshot)
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = document.next_event_sequence.saturating_sub(1)
            > u64::try_from(document.events.len()).unwrap_or(u64::MAX);
        AgentListResult::new(
            self.authority.budget(),
            agents,
            document.events,
            document.total_turns,
            truncated,
        )
        .map_err(MultiAgentStoreError::Protocol)
    }

    pub fn start_turn(
        &self,
        request: &AgentTurnStartRequest,
    ) -> Result<AgentTurnStartResult, MultiAgentStoreError> {
        self.require_matching_authority(request.grant())?;
        request.validate().map_err(MultiAgentStoreError::Protocol)?;
        self.require_write()?;
        let mut document = self.load_or_new()?;
        if document.total_turns >= self.authority.budget().max_total_turns() {
            return Err(MultiAgentStoreError::TotalTurnLimitReached(
                self.authority.budget().max_total_turns(),
            ));
        }
        let now = now_millis();
        let index = document.agent_index(request.agent_id())?;
        {
            let agent = &mut document.agents[index];
            if agent.status == AgentStatus::Running {
                return Err(MultiAgentStoreError::AlreadyRunning(
                    request.agent_id().to_string(),
                ));
            }
            if agent.status == AgentStatus::Interrupted {
                return Err(MultiAgentStoreError::Interrupted(
                    request.agent_id().to_string(),
                ));
            }
            if agent.turns_used >= self.authority.budget().max_turns_per_agent() {
                return Err(MultiAgentStoreError::AgentTurnLimitReached(
                    request.agent_id().to_string(),
                    self.authority.budget().max_turns_per_agent(),
                ));
            }
            if agent.transcript.len() >= MAX_AGENT_TRANSCRIPT_ENTRIES {
                return Err(MultiAgentStoreError::TranscriptLimitReached(
                    request.agent_id().to_string(),
                ));
            }
            agent
                .transcript
                .push(AgentTranscriptEntry::user(request.message())?);
            agent.turns_used = agent.turns_used.saturating_add(1);
            agent.status = AgentStatus::Running;
            agent.updated_at_millis = now;
        }
        document.total_turns = document.total_turns.saturating_add(1);
        let event = document.push_event(
            request.agent_id(),
            AgentEventKind::TurnStarted,
            "child model turn started",
            now,
        )?;
        let snapshot = document.agents[index].snapshot()?;
        let transcript = document.agents[index].transcript.clone();
        self.save(&document)?;
        AgentTurnStartResult::new(snapshot, transcript, event)
            .map_err(MultiAgentStoreError::Protocol)
    }

    pub fn complete_turn(
        &self,
        request: &AgentTurnCompleteRequest,
    ) -> Result<AgentMutationResult, MultiAgentStoreError> {
        self.require_matching_authority(request.grant())?;
        request.validate().map_err(MultiAgentStoreError::Protocol)?;
        self.require_write()?;
        let mut document = self.load_or_new()?;
        let now = now_millis();
        let index = document.agent_index(request.agent_id())?;
        {
            let agent = &mut document.agents[index];
            if agent.status != AgentStatus::Running {
                return Err(MultiAgentStoreError::NotRunning(
                    request.agent_id().to_string(),
                ));
            }
            if agent.transcript.len() >= MAX_AGENT_TRANSCRIPT_ENTRIES {
                return Err(MultiAgentStoreError::TranscriptLimitReached(
                    request.agent_id().to_string(),
                ));
            }
            agent
                .transcript
                .push(AgentTranscriptEntry::assistant(request.reply())?);
            agent.status = AgentStatus::Completed;
            agent.updated_at_millis = now;
        }
        let event = document.push_event(
            request.agent_id(),
            AgentEventKind::Completed,
            "child model turn completed",
            now,
        )?;
        let snapshot = document.agents[index].snapshot()?;
        self.save(&document)?;
        AgentMutationResult::new(vec![snapshot], vec![event])
            .map_err(MultiAgentStoreError::Protocol)
    }

    pub fn fail_turn(
        &self,
        request: &AgentTurnFailRequest,
    ) -> Result<AgentMutationResult, MultiAgentStoreError> {
        self.require_matching_authority(request.grant())?;
        request.validate().map_err(MultiAgentStoreError::Protocol)?;
        self.require_write()?;
        let mut document = self.load_or_new()?;
        let now = now_millis();
        let index = document.agent_index(request.agent_id())?;
        {
            let agent = &mut document.agents[index];
            if agent.status != AgentStatus::Running {
                return Err(MultiAgentStoreError::NotRunning(
                    request.agent_id().to_string(),
                ));
            }
            agent.status = AgentStatus::Failed;
            agent.updated_at_millis = now;
        }
        let event = document.push_event(
            request.agent_id(),
            AgentEventKind::Failed,
            format!("{}: {}", request.code(), request.message()),
            now,
        )?;
        let snapshot = document.agents[index].snapshot()?;
        self.save(&document)?;
        AgentMutationResult::new(vec![snapshot], vec![event])
            .map_err(MultiAgentStoreError::Protocol)
    }

    pub fn interrupt(
        &self,
        request: &AgentInterruptRequest,
    ) -> Result<AgentMutationResult, MultiAgentStoreError> {
        self.require_matching_authority(request.grant())?;
        request.validate().map_err(MultiAgentStoreError::Protocol)?;
        self.require_write()?;
        let mut document = self.load_or_new()?;
        document.agent_index(request.agent_id())?;

        let mut targets = BTreeSet::from([request.agent_id().to_string()]);
        if request.recursive() {
            let mut queue = VecDeque::from([request.agent_id().to_string()]);
            while let Some(parent) = queue.pop_front() {
                for child in document
                    .agents
                    .iter()
                    .filter(|agent| agent.parent_id == parent)
                {
                    if targets.insert(child.id.clone()) {
                        queue.push_back(child.id.clone());
                    }
                }
            }
        }

        let now = now_millis();
        let mut changed = Vec::new();
        let mut events = Vec::new();
        for target in targets {
            let index = document.agent_index(&target)?;
            if document.agents[index].status == AgentStatus::Interrupted {
                continue;
            }
            document.agents[index].status = AgentStatus::Interrupted;
            document.agents[index].updated_at_millis = now;
            let event = document.push_event(
                &target,
                AgentEventKind::Interrupted,
                "branch interrupted by the Host",
                now,
            )?;
            changed.push(document.agents[index].snapshot()?);
            events.push(event);
        }
        self.save(&document)?;
        AgentMutationResult::new(changed, events).map_err(MultiAgentStoreError::Protocol)
    }

    fn require_matching_authority(
        &self,
        authority: &AgentDelegationGrant,
    ) -> Result<(), MultiAgentStoreError> {
        authority
            .validate()
            .map_err(MultiAgentStoreError::Protocol)?;
        if authority.session_id() != self.authority.session_id()
            || authority.workspace().root() != self.authority.workspace().root()
            || !self.authority.budget().contains(&authority.budget())
            || !authority
                .allowed_child_grants()
                .iter()
                .all(|grant| self.authority.allowed_child_grants().contains(grant))
        {
            return Err(MultiAgentStoreError::AuthorityMismatch);
        }
        Ok(())
    }

    fn require_write(&self) -> Result<(), MultiAgentStoreError> {
        if self.writable {
            Ok(())
        } else {
            Err(MultiAgentStoreError::WriteNotGranted)
        }
    }

    fn load_or_new(&self) -> Result<SessionDocument, MultiAgentStoreError> {
        let Some(mut document) = load_document(&self.path)? else {
            return Ok(SessionDocument::new(&self.authority, &self.instance_id));
        };
        document.validate()?;
        if document.session_id != self.authority.session_id()
            || !document.budget.contains(&self.authority.budget())
            || !self
                .authority
                .allowed_child_grants()
                .iter()
                .all(|grant| document.allowed_child_grants.contains(grant))
        {
            return Err(MultiAgentStoreError::AuthorityEscalation);
        }

        if document.coordinator_instance_id != self.instance_id {
            self.require_write()?;
            let now = now_millis();
            let running = document
                .agents
                .iter()
                .filter(|agent| agent.status == AgentStatus::Running)
                .map(|agent| agent.id.clone())
                .collect::<Vec<_>>();
            for id in running {
                let index = document.agent_index(&id)?;
                document.agents[index].status = AgentStatus::Failed;
                document.agents[index].updated_at_millis = now;
                document.push_event(
                    &id,
                    AgentEventKind::Failed,
                    "child turn abandoned after coordinator restart",
                    now,
                )?;
            }
            document.coordinator_instance_id = self.instance_id.clone();
            self.save(&document)?;
        }
        Ok(document)
    }

    fn save(&self, document: &SessionDocument) -> Result<(), MultiAgentStoreError> {
        self.require_write()?;
        document.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| MultiAgentStoreError::InvalidPath(self.path.clone()))?;
        fs::create_dir_all(parent).map_err(|source| MultiAgentStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        if self.authority.workspace().state_root().is_none() {
            let canonical_parent =
                fs::canonicalize(parent).map_err(|source| MultiAgentStoreError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            if !canonical_parent.starts_with(&self.workspace_root) {
                return Err(MultiAgentStoreError::WorkspaceEscape(canonical_parent));
            }
        }
        let mut content =
            serde_json::to_vec_pretty(document).map_err(MultiAgentStoreError::Serialize)?;
        content.push(b'\n');
        if content.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(MultiAgentStoreError::DocumentTooLarge {
                path: self.path.clone(),
                maximum: MAX_DOCUMENT_BYTES,
            });
        }
        replace_file(&self.path, &content)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionDocument {
    version: u32,
    session_id: String,
    coordinator_instance_id: String,
    budget: AgentBudget,
    allowed_child_grants: Vec<GrantKind>,
    total_turns: u16,
    next_agent_sequence: u64,
    next_event_sequence: u64,
    agents: Vec<StoredAgent>,
    events: Vec<AgentEvent>,
}

impl SessionDocument {
    fn new(authority: &AgentDelegationGrant, instance_id: &str) -> Self {
        Self {
            version: DOCUMENT_VERSION,
            session_id: authority.session_id().to_string(),
            coordinator_instance_id: instance_id.to_string(),
            budget: authority.budget(),
            allowed_child_grants: authority.allowed_child_grants().to_vec(),
            total_turns: 0,
            next_agent_sequence: 1,
            next_event_sequence: 1,
            agents: Vec::new(),
            events: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), MultiAgentStoreError> {
        if self.version != DOCUMENT_VERSION {
            return Err(MultiAgentStoreError::UnsupportedVersion(self.version));
        }
        self.budget
            .validate()
            .map_err(MultiAgentStoreError::Protocol)?;
        validate_instance_id(&self.coordinator_instance_id)?;
        if self.next_agent_sequence == 0 || self.next_event_sequence == 0 {
            return Err(MultiAgentStoreError::InvalidDocument(
                "stored sequences must be greater than zero".to_string(),
            ));
        }
        if self.agents.len() > usize::from(MAX_AGENT_COUNT) || self.events.len() > MAX_AGENT_EVENTS
        {
            return Err(MultiAgentStoreError::InvalidDocument(
                "stored agent or event count exceeds protocol limits".to_string(),
            ));
        }
        let mut ids = BTreeSet::new();
        for agent in &self.agents {
            agent.validate()?;
            if !ids.insert(agent.id.as_str()) {
                return Err(MultiAgentStoreError::InvalidDocument(format!(
                    "duplicate agent id `{}`",
                    agent.id
                )));
            }
        }
        for agent in &self.agents {
            if agent.parent_id != ROOT_AGENT_ID && !ids.contains(agent.parent_id.as_str()) {
                return Err(MultiAgentStoreError::InvalidDocument(format!(
                    "agent `{}` has missing parent `{}`",
                    agent.id, agent.parent_id
                )));
            }
        }
        for event in &self.events {
            event.validate().map_err(MultiAgentStoreError::Protocol)?;
        }
        Ok(())
    }

    fn agent_index(&self, id: &str) -> Result<usize, MultiAgentStoreError> {
        self.agents
            .iter()
            .position(|agent| agent.id == id)
            .ok_or_else(|| MultiAgentStoreError::AgentNotFound(id.to_string()))
    }

    fn push_event(
        &mut self,
        agent_id: &str,
        kind: AgentEventKind,
        detail: impl Into<String>,
        at_millis: u128,
    ) -> Result<AgentEvent, MultiAgentStoreError> {
        let event = AgentEvent::new(self.next_event_sequence, agent_id, kind, detail, at_millis)?;
        self.next_event_sequence = self.next_event_sequence.saturating_add(1);
        self.events.push(event.clone());
        if self.events.len() > MAX_AGENT_EVENTS {
            let remove = self.events.len() - MAX_AGENT_EVENTS;
            self.events.drain(..remove);
        }
        Ok(event)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAgent {
    id: String,
    parent_id: String,
    name: String,
    depth: u16,
    status: AgentStatus,
    turns_used: u16,
    child_grants: Vec<GrantKind>,
    transcript: Vec<AgentTranscriptEntry>,
    created_at_millis: u128,
    updated_at_millis: u128,
}

impl StoredAgent {
    fn snapshot(&self) -> Result<AgentSnapshot, MultiAgentStoreError> {
        AgentSnapshot::new(
            &self.id,
            &self.parent_id,
            &self.name,
            self.depth,
            self.status,
            self.turns_used,
            self.child_grants.clone(),
            self.created_at_millis,
            self.updated_at_millis,
        )
        .map_err(MultiAgentStoreError::Protocol)
    }

    fn validate(&self) -> Result<(), MultiAgentStoreError> {
        self.snapshot()?;
        if self.transcript.len() > MAX_AGENT_TRANSCRIPT_ENTRIES {
            return Err(MultiAgentStoreError::InvalidDocument(format!(
                "agent `{}` transcript exceeds its entry limit",
                self.id
            )));
        }
        for entry in &self.transcript {
            entry.validate().map_err(MultiAgentStoreError::Protocol)?;
        }
        Ok(())
    }
}

fn validate_instance_id(value: &str) -> Result<(), MultiAgentStoreError> {
    if value.is_empty()
        || value.len() > 128
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(MultiAgentStoreError::InvalidInstanceId(value.to_string()));
    }
    Ok(())
}

fn load_document(path: &Path) -> Result<Option<SessionDocument>, MultiAgentStoreError> {
    match load_document_at(path) {
        Ok(Some(document)) => Ok(Some(document)),
        Ok(None) => load_document_at(&backup_path(path)),
        Err(primary_error) => match load_document_at(&backup_path(path)) {
            Ok(Some(document)) => Ok(Some(document)),
            Ok(None) | Err(_) => Err(primary_error),
        },
    }
}

fn load_document_at(path: &Path) -> Result<Option<SessionDocument>, MultiAgentStoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MultiAgentStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Err(MultiAgentStoreError::InvalidPath(path.to_path_buf()));
    }
    if metadata.len() > MAX_DOCUMENT_BYTES {
        return Err(MultiAgentStoreError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum: MAX_DOCUMENT_BYTES,
        });
    }
    let content = fs::read(path).map_err(|source| MultiAgentStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&content)
        .map(Some)
        .map_err(|source| MultiAgentStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })
}

fn replace_file(path: &Path, content: &[u8]) -> Result<(), MultiAgentStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| MultiAgentStoreError::InvalidPath(path.to_path_buf()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".multi-agent-{}-{stamp}.tmp", process::id()));
    let backup = backup_path(path);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|source| MultiAgentStoreError::Io {
            path: temp.clone(),
            source,
        })?;
    if let Err(source) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ignored = fs::remove_file(&temp);
        return Err(MultiAgentStoreError::Io { path: temp, source });
    }
    drop(file);
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|source| MultiAgentStoreError::Io {
                path: backup.clone(),
                source,
            })?;
        }
        fs::rename(path, &backup).map_err(|source| MultiAgentStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(&temp, path) {
        if backup.exists() {
            let _ignored = fs::rename(&backup, path);
        }
        let _ignored = fs::remove_file(&temp);
        return Err(MultiAgentStoreError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    if backup.exists() {
        let _ignored = fs::remove_file(backup);
    }
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Debug)]
pub enum MultiAgentStoreError {
    Protocol(AgentProtocolError),
    Workspace {
        path: PathBuf,
        source: std::io::Error,
    },
    NotDirectory(PathBuf),
    InvalidPath(PathBuf),
    WorkspaceEscape(PathBuf),
    InvalidInstanceId(String),
    WriteNotGranted,
    AuthorityMismatch,
    AuthorityEscalation,
    GrantEscalation,
    AgentNotFound(String),
    ParentNotFound(String),
    ParentUnavailable(String),
    AgentLimitReached(u16),
    DepthLimitReached(u16),
    AgentTurnLimitReached(String, u16),
    TotalTurnLimitReached(u16),
    TranscriptLimitReached(String),
    AlreadyRunning(String),
    NotRunning(String),
    Interrupted(String),
    UnsupportedVersion(u32),
    InvalidDocument(String),
    DocumentTooLarge {
        path: PathBuf,
        maximum: u64,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    Serialize(serde_json::Error),
}

impl MultiAgentStoreError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Protocol(_) | Self::InvalidInstanceId(_) => "invalid_request",
            Self::WriteNotGranted => "write_not_granted",
            Self::AuthorityMismatch | Self::AuthorityEscalation | Self::GrantEscalation => {
                "delegation_denied"
            }
            Self::AgentNotFound(_) | Self::ParentNotFound(_) => "agent_not_found",
            Self::ParentUnavailable(_) | Self::Interrupted(_) => "agent_interrupted",
            Self::AgentLimitReached(_)
            | Self::DepthLimitReached(_)
            | Self::AgentTurnLimitReached(_, _)
            | Self::TotalTurnLimitReached(_)
            | Self::TranscriptLimitReached(_) => "agent_budget_exhausted",
            Self::AlreadyRunning(_) => "agent_busy",
            Self::NotRunning(_) => "invalid_agent_state",
            Self::Workspace { .. }
            | Self::NotDirectory(_)
            | Self::InvalidPath(_)
            | Self::WorkspaceEscape(_)
            | Self::UnsupportedVersion(_)
            | Self::InvalidDocument(_)
            | Self::DocumentTooLarge { .. }
            | Self::Io { .. }
            | Self::Parse { .. }
            | Self::Serialize(_) => "multi_agent_storage_error",
        }
    }
}

impl fmt::Display for MultiAgentStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => error.fmt(formatter),
            Self::Workspace { path, source } => {
                write!(
                    formatter,
                    "cannot resolve workspace {}: {source}",
                    path.display()
                )
            }
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "workspace is not a directory: {}",
                    path.display()
                )
            }
            Self::InvalidPath(path) => write!(formatter, "invalid state path: {}", path.display()),
            Self::WorkspaceEscape(path) => write!(
                formatter,
                "multi-agent state resolved outside the granted workspace: {}",
                path.display()
            ),
            Self::InvalidInstanceId(value) => {
                write!(formatter, "coordinator instance id `{value}` is invalid")
            }
            Self::WriteNotGranted => formatter.write_str("Next state write access was not granted"),
            Self::AuthorityMismatch => {
                formatter.write_str("request authority does not match this coordinator store")
            }
            Self::AuthorityEscalation => formatter
                .write_str("request authority exceeds the authority stored for this agent session"),
            Self::GrantEscalation => {
                formatter.write_str("child grants exceed its parent agent grants")
            }
            Self::AgentNotFound(id) => write!(formatter, "agent `{id}` was not found"),
            Self::ParentNotFound(id) => write!(formatter, "parent agent `{id}` was not found"),
            Self::ParentUnavailable(id) => {
                write!(formatter, "parent agent `{id}` is interrupted")
            }
            Self::AgentLimitReached(limit) => {
                write!(formatter, "agent count budget of {limit} is exhausted")
            }
            Self::DepthLimitReached(limit) => {
                write!(formatter, "agent depth budget of {limit} is exhausted")
            }
            Self::AgentTurnLimitReached(id, limit) => {
                write!(formatter, "agent `{id}` exhausted its {limit}-turn budget")
            }
            Self::TotalTurnLimitReached(limit) => {
                write!(formatter, "agent session exhausted its {limit}-turn budget")
            }
            Self::TranscriptLimitReached(id) => {
                write!(formatter, "agent `{id}` transcript is full")
            }
            Self::AlreadyRunning(id) => write!(formatter, "agent `{id}` is already running"),
            Self::NotRunning(id) => write!(formatter, "agent `{id}` has no active turn"),
            Self::Interrupted(id) => write!(formatter, "agent `{id}` is interrupted"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "multi-agent document version {version} is unsupported"
                )
            }
            Self::InvalidDocument(message) => {
                write!(formatter, "multi-agent document is invalid: {message}")
            }
            Self::DocumentTooLarge { path, maximum } => write!(
                formatter,
                "multi-agent document {} exceeds {maximum} bytes",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "multi-agent I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::Parse { path, source } => write!(
                formatter,
                "cannot parse multi-agent document {}: {source}",
                path.display()
            ),
            Self::Serialize(error) => write!(formatter, "cannot encode multi-agent state: {error}"),
        }
    }
}

impl Error for MultiAgentStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Workspace { source, .. } | Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } | Self::Serialize(source) => Some(source),
            _ => None,
        }
    }
}

impl From<AgentProtocolError> for MultiAgentStoreError {
    fn from(error: AgentProtocolError) -> Self {
        Self::Protocol(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture_root(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{}-{stamp}", process::id()))
    }

    fn authority(root: &Path, budget: AgentBudget) -> AgentDelegationGrant {
        AgentDelegationGrant::new(
            yunxi_protocol::WorkspaceGrant::read_write(root),
            "session-1",
            "ticket-1",
            budget,
        )
        .expect("authority")
        .with_allowed_child_grants([GrantKind::WorkspaceRead])
        .expect("child grants")
    }

    #[test]
    fn graph_turns_persist_and_recursive_interrupt_spares_siblings() {
        let root = fixture_root("yunxi-multi-agent-store");
        fs::create_dir_all(&root).expect("workspace");
        let grant = authority(&root, AgentBudget::new(4, 3, 4, 8).expect("budget"));
        let store = CoordinatorStore::from_grant(&grant, "instance-1").expect("store");

        let first = store
            .spawn(
                &AgentSpawnRequest::new(grant.clone(), "inspect tests")
                    .expect("spawn")
                    .with_name("tests")
                    .expect("name")
                    .with_requested_child_grants([GrantKind::WorkspaceRead])
                    .expect("grants"),
            )
            .expect("spawn first");
        let sibling = store
            .spawn(&AgentSpawnRequest::new(grant.clone(), "inspect docs").expect("spawn"))
            .expect("spawn sibling");
        let child = store
            .spawn(
                &AgentSpawnRequest::new(grant.clone(), "inspect nested test")
                    .expect("spawn")
                    .with_parent(first.agent().id())
                    .expect("parent"),
            )
            .expect("spawn child");

        let started = store
            .start_turn(
                &AgentTurnStartRequest::new(grant.clone(), sibling.agent().id(), "inspect docs")
                    .expect("start"),
            )
            .expect("started");
        assert_eq!(started.transcript().len(), 1);
        store
            .complete_turn(
                &AgentTurnCompleteRequest::new(
                    grant.clone(),
                    sibling.agent().id(),
                    "docs are current",
                )
                .expect("complete"),
            )
            .expect("completed");
        let interrupted = store
            .interrupt(
                &AgentInterruptRequest::new(grant.clone(), first.agent().id(), true)
                    .expect("interrupt"),
            )
            .expect("interrupted");
        assert_eq!(interrupted.agents().len(), 2);
        assert!(
            interrupted
                .agents()
                .iter()
                .any(|agent| agent.id() == child.agent().id())
        );

        let list = store.list().expect("list");
        assert_eq!(list.agents().len(), 3);
        assert_eq!(
            list.agents()
                .iter()
                .find(|agent| agent.id() == sibling.agent().id())
                .expect("sibling")
                .status(),
            AgentStatus::Completed
        );
        assert!(store.path().is_file());
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn restart_marks_only_running_branches_failed() {
        let root = fixture_root("yunxi-multi-agent-recovery");
        fs::create_dir_all(&root).expect("workspace");
        let grant = authority(&root, AgentBudget::conservative());
        let first = CoordinatorStore::from_grant(&grant, "instance-1").expect("store");
        let agent = first
            .spawn(&AgentSpawnRequest::new(grant.clone(), "run task").expect("spawn"))
            .expect("spawn");
        first
            .start_turn(
                &AgentTurnStartRequest::new(grant.clone(), agent.agent().id(), "run task")
                    .expect("start"),
            )
            .expect("start turn");

        let restarted = CoordinatorStore::from_grant(&grant, "instance-2").expect("restart");
        let list = restarted.list().expect("recover list");
        assert_eq!(list.agents()[0].status(), AgentStatus::Failed);
        assert!(
            list.events()
                .iter()
                .any(|event| event.detail().contains("coordinator restart"))
        );
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_replacement_recovers_from_backup() {
        let root = fixture_root("yunxi-multi-agent-backup");
        fs::create_dir_all(&root).expect("workspace");
        let grant = authority(&root, AgentBudget::conservative());
        let store = CoordinatorStore::from_grant(&grant, "instance-1").expect("store");
        let spawned = store
            .spawn(&AgentSpawnRequest::new(grant, "recover task").expect("spawn"))
            .expect("spawn agent");
        let backup = backup_path(store.path());
        fs::rename(store.path(), &backup).expect("simulate interrupted replacement");

        let list = store.list().expect("load backup");
        assert_eq!(list.agents().len(), 1);
        assert_eq!(list.agents()[0].id(), spawned.agent().id());
        let _ignored = fs::remove_dir_all(root);
    }
}
