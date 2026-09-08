use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn unique_temp_dir(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("yunxi-cli-{label}-{stamp}"));
    fs::create_dir_all(&path).expect("create test workspace");
    path
}

fn run_cli(args: &[&str], cwd: &Path, home: &Path) -> Output {
    run_cli_with_env(args, cwd, home, &[])
}

fn run_cli_with_env(
    args: &[&str],
    cwd: &Path,
    home: &Path,
    environment: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yunxi-next"));
    command
        .args(args)
        .env("YUNXI_NEXT_HOME", home)
        .env("YUNXI_HOME", home.join("legacy"))
        .env_remove("YUNXI_NEXT_VOICE_ENABLED")
        .env_remove("YUNXI_NEXT_WEIXIN_ENABLED")
        .current_dir(cwd);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("run yunxi-next")
}

fn json_output(output: Output) -> Value {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON CLI output")
}

#[test]
fn help_lists_compatibility_options_and_management_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_yunxi-next"))
        .arg("--help")
        .output()
        .expect("run help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    for token in [
        "--cwd",
        "--provider",
        "--model",
        "--approval",
        "--sandbox",
        "--json",
        "--jsonl",
        "sessions",
        "memory",
        "voice",
        "weixin",
        "migrate",
    ] {
        assert!(help.contains(token), "help is missing {token}");
    }
}

#[test]
fn sessions_and_memory_use_the_requested_workspace_without_a_model() {
    let workspace = unique_temp_dir("management");
    let home = workspace.join("home");
    let sessions = json_output(run_cli(
        &["sessions", "--json", "--cwd", workspace.to_str().unwrap()],
        &workspace,
        &home,
    ));
    assert_eq!(sessions["ok"], true);
    assert_eq!(sessions["command"], "sessions list");
    assert_eq!(
        sessions["workspace"],
        fs::canonicalize(&workspace)
            .expect("canonical workspace")
            .to_string_lossy()
            .as_ref()
    );

    let memory = json_output(run_cli(
        &[
            "memory",
            "status",
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(memory["ok"], true);
    assert_eq!(memory["command"], "memory status");
    fs::remove_dir_all(workspace).expect("remove test workspace");
}

#[test]
fn voice_and_weixin_do_not_claim_production_connectivity() {
    let workspace = unique_temp_dir("fixtures");
    let home = workspace.join("home");

    for args in [
        ["voice", "doctor", "--json"],
        ["weixin", "doctor", "--json"],
    ] {
        let disabled = run_cli(&args, &workspace, &home);
        assert!(!disabled.status.success());
        assert!(String::from_utf8_lossy(&disabled.stderr).contains("plugin is disabled"));
    }

    for (args, enablement) in [
        (
            ["voice", "doctor", "--json"],
            ("YUNXI_NEXT_VOICE_ENABLED", "true"),
        ),
        (
            ["weixin", "doctor", "--json"],
            ("YUNXI_NEXT_WEIXIN_ENABLED", "true"),
        ),
    ] {
        let report = json_output(run_cli_with_env(&args, &workspace, &home, &[enablement]));
        assert_eq!(report["ok"], true);
        assert_eq!(report["productionReady"], false);
        assert!(
            report["mode"]
                .as_str()
                .is_some_and(|mode| mode.starts_with("loopback"))
        );
        assert_eq!(report["isolatedProcess"], true);
        assert!(
            report["note"]
                .as_str()
                .is_some_and(|note| { note.contains("No ") || note.contains("No microphone") })
        );
    }
    fs::remove_dir_all(workspace).expect("remove test workspace");
}

#[test]
fn metadata_commands_reject_jsonl_and_require_json() {
    let workspace = unique_temp_dir("jsonl");
    let output = run_cli(
        &[
            "sessions",
            "list",
            "--jsonl",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &workspace.join("home"),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--jsonl"));

    let output = run_cli(
        &["doctor", "--jsonl", "--cwd", workspace.to_str().unwrap()],
        &workspace,
        &workspace.join("home"),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--jsonl"));
    fs::remove_dir_all(workspace).expect("remove test workspace");
}

#[test]
fn persona_companion_controls_and_memory_management_are_bounded_and_stable() {
    let workspace = unique_temp_dir("capability-management");
    let home = workspace.join("home");

    let persona = json_output(run_cli(&["persona", "status", "--json"], &workspace, &home));
    assert_eq!(persona["schemaVersion"], 1);
    assert_eq!(persona["command"], "persona status");
    assert_eq!(persona["enabled"], true);
    assert!(persona["profiles"].is_array());

    let persona_off = json_output(run_cli(&["persona", "off", "--json"], &workspace, &home));
    assert_eq!(persona_off["command"], "persona off");
    assert_eq!(persona_off["enabled"], false);
    assert!(home.join("persona/config.toml").is_file());

    let companion = json_output(run_cli(
        &["companion", "check", "我有点焦虑", "请温柔回应", "--json"],
        &workspace,
        &home,
    ));
    assert_eq!(companion["command"], "companion check");
    assert_eq!(companion["enabled"], true);
    assert_eq!(companion["decision"]["emotion"], "anxiety");

    let search = json_output(run_cli(
        &["memory", "search", "焦虑", "偏好", "--json"],
        &workspace,
        &home,
    ));
    assert_eq!(search["command"], "memory search");

    let clear_without_confirmation = run_cli(&["memory", "clear", "--json"], &workspace, &home);
    assert!(!clear_without_confirmation.status.success());
    assert!(
        String::from_utf8_lossy(&clear_without_confirmation.stderr)
            .contains("--workspace --confirm")
    );

    let clear = json_output(run_cli(
        &["memory", "clear", "--workspace", "--confirm", "--json"],
        &workspace,
        &home,
    ));
    assert_eq!(clear["command"], "memory clear");
    assert_eq!(clear["scope"], "workspace");
    assert_eq!(clear["confirmed"], true);

    let memory_list = json_output(run_cli(&["memory", "list", "--json"], &workspace, &home));
    assert_eq!(memory_list["command"], "memory list");
    assert!(memory_list["records"].is_array());

    let memory_off = json_output(run_cli(&["memory", "off", "--json"], &workspace, &home));
    assert_eq!(memory_off["command"], "memory off");
    assert_eq!(memory_off["enabled"], false);

    let controls = json_output(run_cli(
        &["controls", "status", "--json"],
        &workspace,
        &home,
    ));
    assert_eq!(controls["command"], "controls status");
    assert_eq!(controls["controls"].as_array().map(Vec::len), Some(3));

    let controls_off = json_output(run_cli(
        &["controls", "off", "memory", "--json"],
        &workspace,
        &home,
    ));
    assert_eq!(controls_off["pluginId"], "yunxi.memory");
    assert_eq!(controls_off["enabled"], false);
    assert_eq!(controls_off["applied"], false);
    assert!(!home.join("legacy").exists());

    fs::remove_dir_all(workspace).expect("remove test workspace");
}

#[test]
fn session_migration_command_is_explicit_read_only_and_reversible() {
    let workspace = unique_temp_dir("migration");
    let home = workspace.join("home");
    let legacy_dir = workspace.join(".yunxi").join("sessions");
    fs::create_dir_all(&legacy_dir).expect("create legacy sessions");
    let legacy_path = legacy_dir.join("legacy-session.json");
    let legacy_bytes = br#"{"id":"legacy-session","cwd":".","prompt":"hello","final_response":"reply","created_at_millis":1}"#;
    fs::write(&legacy_path, legacy_bytes).expect("write legacy session");

    let status = json_output(run_cli(
        &[
            "migrate",
            "status",
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(status["ok"], true);
    assert_eq!(status["readOnly"], true);
    assert_eq!(status["counts"]["ready"], 1);
    assert!(!workspace.join(".yunxi-next").exists());

    let plan = json_output(run_cli(
        &[
            "migrate",
            "sessions",
            "plan",
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["readOnly"], true);
    assert_eq!(plan["plan"]["items"].as_array().map(Vec::len), Some(1));

    let applied = json_output(run_cli(
        &[
            "migrate",
            "sessions",
            "apply",
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(applied["ok"], true);
    assert_eq!(applied["report"]["copied"], 1);
    assert_eq!(applied["legacyUnchanged"], true);
    assert_eq!(
        fs::read(&legacy_path).expect("read legacy bytes"),
        legacy_bytes
    );
    let migrated_path = workspace
        .join(".yunxi-next")
        .join("sessions")
        .join("legacy-session.json");
    assert!(migrated_path.is_file());
    let migration_id = applied["migrationId"].as_str().expect("migration id");

    let rolled_back = json_output(run_cli(
        &[
            "migrate",
            "rollback",
            migration_id,
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(rolled_back["ok"], true);
    assert_eq!(rolled_back["report"]["removed"], 1);
    assert!(!migrated_path.exists());
    assert_eq!(
        fs::read(&legacy_path).expect("read legacy bytes after rollback"),
        legacy_bytes
    );

    fs::remove_dir_all(workspace).expect("remove migration workspace");
}

#[test]
fn legacy_event_migration_replays_and_imports_without_touching_the_source() {
    let workspace = unique_temp_dir("event-migration");
    let home = workspace.join("home");
    let legacy_dir = workspace.join(".yunxi").join("sessions");
    fs::create_dir_all(&legacy_dir).expect("create legacy event directory");
    let source = legacy_dir.join("legacy-events.jsonl");
    let source_argument = ".yunxi/sessions/legacy-events.jsonl";
    let legacy_bytes = concat!(
        "{\"type\":\"user_message\",\"message\":\"hello\",\"api_key\":\"must-not-copy\"}\n",
        "not-json\n",
        "{\"type\":\"assistant_message\",\"message\":\"reply\",\"session_id\":\"legacy\"}\n"
    )
    .as_bytes();
    fs::write(&source, legacy_bytes).expect("write legacy events");

    let status = json_output(run_cli(
        &[
            "migrate",
            "events",
            "status",
            source_argument,
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(status["readOnly"], true);
    assert_eq!(status["summary"]["event_count"], 2);
    assert_eq!(status["summary"]["malformed_line_count"], 1);
    assert!(!workspace.join(".yunxi-next").exists());

    let replay = json_output(run_cli(
        &[
            "migrate",
            "events",
            "replay",
            source_argument,
            "0",
            "1",
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(replay["page"]["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(replay["page"]["events"][0]["event_type"], "user_message");
    assert_eq!(replay["page"]["events"][0]["redacted_fields"][0], "api_key");

    let applied = json_output(run_cli(
        &[
            "migrate",
            "events",
            "apply",
            source_argument,
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(applied["report"]["imported_events"], 2);
    assert_eq!(applied["legacyUnchanged"], true);
    assert_eq!(fs::read(&source).expect("read legacy source"), legacy_bytes);
    let target = PathBuf::from(applied["report"]["target"].as_str().expect("event target"));
    assert!(target.is_file());

    let migration_id = applied["migrationId"].as_str().expect("migration id");
    let rolled_back = json_output(run_cli(
        &[
            "migrate",
            "events",
            "rollback",
            source_argument,
            migration_id,
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(rolled_back["report"]["removed"], true);
    assert!(!target.exists());
    assert_eq!(fs::read(&source).expect("read legacy source"), legacy_bytes);

    fs::remove_dir_all(workspace).expect("remove event migration workspace");
}

#[test]
fn migration_discovers_explicit_legacy_home_without_guessing_sensitive_formats() {
    let workspace = unique_temp_dir("user-home-migration");
    let home = workspace.join("next-home");
    let legacy = home.join("legacy");
    fs::create_dir_all(legacy.join("persona")).expect("create legacy persona");
    fs::create_dir_all(legacy.join("memory")).expect("create legacy memory");
    fs::create_dir_all(legacy.join("mailbox")).expect("create legacy mailbox");
    fs::write(
        legacy.join("persona/config.toml"),
        b"persona_enabled = false\nactive_profile = \"custom\"\n",
    )
    .expect("write legacy persona settings");
    fs::write(legacy.join("persona/soul.txt"), b"legacy soul\n").expect("write legacy soul");
    fs::write(
        legacy.join("settings.json"),
        br#"{"version":1,"revision":2,"capabilities":{"persona":false}}"#,
    )
    .expect("write legacy controls");
    fs::write(
        legacy.join("memory/global-memory.jsonl"),
        b"private legacy memory\n",
    )
    .expect("write legacy memory marker");
    fs::write(legacy.join("mailbox/opaque.bin"), b"opaque legacy mailbox").expect("mailbox");

    fs::create_dir_all(home.join("persona")).expect("create next persona");
    fs::write(home.join("persona/soul.txt"), b"next soul\n").expect("write existing Next soul");

    let plan = json_output(run_cli(
        &[
            "migrate",
            "plan",
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(plan["ok"], true);
    assert_eq!(plan["scope"], "workspace_and_optional_legacy_user_home");
    assert_eq!(plan["legacyUserHome"], legacy.to_string_lossy().as_ref());
    assert_eq!(plan["nextUserHome"], home.to_string_lossy().as_ref());
    let capabilities = plan["plan"]["capabilities"]
        .as_array()
        .expect("capability plan");
    let capability = |name: &str| {
        capabilities
            .iter()
            .find(|value| value["capability"] == name)
            .unwrap_or_else(|| panic!("missing capability {name}"))
    };
    assert_eq!(capability("persona")["scope"], "legacy_user_home");
    assert_eq!(capability("persona")["status"], "ready");
    assert_eq!(capability("persona")["item_count"], 2);
    assert_eq!(capability("controls")["status"], "ready");
    assert_eq!(capability("memory")["status"], "unsupported");
    assert!(
        capability("memory")["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("privacy review"))
    );
    assert_eq!(capability("mailbox")["status"], "unsupported");
    assert!(
        capability("mailbox")["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("format"))
    );
    assert_eq!(capability("relationship")["status"], "unsupported");

    let applied = json_output(run_cli(
        &[
            "migrate",
            "apply",
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(applied["report"]["copied"], 2);
    assert_eq!(applied["report"]["skipped_existing"], 1);
    assert_eq!(
        fs::read(home.join("persona/soul.txt")).unwrap(),
        b"next soul\n"
    );
    assert!(home.join("persona/config.toml").is_file());
    assert!(home.join("settings.json").is_file());
    assert!(!home.join("memory/global-memory.jsonl").exists());
    assert!(!home.join("mailbox/opaque.bin").exists());
    assert_eq!(
        fs::read(legacy.join("persona/config.toml")).unwrap(),
        b"persona_enabled = false\nactive_profile = \"custom\"\n"
    );

    let migration_id = applied["migrationId"].as_str().expect("migration id");
    let rolled_back = json_output(run_cli(
        &[
            "migrate",
            "rollback",
            migration_id,
            "--json",
            "--cwd",
            workspace.to_str().unwrap(),
        ],
        &workspace,
        &home,
    ));
    assert_eq!(rolled_back["report"]["removed"], 2);
    assert_eq!(
        fs::read(home.join("persona/soul.txt")).unwrap(),
        b"next soul\n"
    );
    assert!(!home.join("persona/config.toml").exists());
    assert!(!home.join("settings.json").exists());
    assert_eq!(
        fs::read(legacy.join("mailbox/opaque.bin")).unwrap(),
        b"opaque legacy mailbox"
    );

    fs::remove_dir_all(workspace).expect("remove user-home migration workspace");
}
