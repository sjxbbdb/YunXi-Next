//! Bounded contracts for isolated multi-agent coordination.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{GrantKind, WorkspaceGrant};

pub const TOOL_MULTI_AGENT_SPAWN_OPERATION: &str = "spawn";
pub const TOOL_MULTI_AGENT_LIST_OPERATION: &str = "list";
pub const TOOL_MULTI_AGENT_INSPECT_OPERATION: &str = "inspect";
pub const TOOL_MULTI_AGENT_TURN_START_OPERATION: &str = "turn_start";
pub const TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION: &str = "turn_complete";
pub const TOOL_MULTI_AGENT_TURN_FAIL_OPERATION: &str = "turn_fail";
pub const TOOL_MULTI_AGENT_INTERRUPT_OPERATION: &str = "interrupt";

pub const ROOT_AGENT_ID: &str = "root";
pub const MAX_AGENT_COUNT: u16 = 16;
pub const MAX_AGENT_DEPTH: u16 = 4;
pub const MAX_AGENT_TURNS_PER_AGENT: u16 = 16;
pub const MAX_AGENT_TOTAL_TURNS: u16 = 64;
pub const MAX_AGENT_EVENTS: usize = 256;
pub const MAX_AGENT_TRANSCRIPT_ENTRIES: usize = 64;
pub const MAX_AGENT_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_AGENT_REPLY_BYTES: usize = 256 * 1024;

const MAX_AGENT_ID_BYTES: usize = 128;
const MAX_AGENT_SESSION_ID_BYTES: usize = 128;
const MAX_AGENT_NAME_BYTES: usize = 64;
const MAX_AGENT_TICKET_BYTES: usize = 128;
const MAX_AGENT_DETAIL_BYTES: usize = 4096;
const MAX_DELEGATED_GRANTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBudget {
    max_agents: u16,
    max_depth: u16,
    max_turns_per_agent: u16,
    max_total_turns: u16,
}

impl AgentBudget {
    pub fn new(
        max_agents: u16,
        max_depth: u16,
        max_turns_per_agent: u16,
        max_total_turns: u16,
    ) -> Result<Self, AgentProtocolError> {
        let budget = Self {
            max_agents,
            max_depth,
            max_turns_per_agent,
            max_total_turns,
        };
        budget.validate()?;
        Ok(budget)
    }

    pub fn conservative() -> Self {
        Self {
            max_agents: 4,
            max_depth: 2,
            max_turns_per_agent: 4,
            max_total_turns: 12,
        }
    }

    pub const fn max_agents(self) -> u16 {
        self.max_agents
    }

    pub const fn max_depth(self) -> u16 {
        self.max_depth
    }

    pub const fn max_turns_per_agent(self) -> u16 {
        self.max_turns_per_agent
    }

    pub const fn max_total_turns(self) -> u16 {
        self.max_total_turns
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        validate_limit("max_agents", self.max_agents, MAX_AGENT_COUNT)?;
        validate_limit("max_depth", self.max_depth, MAX_AGENT_DEPTH)?;
        validate_limit(
            "max_turns_per_agent",
            self.max_turns_per_agent,
            MAX_AGENT_TURNS_PER_AGENT,
        )?;
        validate_limit(
            "max_total_turns",
            self.max_total_turns,
            MAX_AGENT_TOTAL_TURNS,
        )
    }

    pub fn contains(&self, requested: &Self) -> bool {
        requested.max_agents <= self.max_agents
            && requested.max_depth <= self.max_depth
            && requested.max_turns_per_agent <= self.max_turns_per_agent
            && requested.max_total_turns <= self.max_total_turns
    }
}

impl Default for AgentBudget {
    fn default() -> Self {
        Self::conservative()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDelegationGrant {
    workspace: WorkspaceGrant,
    session_id: String,
    ticket: String,
    budget: AgentBudget,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed_child_grants: Vec<GrantKind>,
}

impl AgentDelegationGrant {
    pub fn new(
        workspace: WorkspaceGrant,
        session_id: impl Into<String>,
        ticket: impl Into<String>,
        budget: AgentBudget,
    ) -> Result<Self, AgentProtocolError> {
        let grant = Self {
            workspace,
            session_id: session_id.into(),
            ticket: ticket.into(),
            budget,
            allowed_child_grants: Vec::new(),
        };
        grant.validate()?;
        Ok(grant)
    }

    pub fn with_allowed_child_grants(
        mut self,
        grants: impl IntoIterator<Item = GrantKind>,
    ) -> Result<Self, AgentProtocolError> {
        self.allowed_child_grants = grants.into_iter().collect();
        self.validate()?;
        Ok(self)
    }

    pub fn workspace(&self) -> &WorkspaceGrant {
        &self.workspace
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    pub const fn budget(&self) -> AgentBudget {
        self.budget
    }

    pub fn allowed_child_grants(&self) -> &[GrantKind] {
        &self.allowed_child_grants
    }

    pub fn permits(&self, requested: &[GrantKind]) -> bool {
        requested
            .iter()
            .all(|grant| self.allowed_child_grants.contains(grant))
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        validate_token(
            "agent session id",
            &self.session_id,
            MAX_AGENT_SESSION_ID_BYTES,
        )?;
        validate_text(
            "agent delegation ticket",
            &self.ticket,
            MAX_AGENT_TICKET_BYTES,
        )?;
        self.budget.validate()?;
        validate_grants(&self.allowed_child_grants)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    Spawned,
    TurnStarted,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTranscriptEntry {
    role: AgentTranscriptRole,
    content: String,
}

impl AgentTranscriptEntry {
    pub fn user(content: impl Into<String>) -> Result<Self, AgentProtocolError> {
        Self::new(AgentTranscriptRole::User, content, MAX_AGENT_MESSAGE_BYTES)
    }

    pub fn assistant(content: impl Into<String>) -> Result<Self, AgentProtocolError> {
        Self::new(
            AgentTranscriptRole::Assistant,
            content,
            MAX_AGENT_REPLY_BYTES,
        )
    }

    fn new(
        role: AgentTranscriptRole,
        content: impl Into<String>,
        maximum: usize,
    ) -> Result<Self, AgentProtocolError> {
        let entry = Self {
            role,
            content: content.into(),
        };
        validate_text("agent transcript content", &entry.content, maximum)?;
        Ok(entry)
    }

    pub const fn role(&self) -> AgentTranscriptRole {
        self.role
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        let maximum = match self.role {
            AgentTranscriptRole::User => MAX_AGENT_MESSAGE_BYTES,
            AgentTranscriptRole::Assistant => MAX_AGENT_REPLY_BYTES,
        };
        validate_text("agent transcript content", &self.content, maximum)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSnapshot {
    id: String,
    parent_id: String,
    name: String,
    depth: u16,
    status: AgentStatus,
    turns_used: u16,
    child_grants: Vec<GrantKind>,
    created_at_millis: u128,
    updated_at_millis: u128,
}

impl AgentSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        parent_id: impl Into<String>,
        name: impl Into<String>,
        depth: u16,
        status: AgentStatus,
        turns_used: u16,
        child_grants: Vec<GrantKind>,
        created_at_millis: u128,
        updated_at_millis: u128,
    ) -> Result<Self, AgentProtocolError> {
        let snapshot = Self {
            id: id.into(),
            parent_id: parent_id.into(),
            name: name.into(),
            depth,
            status,
            turns_used,
            child_grants,
            created_at_millis,
            updated_at_millis,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn depth(&self) -> u16 {
        self.depth
    }

    pub const fn status(&self) -> AgentStatus {
        self.status
    }

    pub const fn turns_used(&self) -> u16 {
        self.turns_used
    }

    pub fn child_grants(&self) -> &[GrantKind] {
        &self.child_grants
    }

    pub const fn created_at_millis(&self) -> u128 {
        self.created_at_millis
    }

    pub const fn updated_at_millis(&self) -> u128 {
        self.updated_at_millis
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        validate_agent_id(&self.id)?;
        validate_agent_id(&self.parent_id)?;
        validate_text("agent name", &self.name, MAX_AGENT_NAME_BYTES)?;
        validate_limit("agent depth", self.depth, MAX_AGENT_DEPTH)?;
        if self.turns_used > MAX_AGENT_TURNS_PER_AGENT {
            return Err(AgentProtocolError::LimitOutOfRange {
                field: "agent turns used",
                value: self.turns_used,
                maximum: MAX_AGENT_TURNS_PER_AGENT,
            });
        }
        validate_grants(&self.child_grants)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEvent {
    sequence: u64,
    agent_id: String,
    kind: AgentEventKind,
    detail: String,
    at_millis: u128,
}

impl AgentEvent {
    pub fn new(
        sequence: u64,
        agent_id: impl Into<String>,
        kind: AgentEventKind,
        detail: impl Into<String>,
        at_millis: u128,
    ) -> Result<Self, AgentProtocolError> {
        let event = Self {
            sequence,
            agent_id: agent_id.into(),
            kind,
            detail: detail.into(),
            at_millis,
        };
        event.validate()?;
        Ok(event)
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub const fn kind(&self) -> AgentEventKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub const fn at_millis(&self) -> u128 {
        self.at_millis
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        if self.sequence == 0 {
            return Err(AgentProtocolError::ZeroEventSequence);
        }
        validate_agent_id(&self.agent_id)?;
        validate_text("agent event detail", &self.detail, MAX_AGENT_DETAIL_BYTES)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpawnRequest {
    grant: AgentDelegationGrant,
    parent_id: String,
    name: Option<String>,
    task: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requested_child_grants: Vec<GrantKind>,
}

impl AgentSpawnRequest {
    pub fn new(
        grant: AgentDelegationGrant,
        task: impl Into<String>,
    ) -> Result<Self, AgentProtocolError> {
        let request = Self {
            grant,
            parent_id: ROOT_AGENT_ID.to_string(),
            name: None,
            task: task.into(),
            requested_child_grants: Vec::new(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Result<Self, AgentProtocolError> {
        self.parent_id = parent_id.into();
        self.validate()?;
        Ok(self)
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Result<Self, AgentProtocolError> {
        self.name = Some(name.into());
        self.validate()?;
        Ok(self)
    }

    pub fn with_requested_child_grants(
        mut self,
        grants: impl IntoIterator<Item = GrantKind>,
    ) -> Result<Self, AgentProtocolError> {
        self.requested_child_grants = grants.into_iter().collect();
        self.validate()?;
        Ok(self)
    }

    pub fn grant(&self) -> &AgentDelegationGrant {
        &self.grant
    }

    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn task(&self) -> &str {
        &self.task
    }

    pub fn requested_child_grants(&self) -> &[GrantKind] {
        &self.requested_child_grants
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        self.grant.validate()?;
        validate_agent_id(&self.parent_id)?;
        if let Some(name) = &self.name {
            validate_text("agent name", name, MAX_AGENT_NAME_BYTES)?;
        }
        validate_text("agent task", &self.task, MAX_AGENT_MESSAGE_BYTES)?;
        validate_grants(&self.requested_child_grants)?;
        if !self.grant.permits(&self.requested_child_grants) {
            return Err(AgentProtocolError::GrantEscalation);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpawnResult {
    agent: AgentSnapshot,
    event: AgentEvent,
}

impl AgentSpawnResult {
    pub fn new(agent: AgentSnapshot, event: AgentEvent) -> Self {
        Self { agent, event }
    }

    pub fn agent(&self) -> &AgentSnapshot {
        &self.agent
    }

    pub fn event(&self) -> &AgentEvent {
        &self.event
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentListRequest {
    grant: AgentDelegationGrant,
}

impl AgentListRequest {
    pub fn new(grant: AgentDelegationGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &AgentDelegationGrant {
        &self.grant
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        self.grant.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentListResult {
    budget: AgentBudget,
    agents: Vec<AgentSnapshot>,
    events: Vec<AgentEvent>,
    total_turns: u16,
    truncated: bool,
}

impl AgentListResult {
    pub fn new(
        budget: AgentBudget,
        agents: Vec<AgentSnapshot>,
        events: Vec<AgentEvent>,
        total_turns: u16,
        truncated: bool,
    ) -> Result<Self, AgentProtocolError> {
        if agents.len() > usize::from(MAX_AGENT_COUNT) {
            return Err(AgentProtocolError::TooManyAgents {
                count: agents.len(),
                maximum: usize::from(MAX_AGENT_COUNT),
            });
        }
        if events.len() > MAX_AGENT_EVENTS {
            return Err(AgentProtocolError::TooManyEvents {
                count: events.len(),
                maximum: MAX_AGENT_EVENTS,
            });
        }
        budget.validate()?;
        for agent in &agents {
            agent.validate()?;
        }
        for event in &events {
            event.validate()?;
        }
        Ok(Self {
            budget,
            agents,
            events,
            total_turns,
            truncated,
        })
    }

    pub const fn budget(&self) -> AgentBudget {
        self.budget
    }

    pub fn agents(&self) -> &[AgentSnapshot] {
        &self.agents
    }

    pub fn events(&self) -> &[AgentEvent] {
        &self.events
    }

    pub const fn total_turns(&self) -> u16 {
        self.total_turns
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInspectRequest {
    grant: AgentDelegationGrant,
    agent_id: String,
}

impl AgentInspectRequest {
    pub fn new(
        grant: AgentDelegationGrant,
        agent_id: impl Into<String>,
    ) -> Result<Self, AgentProtocolError> {
        let request = Self {
            grant,
            agent_id: agent_id.into(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn grant(&self) -> &AgentDelegationGrant {
        &self.grant
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        self.grant.validate()?;
        validate_agent_id(&self.agent_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInspectResult {
    agent: AgentSnapshot,
    transcript: Vec<AgentTranscriptEntry>,
}

impl AgentInspectResult {
    pub fn new(
        agent: AgentSnapshot,
        transcript: Vec<AgentTranscriptEntry>,
    ) -> Result<Self, AgentProtocolError> {
        agent.validate()?;
        if transcript.len() > MAX_AGENT_TRANSCRIPT_ENTRIES {
            return Err(AgentProtocolError::TooManyTranscriptEntries {
                count: transcript.len(),
                maximum: MAX_AGENT_TRANSCRIPT_ENTRIES,
            });
        }
        for entry in &transcript {
            entry.validate()?;
        }
        Ok(Self { agent, transcript })
    }

    pub fn agent(&self) -> &AgentSnapshot {
        &self.agent
    }

    pub fn transcript(&self) -> &[AgentTranscriptEntry] {
        &self.transcript
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnStartRequest {
    grant: AgentDelegationGrant,
    agent_id: String,
    message: String,
}

impl AgentTurnStartRequest {
    pub fn new(
        grant: AgentDelegationGrant,
        agent_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, AgentProtocolError> {
        let request = Self {
            grant,
            agent_id: agent_id.into(),
            message: message.into(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn grant(&self) -> &AgentDelegationGrant {
        &self.grant
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        self.grant.validate()?;
        validate_agent_id(&self.agent_id)?;
        validate_text("agent message", &self.message, MAX_AGENT_MESSAGE_BYTES)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnStartResult {
    agent: AgentSnapshot,
    transcript: Vec<AgentTranscriptEntry>,
    event: AgentEvent,
}

impl AgentTurnStartResult {
    pub fn new(
        agent: AgentSnapshot,
        transcript: Vec<AgentTranscriptEntry>,
        event: AgentEvent,
    ) -> Result<Self, AgentProtocolError> {
        if transcript.len() > MAX_AGENT_TRANSCRIPT_ENTRIES {
            return Err(AgentProtocolError::TooManyTranscriptEntries {
                count: transcript.len(),
                maximum: MAX_AGENT_TRANSCRIPT_ENTRIES,
            });
        }
        for entry in &transcript {
            entry.validate()?;
        }
        Ok(Self {
            agent,
            transcript,
            event,
        })
    }

    pub fn agent(&self) -> &AgentSnapshot {
        &self.agent
    }

    pub fn transcript(&self) -> &[AgentTranscriptEntry] {
        &self.transcript
    }

    pub fn event(&self) -> &AgentEvent {
        &self.event
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnCompleteRequest {
    grant: AgentDelegationGrant,
    agent_id: String,
    reply: String,
}

impl AgentTurnCompleteRequest {
    pub fn new(
        grant: AgentDelegationGrant,
        agent_id: impl Into<String>,
        reply: impl Into<String>,
    ) -> Result<Self, AgentProtocolError> {
        let request = Self {
            grant,
            agent_id: agent_id.into(),
            reply: reply.into(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn grant(&self) -> &AgentDelegationGrant {
        &self.grant
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn reply(&self) -> &str {
        &self.reply
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        self.grant.validate()?;
        validate_agent_id(&self.agent_id)?;
        validate_text("agent reply", &self.reply, MAX_AGENT_REPLY_BYTES)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnFailRequest {
    grant: AgentDelegationGrant,
    agent_id: String,
    code: String,
    message: String,
}

impl AgentTurnFailRequest {
    pub fn new(
        grant: AgentDelegationGrant,
        agent_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, AgentProtocolError> {
        let request = Self {
            grant,
            agent_id: agent_id.into(),
            code: code.into(),
            message: message.into(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn grant(&self) -> &AgentDelegationGrant {
        &self.grant
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        self.grant.validate()?;
        validate_agent_id(&self.agent_id)?;
        validate_token("agent failure code", &self.code, 64)?;
        validate_text(
            "agent failure message",
            &self.message,
            MAX_AGENT_DETAIL_BYTES,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInterruptRequest {
    grant: AgentDelegationGrant,
    agent_id: String,
    recursive: bool,
}

impl AgentInterruptRequest {
    pub fn new(
        grant: AgentDelegationGrant,
        agent_id: impl Into<String>,
        recursive: bool,
    ) -> Result<Self, AgentProtocolError> {
        let request = Self {
            grant,
            agent_id: agent_id.into(),
            recursive,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn grant(&self) -> &AgentDelegationGrant {
        &self.grant
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub const fn recursive(&self) -> bool {
        self.recursive
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        self.grant.validate()?;
        validate_agent_id(&self.agent_id)?;
        if self.agent_id == ROOT_AGENT_ID {
            return Err(AgentProtocolError::RootCannotBeTargeted);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMutationResult {
    agents: Vec<AgentSnapshot>,
    events: Vec<AgentEvent>,
}

impl AgentMutationResult {
    pub fn new(
        agents: Vec<AgentSnapshot>,
        events: Vec<AgentEvent>,
    ) -> Result<Self, AgentProtocolError> {
        if agents.len() > usize::from(MAX_AGENT_COUNT) {
            return Err(AgentProtocolError::TooManyAgents {
                count: agents.len(),
                maximum: usize::from(MAX_AGENT_COUNT),
            });
        }
        if events.len() > MAX_AGENT_EVENTS {
            return Err(AgentProtocolError::TooManyEvents {
                count: events.len(),
                maximum: MAX_AGENT_EVENTS,
            });
        }
        Ok(Self { agents, events })
    }

    pub fn agents(&self) -> &[AgentSnapshot] {
        &self.agents
    }

    pub fn events(&self) -> &[AgentEvent] {
        &self.events
    }
}

fn validate_limit(field: &'static str, value: u16, maximum: u16) -> Result<(), AgentProtocolError> {
    if value == 0 || value > maximum {
        return Err(AgentProtocolError::LimitOutOfRange {
            field,
            value,
            maximum,
        });
    }
    Ok(())
}

fn validate_agent_id(value: &str) -> Result<(), AgentProtocolError> {
    validate_token("agent id", value, MAX_AGENT_ID_BYTES)
}

fn validate_token(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), AgentProtocolError> {
    if value.is_empty() {
        return Err(AgentProtocolError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(AgentProtocolError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    if !value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(AgentProtocolError::InvalidToken {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), AgentProtocolError> {
    if value.trim().is_empty() {
        return Err(AgentProtocolError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(AgentProtocolError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    if value.contains('\0') {
        return Err(AgentProtocolError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_grants(grants: &[GrantKind]) -> Result<(), AgentProtocolError> {
    if grants.len() > MAX_DELEGATED_GRANTS {
        return Err(AgentProtocolError::TooManyDelegatedGrants {
            count: grants.len(),
            maximum: MAX_DELEGATED_GRANTS,
        });
    }
    let mut seen = BTreeSet::new();
    for grant in grants {
        if matches!(
            grant,
            GrantKind::Approval | GrantKind::ProviderCredential | GrantKind::AgentDelegation
        ) {
            return Err(AgentProtocolError::UndelegableGrant { grant: *grant });
        }
        if !seen.insert(*grant) {
            return Err(AgentProtocolError::DuplicateDelegatedGrant { grant: *grant });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentProtocolError {
    EmptyField {
        field: &'static str,
    },
    FieldTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    ControlCharacter {
        field: &'static str,
    },
    InvalidToken {
        field: &'static str,
        value: String,
    },
    LimitOutOfRange {
        field: &'static str,
        value: u16,
        maximum: u16,
    },
    TooManyDelegatedGrants {
        count: usize,
        maximum: usize,
    },
    DuplicateDelegatedGrant {
        grant: GrantKind,
    },
    UndelegableGrant {
        grant: GrantKind,
    },
    GrantEscalation,
    TooManyAgents {
        count: usize,
        maximum: usize,
    },
    TooManyEvents {
        count: usize,
        maximum: usize,
    },
    TooManyTranscriptEntries {
        count: usize,
        maximum: usize,
    },
    ZeroEventSequence,
    RootCannotBeTargeted,
}

impl fmt::Display for AgentProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(formatter, "{field} cannot be empty"),
            Self::FieldTooLong {
                field,
                length,
                maximum,
            } => write!(formatter, "{field} is {length} bytes; maximum is {maximum}"),
            Self::ControlCharacter { field } => {
                write!(formatter, "{field} contains a NUL character")
            }
            Self::InvalidToken { field, value } => {
                write!(formatter, "{field} `{value}` is not a valid identifier")
            }
            Self::LimitOutOfRange {
                field,
                value,
                maximum,
            } => write!(formatter, "{field} value {value} is outside 1..={maximum}"),
            Self::TooManyDelegatedGrants { count, maximum } => write!(
                formatter,
                "agent delegation contains {count} grants; maximum is {maximum}"
            ),
            Self::DuplicateDelegatedGrant { grant } => {
                write!(formatter, "agent delegation repeats grant `{grant}`")
            }
            Self::UndelegableGrant { grant } => {
                write!(
                    formatter,
                    "grant `{grant}` cannot be delegated to a child agent"
                )
            }
            Self::GrantEscalation => formatter
                .write_str("requested child grants exceed the host-issued delegation authority"),
            Self::TooManyAgents { count, maximum } => {
                write!(
                    formatter,
                    "agent result contains {count} agents; maximum is {maximum}"
                )
            }
            Self::TooManyEvents { count, maximum } => {
                write!(
                    formatter,
                    "agent result contains {count} events; maximum is {maximum}"
                )
            }
            Self::TooManyTranscriptEntries { count, maximum } => write!(
                formatter,
                "agent transcript contains {count} entries; maximum is {maximum}"
            ),
            Self::ZeroEventSequence => {
                formatter.write_str("agent event sequence must be greater than zero")
            }
            Self::RootCannotBeTargeted => {
                formatter.write_str("the root agent cannot be interrupted")
            }
        }
    }
}

impl Error for AgentProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> AgentDelegationGrant {
        AgentDelegationGrant::new(
            WorkspaceGrant::read_write("workspace"),
            "session-1",
            "ticket-1",
            AgentBudget::conservative(),
        )
        .expect("delegation grant")
    }

    #[test]
    fn typed_requests_round_trip_with_bounded_authority() {
        let request = AgentSpawnRequest::new(grant(), "inspect the failing test")
            .expect("spawn request")
            .with_name("tests")
            .expect("agent name");
        let encoded = serde_json::to_string(&request).expect("serialize request");
        assert!(!encoded.contains("provider_credential"));
        let decoded = serde_json::from_str::<AgentSpawnRequest>(&encoded).expect("decode request");
        decoded.validate().expect("validate request");
        assert_eq!(decoded.task(), "inspect the failing test");
    }

    #[test]
    fn child_grants_cannot_exceed_parent_authority() {
        let escalation = AgentSpawnRequest::new(grant(), "read a file")
            .expect("spawn request")
            .with_requested_child_grants([GrantKind::WorkspaceRead]);
        assert!(matches!(
            escalation,
            Err(AgentProtocolError::GrantEscalation)
        ));

        let credential = grant().with_allowed_child_grants([GrantKind::ProviderCredential]);
        assert!(matches!(
            credential,
            Err(AgentProtocolError::UndelegableGrant {
                grant: GrantKind::ProviderCredential
            })
        ));
    }

    #[test]
    fn budget_and_payload_limits_fail_closed() {
        assert!(AgentBudget::new(MAX_AGENT_COUNT + 1, 1, 1, 1).is_err());
        let oversized =
            AgentTurnStartRequest::new(grant(), "agent-1", "x".repeat(MAX_AGENT_MESSAGE_BYTES + 1));
        assert!(matches!(
            oversized,
            Err(AgentProtocolError::FieldTooLong { .. })
        ));
    }

    #[test]
    fn inspect_contract_round_trips_a_bounded_transcript() {
        let request = AgentInspectRequest::new(grant(), "agent-1").expect("inspect request");
        let encoded = serde_json::to_string(&request).expect("serialize inspect request");
        let decoded =
            serde_json::from_str::<AgentInspectRequest>(&encoded).expect("decode inspect request");
        decoded.validate().expect("validate inspect request");

        let agent = AgentSnapshot::new(
            "agent-1",
            ROOT_AGENT_ID,
            "worker",
            1,
            AgentStatus::Completed,
            1,
            Vec::new(),
            1,
            2,
        )
        .expect("snapshot");
        let result = AgentInspectResult::new(
            agent,
            vec![
                AgentTranscriptEntry::user("task").expect("user entry"),
                AgentTranscriptEntry::assistant("result").expect("assistant entry"),
            ],
        )
        .expect("inspect result");
        let decoded = serde_json::from_value::<AgentInspectResult>(
            serde_json::to_value(&result).expect("serialize inspect result"),
        )
        .expect("decode inspect result");
        assert_eq!(decoded.agent().id(), "agent-1");
        assert_eq!(decoded.transcript().len(), 2);
    }
}
