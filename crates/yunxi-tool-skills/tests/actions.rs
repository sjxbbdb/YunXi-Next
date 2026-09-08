//! Regression coverage for the opt-in executable Skill action boundary.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use yunxi_protocol::{
    ActionGrant, SkillActionOutcome, SkillActionRequest, SkillActionSpec, WorkspaceGrant,
};
use yunxi_tool_skills::{SkillActionError, SkillActionExecutor, SkillsConfig};

fn temp_root(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("yunxi-skill-actions-{label}-{stamp}"));
    fs::create_dir_all(&root).expect("root");
    root
}

fn fixture_skill(root: &Path, arguments: &[&str], timeout_millis: u64, max_output_bytes: usize) {
    let skill = root.join("skills").join("review");
    fs::create_dir_all(skill.join("bin")).expect("skill");
    fs::write(skill.join("SKILL.md"), "review instructions\n").expect("skill file");
    fs::write(
        skill.join("tools.json"),
        r#"[{"name":"check","description":"run check","input_schema":{"type":"object"}}]"#,
    )
    .expect("tools file");
    let program = skill.join("bin").join("action-fixture.exe");
    fs::copy(env!("CARGO_BIN_EXE_yunxi-skill-action-fixture"), program).expect("fixture copy");
    let spec = SkillActionSpec::new(
        "check",
        "bin/action-fixture.exe",
        arguments.iter().map(|value| (*value).to_string()).collect(),
        timeout_millis,
        max_output_bytes,
        false,
    )
    .expect("action spec");
    fs::write(
        skill.join("actions.json"),
        serde_json::to_vec(&vec![spec]).expect("actions JSON"),
    )
    .expect("actions file");
}

fn request(input: serde_json::Value) -> SkillActionRequest {
    SkillActionRequest::new("review", "check", input).expect("request")
}

fn grant(root: &PathBuf) -> ActionGrant {
    ActionGrant::approved(WorkspaceGrant::read_only(root), root, "test-ticket")
}

#[test]
fn action_executes_only_when_explicitly_enabled_and_returns_audit_result() {
    let root = temp_root("success");
    fixture_skill(&root, &[], 1_000, 128);
    let config = SkillsConfig::new(root.join("skills"), Vec::<String>::new())
        .expect("config")
        .with_actions_enabled(true);
    let executor = SkillActionExecutor::new(config);
    let declarations = executor.declarations().expect("action declarations");
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0].skill_id(), "review");
    assert_eq!(declarations[0].tool_name(), "check");
    assert!(!declarations[0].requires_workspace_write());
    let result = executor
        .execute(&request(json!({"value":"hello"})), &grant(&root))
        .expect("action");
    assert_eq!(result.outcome(), SkillActionOutcome::Success);
    assert_eq!(result.stdout(), "hello");
    assert!(!result.audit_id().is_empty());
    assert_eq!(result.program(), "bin/action-fixture.exe");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn action_is_fail_closed_by_default_and_without_declaration() {
    let root = temp_root("disabled");
    fixture_skill(&root, &[], 1_000, 128);
    let config = SkillsConfig::new(root.join("skills"), Vec::<String>::new()).expect("config");
    let error = SkillActionExecutor::new(config)
        .execute(&request(json!({})), &grant(&root))
        .expect_err("default must disable actions");
    assert!(matches!(error, SkillActionError::ActionsDisabled));
    let _ = fs::remove_file(root.join("skills").join("review").join("actions.json"));
    let config = SkillsConfig::new(root.join("skills"), Vec::<String>::new())
        .expect("config")
        .with_actions_enabled(true);
    let error = SkillActionExecutor::new(config)
        .execute(&request(json!({})), &grant(&root))
        .expect_err("undeclared action must fail closed");
    assert!(matches!(error, SkillActionError::ActionNotDeclared { .. }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn action_requires_approval_and_rejects_secret_or_network_authority() {
    let root = temp_root("grant");
    fixture_skill(&root, &[], 1_000, 128);
    let config = SkillsConfig::new(root.join("skills"), Vec::<String>::new())
        .expect("config")
        .with_actions_enabled(true);
    let executor = SkillActionExecutor::new(config);
    let denied = ActionGrant::pending(WorkspaceGrant::read_only(&root), &root);
    assert!(matches!(
        executor.execute(&request(json!({})), &denied),
        Err(SkillActionError::Grant(_))
    ));
    let network =
        ActionGrant::approved(WorkspaceGrant::read_only(&root), &root, "ticket").with_network(true);
    assert!(matches!(
        executor.execute(&request(json!({})), &network),
        Err(SkillActionError::ForbiddenGrant)
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn action_rejects_out_of_scope_working_directory() {
    let root = temp_root("scope");
    let outside = temp_root("outside");
    fixture_skill(&root, &[], 1_000, 128);
    let config = SkillsConfig::new(root.join("skills"), Vec::<String>::new())
        .expect("config")
        .with_actions_enabled(true);
    let bad_grant = ActionGrant::approved(WorkspaceGrant::read_only(&root), &outside, "ticket");
    let error = SkillActionExecutor::new(config)
        .execute(&request(json!({})), &bad_grant)
        .expect_err("outside cwd must fail");
    assert!(matches!(error, SkillActionError::OutOfScope { .. }));
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}

#[test]
fn action_bad_frame_and_timeout_do_not_escape_as_panics() {
    let root = temp_root("failure");
    fixture_skill(&root, &["--bad-frame"], 1_000, 128);
    let config = SkillsConfig::new(root.join("skills"), Vec::<String>::new())
        .expect("config")
        .with_actions_enabled(true);
    let executor = SkillActionExecutor::new(config);
    assert!(matches!(
        executor.execute(&request(json!({})), &grant(&root)),
        Err(SkillActionError::BadResponse(_))
    ));
    fixture_skill(&root, &["--crash"], 1_000, 128);
    let executor = SkillActionExecutor::new(
        SkillsConfig::new(root.join("skills"), Vec::<String>::new())
            .expect("config")
            .with_actions_enabled(true),
    );
    assert!(matches!(
        executor.execute(&request(json!({})), &grant(&root)),
        Err(SkillActionError::BadResponse(_))
    ));
    fixture_skill(&root, &["--huge-frame"], 1_000, 128);
    let executor = SkillActionExecutor::new(
        SkillsConfig::new(root.join("skills"), Vec::<String>::new())
            .expect("config")
            .with_actions_enabled(true),
    );
    assert!(matches!(
        executor.execute(&request(json!({})), &grant(&root)),
        Err(SkillActionError::FrameTooLarge { .. })
    ));
    fixture_skill(&root, &["--huge-stderr"], 1_000, 128);
    let executor = SkillActionExecutor::new(
        SkillsConfig::new(root.join("skills"), Vec::<String>::new())
            .expect("config")
            .with_actions_enabled(true),
    );
    assert!(matches!(
        executor.execute(&request(json!({})), &grant(&root)),
        Err(SkillActionError::FrameTooLarge { .. })
    ));
    fixture_skill(&root, &[], 1_000, 4);
    let executor = SkillActionExecutor::new(
        SkillsConfig::new(root.join("skills"), Vec::<String>::new())
            .expect("config")
            .with_actions_enabled(true),
    );
    assert!(matches!(
        executor.execute(&request(json!({"value":"hello"})), &grant(&root)),
        Err(SkillActionError::OutputLimit { maximum: 4 })
    ));
    fixture_skill(&root, &[], 1_000, 128);
    let executor = SkillActionExecutor::new(
        SkillsConfig::new(root.join("skills"), Vec::<String>::new())
            .expect("config")
            .with_actions_enabled(true),
    );
    let result = executor
        .execute(
            &request(json!({"sleep_millis":200})),
            &grant(&root).with_limits(20, 128),
        )
        .expect("timeout is a result");
    assert_eq!(result.outcome(), SkillActionOutcome::TimedOut);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cancellation_kills_a_real_action_child_and_returns_a_bounded_result() {
    let root = temp_root("cancel");
    fixture_skill(&root, &[], 1_000, 128);
    let executor = SkillActionExecutor::new(
        SkillsConfig::new(root.join("skills"), Vec::<String>::new())
            .expect("config")
            .with_actions_enabled(true),
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_for_thread = Arc::clone(&cancelled);
    let action_request = request(json!({"sleep_millis":500}));
    let action_grant = grant(&root);
    let handle = thread::spawn(move || {
        executor.execute_with_cancellation(&action_request, &action_grant, || {
            cancel_for_thread.load(Ordering::Acquire)
        })
    });
    thread::sleep(std::time::Duration::from_millis(40));
    cancelled.store(true, Ordering::Release);
    let result = handle
        .join()
        .expect("executor thread")
        .expect("cancel result");
    assert_eq!(result.outcome(), SkillActionOutcome::Cancelled);
    assert!(result.stderr().len() <= 128);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn action_input_and_executable_metadata_remain_bounded() {
    let oversized = SkillActionRequest::new("review", "check", json!({"x":"a".repeat(70_000)}));
    assert!(oversized.is_err());
    assert!(
        SkillActionSpec::new("check", "../outside.exe", Vec::new(), 1_000, 128, false,).is_err()
    );
}
