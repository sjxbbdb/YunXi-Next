//! Process-level verification of CLI, kernel, plugin protocol, and HTTP client.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, process};

#[test]
fn once_mode_crosses_the_isolated_plugin_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || serve_one_request(listener));

    let child = configured_cli(address)
        .args(["--once", "hello from CLI"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI");
    let output = wait_for_cli(child);

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request_body = server.join().expect("join mock API");
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("UTF-8 CLI output")
            .trim(),
        "fixture reply"
    );
    assert!(request_body.contains("\"model\":\"fixture-model\""));
    assert!(request_body.contains("\"content\":\"hello from CLI\""));
    assert!(request_body.contains("YunXi Next Development Instructions"));
    assert!(request_body.contains("yunxi_persona_context"));
}

#[test]
fn enabled_skills_inject_bounded_context_and_dynamic_metadata_tools() {
    let workspace = unique_temp_dir("yunxi-skills-context");
    let skill_dir = workspace.join("skills").join("review");
    fs::create_dir_all(&skill_dir).expect("create Skill directory");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: Code Review\ndescription: Review source\n---\nInspect the changed lines before answering.\n",
    )
    .expect("write Skill instructions");
    fs::write(
        skill_dir.join("tools.json"),
        r#"[{"name":"check","description":"Inspect metadata","input_schema":{"type":"object"}}]"#,
    )
    .expect("write Skill tool metadata");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (stream, body) = accept_request(&listener);
        assert!(body.contains("Inspect the changed lines"), "body: {body}");
        assert!(body.contains("skill.review.check"), "body: {body}");
        write_response(
            stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"skills are active"},"finish_reason":"stop"}]}"#,
        );
    });

    let child = configured_cli(address)
        .current_dir(&workspace)
        .args(["--once", "review this change"])
        .env("YUNXI_NEXT_SKILLS_ENABLED", "true")
        .env("YUNXI_NEXT_SKILLS_ROOT", workspace.join("skills"))
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI with Skills");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("UTF-8 output")
            .trim(),
        "skills are active"
    );
    let _ignored = fs::remove_dir_all(workspace);
}

#[test]
fn metadata_only_skill_tool_call_is_rejected_without_approval() {
    let workspace = unique_temp_dir("yunxi-skills-tool-call");
    let skill_dir = workspace.join("skills").join("review");
    fs::create_dir_all(&skill_dir).expect("create Skill directory");
    fs::write(skill_dir.join("SKILL.md"), "review instructions\n").expect("write Skill");
    fs::write(
        skill_dir.join("tools.json"),
        r#"[{"name":"check","description":"Inspect metadata","input_schema":{"type":"object"}}]"#,
    )
    .expect("write Skill tool metadata");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&listener);
        assert!(
            first_body.contains("skill.review.check"),
            "body: {first_body}"
        );
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"skill-call-1","type":"function","function":{"name":"skill.review.check","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        let (second_stream, second_body) = accept_request(&listener);
        assert!(
            second_body.contains("skill_tool_unavailable"),
            "body: {second_body}"
        );
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"metadata tool stayed isolated"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_SKILLS_ENABLED", "true")
        .env("YUNXI_NEXT_SKILLS_ROOT", workspace.join("skills"))
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI with Skill tool metadata");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"invoke the declared Skill tool\n/quit\n")
        .expect("write Skill tool prompt");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("metadata tool stayed isolated"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Approval required"));
    let _ignored = fs::remove_dir_all(workspace);
}

#[test]
fn disabled_skill_is_not_injected_or_projected_to_the_model() {
    let workspace = unique_temp_dir("yunxi-skills-disabled");
    let skill_dir = workspace.join("skills").join("review");
    fs::create_dir_all(&skill_dir).expect("create Skill directory");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: Code Review\ndescription: Review source\n---\nDo not appear in this request.\n",
    )
    .expect("write Skill instructions");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (stream, body) = accept_request(&listener);
        assert!(!body.contains("Do not appear"), "body: {body}");
        assert!(!body.contains("skill.review"), "body: {body}");
        write_response(
            stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"disabled is isolated"},"finish_reason":"stop"}]}"#,
        );
    });

    let child = configured_cli(address)
        .current_dir(&workspace)
        .args(["--once", "check disabled behavior"])
        .env("YUNXI_NEXT_SKILLS_ENABLED", "true")
        .env("YUNXI_NEXT_SKILLS_DISABLED", "review")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI with disabled Skill");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("disabled is isolated"));
    let _ignored = fs::remove_dir_all(workspace);
}

#[test]
fn crashed_skills_context_route_does_not_stop_model_chat() {
    let workspace = unique_temp_dir("yunxi-skills-crash");
    let skill_dir = workspace.join("skills").join("review");
    fs::create_dir_all(&skill_dir).expect("create Skill directory");
    fs::write(skill_dir.join("SKILL.md"), "instructions before crash\n")
        .expect("write Skill instructions");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (stream, body) = accept_request(&listener);
        assert!(!body.contains("instructions before crash"), "body: {body}");
        write_response(
            stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"model survives Skills"},"finish_reason":"stop"}]}"#,
        );
    });

    let child = configured_cli(address)
        .current_dir(&workspace)
        .args(["--once", "continue after Skill failure"])
        .env("YUNXI_NEXT_SKILLS_ENABLED", "true")
        .env("YUNXI_NEXT_SKILLS_MODE", "crash-context")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI with crashing Skill");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("model survives Skills"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Skills capability degraded"));
    let _ignored = fs::remove_dir_all(workspace);
}

#[test]
fn api_failure_is_contained_and_the_plugin_serves_the_next_turn() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (first_stream, _) = accept_request(&listener);
        write_response(
            first_stream,
            "500 Internal Server Error",
            r#"{"error":{"message":"temporary fixture failure"}}"#,
        );
        let (second_stream, second_body) = accept_request(&listener);
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"recovered reply"},"finish_reason":"stop"}]}"#,
        );
        second_body
    });

    let mut child = configured_cli(address)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"first request\nsecond request\n/quit\n")
        .expect("write CLI input");

    let output = wait_for_cli(child);
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let second_body = server.join().expect("join mock API");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    assert!(stdout.contains("temporary fixture failure"));
    assert!(stdout.contains("recovered reply"));
    assert!(second_body.contains("\"content\":\"second request\""));
    assert!(!second_body.contains("\"content\":\"first request\""));
}

#[test]
fn model_tool_call_waits_for_approval_then_resumes_with_tool_result() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&listener);
        assert!(first_body.contains("\"tools\":["));
        assert!(first_body.contains("shell.execute"));
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"shell.execute","arguments":"{\"command\":\"echo auto-ok\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );

        let (second_stream, second_body) = accept_request(&listener);
        assert!(second_body.contains("\"role\":\"assistant\""));
        assert!(second_body.contains("\"tool_calls\""));
        assert!(second_body.contains("\"role\":\"tool\""));
        assert!(second_body.contains("auto-ok"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"tool loop complete"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .env("YUNXI_NEXT_SHELL_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"run the command\n/approve\n/quit\n")
        .expect("write tool loop input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(
        stdout.contains("Approval required for model tool action"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("tool: shell.execute"), "stdout: {stdout}");
    assert!(stdout.contains("tool loop complete"), "stdout: {stdout}");
}

#[test]
fn mcp_stdio_tools_are_discovered_and_require_host_approval() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&listener);
        assert!(
            first_body.contains("mcp.fixture.echo"),
            "MCP tool catalog missing from request: {first_body}"
        );
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"mcp-1","type":"function","function":{"name":"mcp.fixture.echo","arguments":"{\"text\":\"hello\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );

        let (second_stream, second_body) = accept_request(&listener);
        assert!(second_body.contains("mcp.fixture.echo"));
        assert!(second_body.contains("fixture"));
        assert!(second_body.contains("hello"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"mcp loop complete"},"finish_reason":"stop"}]}"#,
        );
    });

    let mcp_command = env!("CARGO_BIN_EXE_yunxi-next");
    let mut child = configured_cli(address)
        .env("YUNXI_NEXT_MCP_ENABLED", "true")
        .env("YUNXI_NEXT_MCP_COMMAND", mcp_command)
        .env("YUNXI_NEXT_MCP_ARGS_JSON", r#"["__mcp-fixture"]"#)
        .env(
            "YUNXI_NEXT_MCP_ENV_JSON",
            r#"{"YUNXI_MCP_FIXTURE_MODE":"normal"}"#,
        )
        .env("YUNXI_NEXT_MCP_NAME", "fixture")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI with MCP");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"call the MCP tool\n/approve\n/quit\n")
        .expect("write MCP tool input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(
        stdout.contains("tool: mcp.fixture.echo"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("mcp loop complete"), "stdout: {stdout}");
}

#[test]
fn crashed_mcp_server_isolated_from_model_route_and_returns_tool_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&listener);
        assert!(first_body.contains("mcp.fixture.echo"));
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"mcp-crash-1","type":"function","function":{"name":"mcp.fixture.echo","arguments":"{\"text\":\"crash\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );

        let (second_stream, second_body) = accept_request(&listener);
        assert!(second_body.contains("mcp_unavailable"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"model recovered after MCP crash"},"finish_reason":"stop"}]}"#,
        );
    });

    let mcp_command = env!("CARGO_BIN_EXE_yunxi-next");
    let mut child = configured_cli(address)
        .env("YUNXI_NEXT_MCP_ENABLED", "true")
        .env("YUNXI_NEXT_MCP_COMMAND", mcp_command)
        .env("YUNXI_NEXT_MCP_ARGS_JSON", r#"["__mcp-fixture"]"#)
        .env(
            "YUNXI_NEXT_MCP_ENV_JSON",
            r#"{"YUNXI_MCP_FIXTURE_MODE":"call-crash"}"#,
        )
        .env("YUNXI_NEXT_MCP_NAME", "fixture")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI with crashing MCP");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"call the crashing MCP tool\n/approve\n/quit\n")
        .expect("write crashing MCP input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("mcp_unavailable"), "stdout: {stdout}");
    assert!(
        stdout.contains("model recovered after MCP crash"),
        "stdout: {stdout}"
    );
}

#[test]
fn model_patch_tool_uses_workspace_write_grant_after_approval() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let workspace = unique_temp_dir("yunxi-model-patch-tool");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::write(workspace.join("target.txt"), "before\n").expect("write target");
    let server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&listener);
        assert!(first_body.contains("patch.apply"));
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"patch-1","type":"function","function":{"name":"patch.apply","arguments":"{\"patch\":\"*** Begin Patch\\n*** Update File: target.txt\\n@@\\n-before\\n+after\\n*** End Patch\\n\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        let (second_stream, second_body) = accept_request(&listener);
        assert!(second_body.contains("\"role\":\"tool\""));
        assert!(second_body.contains("target.txt"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"patch loop complete"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_PATCH_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"apply the patch\n/approve\n/quit\n")
        .expect("write patch tool input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(workspace.join("target.txt")).expect("read patched target"),
        "after\n"
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("tool: patch.apply"), "stdout: {stdout}");
    assert!(stdout.contains("patch loop complete"), "stdout: {stdout}");
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn denied_model_tool_has_no_side_effect_and_returns_rejection_to_model() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let workspace = unique_temp_dir("yunxi-model-denied-tool");
    fs::create_dir_all(&workspace).expect("create workspace");
    let marker = workspace.join("marker.txt");
    let server = thread::spawn(move || {
        let (first_stream, _first_body) = accept_request(&listener);
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"deny-1","type":"function","function":{"name":"shell.execute","arguments":"{\"command\":\"echo should-not-run > marker.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        let (second_stream, second_body) = accept_request(&listener);
        assert!(second_body.contains("user_denied"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"denial handled"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_SHELL_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"do not run it\n/deny\n/quit\n")
        .expect("write denial input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(output.status.success());
    assert!(!marker.exists());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("denial handled"), "stdout: {stdout}");
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn cancelled_model_tool_has_no_side_effect_and_returns_cancellation_to_model() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let workspace = unique_temp_dir("yunxi-model-cancelled-tool");
    fs::create_dir_all(&workspace).expect("create workspace");
    let marker = workspace.join("marker.txt");
    let server = thread::spawn(move || {
        let (first_stream, _first_body) = accept_request(&listener);
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"cancel-1","type":"function","function":{"name":"shell.execute","arguments":"{\"command\":\"echo should-not-run > marker.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        let (second_stream, second_body) = accept_request(&listener);
        assert!(second_body.contains("cancelled"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"cancellation handled"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_SHELL_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"do not run it\n/cancel\n/quit\n")
        .expect("write cancellation input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(output.status.success());
    assert!(!marker.exists());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("cancellation handled"), "stdout: {stdout}");
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn model_tool_timeout_is_bounded_and_recoverable() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let command = if cfg!(windows) {
        "ping -n 5 127.0.0.1"
    } else {
        "sleep 2"
    };
    let server = thread::spawn(move || {
        let (first_stream, _first_body) = accept_request(&listener);
        let arguments = serde_json::json!({
            "command": command,
            "timeout_millis": 50
        })
        .to_string();
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": "timeout-1",
                        "type": "function",
                        "function": {
                            "name": "shell.execute",
                            "arguments": arguments
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string();
        write_response(first_stream, "200 OK", &response);
        let (second_stream, second_body) = accept_request(&listener);
        assert!(
            second_body.contains("timed_out"),
            "second body: {second_body}"
        );
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"timeout handled"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .env("YUNXI_NEXT_SHELL_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"run briefly\n/approve\n/quit\n")
        .expect("write timeout input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("timeout handled"), "stdout: {stdout}");
}

#[test]
fn tool_rejection_is_returned_to_the_model_without_writing_files() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let workspace = unique_temp_dir("yunxi-model-rejected-tool");
    fs::create_dir_all(&workspace).expect("create workspace");
    let marker = workspace.join("marker.txt");
    let server = thread::spawn(move || {
        let (first_stream, _first_body) = accept_request(&listener);
        write_response(
            first_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"reject-1","type":"function","function":{"name":"shell.execute","arguments":"{\"command\":\"echo should-not-write > marker.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        let (second_stream, second_body) = accept_request(&listener);
        assert!(second_body.contains("write_not_granted"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"rejection handled"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_SHELL_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"try a write\n/approve\n/quit\n")
        .expect("write rejection input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(output.status.success());
    assert!(!marker.exists());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("rejection handled"), "stdout: {stdout}");
    assert!(
        stdout.contains("warning: model tool `shell.execute` failed"),
        "stdout: {stdout}"
    );
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn tool_loop_round_limit_is_visible_and_a_later_turn_recovers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        for round in 1..=8 {
            let (stream, body) = accept_request(&listener);
            assert!(body.contains("shell.execute"));
            let response = serde_json::json!({
                "choices": [{
                    "message": {
                        "content": serde_json::Value::Null,
                        "tool_calls": [{
                            "id": format!("limit-{round}"),
                            "type": "function",
                            "function": {
                                "name": "shell.execute",
                                "arguments": serde_json::json!({"command": "echo bounded"}).to_string()
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
            .to_string();
            write_response(stream, "200 OK", &response);
        }
        let (stream, body) = accept_request(&listener);
        assert!(body.contains("after limit"));
        write_response(
            stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"recovered after limit"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .env("YUNXI_NEXT_SHELL_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(
            b"reach the limit\n/approve\n/approve\n/approve\n/approve\n/approve\n/approve\n/approve\n/approve\nafter limit\n/quit\n",
        )
        .expect("write round-limit input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(
        stdout.contains("model tool loop stopped"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("recovered after limit"), "stdout: {stdout}");
}

#[test]
fn read_only_file_tools_search_and_view_without_approval_or_write_access() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let workspace = unique_temp_dir("yunxi-file-tools-e2e");
    fs::create_dir_all(workspace.join("src")).expect("create workspace");
    fs::write(workspace.join("src/main.rs"), "fn main() {}\n").expect("write source");
    let server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&listener);
        assert!(first_body.contains("file.search"));
        assert!(first_body.contains("file.read"));
        let first_response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": "search-1",
                        "type": "function",
                        "function": {
                            "name": "file.search",
                            "arguments": "{\"query\":\"main\",\"path\":\"src\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string();
        write_response(first_stream, "200 OK", &first_response);

        let (second_stream, second_body) = accept_request(&listener);
        assert!(
            second_body.contains("src/main.rs"),
            "second body: {second_body}"
        );
        let second_response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": "read-1",
                        "type": "function",
                        "function": {
                            "name": "file.read",
                            "arguments": "{\"path\":\"src/main.rs\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string();
        write_response(second_stream, "200 OK", &second_response);

        let (third_stream, third_body) = accept_request(&listener);
        assert!(third_body.contains("fn main"));
        write_response(
            third_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"read-only file tools complete"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_FILES_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"inspect the source\n/quit\n")
        .expect("write file tool input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(
        stdout.contains("read-only file tools complete"),
        "stdout: {stdout}"
    );
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn disabled_optional_capabilities_never_launch_or_register_routes() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused API endpoint");
    let address = listener.local_addr().expect("read API endpoint");
    let mut child = configured_cli(address)
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .env("YUNXI_NEXT_MEMORY_ENABLED", "false")
        .env("YUNXI_NEXT_STORAGE_ENABLED", "false")
        .env("YUNXI_NEXT_COMPANION_ENABLED", "false")
        .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
        .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
        .env("YUNXI_NEXT_SHELL_ENABLED", "false")
        .env("YUNXI_NEXT_PATCH_ENABLED", "false")
        .env("YUNXI_NEXT_FILES_ENABLED", "false")
        .env("YUNXI_NEXT_MCP_ENABLED", "false")
        .env("YUNXI_NEXT_SKILLS_ENABLED", "false")
        .env("YUNXI_NEXT_MULTI_AGENT_ENABLED", "false")
        .env("YUNXI_NEXT_MCP_COMMAND", "this-command-must-not-launch")
        .env("YUNXI_NEXT_SKILLS_PLUGIN", "this-plugin-must-not-launch")
        .env(
            "YUNXI_NEXT_MULTI_AGENT_PLUGIN",
            "this-plugin-must-not-launch",
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"/status\n/quit\n")
        .expect("write CLI commands");

    let output = wait_for_cli(child);

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    assert!(stdout.contains("plugins: 1 | capabilities: 1 | failed: 0"));
}

#[test]
fn approved_multi_agent_spawn_uses_an_isolated_model_process_and_persists_result() {
    let workspace = unique_temp_dir("yunxi-multi-agent-e2e");
    fs::create_dir_all(&workspace).expect("create workspace");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (parent_stream, parent_body) = accept_request(&listener);
        assert!(parent_body.contains("agent.spawn"), "body: {parent_body}");
        write_response(
            parent_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"agent-spawn-1","type":"function","function":{"name":"agent.spawn","arguments":"{\"task\":\"analyze the delegated fixture\",\"name\":\"fixture\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );

        let (child_stream, child_body) = accept_request(&listener);
        assert!(
            child_body.contains("isolated YunXi child agent"),
            "child body: {child_body}"
        );
        assert!(
            child_body.contains("analyze the delegated fixture"),
            "child body: {child_body}"
        );
        assert!(
            !child_body.contains("\"tools\""),
            "child body: {child_body}"
        );
        write_response(
            child_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"child isolated result"},"finish_reason":"stop"}]}"#,
        );

        let (continuation_stream, continuation_body) = accept_request(&listener);
        assert!(
            continuation_body.contains("child isolated result"),
            "continuation body: {continuation_body}"
        );
        write_response(
            continuation_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"parent used child result"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_MULTI_AGENT_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI with multi-agent");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"delegate the fixture\n/approve\n/quit\n")
        .expect("write multi-agent prompt");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("Approval required"), "stdout: {stdout}");
    assert!(
        stdout.contains("parent used child result"),
        "stdout: {stdout}"
    );
    let state_path = fs::read_dir(workspace.join(".yunxi-next/multi-agent"))
        .expect("read multi-agent state")
        .next()
        .expect("state entry")
        .expect("state path")
        .path();
    let state = fs::read_to_string(state_path).expect("read state");
    assert!(state.contains("child isolated result"));
    assert!(state.contains("\"status\": \"completed\""));
    assert!(!state.contains("fixture-secret"));
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn child_model_api_failure_ends_only_that_branch_and_parent_model_continues() {
    let workspace = unique_temp_dir("yunxi-multi-agent-failure");
    fs::create_dir_all(&workspace).expect("create workspace");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (parent_stream, _) = accept_request(&listener);
        write_response(
            parent_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"agent-spawn-fail","type":"function","function":{"name":"agent.spawn","arguments":"{\"task\":\"fail in isolation\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );

        let (child_stream, child_body) = accept_request(&listener);
        assert!(child_body.contains("fail in isolation"));
        write_response(
            child_stream,
            "500 Internal Server Error",
            r#"{"error":{"message":"child fixture failed"}}"#,
        );

        let (continuation_stream, continuation_body) = accept_request(&listener);
        assert!(
            continuation_body.contains("child_model_failed"),
            "continuation body: {continuation_body}"
        );
        write_response(
            continuation_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"parent survived child failure"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_MULTI_AGENT_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI with multi-agent");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"delegate failing task\n/approve\n/quit\n")
        .expect("write multi-agent prompt");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("parent survived child failure"));
    let state_path = fs::read_dir(workspace.join(".yunxi-next/multi-agent"))
        .expect("read multi-agent state")
        .next()
        .expect("state entry")
        .expect("state path")
        .path();
    let state = fs::read_to_string(state_path).expect("read state");
    assert!(state.contains("\"status\": \"failed\""));
    assert!(state.contains("child_model_failed"));
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn shell_action_requires_approval_and_crosses_the_plugin_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused API endpoint");
    let address = listener.local_addr().expect("read API endpoint");
    drop(listener);
    let workspace = unique_temp_dir("yunxi-shell-e2e");
    fs::create_dir_all(&workspace).expect("create workspace");

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_SHELL_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"/shell echo action-ok\n/approve\n/quit\n")
        .expect("write action commands");
    let output = wait_for_cli(child);
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("Approval required for shell action."));
    assert!(stdout.contains("shell exit: 0"));
    assert!(stdout.contains("action-ok"));
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn patch_action_requires_approval_and_applies_a_workspace_file() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused API endpoint");
    let address = listener.local_addr().expect("read API endpoint");
    drop(listener);
    let workspace = unique_temp_dir("yunxi-patch-e2e");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::write(workspace.join("target.txt"), "before\n").expect("write target");
    fs::write(
        workspace.join("change.patch"),
        "*** Begin Patch\n*** Update File: target.txt\n@@\n-before\n+after\n*** End Patch\n",
    )
    .expect("write patch");

    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_PATCH_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"/patch change.patch\n/approve\n/quit\n")
        .expect("write patch commands");
    let output = wait_for_cli(child);
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("Approval required for patch action."));
    assert!(stdout.contains("patch applied: "));
    assert_eq!(
        fs::read_to_string(workspace.join("target.txt")).expect("read target"),
        "after\n"
    );
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn legacy_memory_is_recalled_through_memory_and_persona_processes() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || serve_one_request(listener));
    let home = unique_temp_dir("yunxi-memory-e2e");
    fs::create_dir_all(home.join("memory")).expect("create memory directory");
    let memory = concat!(
        "{\"id\":\"reply-style\",\"schema_version\":3,",
        "\"scope\":\"global_user\",\"kind\":\"preference\",",
        "\"content\":\"用户偏好：回答保持简洁\",\"confidence\":0.95,",
        "\"importance\":0.9,\"sensitivity\":\"low\",\"status\":\"active\",",
        "\"created_at_millis\":1,\"updated_at_millis\":1}\n"
    );
    fs::write(home.join("memory/global-memory.jsonl"), memory).expect("write legacy memory");

    let child = configured_cli(address)
        .env("YUNXI_HOME", &home)
        .env("YUNXI_NEXT_MEMORY_ENABLED", "true")
        .args(["--once", "hello with memory"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI");
    let output = wait_for_cli(child);
    let request_body = server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(request_body.contains("用户偏好：回答保持简洁"));
    assert!(request_body.contains("boot_memory_context"));
    fs::remove_dir_all(home).expect("remove memory fixture");
}

#[test]
fn session_storage_persists_and_resumes_across_cli_processes() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || serve_requests(listener, 2));
    let workspace = unique_temp_dir("yunxi-session-e2e");
    fs::create_dir_all(&workspace).expect("create workspace");

    let first = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .args(["--once", "first saved turn"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch first CLI");
    let first = wait_for_cli(first);
    assert!(first.status.success());

    let session_path = fs::read_dir(workspace.join(".yunxi-next/sessions"))
        .expect("read session directory")
        .map(|entry| entry.expect("session entry").path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .expect("saved session");
    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(&session_path).expect("read session"))
            .expect("parse session");
    let session_id = session["id"].as_str().expect("session id").to_string();

    let mut second = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch second CLI");
    write!(
        second.stdin.take().expect("open CLI stdin"),
        "/resume {session_id}\nsecond saved turn\n/quit\n"
    )
    .expect("write CLI commands");
    let second = wait_for_cli(second);
    assert!(
        second.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let bodies = server.join().expect("join mock API");
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].contains("first saved turn"));
    assert!(bodies[1].contains("second saved turn"));
    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(&session_path).expect("read updated session"))
            .expect("parse updated session");
    assert_eq!(session["messages"].as_array().expect("messages").len(), 4);
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn successful_turn_writes_project_memory_through_memory_process() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || serve_one_request(listener));
    let workspace = unique_temp_dir("yunxi-memory-write-e2e");
    fs::create_dir_all(&workspace).expect("create workspace");

    let child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_MEMORY_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .args(["--once", "这是项目硬性要求：所有插件必须有测试"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let memory = fs::read_to_string(workspace.join(".yunxi-next/memory/workspace-memory.jsonl"))
        .expect("read written memory");
    assert!(memory.contains("项目硬性约束"));
    assert!(!workspace.join(".yunxi/memory").exists());
    fs::remove_dir_all(workspace).expect("remove workspace");
}

#[test]
fn companion_decision_is_injected_into_the_model_request() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || serve_one_request(listener));

    let child = configured_cli(address)
        .env("YUNXI_NEXT_COMPANION_ENABLED", "true")
        .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
        .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .args(["--once", "我现在非常焦虑，不知道怎么做"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    let output = wait_for_cli(child);
    let request = server.join().expect("join mock API");

    assert!(output.status.success());
    assert!(request.contains("yunxi_companion_policy"));
    assert!(request.contains("without diagnosis"));
}

#[test]
fn proactive_plan_is_enqueued_and_listed_through_mailbox_process() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || serve_one_request(listener));
    let workspace = unique_temp_dir("yunxi-mailbox-e2e");
    fs::create_dir_all(&workspace).expect("create workspace");
    let mut child = configured_cli(address)
        .current_dir(&workspace)
        .env("YUNXI_NEXT_COMPANION_ENABLED", "true")
        .env("YUNXI_NEXT_MAILBOX_ENABLED", "true")
        .env("YUNXI_NEXT_SCHEDULER_ENABLED", "true")
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"unfinished task: migrate state\n/mailbox\n/quit\n")
        .expect("write CLI input");
    let output = wait_for_cli(child);
    server.join().expect("join mock API");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("Unread: 1"));
    assert!(stdout.contains("YunXi follow-up"));
    let item = fs::read_dir(workspace.join(".yunxi-next/mailbox"))
        .expect("read mailbox")
        .map(|entry| entry.expect("mailbox entry").path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .expect("encrypted item");
    let raw = fs::read_to_string(item).expect("read encrypted item");
    assert!(!raw.contains("还可以继续处理"));
    fs::remove_dir_all(workspace).expect("remove workspace");
}

fn serve_one_request(listener: TcpListener) -> String {
    let (stream, body) = accept_request(&listener);
    write_response(
        stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":"fixture reply"},"finish_reason":"stop"}]}"#,
    );
    body
}

fn serve_requests(listener: TcpListener, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            let (stream, body) = accept_request(&listener);
            write_response(
                stream,
                "200 OK",
                &format!(
                    "{{\"choices\":[{{\"message\":{{\"content\":\"fixture reply {}\"}},\"finish_reason\":\"stop\"}}]}}",
                    index + 1
                ),
            );
            body
        })
        .collect()
}

fn accept_request(listener: &TcpListener) -> (std::net::TcpStream, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let (stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "mock API received no request");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("mock API accept failed: {error}"),
        }
    };
    stream
        .set_nonblocking(false)
        .expect("make accepted API stream blocking");
    let mut reader = BufReader::new(stream.try_clone().expect("clone API stream"));
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read API request line");
    assert!(request_line.starts_with("POST /v1/chat/completions "));

    let mut content_length = None;
    let mut authorization = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read API header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => {
                    content_length =
                        Some(value.trim().parse::<usize>().expect("valid content length"));
                }
                "authorization" => authorization = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    assert_eq!(authorization.as_deref(), Some("Bearer fixture-secret"));

    let mut body = vec![0; content_length.expect("request content length")];
    reader.read_exact(&mut body).expect("read API request body");
    let body = String::from_utf8(body).expect("UTF-8 API request");

    (stream, body)
}

fn write_response(mut stream: std::net::TcpStream, status: &str, response: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.len(),
        response
    )
    .expect("write mock API response");
}

fn configured_cli(address: std::net::SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yunxi-next"));
    command
        .env("YUNXI_PROVIDER_PROFILE", "fixture")
        .env("YUNXI_PROVIDER_BASE_URL", format!("http://{address}/v1"))
        .env("YUNXI_PROVIDER_API_KEY", "fixture-secret")
        .env("YUNXI_AGENT_MODEL", "fixture-model")
        .env("YUNXI_PROVIDER_TIMEOUT_MILLIS", "3000")
        .env("YUNXI_NEXT_MEMORY_ENABLED", "false")
        .env("YUNXI_NEXT_COMPANION_ENABLED", "false")
        .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
        .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
        .env("YUNXI_NEXT_STORAGE_ENABLED", "false")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    command
}

fn wait_for_cli(mut child: std::process::Child) -> std::process::Output {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().expect("poll YunXi CLI").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ignored = child.kill();
            panic!("YunXi CLI did not exit within the test deadline");
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().expect("collect YunXi output")
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{unique}", process::id()))
}
