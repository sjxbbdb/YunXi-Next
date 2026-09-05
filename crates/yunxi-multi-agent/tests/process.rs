//! Real-process verification of the multi-agent coordinator contract.

use std::fs;
use std::process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_multi_agent::MULTI_AGENT_PLUGIN_ID;
use yunxi_plugin_host::{PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    AgentBudget, AgentDelegationGrant, AgentListRequest, AgentListResult, AgentSpawnRequest,
    AgentSpawnResult, AgentStatus, AgentTurnCompleteRequest, AgentTurnStartRequest,
    AgentTurnStartResult, CapabilityDescriptor, GrantKind, TOOL_MULTI_AGENT_LIST_OPERATION,
    TOOL_MULTI_AGENT_SPAWN_OPERATION, TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION,
    TOOL_MULTI_AGENT_TURN_START_OPERATION, WorkspaceGrant, capabilities,
};

#[test]
fn coordinator_process_persists_typed_child_state_across_restart() {
    let root = fixture_root();
    fs::create_dir_all(&root).expect("workspace");
    let capability = CapabilityDescriptor::new(
        capabilities::TOOL_MULTI_AGENT,
        capabilities::TOOL_MULTI_AGENT_VERSION,
    )
    .expect("capability");
    let grant = AgentDelegationGrant::new(
        WorkspaceGrant::read_write(&root),
        "process-session",
        "process-ticket",
        AgentBudget::conservative(),
    )
    .expect("grant");

    let mut first = launch_host();
    let spawned = first
        .invoke::<_, AgentSpawnResult>(
            &capability,
            TOOL_MULTI_AGENT_SPAWN_OPERATION,
            &AgentSpawnRequest::new(grant.clone(), "summarize the fixture").expect("spawn"),
        )
        .expect("spawn agent");
    let agent_id = spawned.agent().id().to_string();
    let started = first
        .invoke::<_, AgentTurnStartResult>(
            &capability,
            TOOL_MULTI_AGENT_TURN_START_OPERATION,
            &AgentTurnStartRequest::new(grant.clone(), &agent_id, "summarize the fixture")
                .expect("turn request"),
        )
        .expect("start turn");
    assert_eq!(started.agent().status(), AgentStatus::Running);
    first
        .invoke::<_, yunxi_protocol::AgentMutationResult>(
            &capability,
            TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION,
            &AgentTurnCompleteRequest::new(grant.clone(), &agent_id, "fixture summary")
                .expect("completion"),
        )
        .expect("complete turn");
    first.shutdown();

    let mut restarted = launch_host();
    let listed = restarted
        .invoke::<_, AgentListResult>(
            &capability,
            TOOL_MULTI_AGENT_LIST_OPERATION,
            &AgentListRequest::new(grant),
        )
        .expect("list after restart");
    assert_eq!(listed.agents().len(), 1);
    assert_eq!(listed.agents()[0].id(), agent_id);
    assert_eq!(listed.agents()[0].status(), AgentStatus::Completed);
    assert_eq!(listed.total_turns(), 1);
    restarted.shutdown();

    let persisted = fs::read_to_string(root.join(".yunxi-next/multi-agent/process-session.json"))
        .expect("persisted document");
    assert!(persisted.contains("fixture summary"));
    assert!(!persisted.contains("provider_credential"));
    let _ignored = fs::remove_dir_all(root);
}

fn launch_host() -> ProcessPluginHost {
    let id = PluginId::new(MULTI_AGENT_PLUGIN_ID).expect("plugin id");
    let launch = PluginLaunch::new(
        id,
        PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-multi-agent")),
    )
    .with_display_name("Multi-agent process fixture")
    .with_handshake_timeout(Duration::from_secs(3))
    .with_io_timeouts(Some(Duration::from_secs(3)), Some(Duration::from_secs(3)))
    .with_required_grants([
        GrantKind::Approval,
        GrantKind::WorkspaceRead,
        GrantKind::WorkspaceWrite,
        GrantKind::AgentDelegation,
    ]);
    let mut host = ProcessPluginHost::new();
    host.launch(launch).expect("launch coordinator");
    host
}

fn fixture_root() -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "yunxi-multi-agent-process-{}-{stamp}",
        process::id()
    ))
}
