//! Standalone management commands that do not require a model provider.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::args::{
    CompanionCommand, ControlScope, ControlsCommand, ManagementCommand, ManagementOptions,
    MemoryCommand, PersonaCommand, SessionCommand, VoiceCommand, WeixinCommand, WeixinPairCommand,
    WeixinSessionCommand,
};
use crate::migration;
use crate::session::{
    ChatBackend, ChatSession, CompanionPluginHost, MemoryPluginHost, PersonaPluginHost,
    StoragePluginHost, VoicePluginHost, WeixinPluginHost,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use yunxi_companion::{
    CompanionCheckResult, CompanionClearResult, CompanionHistoryResult, CompanionStatus,
};
use yunxi_memory::{
    MemoryListResult as PluginMemoryListResult, MemoryMutationResult as PluginMemoryMutationResult,
    MemoryRecordSummary as PluginMemoryRecordSummary,
    MemoryStatusReport as PluginMemoryStatusReport,
};
use yunxi_persona::{PersonaProfileSummary as PluginPersonaProfileSummary, PersonaStatus};
use yunxi_protocol::{
    COMPANION_MANAGEMENT_CHECK_OPERATION, COMPANION_MANAGEMENT_CLEAR_OPERATION,
    COMPANION_MANAGEMENT_HISTORY_OPERATION, COMPANION_MANAGEMENT_SET_ENABLED_OPERATION,
    COMPANION_MANAGEMENT_STATUS_OPERATION, ChatMessage, ChatRole, CompanionCheckRequest,
    CompanionClearRequest, CompanionHistoryRequest, CompanionSetEnabledRequest,
    CompanionStatusRequest, MAX_PERSONA_PROFILE_BYTES, MEMORY_MANAGEMENT_CLEAR_OPERATION,
    MEMORY_MANAGEMENT_MUTATE_OPERATION, MEMORY_MANAGEMENT_QUERY_OPERATION,
    MEMORY_MANAGEMENT_SET_ENABLED_OPERATION, MEMORY_MANAGEMENT_SHOW_OPERATION,
    MEMORY_MANAGEMENT_STATUS_OPERATION, MemoryClearRequest, MemoryClearScope,
    MemoryManagementScope, MemoryMutationAction, MemoryMutationRequest, MemoryQueryRequest,
    MemorySetEnabledRequest, MemoryShowRequest, MemoryStatusRequest,
    PERSONA_MANAGEMENT_IMPORT_OPERATION, PERSONA_MANAGEMENT_PROFILE_OPERATION,
    PERSONA_MANAGEMENT_RESET_OPERATION, PERSONA_MANAGEMENT_SET_ACTIVE_OPERATION,
    PERSONA_MANAGEMENT_SET_ENABLED_OPERATION, PERSONA_MANAGEMENT_STATUS_OPERATION,
    PersonaImportRequest, PersonaProfileRequest, PersonaResetRequest, PersonaSetActiveRequest,
    PersonaSetEnabledRequest, PersonaStatusRequest, STORAGE_SESSIONS_LIST_OPERATION,
    STORAGE_SESSIONS_LOAD_OPERATION, STORAGE_SESSIONS_MUTATE_OPERATION, SessionListRequest,
    SessionListResult, SessionLoadRequest, SessionLoadResult, SessionMutation,
    SessionMutationRequest, SessionMutationResult, SessionSnapshot, WorkspaceGrant,
};
use yunxi_settings::{CapabilitySettingsStore, next_state_root};
use yunxi_voice::{
    AudioChunk, AudioCodec, AudioFormat, ChatRequest as VoiceChatRequest,
    RequestId as VoiceRequestId, SpeakRequest, StreamId as VoiceStreamId, StreamStatus,
    TalkRequest, TranscribeRequest,
};

const MAX_SESSION_HISTORY_ITEMS: usize = 128;
const MAX_SESSION_ROLLOUT_ITEMS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ControlAuditRecord {
    timestamp_millis: u128,
    scope: String,
    verb: String,
    outcome: String,
    detail: String,
    source: String,
}

#[derive(Clone, Debug, Serialize)]
struct SessionThreadView {
    id: String,
    parent_id: Option<String>,
    cwd: PathBuf,
    archived: bool,
    pinned: bool,
    legacy: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SessionRolloutItem {
    index: usize,
    role: String,
    content: String,
}

#[derive(Clone, Debug, Serialize)]
struct SessionHistoryItem {
    session_id: String,
    kind: String,
    content: String,
}

#[derive(Clone, Debug, Serialize)]
struct SessionGraphNode {
    id: String,
    parent_id: Option<String>,
    title: String,
    message_count: usize,
    archived: bool,
    pinned: bool,
    legacy: bool,
    updated_at_millis: u128,
}

#[derive(Clone, Debug, Serialize)]
struct ControlScopeSnapshot {
    name: String,
    enabled: Option<bool>,
    summary: String,
    source: String,
    clear_effect: Option<String>,
}

const MAX_MEMORY_LINE_BYTES: usize = 1024 * 1024;
const MAX_COMPANION_HISTORY_RECORDS: usize = yunxi_protocol::MAX_COMPANION_HISTORY_RECORDS;
const MAX_CONTROL_AUDIT_RECORDS: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryScopeFilter {
    All,
    Global,
    Workspace,
}

pub(crate) fn run(options: ManagementOptions) -> Result<(), ManagementError> {
    let cwd = resolve_cwd(options.cwd.as_deref())?;
    let value = execute(&options.command, &cwd)?;
    if options.json || options.jsonl {
        let encoded = if options.jsonl {
            serde_json::to_string(&value)
        } else {
            serde_json::to_string_pretty(&value)
        }
        .map_err(|error| ManagementError::Output(error.to_string()))?;
        println!("{encoded}");
    } else {
        render_human(&value);
    }
    Ok(())
}

pub(crate) fn execute(command: &ManagementCommand, cwd: &Path) -> Result<Value, ManagementError> {
    match command {
        ManagementCommand::Sessions(command) => sessions(command, cwd),
        ManagementCommand::Memory(command) => memory(command, cwd),
        ManagementCommand::Persona(command) => persona(command, cwd),
        ManagementCommand::Companion(command) => companion(command, cwd),
        ManagementCommand::Controls(command) => controls(command, cwd),
        ManagementCommand::Voice(command) => voice(command, cwd),
        ManagementCommand::Weixin(command) => weixin(command, cwd),
        ManagementCommand::Migrate(command) => {
            migration::execute(command, cwd).map_err(ManagementError::Storage)
        }
    }
}

fn sessions(command: &SessionCommand, cwd: &Path) -> Result<Value, ManagementError> {
    let mut store = StoragePluginHost::launch(cwd).map_err(ManagementError::Capability)?;
    match command {
        SessionCommand::List { include_archived } => {
            let result: SessionListResult = store
                .invoke(
                    STORAGE_SESSIONS_LIST_OPERATION,
                    &SessionListRequest::new(next_session_grant(cwd, false))
                        .with_archived(*include_archived)
                        .with_limit(200),
                )
                .map_err(ManagementError::Capability)?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "sessions list",
                "workspace": cwd,
                "includeArchived": include_archived,
                "truncated": result.truncated(),
                "warnings": result.warnings(),
                "sessions": result.sessions(),
            }))
        }
        SessionCommand::Show(id) => {
            let session = load_session_snapshot(&mut store, cwd, id)?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "sessions show",
                "session": session,
                "warnings": [],
            }))
        }
        SessionCommand::Rollout(id) => {
            let session = load_session_snapshot(&mut store, cwd, id)?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "sessions rollout",
                "thread": session_thread_view(&session),
                "prompt": first_user_message(&session).unwrap_or_default(),
                "items": session_rollout_items(&session),
                "finalResponse": last_assistant_message(&session),
                "status": if last_assistant_message(&session).is_some() { "completed" } else { "unknown" },
                "session": session,
                "truncated": session.messages().len() > MAX_SESSION_ROLLOUT_ITEMS,
            }))
        }
        SessionCommand::History(id) => {
            let sessions = load_session_history(&mut store, cwd, id)?;
            let items = history_items(&sessions);
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "sessions history",
                "sessionId": id,
                "sessions": sessions,
                "items": items,
                "truncated": items.len() > MAX_SESSION_HISTORY_ITEMS,
            }))
        }
        SessionCommand::Graph => {
            let sessions = load_all_sessions(&mut store, cwd)?;
            let graph = session_graph(&sessions);
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "sessions graph",
                "graph": graph,
            }))
        }
        SessionCommand::Resume { id, prompt } => {
            let session = load_session_snapshot(&mut store, cwd, id)?;
            let options = crate::args::SessionOptions {
                cwd: Some(cwd.to_path_buf()),
                provider: session.provider().map(str::to_string),
                model: session.model().map(str::to_string),
                ..Default::default()
            };
            let mut host = ChatSession::launch_with_options(&options)
                .map_err(|error| ManagementError::Capability(error.to_string()))?;
            let messages = host
                .activate_web_session(id)
                .map_err(ManagementError::Capability)?;
            let resumed_prompt = if prompt.is_empty() {
                "Continue from the previous session.".to_string()
            } else {
                prompt.join(" ")
            };
            let mut conversation = messages.clone();
            conversation.push(ChatMessage::user(resumed_prompt.clone()));
            let reply = host
                .complete(&conversation)
                .map_err(|error| ManagementError::Capability(error.to_string()))?;
            let resumed = host.status();
            let updated = load_session_snapshot(&mut store, cwd, id)?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "sessions resume",
                "sessionId": id,
                "prompt": resumed_prompt,
                "reply": reply,
                "messages": conversation,
                "session": updated,
                "provider": host.provider(),
                "model": host.model(),
                "backend": {
                    "kernel": resumed.kernel,
                    "plugin": resumed.plugin,
                    "protocolReady": resumed.protocol_ready,
                    "plugins": resumed.plugins,
                    "capabilities": resumed.capabilities,
                    "failedPlugins": resumed.failed_plugins,
                },
                "persisted": true,
            }))
        }
        SessionCommand::Archive(id) => {
            mutate_session(&mut store, cwd, id, SessionMutation::Archive, "archive")
        }
        SessionCommand::Unarchive(id) => {
            mutate_session(&mut store, cwd, id, SessionMutation::Unarchive, "unarchive")
        }
        SessionCommand::Pin(id) => mutate_session(&mut store, cwd, id, SessionMutation::Pin, "pin"),
        SessionCommand::Unpin(id) => {
            mutate_session(&mut store, cwd, id, SessionMutation::Unpin, "unpin")
        }
        SessionCommand::Fork(id) => {
            mutate_session(&mut store, cwd, id, SessionMutation::Fork, "fork")
        }
    }
}

fn mutate_session(
    store: &mut StoragePluginHost,
    cwd: &Path,
    id: &str,
    mutation: SessionMutation,
    command: &str,
) -> Result<Value, ManagementError> {
    let result: SessionMutationResult = store
        .invoke(
            STORAGE_SESSIONS_MUTATE_OPERATION,
            &SessionMutationRequest::new(next_session_grant(cwd, true), id, mutation),
        )
        .map_err(ManagementError::Capability)?;
    let session = result
        .session()
        .ok_or_else(|| ManagementError::NotFound(format!("session `{id}`")))?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": format!("sessions {command}"),
        "session": session,
        "warnings": result.warnings(),
    }))
}

fn next_session_grant(cwd: &Path, writable: bool) -> WorkspaceGrant {
    let value = json!({
        "root": cwd,
        "state_root": next_state_root(),
        "legacy_read": false,
        "next_write": writable,
        "workspace_write": false,
    });
    serde_json::from_value(value).expect("construct next-only session grant")
}

fn management_state_grant(cwd: &Path, writable: bool) -> WorkspaceGrant {
    if writable {
        WorkspaceGrant::read_write(cwd).with_state_root(next_state_root())
    } else {
        WorkspaceGrant::read_only(cwd).with_state_root(next_state_root())
    }
}

fn load_session_snapshot(
    store: &mut StoragePluginHost,
    cwd: &Path,
    id: &str,
) -> Result<SessionSnapshot, ManagementError> {
    let result: SessionLoadResult = store
        .invoke(
            STORAGE_SESSIONS_LOAD_OPERATION,
            &SessionLoadRequest::new(next_session_grant(cwd, false), id),
        )
        .map_err(ManagementError::Capability)?;
    result
        .session()
        .cloned()
        .ok_or_else(|| ManagementError::NotFound(format!("session `{id}`")))
}

fn load_all_sessions(
    store: &mut StoragePluginHost,
    cwd: &Path,
) -> Result<Vec<SessionSnapshot>, ManagementError> {
    let result: SessionListResult = store
        .invoke(
            STORAGE_SESSIONS_LIST_OPERATION,
            &SessionListRequest::new(next_session_grant(cwd, false))
                .with_archived(true)
                .with_limit(200),
        )
        .map_err(ManagementError::Capability)?;
    let mut sessions = Vec::new();
    for summary in result.sessions() {
        let loaded: SessionLoadResult = store
            .invoke(
                STORAGE_SESSIONS_LOAD_OPERATION,
                &SessionLoadRequest::new(next_session_grant(cwd, false), summary.id()),
            )
            .map_err(ManagementError::Capability)?;
        let session = loaded
            .session()
            .cloned()
            .ok_or_else(|| ManagementError::NotFound(format!("session `{}`", summary.id())))?;
        sessions.push(session);
    }
    Ok(sessions)
}

fn load_session_history(
    store: &mut StoragePluginHost,
    cwd: &Path,
    id: &str,
) -> Result<Vec<SessionSnapshot>, ManagementError> {
    let mut sessions = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = Some(id.to_string());
    while let Some(current_id) = current {
        if sessions.len() >= MAX_SESSION_HISTORY_ITEMS {
            break;
        }
        if !seen.insert(current_id.clone()) {
            return Err(ManagementError::Storage(format!(
                "cycle detected while loading session history at `{current_id}`"
            )));
        }
        let loaded: SessionLoadResult = store
            .invoke(
                STORAGE_SESSIONS_LOAD_OPERATION,
                &SessionLoadRequest::new(next_session_grant(cwd, false), &current_id),
            )
            .map_err(ManagementError::Capability)?;
        let session = loaded
            .session()
            .cloned()
            .ok_or_else(|| ManagementError::NotFound(format!("session `{current_id}`")))?;
        current = session.parent_id().map(str::to_string);
        sessions.push(session);
    }
    sessions.reverse();
    Ok(sessions)
}

fn session_thread_view(session: &SessionSnapshot) -> Value {
    json!(SessionThreadView {
        id: session.id().to_string(),
        parent_id: session.parent_id().map(str::to_string),
        cwd: session.cwd().to_path_buf(),
        archived: session.archived(),
        pinned: session.pinned(),
        legacy: session.legacy(),
    })
}

fn session_rollout_items(session: &SessionSnapshot) -> Vec<SessionRolloutItem> {
    session
        .messages()
        .iter()
        .take(MAX_SESSION_ROLLOUT_ITEMS)
        .enumerate()
        .map(|(index, message)| SessionRolloutItem {
            index,
            role: chat_role_label(message.role()).to_string(),
            content: message.content().to_string(),
        })
        .collect()
}

fn history_items(sessions: &[SessionSnapshot]) -> Vec<SessionHistoryItem> {
    let mut items = Vec::new();
    for session in sessions {
        items.push(SessionHistoryItem {
            session_id: session.id().to_string(),
            kind: "user".to_string(),
            content: first_user_message(session).unwrap_or_default(),
        });
        if let Some(reply) = last_assistant_message(session) {
            items.push(SessionHistoryItem {
                session_id: session.id().to_string(),
                kind: "assistant".to_string(),
                content: reply,
            });
        }
    }
    items
}

fn session_graph(sessions: &[SessionSnapshot]) -> Value {
    let mut nodes = Vec::new();
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut roots = Vec::new();
    for session in sessions {
        let id = session.id().to_string();
        if let Some(parent_id) = session.parent_id() {
            children
                .entry(parent_id.to_string())
                .or_default()
                .push(id.clone());
        } else {
            roots.push(id.clone());
        }
        nodes.push(SessionGraphNode {
            id,
            parent_id: session.parent_id().map(str::to_string),
            title: session.title().to_string(),
            message_count: session.messages().len(),
            archived: session.archived(),
            pinned: session.pinned(),
            legacy: session.legacy(),
            updated_at_millis: session.updated_at_millis(),
        });
    }
    json!({
        "roots": roots,
        "children": children,
        "sessions": nodes,
    })
}

fn first_user_message(session: &SessionSnapshot) -> Option<String> {
    session
        .messages()
        .iter()
        .find(|message| matches!(message.role(), ChatRole::User))
        .map(|message| message.content().to_string())
}

fn last_assistant_message(session: &SessionSnapshot) -> Option<String> {
    session
        .messages()
        .iter()
        .rev()
        .find(|message| matches!(message.role(), ChatRole::Assistant))
        .map(|message| message.content().to_string())
}

fn chat_role_label(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn memory(command: &MemoryCommand, cwd: &Path) -> Result<Value, ManagementError> {
    let mut host = MemoryPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
    match command {
        MemoryCommand::Status => memory_status_value(&mut host, cwd),
        MemoryCommand::List { global, workspace } => {
            memory_list_value(&mut host, cwd, memory_scope_filter(*global, *workspace))
        }
        MemoryCommand::Show(id) => memory_show_value(&mut host, cwd, id),
        MemoryCommand::Pending => memory_pending_value(&mut host, cwd),
        MemoryCommand::Search {
            query,
            global,
            workspace,
        } => memory_search_value(
            &mut host,
            cwd,
            query,
            memory_scope_filter(*global, *workspace),
        ),
        MemoryCommand::Approve(id) => memory_mutation_value(
            &mut host,
            cwd,
            id,
            MemoryMutationAction::Approve,
            "memory approve",
        ),
        MemoryCommand::Reject(id) => memory_mutation_value(
            &mut host,
            cwd,
            id,
            MemoryMutationAction::Reject,
            "memory reject",
        ),
        MemoryCommand::Delete(id) => memory_mutation_value(
            &mut host,
            cwd,
            id,
            MemoryMutationAction::Archive,
            "memory delete",
        ),
        MemoryCommand::Clear { workspace, confirm } => {
            memory_clear_value(&mut host, cwd, *workspace, *confirm)
        }
        MemoryCommand::On => memory_toggle_value(&mut host, cwd, true),
        MemoryCommand::Off => memory_toggle_value(&mut host, cwd, false),
    }
}

fn persona(command: &PersonaCommand, cwd: &Path) -> Result<Value, ManagementError> {
    let mut host = PersonaPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
    match command {
        PersonaCommand::Status => persona_status_value(&mut host, cwd),
        PersonaCommand::List => persona_list_value(&mut host, cwd),
        PersonaCommand::Profile(id) => persona_profile_value(&mut host, cwd, id.as_deref()),
        PersonaCommand::Import(path) => persona_import_value(&mut host, cwd, path),
        PersonaCommand::Set(id) => persona_set_value(&mut host, cwd, id),
        PersonaCommand::On => persona_toggle_value(&mut host, cwd, true),
        PersonaCommand::Off => persona_toggle_value(&mut host, cwd, false),
    }
}

fn persona_result(command: &str, report: PersonaStatus) -> Result<Value, ManagementError> {
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": command,
        "enabled": report.enabled,
        "activeProfile": report.active_profile,
        "activeProfileSummary": report.active_profile_summary,
        "profiles": report.profiles,
        "warnings": report.warnings,
        "settingsPath": report.settings_path,
        "persisted": command != "persona status",
    }))
}

fn companion(command: &CompanionCommand, cwd: &Path) -> Result<Value, ManagementError> {
    let mut host = CompanionPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
    match command {
        CompanionCommand::Status => companion_status_value(&mut host, cwd),
        CompanionCommand::Check(prompt) => companion_check_value(&mut host, cwd, prompt),
        CompanionCommand::History => companion_history_value(&mut host, cwd),
        CompanionCommand::Clear { confirm } => companion_clear_value(&mut host, cwd, *confirm),
        CompanionCommand::On => companion_toggle_value(&mut host, cwd, true),
        CompanionCommand::Off => companion_toggle_value(&mut host, cwd, false),
    }
}

fn controls(command: &ControlsCommand, cwd: &Path) -> Result<Value, ManagementError> {
    const COMPONENTS: [(&str, &str); 3] = [
        ("persona", "yunxi.persona"),
        ("memory", "yunxi.memory"),
        ("companion", "yunxi.companion"),
    ];
    let mut settings = yunxi_settings::CapabilitySettingsStore::from_environment();
    match command {
        ControlsCommand::Status => Ok(json!({
            "schemaVersion": 1,
            "ok": true,
            "command": "controls status",
            "settingsPath": settings.path(),
            "revision": settings.revision(),
            "controls": COMPONENTS.iter().map(|(name, id)| json!({
                "name": name,
                "pluginId": id,
                "enabled": settings.plugin_enabled(id, true),
                "defaultEnabled": true,
                "requiresRestart": true,
            })).collect::<Vec<_>>(),
            "warnings": settings.take_warnings(),
            "note": "standalone settings changes are applied on the next Host restart",
        })),
        ControlsCommand::Show(scope) => control_show_value(cwd, *scope, &mut settings),
        ControlsCommand::Clear { scope, confirm } => {
            control_clear_value(cwd, *scope, *confirm, &mut settings)
        }
        ControlsCommand::Refresh => control_refresh_value(cwd, &mut settings),
        ControlsCommand::Audit => control_audit_value(cwd),
        ControlsCommand::Enable(name) | ControlsCommand::Disable(name) => {
            let enabled = matches!(command, ControlsCommand::Enable(_));
            let (_, plugin_id) = COMPONENTS
                .iter()
                .find(|(candidate, id)| *candidate == name || *id == name)
                .ok_or_else(|| ManagementError::NotFound(format!("control `{name}`")))?;
            let changed = settings
                .set_plugin(*plugin_id, enabled, Some(settings.revision()))
                .map_err(|error| ManagementError::Capability(error.to_string()))?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": if enabled { "controls enable" } else { "controls disable" },
                "name": name,
                "pluginId": plugin_id,
                "enabled": settings.plugin_enabled(plugin_id, true),
                "changed": changed,
                "persisted": true,
                "applied": false,
                "liveApplied": false,
                "requiresRestart": true,
                "settingsRevision": settings.revision(),
            }))
        }
    }
}

fn memory_status_value(host: &mut MemoryPluginHost, cwd: &Path) -> Result<Value, ManagementError> {
    let report: PluginMemoryStatusReport = host
        .invoke(
            MEMORY_MANAGEMENT_STATUS_OPERATION,
            &MemoryStatusRequest::new(management_state_grant(cwd, false)),
        )
        .map_err(ManagementError::Capability)?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "memory status",
        "workspace": cwd,
        "workspaceFingerprint": report.workspace_fingerprint,
        "enabled": report.enabled,
        "counts": report.counts,
        "warnings": report.warnings,
        "settingsPath": report.settings_path,
        "scope": "all",
        "source": "next",
    }))
}

fn memory_list_value(
    host: &mut MemoryPluginHost,
    cwd: &Path,
    scope: MemoryScopeFilter,
) -> Result<Value, ManagementError> {
    let result: PluginMemoryListResult = host
        .invoke(
            MEMORY_MANAGEMENT_QUERY_OPERATION,
            &MemoryQueryRequest::list(
                management_state_grant(cwd, false),
                plugin_memory_scope(scope),
                200,
            ),
        )
        .map_err(ManagementError::Capability)?;
    let scope_name = memory_scope_filter_name(scope);
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "memory list",
        "workspace": cwd,
        "scope": scope_name,
        "records": result.records,
        "truncated": result.truncated,
        "warnings": result.warnings,
    }))
}

fn memory_show_value(
    host: &mut MemoryPluginHost,
    cwd: &Path,
    id: &str,
) -> Result<Value, ManagementError> {
    let record: PluginMemoryRecordSummary = host
        .invoke::<_, Option<PluginMemoryRecordSummary>>(
            MEMORY_MANAGEMENT_SHOW_OPERATION,
            &MemoryShowRequest::new(management_state_grant(cwd, false), id),
        )
        .map_err(ManagementError::Capability)?
        .ok_or_else(|| ManagementError::NotFound(format!("memory `{id}`")))?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "memory show",
        "record": record,
        "warnings": [],
    }))
}

fn memory_pending_value(host: &mut MemoryPluginHost, cwd: &Path) -> Result<Value, ManagementError> {
    let result: PluginMemoryListResult = host
        .invoke(
            MEMORY_MANAGEMENT_QUERY_OPERATION,
            &MemoryQueryRequest::pending(management_state_grant(cwd, false), 200),
        )
        .map_err(ManagementError::Capability)?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "memory pending",
        "workspace": cwd,
        "records": result.records,
        "truncated": result.truncated,
        "warnings": result.warnings,
    }))
}

fn memory_search_value(
    host: &mut MemoryPluginHost,
    cwd: &Path,
    query: &str,
    scope: MemoryScopeFilter,
) -> Result<Value, ManagementError> {
    let query = query.trim().to_lowercase();
    let result: PluginMemoryListResult = host
        .invoke(
            MEMORY_MANAGEMENT_QUERY_OPERATION,
            &MemoryQueryRequest::search(
                management_state_grant(cwd, false),
                plugin_memory_scope(scope),
                &query,
                200,
            ),
        )
        .map_err(ManagementError::Capability)?;
    let scope_name = memory_scope_filter_name(scope);
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "memory search",
        "workspace": cwd,
        "scope": scope_name,
        "query": query,
        "records": result.records,
        "truncated": result.truncated,
        "warnings": result.warnings,
    }))
}

fn memory_mutation_value(
    host: &mut MemoryPluginHost,
    cwd: &Path,
    id: &str,
    action: MemoryMutationAction,
    command: &str,
) -> Result<Value, ManagementError> {
    let mut result: PluginMemoryMutationResult = host
        .invoke(
            MEMORY_MANAGEMENT_MUTATE_OPERATION,
            &MemoryMutationRequest::new(management_state_grant(cwd, true), id, action),
        )
        .map_err(ManagementError::Capability)?;
    let record = result
        .record
        .take()
        .ok_or_else(|| ManagementError::NotFound(format!("memory `{id}`")))?;
    let detail = format!(
        "memory id={id} status={} scope={}",
        record.status, record.scope
    );
    if let Err(error) =
        append_control_audit(ControlScope::Memory, "update", "completed", detail.clone())
    {
        result.warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": command,
        "id": id,
        "record": record,
        "changed": result.changed,
        "warnings": result.warnings,
    }))
}

fn memory_clear_value(
    host: &mut MemoryPluginHost,
    cwd: &Path,
    workspace: bool,
    confirm: bool,
) -> Result<Value, ManagementError> {
    if !workspace || !confirm {
        return Err(ManagementError::Capability(
            "memory clear requires --workspace --confirm".to_string(),
        ));
    }
    let mut result: PluginMemoryMutationResult = host
        .invoke(
            MEMORY_MANAGEMENT_CLEAR_OPERATION,
            &MemoryClearRequest::new(
                management_state_grant(cwd, true),
                MemoryClearScope::Workspace,
            ),
        )
        .map_err(ManagementError::Capability)?;
    if let Err(error) = append_control_audit(
        ControlScope::Memory,
        "clear",
        "completed",
        format!("archived workspace memory records={}", result.affected),
    ) {
        result.warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "memory clear",
        "scope": "workspace",
        "confirmed": true,
        "changed": result.changed,
        "affected": result.affected,
        "record": result.record,
        "warnings": result.warnings,
    }))
}

fn memory_toggle_value(
    host: &mut MemoryPluginHost,
    cwd: &Path,
    enabled: bool,
) -> Result<Value, ManagementError> {
    let refreshed: PluginMemoryStatusReport = host
        .invoke(
            MEMORY_MANAGEMENT_SET_ENABLED_OPERATION,
            &MemorySetEnabledRequest::new(management_state_grant(cwd, true), enabled),
        )
        .map_err(ManagementError::Capability)?;
    let mut warnings = refreshed.warnings.clone();
    if let Err(error) = append_control_audit(
        ControlScope::Memory,
        if enabled { "enable" } else { "disable" },
        "completed",
        format!("memory enabled={enabled}"),
    ) {
        warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": if enabled { "memory on" } else { "memory off" },
        "enabled": refreshed.enabled,
        "counts": refreshed.counts,
        "warnings": warnings,
        "settingsPath": refreshed.settings_path,
        "persisted": true,
        "requiresRestart": false,
    }))
}

fn plugin_memory_scope(scope: MemoryScopeFilter) -> MemoryManagementScope {
    match scope {
        MemoryScopeFilter::All => MemoryManagementScope::All,
        MemoryScopeFilter::Global => MemoryManagementScope::Global,
        MemoryScopeFilter::Workspace => MemoryManagementScope::Workspace,
    }
}

fn persona_status_value(
    host: &mut PersonaPluginHost,
    cwd: &Path,
) -> Result<Value, ManagementError> {
    let report = plugin_persona_status(host, cwd, false)?;
    persona_result("persona status", report)
}

fn persona_list_value(host: &mut PersonaPluginHost, cwd: &Path) -> Result<Value, ManagementError> {
    let report = plugin_persona_status(host, cwd, false)?;
    let mut warnings = report.warnings.clone();
    if let Err(error) = append_control_audit(
        ControlScope::Persona,
        "show",
        "completed",
        format!("persona list profiles={}", report.profiles.len()),
    ) {
        warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "persona list",
        "profiles": report.profiles,
        "warnings": warnings,
    }))
}

fn persona_profile_value(
    host: &mut PersonaPluginHost,
    cwd: &Path,
    id: Option<&str>,
) -> Result<Value, ManagementError> {
    let report = plugin_persona_status(host, cwd, false)?;
    let selected = id.unwrap_or(&report.active_profile);
    let profile: PluginPersonaProfileSummary = host
        .invoke::<_, Option<PluginPersonaProfileSummary>>(
            PERSONA_MANAGEMENT_PROFILE_OPERATION,
            &PersonaProfileRequest::new(
                management_state_grant(cwd, false),
                Some(selected.to_string()),
            ),
        )
        .map_err(ManagementError::Capability)?
        .ok_or_else(|| ManagementError::NotFound(format!("persona profile `{selected}`")))?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "persona profile",
        "profile": profile,
        "active": report.active_profile == selected,
        "warnings": report.warnings,
    }))
}

fn persona_import_value(
    host: &mut PersonaPluginHost,
    cwd: &Path,
    path: &str,
) -> Result<Value, ManagementError> {
    let path = resolve_input_path(path);
    let metadata = fs::metadata(&path).map_err(|error| {
        ManagementError::Capability(format!("failed to inspect {}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(ManagementError::Capability(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_PERSONA_PROFILE_BYTES as u64 {
        return Err(ManagementError::Capability(format!(
            "{} is {} bytes; maximum is {MAX_PERSONA_PROFILE_BYTES}",
            path.display(),
            metadata.len()
        )));
    }
    let content = fs::read_to_string(&path).map_err(|error| {
        ManagementError::Capability(format!("failed to read {}: {error}", path.display()))
    })?;
    let profile: PluginPersonaProfileSummary = host
        .invoke(
            PERSONA_MANAGEMENT_IMPORT_OPERATION,
            &PersonaImportRequest::new(management_state_grant(cwd, true), content),
        )
        .map_err(ManagementError::Capability)?;
    let mut warnings = Vec::new();
    if let Err(error) = append_control_audit(
        ControlScope::Persona,
        "import",
        "completed",
        format!("imported persona profile {}", profile.id),
    ) {
        warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "persona import",
        "profile": profile,
        "persisted": true,
        "source": "next",
        "warnings": warnings,
    }))
}

fn persona_set_value(
    host: &mut PersonaPluginHost,
    cwd: &Path,
    id: &str,
) -> Result<Value, ManagementError> {
    let refreshed: PersonaStatus = host
        .invoke(
            PERSONA_MANAGEMENT_SET_ACTIVE_OPERATION,
            &PersonaSetActiveRequest::new(management_state_grant(cwd, true), id),
        )
        .map_err(|error| {
            if error.contains("was not found") {
                ManagementError::NotFound(format!("persona profile `{id}`"))
            } else {
                ManagementError::Capability(error)
            }
        })?;
    let mut warnings = refreshed.warnings.clone();
    if let Err(error) = append_control_audit(
        ControlScope::Persona,
        "update",
        "completed",
        format!("persona active_profile={id}"),
    ) {
        warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "persona set",
        "enabled": refreshed.enabled,
        "activeProfile": refreshed.active_profile,
        "activeProfileSummary": refreshed.active_profile_summary,
        "profiles": refreshed.profiles,
        "warnings": warnings,
        "settingsPath": refreshed.settings_path,
        "persisted": true,
    }))
}

fn persona_toggle_value(
    host: &mut PersonaPluginHost,
    cwd: &Path,
    enabled: bool,
) -> Result<Value, ManagementError> {
    let refreshed: PersonaStatus = host
        .invoke(
            PERSONA_MANAGEMENT_SET_ENABLED_OPERATION,
            &PersonaSetEnabledRequest::new(management_state_grant(cwd, true), enabled),
        )
        .map_err(ManagementError::Capability)?;
    let mut warnings = refreshed.warnings.clone();
    if let Err(error) = append_control_audit(
        ControlScope::Persona,
        if enabled { "enable" } else { "disable" },
        "completed",
        format!("persona enabled={enabled}"),
    ) {
        warnings.push(error);
    }
    persona_result(
        if enabled { "persona on" } else { "persona off" },
        PersonaStatus {
            warnings,
            ..refreshed
        },
    )
}

fn plugin_persona_status(
    host: &mut PersonaPluginHost,
    cwd: &Path,
    writable: bool,
) -> Result<PersonaStatus, ManagementError> {
    host.invoke(
        PERSONA_MANAGEMENT_STATUS_OPERATION,
        &PersonaStatusRequest::new(management_state_grant(cwd, writable)),
    )
    .map_err(ManagementError::Capability)
}

fn companion_status_value(
    host: &mut CompanionPluginHost,
    cwd: &Path,
) -> Result<Value, ManagementError> {
    let status: CompanionStatus = host
        .invoke(
            COMPANION_MANAGEMENT_STATUS_OPERATION,
            &CompanionStatusRequest::new(management_state_grant(cwd, false)),
        )
        .map_err(ManagementError::Capability)?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "companion status",
        "enabled": status.enabled,
        "settingsPath": status.settings_path,
        "historyPath": status.history_path,
        "historyCount": status.history_count,
        "warnings": status.warnings,
    }))
}

fn companion_check_value(
    host: &mut CompanionPluginHost,
    cwd: &Path,
    prompt: &str,
) -> Result<Value, ManagementError> {
    let mut result: CompanionCheckResult = host
        .invoke(
            COMPANION_MANAGEMENT_CHECK_OPERATION,
            &CompanionCheckRequest::new(management_state_grant(cwd, true), prompt),
        )
        .map_err(ManagementError::Capability)?;
    if let Err(error) = append_control_audit(
        ControlScope::Companion,
        "check",
        "completed",
        format!("prompt={prompt}"),
    ) {
        result.warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "companion check",
        "enabled": result.enabled,
        "settingsPath": result.settings_path,
        "warnings": result.warnings,
        "historyPath": result.history_path,
        "historyCount": result.history_count,
        "decision": result.decision,
        "note": result.note,
    }))
}

fn companion_history_value(
    host: &mut CompanionPluginHost,
    cwd: &Path,
) -> Result<Value, ManagementError> {
    let result: CompanionHistoryResult = host
        .invoke(
            COMPANION_MANAGEMENT_HISTORY_OPERATION,
            &CompanionHistoryRequest::new(
                management_state_grant(cwd, false),
                MAX_COMPANION_HISTORY_RECORDS,
            ),
        )
        .map_err(ManagementError::Capability)?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "companion history",
        "historyPath": result.history_path,
        "records": result.records,
        "truncated": result.truncated,
        "warnings": result.warnings,
    }))
}

fn companion_clear_value(
    host: &mut CompanionPluginHost,
    cwd: &Path,
    confirm: bool,
) -> Result<Value, ManagementError> {
    if !confirm {
        return Err(ManagementError::Capability(
            "companion clear requires --confirm".to_string(),
        ));
    }
    let mut result: CompanionClearResult = host
        .invoke(
            COMPANION_MANAGEMENT_CLEAR_OPERATION,
            &CompanionClearRequest::new(management_state_grant(cwd, true)),
        )
        .map_err(ManagementError::Capability)?;
    if let Err(error) = append_control_audit(
        ControlScope::Companion,
        "clear",
        "completed",
        format!("cleared companion history records={}", result.cleared),
    ) {
        result.warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "companion clear",
        "confirmed": true,
        "cleared": result.cleared,
        "historyPath": result.history_path,
        "warnings": result.warnings,
    }))
}

fn companion_toggle_value(
    host: &mut CompanionPluginHost,
    cwd: &Path,
    enabled: bool,
) -> Result<Value, ManagementError> {
    let report: CompanionStatus = host
        .invoke(
            COMPANION_MANAGEMENT_SET_ENABLED_OPERATION,
            &CompanionSetEnabledRequest::new(management_state_grant(cwd, true), enabled),
        )
        .map_err(ManagementError::Capability)?;
    let mut warnings = report.warnings;
    if let Err(error) = append_control_audit(
        ControlScope::Companion,
        if enabled { "enable" } else { "disable" },
        "completed",
        format!("companion enabled={enabled}"),
    ) {
        warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": if enabled { "companion on" } else { "companion off" },
        "enabled": report.enabled,
        "settingsPath": report.settings_path,
        "warnings": warnings,
        "persisted": true,
        "requiresRestart": false,
    }))
}

fn control_refresh_value(
    _cwd: &Path,
    settings: &mut CapabilitySettingsStore,
) -> Result<Value, ManagementError> {
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "controls refresh",
        "refreshed": true,
        "snapshot": controls_status_value(settings)?,
    }))
}

fn control_audit_value(_cwd: &Path) -> Result<Value, ManagementError> {
    let (records, warnings) = load_control_audit_records()?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "controls audit",
        "auditPath": control_audit_path(),
        "records": records,
        "truncated": records.len() == MAX_CONTROL_AUDIT_RECORDS,
        "warnings": warnings,
    }))
}

fn control_scope_name(scope: ControlScope) -> &'static str {
    match scope {
        ControlScope::Companion => "companion",
        ControlScope::Memory => "memory",
        ControlScope::Persona => "persona",
        ControlScope::Relationship => "relationship",
    }
}

fn append_control_audit(
    scope: ControlScope,
    verb: &str,
    outcome: &str,
    detail: impl Into<String>,
) -> Result<(), String> {
    append_jsonl_record(
        &control_audit_path(),
        &ControlAuditRecord {
            timestamp_millis: now_millis(),
            scope: control_scope_name(scope).to_string(),
            verb: verb.to_string(),
            outcome: outcome.to_string(),
            detail: detail.into(),
            source: "yunxi-cli".to_string(),
        },
    )
}

fn load_control_audit_records() -> Result<(Vec<ControlAuditRecord>, Vec<String>), ManagementError> {
    let mut warnings = Vec::new();
    let records = read_jsonl_records::<ControlAuditRecord>(
        &control_audit_path(),
        &mut warnings,
        MAX_CONTROL_AUDIT_RECORDS,
        16 * 1024 * 1024,
        MAX_MEMORY_LINE_BYTES,
    );
    Ok((records, warnings))
}

fn control_audit_path() -> PathBuf {
    next_state_root().join("controls").join("audit.jsonl")
}

fn control_show_value(
    cwd: &Path,
    scope: ControlScope,
    settings: &mut CapabilitySettingsStore,
) -> Result<Value, ManagementError> {
    let plugin_id = match scope {
        ControlScope::Companion => "yunxi.companion",
        ControlScope::Memory | ControlScope::Relationship => "yunxi.memory",
        ControlScope::Persona => "yunxi.persona",
    };
    let (snapshot, details) = if !settings.plugin_enabled(plugin_id, true) {
        (
            ControlScopeSnapshot {
                name: control_scope_name(scope).to_string(),
                enabled: Some(false),
                summary: "plugin disabled".to_string(),
                source: "settings".to_string(),
                clear_effect: None,
            },
            json!({
                "available": false,
                "reason": "plugin is disabled",
                "pluginId": plugin_id,
            }),
        )
    } else {
        match scope {
            ControlScope::Companion => {
                let mut host =
                    CompanionPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
                let status: CompanionStatus = host
                    .invoke(
                        COMPANION_MANAGEMENT_STATUS_OPERATION,
                        &CompanionStatusRequest::new(management_state_grant(cwd, false)),
                    )
                    .map_err(ManagementError::Capability)?;
                let history: CompanionHistoryResult = host
                    .invoke(
                        COMPANION_MANAGEMENT_HISTORY_OPERATION,
                        &CompanionHistoryRequest::new(management_state_grant(cwd, false), 20),
                    )
                    .map_err(ManagementError::Capability)?;
                (
                    ControlScopeSnapshot {
                        name: "companion".to_string(),
                        enabled: Some(status.enabled),
                        summary: format!("{} history records", status.history_count),
                        source: "next".to_string(),
                        clear_effect: Some("clears companion history".to_string()),
                    },
                    json!({
                        "status": status,
                        "historyPath": history.history_path,
                        "history": history.records,
                        "truncated": history.truncated,
                        "warnings": history.warnings,
                    }),
                )
            }
            ControlScope::Memory | ControlScope::Relationship => {
                let mut host =
                    MemoryPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
                let status: PluginMemoryStatusReport = host
                    .invoke(
                        MEMORY_MANAGEMENT_STATUS_OPERATION,
                        &MemoryStatusRequest::new(management_state_grant(cwd, false)),
                    )
                    .map_err(ManagementError::Capability)?;
                let relationship = matches!(scope, ControlScope::Relationship);
                let records: PluginMemoryListResult = host
                    .invoke(
                        MEMORY_MANAGEMENT_QUERY_OPERATION,
                        &MemoryQueryRequest::list(
                            management_state_grant(cwd, false),
                            if relationship {
                                MemoryManagementScope::Relationship
                            } else {
                                MemoryManagementScope::All
                            },
                            20,
                        ),
                    )
                    .map_err(ManagementError::Capability)?;
                let snapshot = if relationship {
                    ControlScopeSnapshot {
                        name: "relationship".to_string(),
                        enabled: None,
                        summary: format!("{} relationship records", records.records.len()),
                        source: "next".to_string(),
                        clear_effect: Some("archives relationship memory".to_string()),
                    }
                } else {
                    ControlScopeSnapshot {
                        name: "memory".to_string(),
                        enabled: Some(status.enabled),
                        summary: format!(
                            "{} records (active={}, pending={}, rejected={}, archived={})",
                            status.counts.total,
                            status.counts.active,
                            status.counts.pending,
                            status.counts.rejected,
                            status.counts.archived
                        ),
                        source: "next".to_string(),
                        clear_effect: Some("archives workspace memory".to_string()),
                    }
                };
                let details = if relationship {
                    json!({
                        "records": records.records,
                        "truncated": records.truncated,
                        "warnings": records.warnings,
                    })
                } else {
                    json!({
                        "status": status,
                        "records": records.records,
                        "truncated": records.truncated,
                        "warnings": records.warnings,
                    })
                };
                (snapshot, details)
            }
            ControlScope::Persona => {
                let mut host =
                    PersonaPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
                let status = plugin_persona_status(&mut host, cwd, false)?;
                (
                    ControlScopeSnapshot {
                        name: "persona".to_string(),
                        enabled: Some(status.enabled),
                        summary: format!("active profile {}", status.active_profile),
                        source: "next".to_string(),
                        clear_effect: Some("resets persona settings to defaults".to_string()),
                    },
                    json!({ "status": status }),
                )
            }
        }
    };
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "controls show",
        "scope": control_scope_name(scope),
        "snapshot": snapshot,
        "details": details,
    }))
}

fn controls_status_value(settings: &mut CapabilitySettingsStore) -> Result<Value, ManagementError> {
    const COMPONENTS: [(&str, &str); 3] = [
        ("persona", "yunxi.persona"),
        ("memory", "yunxi.memory"),
        ("companion", "yunxi.companion"),
    ];
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "controls status",
        "settingsPath": settings.path(),
        "revision": settings.revision(),
        "controls": COMPONENTS.iter().map(|(name, id)| json!({
            "name": name,
            "pluginId": id,
            "enabled": settings.plugin_enabled(id, true),
            "defaultEnabled": true,
            "requiresRestart": true,
        })).collect::<Vec<_>>(),
        "warnings": settings.take_warnings(),
        "note": "standalone settings changes are applied on the next Host restart",
    }))
}

fn control_clear_value(
    cwd: &Path,
    scope: ControlScope,
    confirm: bool,
    _settings: &mut CapabilitySettingsStore,
) -> Result<Value, ManagementError> {
    if !confirm {
        return Err(ManagementError::Capability(
            "control clear requires --confirm".to_string(),
        ));
    }
    let (result, mut warnings) = match scope {
        ControlScope::Companion => {
            let mut host = CompanionPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
            let result: CompanionClearResult = host
                .invoke(
                    COMPANION_MANAGEMENT_CLEAR_OPERATION,
                    &CompanionClearRequest::new(management_state_grant(cwd, true)),
                )
                .map_err(ManagementError::Capability)?;
            (
                format!("cleared companion history records={}", result.cleared),
                result.warnings,
            )
        }
        ControlScope::Memory => {
            let mut host = MemoryPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
            let mutation: PluginMemoryMutationResult = host
                .invoke(
                    MEMORY_MANAGEMENT_CLEAR_OPERATION,
                    &MemoryClearRequest::new(
                        management_state_grant(cwd, true),
                        MemoryClearScope::Workspace,
                    ),
                )
                .map_err(ManagementError::Capability)?;
            (
                format!("archived workspace memory affected={}", mutation.affected),
                mutation.warnings,
            )
        }
        ControlScope::Persona => {
            let mut host = PersonaPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
            let status: PersonaStatus = host
                .invoke(
                    PERSONA_MANAGEMENT_RESET_OPERATION,
                    &PersonaResetRequest::new(management_state_grant(cwd, true)),
                )
                .map_err(ManagementError::Capability)?;
            (
                "reset persona settings to defaults".to_string(),
                status.warnings,
            )
        }
        ControlScope::Relationship => {
            let mut host = MemoryPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
            let mutation: PluginMemoryMutationResult = host
                .invoke(
                    MEMORY_MANAGEMENT_CLEAR_OPERATION,
                    &MemoryClearRequest::new(
                        management_state_grant(cwd, true),
                        MemoryClearScope::Relationship,
                    ),
                )
                .map_err(ManagementError::Capability)?;
            (
                format!(
                    "archived relationship memory affected={}",
                    mutation.affected
                ),
                mutation.warnings,
            )
        }
    };
    if let Err(error) = append_control_audit(scope, "clear", "completed", result.clone()) {
        warnings.push(error);
    }
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "controls clear",
        "scope": control_scope_name(scope),
        "confirmed": true,
        "result": result,
        "warnings": warnings,
    }))
}

fn memory_scope_filter(global: bool, workspace: bool) -> MemoryScopeFilter {
    if workspace {
        MemoryScopeFilter::Workspace
    } else if global {
        MemoryScopeFilter::Global
    } else {
        MemoryScopeFilter::All
    }
}

fn memory_scope_filter_name(scope: MemoryScopeFilter) -> &'static str {
    match scope {
        MemoryScopeFilter::All => "all",
        MemoryScopeFilter::Global => "global",
        MemoryScopeFilter::Workspace => "workspace",
    }
}

fn append_jsonl_record<T: Serialize>(path: &Path, record: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut line = serde_json::to_vec(record).map_err(|error| error.to_string())?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(&line)
        .and_then(|_| file.sync_data())
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn read_jsonl_records<T>(
    path: &Path,
    warnings: &mut Vec<String>,
    max_records: usize,
    max_file_bytes: u64,
    max_line_bytes: usize,
) -> Vec<T>
where
    T: for<'de> Deserialize<'de>,
{
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            warnings.push(format!("failed to inspect {}: {error}", path.display()));
            return Vec::new();
        }
    };
    if !metadata.is_file() {
        warnings.push(format!("path is not a file: {}", path.display()));
        return Vec::new();
    }
    if metadata.len() > max_file_bytes {
        warnings.push(format!(
            "file {} is {} bytes; maximum is {max_file_bytes}",
            path.display(),
            metadata.len()
        ));
        return Vec::new();
    }
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            warnings.push(format!("failed to read {}: {error}", path.display()));
            return Vec::new();
        }
    };
    let mut records = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if records.len() >= max_records {
            warnings.push(format!(
                "record limit reached at {max_records}; remaining entries in {} were skipped",
                path.display()
            ));
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > max_line_bytes {
            warnings.push(format!(
                "line {} in {} exceeds {max_line_bytes} bytes",
                index + 1,
                path.display()
            ));
            continue;
        }
        match serde_json::from_str::<T>(line) {
            Ok(record) => records.push(record),
            Err(error) => warnings.push(format!(
                "failed to parse line {} in {}: {error}",
                index + 1,
                path.display()
            )),
        }
    }
    records
}

fn resolve_input_path(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn voice(command: &VoiceCommand, cwd: &Path) -> Result<Value, ManagementError> {
    let mut host = VoicePluginHost::launch(cwd).map_err(ManagementError::Capability)?;
    let (operation, payload) = match command {
        VoiceCommand::Status | VoiceCommand::Doctor => {
            ("doctor", json!({ "request_id": "cli-doctor" }))
        }
        VoiceCommand::Devices => ("enumerate_devices", json!({ "request_id": "cli-devices" })),
        VoiceCommand::Transcribe(text) => (
            "transcribe",
            serde_json::to_value(sample_transcribe_request(text)?)
                .map_err(|error| ManagementError::Output(error.to_string()))?,
        ),
        VoiceCommand::Speak(text) => (
            "speak",
            serde_json::to_value(sample_speak_request(text)?)
                .map_err(|error| ManagementError::Output(error.to_string()))?,
        ),
        VoiceCommand::Chat(text) => (
            "chat",
            serde_json::to_value(
                VoiceChatRequest::new(
                    VoiceRequestId::new("cli-chat").map_err(voice_error)?,
                    "cli-loopback",
                    text,
                )
                .map_err(voice_error)?,
            )
            .map_err(|error| ManagementError::Output(error.to_string()))?,
        ),
        VoiceCommand::Talk(text) => (
            "talk",
            serde_json::to_value(
                TalkRequest::new(
                    VoiceRequestId::new("cli-talk").map_err(voice_error)?,
                    sample_transcribe_request(text)?,
                    cli_audio_format().map_err(voice_error)?,
                    StreamStatus::new(),
                )
                .map_err(voice_error)?,
            )
            .map_err(|error| ManagementError::Output(error.to_string()))?,
        ),
    };
    let report = host
        .invoke(operation, &payload)
        .map_err(ManagementError::Capability)?;
    let sidecar_configured =
        std::env::var_os("YUNXI_VOICE_SIDECAR_PROGRAM").is_some_and(|value| !value.is_empty());
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": voice_command_name(command),
        "operation": operation,
        "mode": if sidecar_configured { "sidecar" } else { "loopback-fixture" },
        "productionReady": false,
        "isolatedProcess": true,
        "note": if sidecar_configured {
            "The configured Voice sidecar ran through the supervised plugin Host; hardware readiness is reported by voice doctor and devices."
        } else {
            "No microphone, speaker, codec, or provider credentials are configured; the supervised Voice plugin uses deterministic loopback."
        },
        "report": report,
    }))
}

fn weixin(command: &WeixinCommand, cwd: &Path) -> Result<Value, ManagementError> {
    let mut host = WeixinPluginHost::launch(cwd).map_err(ManagementError::Capability)?;
    let (operation, payload) = match command {
        WeixinCommand::Status => (yunxi_weixin::STATUS_OPERATION, json!({})),
        WeixinCommand::Doctor => (yunxi_weixin::RUNTIME_DOCTOR_OPERATION, json!({})),
        WeixinCommand::Login => (yunxi_weixin::LOGIN_OPERATION, json!({})),
        WeixinCommand::PollLogin(verify_code) => (
            yunxi_weixin::POLL_LOGIN_OPERATION,
            json!({ "verify_code": verify_code }),
        ),
        WeixinCommand::Serve => {
            let max_polls = std::env::var("YUNXI_WEIXIN_MAX_POLLS")
                .ok()
                .map(|value| {
                    value.parse::<usize>().map_err(|_| {
                        ManagementError::Capability(
                            "YUNXI_WEIXIN_MAX_POLLS must be a positive integer".to_owned(),
                        )
                    })
                })
                .transpose()?;
            (
                yunxi_weixin::SERVE_OPERATION,
                json!({ "max_polls": max_polls, "require_approval": false }),
            )
        }
        WeixinCommand::Pair(pair) => {
            let payload = match pair {
                WeixinPairCommand::Request(peer_id) => {
                    json!({ "action": "request", "peer_id": peer_id })
                }
                WeixinPairCommand::Approve(request_id) => {
                    json!({ "action": "approve", "request_id": request_id })
                }
                WeixinPairCommand::Deny { request_id, reason } => json!({
                    "action": "deny",
                    "request_id": request_id,
                    "reason": reason,
                }),
            };
            (yunxi_weixin::PAIR_OPERATION, payload)
        }
        WeixinCommand::Session(session) => {
            let payload = match session {
                WeixinSessionCommand::Bind {
                    session_id,
                    peer_id,
                } => json!({
                    "action": "bind",
                    "session_id": session_id,
                    "peer_id": peer_id,
                }),
                WeixinSessionCommand::Unbind(session_id) => {
                    json!({ "action": "unbind", "session_id": session_id })
                }
                WeixinSessionCommand::List => json!({ "action": "list" }),
            };
            (yunxi_weixin::SESSION_OPERATION, payload)
        }
        WeixinCommand::Logout => (yunxi_weixin::LOGOUT_OPERATION, json!({})),
    };
    let runtime = host
        .invoke(operation, &payload)
        .map_err(ManagementError::Capability)?;
    let mode = runtime
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if yunxi_weixin::weixin_production_requested() {
                "production"
            } else {
                "loopback-fixture"
            }
        })
        .to_owned();
    let report = runtime.get("report").cloned().unwrap_or(runtime);
    let production_ready = report
        .get("production_ready")
        .or_else(|| report.get("productionReady"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": weixin_command_name(command),
        "operation": operation,
        "mode": mode,
        "productionReady": production_ready,
        "isolatedProcess": true,
        "note": if mode == "production" {
            "The explicit HTTPS iLink transport ran inside the supervised Weixin plugin; account authentication remains operator-controlled."
        } else {
            "No external Weixin account is connected; the supervised plugin uses deterministic iLink loopback."
        },
        "report": report,
    }))
}

fn voice_command_name(command: &VoiceCommand) -> &'static str {
    match command {
        VoiceCommand::Status => "voice status",
        VoiceCommand::Doctor => "voice doctor",
        VoiceCommand::Devices => "voice devices",
        VoiceCommand::Transcribe(_) => "voice transcribe",
        VoiceCommand::Speak(_) => "voice speak",
        VoiceCommand::Chat(_) => "voice chat",
        VoiceCommand::Talk(_) => "voice talk",
    }
}

fn weixin_command_name(command: &WeixinCommand) -> &'static str {
    match command {
        WeixinCommand::Status => "weixin status",
        WeixinCommand::Doctor => "weixin doctor",
        WeixinCommand::Login => "weixin login",
        WeixinCommand::PollLogin(_) => "weixin poll-login",
        WeixinCommand::Serve => "weixin serve",
        WeixinCommand::Pair(_) => "weixin pair",
        WeixinCommand::Session(_) => "weixin session",
        WeixinCommand::Logout => "weixin logout",
    }
}

fn voice_error(error: impl fmt::Display) -> ManagementError {
    ManagementError::Capability(error.to_string())
}

fn cli_audio_format() -> Result<AudioFormat, yunxi_voice::VoiceProviderError> {
    AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).map_err(Into::into)
}

fn sample_transcribe_request(text: &str) -> Result<TranscribeRequest, ManagementError> {
    let stream_id = VoiceStreamId::new("cli-stream").map_err(voice_error)?;
    let format = cli_audio_format().map_err(voice_error)?;
    let chunk =
        AudioChunk::new(stream_id.clone(), 0, format, vec![0; 8], true).map_err(voice_error)?;
    // The loopback transcriber emits deterministic text; the input text is
    // retained as a bounded request label for callers inspecting the result.
    let _ = text;
    TranscribeRequest::new(
        VoiceRequestId::new("cli-transcribe").map_err(voice_error)?,
        stream_id,
        format,
        vec![chunk],
        true,
        StreamStatus::new(),
    )
    .map_err(voice_error)
}

fn sample_speak_request(text: &str) -> Result<SpeakRequest, ManagementError> {
    SpeakRequest::new(
        VoiceRequestId::new("cli-speak").map_err(voice_error)?,
        VoiceStreamId::new("cli-stream").map_err(voice_error)?,
        text,
        cli_audio_format().map_err(voice_error)?,
        StreamStatus::new(),
    )
    .map_err(voice_error)
}

fn render_human(value: &Value) {
    println!(
        "{}: {}",
        value["command"].as_str().unwrap_or("management"),
        if value["ok"].as_bool().unwrap_or(false) {
            "ok"
        } else {
            "failed"
        }
    );
    if let Some(note) = value["note"].as_str() {
        println!("note: {note}");
    }
    if let Some(sessions) = value["sessions"].as_array() {
        if sessions.is_empty() {
            println!("No saved sessions.");
        } else {
            for session in sessions {
                println!(
                    "{} | {} messages | {}",
                    session["id"].as_str().unwrap_or("unknown"),
                    session["messageCount"].as_u64().unwrap_or_default(),
                    session["title"].as_str().unwrap_or("untitled")
                );
            }
        }
    }
    if let Some(report) = value["report"].as_object() {
        println!(
            "state: {}",
            report
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        );
        println!(
            "authenticated: {}",
            report
                .get("authenticated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
    }
    if let Some(doctor) = value["doctor"].as_object() {
        println!(
            "doctor: {}",
            doctor
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        );
    }
}

fn resolve_cwd(path: Option<&Path>) -> Result<PathBuf, ManagementError> {
    let path = path.unwrap_or_else(|| Path::new("."));
    let path = std::fs::canonicalize(path)
        .map_err(|error| ManagementError::Workspace(format!("{}: {error}", path.display())))?;
    if !path.is_dir() {
        return Err(ManagementError::Workspace(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    Ok(path)
}

#[derive(Debug)]
pub(crate) enum ManagementError {
    Workspace(String),
    Storage(String),
    Capability(String),
    NotFound(String),
    Output(String),
}

impl fmt::Display for ManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(message) => write!(formatter, "workspace error: {message}"),
            Self::Storage(message) => write!(formatter, "storage error: {message}"),
            Self::Capability(message) => write!(formatter, "capability error: {message}"),
            Self::NotFound(message) => write!(formatter, "{message} was not found"),
            Self::Output(message) => write!(formatter, "output error: {message}"),
        }
    }
}

impl Error for ManagementError {}
