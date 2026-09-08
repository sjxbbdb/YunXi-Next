//! Process-level coverage for the Web command and its shared Host boundary.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use yunxi_multi_agent::CoordinatorStore;
use yunxi_protocol::{
    AgentBudget, AgentDelegationGrant, AgentSpawnRequest, AgentTurnStartRequest, WorkspaceGrant,
};
use yunxi_voice::{
    AudioChunk, AudioCodec, AudioFormat, CancelRequest, ChatRequest as VoiceChatRequest,
    RequestId as VoiceRequestId, SpeakRequest, StreamId as VoiceStreamId, StreamStatus,
    TalkRequest, TranscribeRequest,
};
use yunxi_web_contract::{EventChannel, RpcId, RpcMessage, RpcResult, parse_event_message};

#[test]
fn web_sessions_keep_independent_histories() {
    let workspace = unique_temp_dir("yunxi-web-session-isolation");
    fs::create_dir_all(&workspace).expect("create isolated Web workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");
    let model_server = thread::spawn(move || {
        for reply in ["reply for session one", "reply for session two"] {
            let (stream, body) = accept_request(&model_listener);
            assert!(body.contains("session one") || body.contains("session two"));
            write_response(
                stream,
                "200 OK",
                &format!(
                    r#"{{"choices":[{{"message":{{"content":"{reply}"}},"finish_reason":"stop"}}]}}"#
                ),
            );
        }
    });

    let mut web = WebChild::spawn_chat(&workspace, model_address);
    let address = web.wait_for_address();
    let first_id =
        rpc_call(address, "isolation-create-1", "session.create", json!({}))["sessionId"]
            .as_str()
            .expect("first session id")
            .to_string();
    let second_id =
        rpc_call(address, "isolation-create-2", "session.create", json!({}))["sessionId"]
            .as_str()
            .expect("second session id")
            .to_string();
    assert_ne!(first_id, second_id);

    for (rpc_id, session_id, content) in [
        ("isolation-prompt-1", first_id.as_str(), "session one"),
        ("isolation-prompt-2", second_id.as_str(), "session two"),
    ] {
        assert_eq!(
            rpc_call(
                address,
                rpc_id,
                "session.prompt",
                json!({
                    "sessionId": session_id,
                    "mode": "queue",
                    "content": [{ "type": "text", "text": content }],
                }),
            ),
            json!({ "accepted": true })
        );
        wait_for_mux_events(address, |events| {
            events.iter().any(|event| {
                event.payload["type"] == "session/event"
                    && event.payload["sessionId"] == session_id
                    && event.payload["event"]["type"] == "assistant/message"
            })
        });
    }

    let first_history = rpc_call(
        address,
        "isolation-history-1",
        "session.history",
        json!({ "sessionId": first_id, "maxMessages": 16 }),
    );
    let second_history = rpc_call(
        address,
        "isolation-history-2",
        "session.history",
        json!({ "sessionId": second_id, "maxMessages": 16 }),
    );
    let first_text = history_user_texts(&first_history);
    let second_text = history_user_texts(&second_history);
    assert!(first_text.iter().any(|text| text == "session one"));
    assert!(!first_text.iter().any(|text| text == "session two"));
    assert!(second_text.iter().any(|text| text == "session two"));
    assert!(!second_text.iter().any(|text| text == "session one"));

    web.stop();
    model_server.join().expect("join isolated model fixture");
    remove_workspace(&workspace);
}

#[test]
fn web_sessions_prompt_and_cancel_independently() {
    let workspace = unique_temp_dir("yunxi-web-session-concurrency");
    fs::create_dir_all(&workspace).expect("create concurrent Web workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");
    let model_server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&model_listener);
        assert!(first_body.contains("long session"));
        let stalled = thread::spawn(move || {
            let mut stream = first_stream;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"before cancel\"}}]}\n\n",
                )
                .expect("write stalled stream frame");
            stream.flush().expect("flush stalled stream frame");
            thread::sleep(Duration::from_millis(750));
        });

        let (second_stream, second_body) = accept_request(&model_listener);
        assert!(second_body.contains("short session"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"short session reply"},"finish_reason":"stop"}]}"#,
        );
        stalled.join().expect("join stalled stream");
    });

    let mut web = WebChild::spawn_chat(&workspace, model_address);
    let address = web.wait_for_address();
    let first_id =
        rpc_call(address, "concurrency-create-1", "session.create", json!({}))["sessionId"]
            .as_str()
            .expect("long session id")
            .to_string();
    let second_id =
        rpc_call(address, "concurrency-create-2", "session.create", json!({}))["sessionId"]
            .as_str()
            .expect("short session id")
            .to_string();

    assert_eq!(
        rpc_call(
            address,
            "concurrency-prompt-1",
            "session.prompt",
            json!({
                "sessionId": first_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "long session" }],
            }),
        ),
        json!({ "accepted": true })
    );
    let while_running = rpc_call(
        address,
        "concurrency-running-list",
        "session.list",
        json!({}),
    );
    let running_items = while_running["items"]
        .as_array()
        .expect("running session list items");
    assert_eq!(
        running_items
            .iter()
            .find(|summary| summary["sessionId"] == first_id)
            .expect("running session summary")["running"],
        true
    );
    assert_eq!(
        rpc_call(
            address,
            "concurrency-prompt-2",
            "session.prompt",
            json!({
                "sessionId": second_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "short session" }],
            }),
        ),
        json!({ "accepted": true })
    );
    wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["sessionId"] == second_id
                && event.payload["event"]["type"] == "assistant/message"
                && event.payload["event"]["data"]["message"]["content"][0]["text"]
                    == "short session reply"
        })
    });

    assert_eq!(
        rpc_call(
            address,
            "concurrency-cancel-1",
            "session.cancel",
            json!({ "sessionId": first_id }),
        ),
        json!({ "accepted": true })
    );
    wait_for_session_idle(address, &first_id);
    wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["sessionId"] == first_id
                && event.payload["event"]["type"] == "turn/end"
                && event.payload["event"]["data"]["reason"]["kind"] == "cancelled"
        })
    });

    web.stop();
    model_server.join().expect("join concurrent model fixture");
    remove_workspace(&workspace);
}

#[test]
fn web_command_serves_chat_history_events_and_approval() {
    let workspace = unique_temp_dir("yunxi-web-host");
    fs::create_dir_all(&workspace).expect("create Web workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");
    let model_server = thread::spawn(move || serve_model_requests(model_listener));

    let mut web = WebChild::spawn_chat(&workspace, model_address);
    let address = web.wait_for_address();

    let created = rpc_call(address, "create-1", "session.create", json!({}));
    let session_id = created["sessionId"]
        .as_str()
        .expect("created session id")
        .to_string();
    let listed = rpc_call(address, "list-1", "session.list", json!({}));
    let created_summary = listed["items"]
        .as_array()
        .expect("session list items")
        .iter()
        .find(|summary| summary["sessionId"] == session_id)
        .expect("created session summary");
    assert_eq!(created_summary["running"], false);

    let first = rpc_call(
        address,
        "prompt-1",
        "session.prompt",
        json!({
            "sessionId": session_id,
            "mode": "queue",
            "content": [{ "type": "text", "text": "hello from web" }]
        }),
    );
    assert_eq!(first, json!({ "accepted": true }));

    let first_events = wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["event"]["type"] == "assistant/message"
        })
    });
    assert!(first_events.iter().any(|event| {
        event.payload["type"] == "session/subscribed"
            && event.payload["sessionId"] == session_id
            && event.payload["lastSeq"] == -1
    }));
    assert!(first_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "user/message"
            && event.payload["event"]["data"]["content"][0]["text"] == "hello from web"
    }));
    let first_host_events = get_events_for_channel(address, "/api/events.host", EventChannel::Host);
    let event_journal = workspace.join("next-home").join("web").join("events.jsonl");
    assert!(
        event_journal.is_file(),
        "Web SSE should create its durable event journal at {}",
        event_journal.display()
    );
    let journal_bytes = fs::read(&event_journal).expect("read Web event journal");
    assert!(
        !journal_bytes.is_empty(),
        "Web event journal should contain events"
    );
    let running_states = first_host_events
        .iter()
        .filter(|event| event.payload["type"] == "host/session-status")
        .map(|event| event.payload["running"].clone())
        .collect::<Vec<_>>();
    assert_eq!(running_states, vec![json!(true), json!(false)]);
    assert!(first_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
            && event.payload["event"]["data"]["message"]["content"][0]["text"]
                == "web fixture reply"
    }));

    let second = rpc_call(
        address,
        "prompt-2",
        "session.prompt",
        json!({
            "sessionId": session_id,
            "mode": "queue",
            "content": [{ "type": "text", "text": "run the approved command" }]
        }),
    );
    assert_eq!(second, json!({ "accepted": true }));

    let approval_events = wait_for_mux_events(address, |events| {
        events
            .iter()
            .any(|event| event.payload["type"] == "approval/requested")
    });
    let approval = approval_events
        .iter()
        .find(|event| event.payload["type"] == "approval/requested")
        .expect("approval event");
    let approval_rpc_id = approval.rpc_id.clone();
    let approval_id = approval.payload["approvalId"]
        .as_str()
        .expect("approval id")
        .to_string();
    assert_eq!(
        approval.payload["sessionId"].as_str(),
        Some(session_id.as_str())
    );
    assert_eq!(approval.payload["toolName"].as_str(), Some("shell.execute"));

    let response = RpcMessage::client_response(
        RpcId::new(approval_rpc_id).expect("approval response id"),
        RpcResult::success(json!({
            "sessionId": session_id,
            "approvalId": approval_id,
            "outcome": "allowed-once"
        })),
    )
    .expect("approval response");
    let receipt = post_json(
        address,
        "/api/respond",
        response.encode().expect("encode response"),
    );
    assert_eq!(receipt, json!({ "accepted": true }));

    let resolved_events = wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["event"]["type"] == "assistant/message"
                && event.payload["event"]["data"]["message"]["content"][0]["text"]
                    == "web approval complete"
        })
    });
    assert!(
        resolved_events
            .iter()
            .any(|event| event.payload["type"] == "approval/resolved")
    );
    assert!(resolved_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
            && event.payload["event"]["data"]["message"]["content"][0]["text"]
                == "web approval complete"
    }));

    let history = rpc_call(
        address,
        "history-1",
        "session.history",
        json!({ "sessionId": session_id, "maxMessages": 16 }),
    );
    let history_events = history["events"].as_array().expect("history events");
    assert!(history_events.len() >= 12);
    assert_eq!(history_events[0]["event"]["type"], "turn/start");
    assert_eq!(history_events[1]["event"]["type"], "user/message");
    assert!(history_events.iter().any(|entry| {
        entry["event"]["type"] == "assistant/message"
            && entry["event"]["data"]["message"]["content"][0]["text"] == "web fixture reply"
    }));
    assert!(history_events.iter().any(|entry| {
        entry["event"]["type"] == "tool/call" && entry["event"]["data"]["name"] == "shell.execute"
    }));
    assert!(history_events.iter().any(|entry| {
        entry["event"]["type"] == "tool/result"
            && entry["event"]["data"]["message"]["source"]["kind"] == "tool"
    }));
    assert!(history_events.iter().any(|entry| {
        entry["event"]["type"] == "assistant/message"
            && entry["event"]["data"]["message"]["content"][0]["text"] == "web approval complete"
    }));
    assert_eq!(
        history_events
            .iter()
            .filter(|entry| entry["event"]["type"] == "turn/end")
            .count(),
        2
    );
    assert_eq!(history["hasMore"], false);

    let replay = rpc_call(
        address,
        "history-after-seq",
        "session.history",
        json!({ "sessionId": session_id, "afterSeq": 5, "maxMessages": 16 }),
    );
    let replay_events = replay["events"].as_array().expect("replay events");
    assert!(replay_events.iter().any(|entry| {
        entry["event"]["type"] == "user/message"
            && entry["event"]["data"]["content"][0]["text"] == "run the approved command"
    }));
    assert!(!replay_events.iter().any(|entry| {
        entry["event"]["type"] == "user/message"
            && entry["event"]["data"]["content"][0]["text"] == "hello from web"
    }));
    assert_eq!(replay["replayGap"], false);

    web.stop();
    model_server.join().expect("join model fixture");
    remove_workspace(&workspace);
}

#[test]
fn web_projects_isolated_multi_agent_tree_and_history() {
    let workspace = unique_temp_dir("yunxi-web-multi-agent");
    fs::create_dir_all(&workspace).expect("create multi-agent Web workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");
    let model_server = thread::spawn(move || {
        let (parent_stream, parent_body) = accept_request(&model_listener);
        assert!(
            parent_body.contains("agent.spawn"),
            "parent body: {parent_body}"
        );
        write_response(
            parent_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"web-agent-spawn","type":"function","function":{"name":"agent.spawn","arguments":"{\"task\":\"inspect the Web fixture\",\"name\":\"web-worker\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );

        let (child_stream, child_body) = accept_request(&model_listener);
        assert!(
            child_body.contains("isolated YunXi child agent"),
            "child body: {child_body}"
        );
        assert!(
            child_body.contains("inspect the Web fixture"),
            "child body: {child_body}"
        );
        assert!(
            !child_body.contains("\"tools\""),
            "child body: {child_body}"
        );
        write_response(
            child_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"Web child result"},"finish_reason":"stop"}]}"#,
        );

        let (continuation_stream, continuation_body) = accept_request(&model_listener);
        assert!(
            continuation_body.contains("Web child result"),
            "continuation body: {continuation_body}"
        );
        write_response(
            continuation_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"Web parent completed"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut command = WebChild::command(&workspace, model_address);
    command
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .env("YUNXI_NEXT_MEMORY_ENABLED", "false")
        .env("YUNXI_NEXT_COMPANION_ENABLED", "false")
        .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
        .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
        .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
        .env("YUNXI_NEXT_SHELL_ENABLED", "false")
        .env("YUNXI_NEXT_PATCH_ENABLED", "false")
        .env("YUNXI_NEXT_FILES_ENABLED", "false")
        .env("YUNXI_NEXT_MCP_ENABLED", "false")
        .env("YUNXI_NEXT_SKILLS_ENABLED", "false")
        .env("YUNXI_NEXT_MULTI_AGENT_ENABLED", "true");
    let mut web = WebChild::start(command);
    let address = web.wait_for_address();

    let created = rpc_call(address, "multi-create", "session.create", json!({}));
    let root_session_id = created["sessionId"]
        .as_str()
        .expect("root session id")
        .to_string();
    assert_eq!(
        rpc_call(
            address,
            "multi-prompt",
            "session.prompt",
            json!({
                "sessionId": root_session_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "delegate the Web fixture" }]
            }),
        ),
        json!({ "accepted": true })
    );

    let approval_events = wait_for_mux_events(address, |events| {
        events
            .iter()
            .any(|event| event.payload["type"] == "approval/requested")
    });
    let approval = approval_events
        .iter()
        .find(|event| event.payload["type"] == "approval/requested")
        .expect("multi-agent approval event");
    assert_eq!(approval.payload["toolName"], "agent.spawn");
    let approval_response = RpcMessage::client_response(
        RpcId::new(approval.rpc_id.clone()).expect("approval response id"),
        RpcResult::success(json!({
            "sessionId": root_session_id,
            "approvalId": approval.payload["approvalId"],
            "outcome": "allowed-once",
        })),
    )
    .expect("approval response");
    assert_eq!(
        post_json(
            address,
            "/api/respond",
            approval_response
                .encode()
                .expect("encode approval response"),
        ),
        json!({ "accepted": true })
    );
    wait_for_session_idle(address, &root_session_id);

    let catalog = rpc_call(
        address,
        "multi-list",
        "subagent.list",
        json!({ "parentSessionId": root_session_id }),
    );
    assert_eq!(catalog["parentAvailable"], true);
    let entries = catalog["entries"].as_array().expect("subagent entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["kind"], "child");
    assert_eq!(entries[0]["id"], "agent-1");
    assert_eq!(entries[0]["mode"], "continuable");
    assert_eq!(entries[0]["label"], "web-worker");
    assert_eq!(entries[0]["activity"], "inactive");
    assert_eq!(entries[0]["hasChildren"], false);

    let sessions = rpc_call(address, "multi-sessions", "session.list", json!({}));
    let child_summary = sessions["items"]
        .as_array()
        .expect("session summaries")
        .iter()
        .find(|summary| summary["sessionId"] == "agent-1")
        .expect("child session summary");
    assert_eq!(child_summary["origin"], "subagent");
    assert_eq!(child_summary["parentSessionId"], root_session_id);

    let history = rpc_call(
        address,
        "multi-history",
        "subagent.history",
        json!({
            "parentSessionId": root_session_id,
            "childSessionId": "agent-1",
            "mode": "one-shot",
            "maxMessages": 16,
        }),
    );
    assert_eq!(history["hasMore"], false);
    let history_events = history["events"].as_array().expect("child history events");
    assert_eq!(history_events.len(), 6);
    assert!(history_events.iter().any(|event| {
        event["event"]["type"] == "user/message"
            && event["event"]["data"]["content"][0]["text"] == "inspect the Web fixture"
    }));
    assert!(history_events.iter().any(|event| {
        event["event"]["type"] == "assistant/message"
            && event["event"]["data"]["message"]["content"][0]["text"] == "Web child result"
    }));

    web.stop();
    model_server.join().expect("join multi-agent model fixture");
    remove_workspace(&workspace);
}

#[test]
fn web_recovers_a_persisted_running_subagent_when_its_session_is_reattached() {
    let workspace = unique_temp_dir("yunxi-web-multi-agent-recovery");
    fs::create_dir_all(&workspace).expect("create multi-agent recovery workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut initial_command = WebChild::command(&workspace, model_address);
    configure_multi_agent_web(&mut initial_command);
    let mut initial_web = WebChild::start(initial_command);
    let initial_address = initial_web.wait_for_address();
    let root_session_id = rpc_call(
        initial_address,
        "recovery-create",
        "session.create",
        json!({}),
    )["sessionId"]
        .as_str()
        .expect("recovery root session id")
        .to_string();
    initial_web.stop();

    let authority = AgentDelegationGrant::new(
        WorkspaceGrant::read_write(&workspace),
        root_session_id.clone(),
        "stale-web-worker",
        AgentBudget::conservative(),
    )
    .expect("recovery authority");
    let store = CoordinatorStore::from_grant(&authority, "stale-web-host")
        .expect("create recovery coordinator store");
    let spawned = store
        .spawn(
            &AgentSpawnRequest::new(authority.clone(), "recover automatically after restart")
                .expect("recovery spawn request"),
        )
        .expect("seed child agent");
    let child_session_id = spawned.agent().id().to_string();
    store
        .start_turn(
            &AgentTurnStartRequest::new(
                authority,
                child_session_id.clone(),
                "recover automatically after restart",
            )
            .expect("recovery turn request"),
        )
        .expect("leave child turn running");

    let model_server = thread::spawn(move || {
        let (stream, body) = accept_request(&model_listener);
        assert!(
            body.contains("recover automatically after restart"),
            "recovered child body: {body}"
        );
        write_response(
            stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"recovered child result"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut restarted_command = WebChild::command(&workspace, model_address);
    configure_multi_agent_web(&mut restarted_command);
    let mut restarted_web = WebChild::start(restarted_command);
    let restarted_address = restarted_web.wait_for_address();
    let root_history = rpc_call(
        restarted_address,
        "recovery-attach",
        "session.history",
        json!({ "sessionId": root_session_id, "maxMessages": 16 }),
    );
    assert!(root_history["events"].is_array());

    let recovery_events = wait_for_mux_events(restarted_address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "subagent/message"
                && event.payload["childSessionId"] == child_session_id
                && event.payload["content"] == "recovered child result"
                && event.payload["final"] == true
        })
    });
    assert!(recovery_events.iter().any(|event| {
        event.payload["type"] == "subagent/state"
            && event.payload["childSessionId"] == child_session_id
            && event.payload["state"] == "running"
            && event.payload["recovered"] == true
    }));

    let child_history = rpc_call(
        restarted_address,
        "recovery-child-history",
        "subagent.history",
        json!({
            "parentSessionId": root_session_id,
            "childSessionId": child_session_id,
            "mode": "continuable",
            "maxMessages": 16,
        }),
    );
    assert!(
        child_history["events"]
            .as_array()
            .expect("recovered child history")
            .iter()
            .any(|event| {
                event["event"]["type"] == "assistant/message"
                    && event["event"]["data"]["message"]["content"][0]["text"]
                        == "recovered child result"
            })
    );

    restarted_web.stop();
    model_server.join().expect("join recovery model fixture");
    remove_workspace(&workspace);
}

#[test]
fn web_runs_continuable_subagents_in_parallel_with_independent_models() {
    let workspace = unique_temp_dir("yunxi-web-multi-agent-parallel");
    fs::create_dir_all(&workspace).expect("create parallel multi-agent workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut initial_command = WebChild::command(&workspace, model_address);
    configure_multi_agent_web(&mut initial_command);
    let mut initial_web = WebChild::start(initial_command);
    let initial_address = initial_web.wait_for_address();
    let root_session_id = rpc_call(
        initial_address,
        "parallel-create",
        "session.create",
        json!({}),
    )["sessionId"]
        .as_str()
        .expect("parallel root session id")
        .to_string();
    initial_web.stop();

    let authority = AgentDelegationGrant::new(
        WorkspaceGrant::read_write(&workspace),
        root_session_id.clone(),
        "parallel-web-workers",
        AgentBudget::conservative(),
    )
    .expect("parallel authority");
    let store = CoordinatorStore::from_grant(&authority, "parallel-seed-host")
        .expect("create parallel coordinator store");
    let first_child = store
        .spawn(
            &AgentSpawnRequest::new(authority.clone(), "seed first child")
                .expect("first spawn request")
                .with_name("parallel-one")
                .expect("first child name"),
        )
        .expect("seed first child")
        .agent()
        .id()
        .to_string();
    let second_child = store
        .spawn(
            &AgentSpawnRequest::new(authority, "seed second child")
                .expect("second spawn request")
                .with_name("parallel-two")
                .expect("second child name"),
        )
        .expect("seed second child")
        .agent()
        .id()
        .to_string();

    let model_server = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..2 {
            requests.push(accept_request(&model_listener));
        }
        assert!(requests.iter().any(|(_, body)| {
            body.contains("parallel request one") && body.contains("model-one")
        }));
        assert!(requests.iter().any(|(_, body)| {
            body.contains("parallel request two") && body.contains("model-two")
        }));
        for (stream, body) in requests {
            let reply = if body.contains("parallel request one") {
                "parallel result one"
            } else {
                "parallel result two"
            };
            write_response(
                stream,
                "200 OK",
                &format!(
                    r#"{{"choices":[{{"message":{{"content":"{reply}"}},"finish_reason":"stop"}}]}}"#
                ),
            );
        }
    });

    let mut restarted_command = WebChild::command(&workspace, model_address);
    configure_multi_agent_web(&mut restarted_command);
    let mut web = WebChild::start(restarted_command);
    let address = web.wait_for_address();
    let _ = rpc_call(
        address,
        "parallel-attach",
        "session.history",
        json!({ "sessionId": root_session_id, "maxMessages": 16 }),
    );

    let started = Instant::now();
    for (rpc_id, child_id, content, model) in [
        (
            "parallel-prompt-one",
            first_child.as_str(),
            "parallel request one",
            "model-one",
        ),
        (
            "parallel-prompt-two",
            second_child.as_str(),
            "parallel request two",
            "model-two",
        ),
    ] {
        let result = rpc_call(
            address,
            rpc_id,
            "subagent.prompt",
            json!({
                "parentSessionId": root_session_id,
                "childSessionId": child_id,
                "mode": "continuable",
                "content": [{ "type": "text", "text": content }],
                "model": model,
            }),
        );
        assert_eq!(result["accepted"], true);
        assert_eq!(result["running"], true);
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "subagent.prompt waited for a model response instead of returning asynchronously"
    );

    let events = wait_for_mux_events(address, |events| {
        [
            (first_child.as_str(), "parallel result one"),
            (second_child.as_str(), "parallel result two"),
        ]
        .iter()
        .all(|(child_id, reply)| {
            events.iter().any(|event| {
                event.payload["type"] == "subagent/message"
                    && event.payload["childSessionId"] == *child_id
                    && event.payload["content"] == *reply
                    && event.payload["final"] == true
            })
        })
    });
    assert!(events.iter().any(|event| {
        event.payload["type"] == "subagent/state"
            && event.payload["childSessionId"] == first_child
            && event.payload["state"] == "completed"
    }));
    assert!(events.iter().any(|event| {
        event.payload["type"] == "subagent/state"
            && event.payload["childSessionId"] == second_child
            && event.payload["state"] == "completed"
    }));

    web.stop();
    model_server.join().expect("join parallel model fixture");
    remove_workspace(&workspace);
}

#[test]
fn web_interrupts_only_the_active_subagent_and_keeps_its_sibling_usable() {
    let workspace = unique_temp_dir("yunxi-web-multi-agent-interrupt");
    fs::create_dir_all(&workspace).expect("create multi-agent interrupt workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut initial_command = WebChild::command(&workspace, model_address);
    configure_multi_agent_web(&mut initial_command);
    let mut initial_web = WebChild::start(initial_command);
    let initial_address = initial_web.wait_for_address();
    let root_session_id = rpc_call(
        initial_address,
        "interrupt-create",
        "session.create",
        json!({}),
    )["sessionId"]
        .as_str()
        .expect("interrupt root session id")
        .to_string();
    initial_web.stop();

    let authority = AgentDelegationGrant::new(
        WorkspaceGrant::read_write(&workspace),
        root_session_id.clone(),
        "interrupt-web-workers",
        AgentBudget::conservative(),
    )
    .expect("interrupt authority");
    let store = CoordinatorStore::from_grant(&authority, "interrupt-seed-host")
        .expect("create interrupt coordinator store");
    let cancelled_child = store
        .spawn(
            &AgentSpawnRequest::new(authority.clone(), "seed cancellable child")
                .expect("cancellable spawn request"),
        )
        .expect("seed cancellable child")
        .agent()
        .id()
        .to_string();
    let healthy_child = store
        .spawn(
            &AgentSpawnRequest::new(authority, "seed healthy child")
                .expect("healthy spawn request"),
        )
        .expect("seed healthy child")
        .agent()
        .id()
        .to_string();

    let model_server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&model_listener);
        assert!(first_body.contains("cancel this child now"));
        let stalled = thread::spawn(move || {
            let mut stream = first_stream;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"child-visible-before-cancel\"}}]}\n\n",
                )
                .expect("write child SSE frame");
            stream.flush().expect("flush child SSE frame");
            thread::sleep(Duration::from_millis(1_200));
        });

        let (second_stream, second_body) = accept_request(&model_listener);
        assert!(second_body.contains("healthy sibling after cancel"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"healthy sibling result"},"finish_reason":"stop"}]}"#,
        );
        stalled.join().expect("join stalled child response");
    });

    let mut restarted_command = WebChild::command(&workspace, model_address);
    configure_multi_agent_web(&mut restarted_command);
    let mut web = WebChild::start(restarted_command);
    let address = web.wait_for_address();
    let _ = rpc_call(
        address,
        "interrupt-attach",
        "session.history",
        json!({ "sessionId": root_session_id, "maxMessages": 16 }),
    );

    let started = rpc_call(
        address,
        "interrupt-prompt",
        "subagent.prompt",
        json!({
            "parentSessionId": root_session_id,
            "childSessionId": cancelled_child,
            "mode": "continuable",
            "content": [{ "type": "text", "text": "cancel this child now" }],
        }),
    );
    assert_eq!(started["accepted"], true);
    wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "subagent/message"
                && event.payload["childSessionId"] == cancelled_child
                && event.payload["delta"] == "child-visible-before-cancel"
                && event.payload["final"] == false
        })
    });

    let cancelled_at = Instant::now();
    let cancellation = rpc_call(
        address,
        "interrupt-active",
        "subagent.interrupt",
        json!({
            "parentSessionId": root_session_id,
            "childSessionId": cancelled_child,
            "mode": "continuable",
        }),
    );
    assert_eq!(cancellation["accepted"], true);
    assert_eq!(cancellation["cancellationRequested"], true);
    assert!(
        cancelled_at.elapsed() < Duration::from_millis(500),
        "subagent interrupt waited for the stalled provider response"
    );
    wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "subagent/state"
                && event.payload["childSessionId"] == cancelled_child
                && event.payload["state"] == "cancelled"
        })
    });

    let sibling = rpc_call(
        address,
        "interrupt-sibling-prompt",
        "subagent.prompt",
        json!({
            "parentSessionId": root_session_id,
            "childSessionId": healthy_child,
            "mode": "continuable",
            "content": [{ "type": "text", "text": "healthy sibling after cancel" }],
        }),
    );
    assert_eq!(sibling["accepted"], true);
    wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "subagent/message"
                && event.payload["childSessionId"] == healthy_child
                && event.payload["content"] == "healthy sibling result"
                && event.payload["final"] == true
        })
    });

    web.stop();
    model_server.join().expect("join interrupt model fixture");
    remove_workspace(&workspace);
}

#[test]
fn capability_settings_apply_live_and_persist() {
    let workspace = unique_temp_dir("yunxi-web-settings");
    fs::create_dir_all(&workspace).expect("create settings workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut web = WebChild::spawn_settings(&workspace, model_address);
    let address = web.wait_for_address();
    let initial_health = rpc_call(address, "health-initial", "health.status", json!({}));
    let initial_capabilities = initial_health["capabilities"]
        .as_u64()
        .expect("initial capability count");

    let described = rpc_call(address, "settings-describe", "settings.describe", json!({}));
    assert_eq!(described["writable"], true);
    let capabilities = settings_namespace(&described);
    assert_eq!(capabilities["revision"], 0);
    assert_eq!(capabilities["value"]["context"], true);
    assert_eq!(capabilities["value"]["multi_agent"], false);
    assert_eq!(
        capabilities["value"]
            .as_object()
            .expect("capability values")
            .len(),
        15
    );
    assert_eq!(capabilities["applies"], "live");
    let encoded_description = described.to_string();
    assert!(!encoded_description.contains("fixture-secret"));
    assert!(!encoded_description.contains(&workspace.to_string_lossy().to_string()));

    let created = rpc_call(address, "settings-session", "session.create", json!({}));
    let session_id = created["sessionId"]
        .as_str()
        .expect("settings session id")
        .to_string();

    let updated = rpc_call(
        address,
        "settings-update",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "context": false },
            "expectedRevision": 0,
        }),
    );
    assert_eq!(updated["revision"], 1);
    assert_eq!(updated["value"]["context"], false);
    assert_eq!(updated["user"]["context"], false);

    let live_health_after_update =
        rpc_call(address, "health-after-update", "health.status", json!({}));
    assert_eq!(
        live_health_after_update["capabilities"],
        initial_capabilities - 1
    );
    let live_inventory_after_update = rpc_call(
        address,
        "inventory-after-update",
        "pluginInventory/list",
        json!({}),
    );
    let live_context = inventory_entry(&live_inventory_after_update, "yunxi.context");
    assert_eq!(live_context["enabled"], false);
    assert_eq!(live_context["fiberPhase"], Value::Null);

    let sessions_after_update =
        rpc_call(address, "sessions-after-update", "session.list", json!({}));
    assert!(
        sessions_after_update["items"]
            .as_array()
            .expect("session list")
            .iter()
            .any(|item| item["sessionId"] == session_id)
    );
    assert_eq!(
        rpc_call(
            address,
            "history-after-update",
            "session.history",
            json!({ "sessionId": session_id }),
        )["events"],
        json!([])
    );

    let settings_path = workspace.join("next-home").join("settings.json");
    let persisted: Value = serde_json::from_slice(
        &fs::read(&settings_path).expect("read persisted capability settings"),
    )
    .expect("decode persisted capability settings");
    assert_eq!(persisted["version"], 1);
    assert_eq!(persisted["revision"], 1);
    assert_eq!(persisted["capabilities"]["context"], false);
    assert!(!persisted.to_string().contains("fixture-secret"));

    let host_events = get_events_for_channel(address, "/api/events.host", EventChannel::Host);
    assert!(host_events.iter().any(|event| {
        event.payload["type"] == "host/remote-event"
            && event.payload["event"] == "settings/document-updated"
            && event.payload["args"] == json!(["yunxi-capabilities", 1])
    }));

    let RpcResult::Failure(conflict) = rpc_result(
        address,
        "settings-conflict",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "shell": true },
            "expectedRevision": 0,
        }),
    ) else {
        panic!("stale settings write must fail");
    };
    assert_eq!(conflict.code(), "settings-conflict");
    assert_eq!(conflict.details()["currentRevision"], 1);

    let RpcResult::Failure(unknown_field) = rpc_result(
        address,
        "settings-unknown",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "unknown": true },
            "expectedRevision": 1,
        }),
    ) else {
        panic!("unknown capability field must fail");
    };
    assert_eq!(unknown_field.code(), "settings-rejected");

    let RpcResult::Failure(invalid_path) = rpc_result(
        address,
        "settings-path",
        "settings.mutate",
        json!({
            "ns": "yunxi-capabilities",
            "ops": [{ "op": "set", "path": ["context", "nested"], "value": true }],
            "expectedRevision": 1,
        }),
    ) else {
        panic!("nested capability path must fail");
    };
    assert_eq!(invalid_path.code(), "invalid-payload");

    let mutated = rpc_call(
        address,
        "settings-mutate",
        "settings.mutate",
        json!({
            "ns": "yunxi-capabilities",
            "ops": [{ "op": "set", "path": ["files"], "value": true }],
            "expectedRevision": 1,
        }),
    );
    assert_eq!(mutated["revision"], 2);
    assert_eq!(mutated["user"]["files"], true);

    let replaced = rpc_call(
        address,
        "settings-replace",
        "settings.replace",
        json!({
            "ns": "yunxi-capabilities",
            "section": { "context": false, "storage": true },
            "expectedRevision": 2,
        }),
    );
    assert_eq!(replaced["revision"], 3);
    assert!(replaced["user"].get("files").is_none());
    assert_eq!(replaced["user"]["storage"], true);

    let unset = rpc_call(
        address,
        "settings-unset",
        "settings.mutate",
        json!({
            "ns": "yunxi-capabilities",
            "ops": [{ "op": "unset", "path": ["storage"] }],
            "expectedRevision": 3,
        }),
    );
    assert_eq!(unset["revision"], 4);
    assert_eq!(unset["user"], json!({ "context": false }));

    let live_health = rpc_call(address, "health-live", "health.status", json!({}));
    assert_eq!(live_health["capabilities"], initial_capabilities - 1);
    let live_inventory = rpc_call(address, "inventory-live", "pluginInventory/list", json!({}));
    assert_eq!(
        inventory_entry(&live_inventory, "yunxi.context")["enabled"],
        false
    );
    web.stop();

    let model_server = thread::spawn(move || {
        let (stream, body) = accept_request(&model_listener);
        assert!(
            !body.contains("YunXi Next Development Instructions"),
            "disabled Context route leaked into model request: {body}"
        );
        write_response(
            stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"settings restart applied"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut restarted = WebChild::spawn_settings(&workspace, model_address);
    let restarted_address = restarted.wait_for_address();
    let restarted_settings = rpc_call(
        restarted_address,
        "settings-restarted",
        "settings.describe",
        json!({}),
    );
    assert_eq!(settings_namespace(&restarted_settings)["revision"], 4);
    assert_eq!(
        settings_namespace(&restarted_settings)["user"],
        json!({ "context": false })
    );

    let restarted_inventory = rpc_call(
        restarted_address,
        "inventory-restarted",
        "pluginInventory/list",
        json!({}),
    );
    let context = inventory_entry(&restarted_inventory, "yunxi.context");
    assert_eq!(context["enabled"], false);
    assert_eq!(context["fiberPhase"], Value::Null);
    assert!(context.get("manifest").is_none());
    let restarted_health = rpc_call(
        restarted_address,
        "health-restarted",
        "health.status",
        json!({}),
    );
    assert_eq!(restarted_health["capabilities"], initial_capabilities - 1);

    let created = rpc_call(
        restarted_address,
        "restart-session",
        "session.create",
        json!({}),
    );
    let session_id = created["sessionId"].as_str().expect("session id");
    assert_eq!(
        rpc_call(
            restarted_address,
            "restart-prompt",
            "session.prompt",
            json!({
                "sessionId": session_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "verify restarted settings" }],
            }),
        ),
        json!({ "accepted": true })
    );
    let events = wait_for_mux_events(restarted_address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["event"]["type"] == "assistant/message"
                && event.payload["event"]["data"]["message"]["content"][0]["text"]
                    == "settings restart applied"
        })
    });
    assert!(events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
            && event.payload["event"]["data"]["message"]["content"][0]["text"]
                == "settings restart applied"
    }));

    restarted.stop();
    model_server.join().expect("join restarted model fixture");
    remove_workspace(&workspace);
}

#[test]
fn web_management_facades_are_live_and_follow_plugin_switches() {
    let workspace = unique_temp_dir("yunxi-web-management-facades");
    fs::create_dir_all(&workspace).expect("create management workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut command = WebChild::command(&workspace, model_address);
    for (name, value) in [
        ("YUNXI_NEXT_CONTEXT_ENABLED", "false"),
        ("YUNXI_NEXT_PERSONA_ENABLED", "true"),
        ("YUNXI_NEXT_MEMORY_ENABLED", "true"),
        ("YUNXI_NEXT_COMPANION_ENABLED", "false"),
        ("YUNXI_NEXT_STORAGE_ENABLED", "true"),
        ("YUNXI_NEXT_MAILBOX_ENABLED", "true"),
        ("YUNXI_NEXT_SCHEDULER_ENABLED", "false"),
        ("YUNXI_NEXT_SHELL_ENABLED", "false"),
        ("YUNXI_NEXT_PATCH_ENABLED", "false"),
        ("YUNXI_NEXT_FILES_ENABLED", "false"),
        ("YUNXI_NEXT_MCP_ENABLED", "false"),
        ("YUNXI_NEXT_SKILLS_ENABLED", "false"),
        ("YUNXI_NEXT_MULTI_AGENT_ENABLED", "false"),
        ("YUNXI_NEXT_VOICE_ENABLED", "false"),
        ("YUNXI_NEXT_WEIXIN_ENABLED", "false"),
    ] {
        command.env(name, value);
    }
    let mut web = WebChild::start(command);
    let address = web.wait_for_address();

    let memory_status = rpc_call(
        address,
        "management-memory-status",
        "memory.status",
        json!({}),
    );
    assert_eq!(memory_status["schemaVersion"], 1);
    assert_eq!(memory_status["enabled"], true);
    assert!(memory_status["counts"].is_object());

    let memory_list = rpc_call(address, "management-memory-list", "memory.list", json!({}));
    assert_eq!(memory_list["schemaVersion"], 1);
    assert!(memory_list["result"]["records"].is_array());

    let persona_status = rpc_call(
        address,
        "management-persona-status",
        "persona.status",
        json!({}),
    );
    assert_eq!(persona_status["schemaVersion"], 1);
    assert_eq!(persona_status["status"]["enabled"], true);
    assert!(persona_status["status"]["profiles"].is_array());

    let persona_list = rpc_call(
        address,
        "management-persona-list",
        "persona.list",
        json!({}),
    );
    assert_eq!(persona_list["schemaVersion"], 1);
    assert!(
        persona_list["profiles"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let active_profile = persona_status["status"]["active_profile"]
        .as_str()
        .expect("active persona profile");
    let persona_profile = rpc_call(
        address,
        "management-persona-profile",
        "persona.profile",
        json!({ "id": active_profile }),
    );
    assert_eq!(persona_profile["profile"]["id"], active_profile);

    let relationship = rpc_call(
        address,
        "management-relationship-list",
        "relationship.list",
        json!({ "limit": 20 }),
    );
    assert_eq!(relationship["schemaVersion"], 1);
    assert!(relationship["records"].is_array());

    let mailbox = rpc_call(
        address,
        "management-mailbox-list",
        "mailbox.list",
        json!({}),
    );
    assert_eq!(mailbox["schemaVersion"], 1);
    assert!(mailbox["items"].is_array());
    assert_eq!(mailbox["unreadCount"], 0);

    let updated = rpc_call(
        address,
        "management-memory-disable",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "memory": false },
            "expectedRevision": 0,
        }),
    );
    assert_eq!(updated["revision"], 1);
    assert_eq!(updated["value"]["memory"], false);

    let RpcResult::Failure(error) = rpc_result(
        address,
        "management-memory-disabled",
        "memory.status",
        json!({}),
    ) else {
        panic!("disabled memory route must fail closed");
    };
    assert_eq!(error.code(), "plugin-disabled");

    web.stop();
    drop(model_listener);
    remove_workspace(&workspace);
}

#[test]
fn voice_and_weixin_plugins_are_projected_only_when_enabled() {
    let workspace = unique_temp_dir("yunxi-web-auxiliary-plugins");
    fs::create_dir_all(&workspace).expect("create auxiliary plugin workspace");

    // Startup only needs the model plugin handshake. Keep the provider
    // listener open without serving requests because this test exercises the
    // inventory and plugin boundaries, not model completion.
    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut command = WebChild::command(&workspace, model_address);
    for (name, value) in [
        ("YUNXI_NEXT_CONTEXT_ENABLED", "false"),
        ("YUNXI_NEXT_PERSONA_ENABLED", "false"),
        ("YUNXI_NEXT_MEMORY_ENABLED", "false"),
        ("YUNXI_NEXT_COMPANION_ENABLED", "false"),
        ("YUNXI_NEXT_STORAGE_ENABLED", "false"),
        ("YUNXI_NEXT_MAILBOX_ENABLED", "false"),
        ("YUNXI_NEXT_SCHEDULER_ENABLED", "false"),
        ("YUNXI_NEXT_SHELL_ENABLED", "false"),
        ("YUNXI_NEXT_PATCH_ENABLED", "false"),
        ("YUNXI_NEXT_FILES_ENABLED", "false"),
        ("YUNXI_NEXT_MCP_ENABLED", "false"),
        ("YUNXI_NEXT_SKILLS_ENABLED", "false"),
        ("YUNXI_NEXT_MULTI_AGENT_ENABLED", "false"),
        ("YUNXI_NEXT_VOICE_ENABLED", "true"),
        ("YUNXI_NEXT_WEIXIN_ENABLED", "true"),
    ] {
        command.env(name, value);
    }
    let mut web = WebChild::start(command);
    let address = web.wait_for_address();
    let inventory = rpc_call(
        address,
        "auxiliary-inventory",
        "pluginInventory/list",
        json!({}),
    );

    let voice = inventory_entry(&inventory, "yunxi.voice.fixture");
    assert_eq!(voice["enabled"], true);
    assert_eq!(voice["fiberPhase"], "active");
    assert!(voice.get("manifest").is_none());

    let weixin = inventory_entry(&inventory, "yunxi.channel.weixin");
    assert_eq!(weixin["enabled"], true);
    assert_eq!(weixin["fiberPhase"], "active");
    assert!(weixin.get("manifest").is_none());

    web.stop();
    drop(model_listener);
    remove_workspace(&workspace);
}

#[test]
fn web_voice_runtime_is_bounded_and_revoked_by_plugin_switch() {
    let workspace = unique_temp_dir("yunxi-web-voice-runtime");
    fs::create_dir_all(&workspace).expect("create Voice runtime workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut command = WebChild::command(&workspace, model_address);
    for (name, value) in [
        ("YUNXI_NEXT_CONTEXT_ENABLED", "false"),
        ("YUNXI_NEXT_PERSONA_ENABLED", "false"),
        ("YUNXI_NEXT_MEMORY_ENABLED", "false"),
        ("YUNXI_NEXT_COMPANION_ENABLED", "false"),
        ("YUNXI_NEXT_STORAGE_ENABLED", "true"),
        ("YUNXI_NEXT_MAILBOX_ENABLED", "false"),
        ("YUNXI_NEXT_SCHEDULER_ENABLED", "false"),
        ("YUNXI_NEXT_SHELL_ENABLED", "false"),
        ("YUNXI_NEXT_PATCH_ENABLED", "false"),
        ("YUNXI_NEXT_FILES_ENABLED", "false"),
        ("YUNXI_NEXT_MCP_ENABLED", "false"),
        ("YUNXI_NEXT_SKILLS_ENABLED", "false"),
        ("YUNXI_NEXT_MULTI_AGENT_ENABLED", "false"),
        ("YUNXI_NEXT_VOICE_ENABLED", "true"),
        ("YUNXI_NEXT_WEIXIN_ENABLED", "false"),
    ] {
        command.env(name, value);
    }
    let mut web = WebChild::start(command);
    let address = web.wait_for_address();

    let doctor = rpc_call(address, "voice-doctor", "voice.doctor", json!({}));
    assert_eq!(doctor["source"], "yunxi-voice");
    assert_eq!(doctor["operation"], "doctor");
    assert!(doctor["report"].is_object());

    let devices = rpc_call(address, "voice-devices", "voice.devices", json!({}));
    assert_eq!(devices["operation"], "enumerate_devices");
    assert!(devices["report"].is_object());

    let format = AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).expect("Voice format");
    let stream_id = VoiceStreamId::new("web-voice-stream").expect("Voice stream id");
    let input = TranscribeRequest::new(
        VoiceRequestId::new("web-voice-transcribe").expect("Voice request id"),
        stream_id.clone(),
        format,
        vec![AudioChunk::new(stream_id.clone(), 0, format, vec![0; 8], true).expect("Voice chunk")],
        true,
        StreamStatus::new(),
    )
    .expect("transcribe request");
    let transcribed = rpc_call(
        address,
        "voice-transcribe",
        "voice.transcribe",
        json!(input.clone()),
    );
    assert_eq!(transcribed["operation"], "transcribe");
    assert!(
        transcribed["report"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );

    let spoken = rpc_call(
        address,
        "voice-speak",
        "voice.speak",
        json!(
            SpeakRequest::new(
                VoiceRequestId::new("web-voice-speak").expect("Voice request id"),
                VoiceStreamId::new("web-voice-output").expect("Voice stream id"),
                "hello from Web",
                format,
                StreamStatus::new(),
            )
            .expect("speak request")
        ),
    );
    assert_eq!(spoken["operation"], "speak");
    assert!(
        spoken["report"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );

    let chatted = rpc_call(
        address,
        "voice-chat",
        "voice.chat",
        json!(
            VoiceChatRequest::new(
                VoiceRequestId::new("web-voice-chat").expect("Voice request id"),
                "web-conversation",
                "hello",
            )
            .expect("chat request")
        ),
    );
    assert_eq!(chatted["operation"], "chat");
    assert!(chatted["report"].is_array());

    let talked = rpc_call(
        address,
        "voice-talk",
        "voice.talk",
        json!(
            TalkRequest::new(
                VoiceRequestId::new("web-voice-talk").expect("Voice request id"),
                input,
                format,
                StreamStatus::new(),
            )
            .expect("talk request")
        ),
    );
    assert_eq!(talked["operation"], "talk");
    assert!(talked["report"].is_array());

    let cancelled = rpc_call(
        address,
        "voice-cancel",
        "voice.cancel",
        json!(CancelRequest::new(stream_id, "Web cancellation").expect("cancel request")),
    );
    assert_eq!(cancelled["operation"], "cancel");
    assert_eq!(cancelled["report"]["status"]["cancellation"], "cancelled");

    let invalid = rpc_result(
        address,
        "voice-unknown-field",
        "voice.doctor",
        json!({ "credential": "must-not-cross-boundary" }),
    );
    let RpcResult::Failure(error) = invalid else {
        panic!("unknown Voice fields must fail at the Web boundary");
    };
    assert_eq!(error.code(), "invalid-payload");

    let disabled = rpc_call(
        address,
        "voice-disable",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "voice": false },
            "expectedRevision": 0,
        }),
    );
    assert_eq!(disabled["value"]["voice"], false);
    let revoked = rpc_result(address, "voice-revoked", "voice.doctor", json!({}));
    let RpcResult::Failure(error) = revoked else {
        panic!("disabled Voice route must fail closed");
    };
    assert_eq!(error.code(), "plugin-disabled");

    web.stop();
    drop(model_listener);
    remove_workspace(&workspace);
}

#[test]
fn web_weixin_runtime_is_bounded_and_revoked_by_plugin_switch() {
    let workspace = unique_temp_dir("yunxi-web-weixin-runtime");
    fs::create_dir_all(&workspace).expect("create Weixin runtime workspace");

    // The model endpoint is only needed for the mandatory model-plugin
    // handshake. This test exercises the isolated Weixin process exclusively.
    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut command = WebChild::command(&workspace, model_address);
    for (name, value) in [
        ("YUNXI_NEXT_CONTEXT_ENABLED", "false"),
        ("YUNXI_NEXT_PERSONA_ENABLED", "false"),
        ("YUNXI_NEXT_MEMORY_ENABLED", "false"),
        ("YUNXI_NEXT_COMPANION_ENABLED", "false"),
        ("YUNXI_NEXT_STORAGE_ENABLED", "true"),
        ("YUNXI_NEXT_MAILBOX_ENABLED", "false"),
        ("YUNXI_NEXT_SCHEDULER_ENABLED", "false"),
        ("YUNXI_NEXT_SHELL_ENABLED", "false"),
        ("YUNXI_NEXT_PATCH_ENABLED", "false"),
        ("YUNXI_NEXT_FILES_ENABLED", "false"),
        ("YUNXI_NEXT_MCP_ENABLED", "false"),
        ("YUNXI_NEXT_SKILLS_ENABLED", "false"),
        ("YUNXI_NEXT_MULTI_AGENT_ENABLED", "false"),
        ("YUNXI_NEXT_VOICE_ENABLED", "false"),
        ("YUNXI_NEXT_WEIXIN_ENABLED", "true"),
    ] {
        command.env(name, value);
    }
    let mut web = WebChild::start(command);
    let address = web.wait_for_address();

    let status = rpc_call(address, "weixin-status", "weixin.status", json!({}));
    assert_eq!(status["schemaVersion"], 1);
    assert_eq!(status["source"], "yunxi-weixin");
    assert_eq!(status["operation"], "status");
    assert_eq!(status["mode"], "loopback");
    assert_eq!(status["report"]["state"], "logged_out");
    assert_eq!(status["report"]["productionReady"], false);
    assert_eq!(status["report"]["credential_stored"], false);

    let doctor = rpc_call(address, "weixin-doctor", "weixin.doctor", json!({}));
    assert_eq!(doctor["operation"], "doctor");
    assert!(doctor["report"]["checks"].is_array());

    let login = rpc_call(address, "weixin-login", "weixin.login", json!({}));
    assert_eq!(login["report"]["state"], "awaiting_qr");
    assert_eq!(login["report"]["qrcode"], "loopback-qr");
    assert_eq!(login["report"]["credential_stored"], false);

    let invalid_poll = rpc_result(
        address,
        "weixin-poll-login-invalid",
        "weixin.pollLogin",
        json!({ "verifyCode": "" }),
    );
    let RpcResult::Failure(error) = invalid_poll else {
        panic!("an empty verification code must fail at the Web boundary");
    };
    assert_eq!(error.code(), "invalid-payload");
    let logged_in = rpc_call(address, "weixin-poll-login", "weixin.pollLogin", json!({}));
    assert_eq!(logged_in["report"]["state"], "loopback");
    assert_eq!(logged_in["report"]["credential_stored"], true);

    let lifecycle_started_at = Instant::now();
    let started = rpc_call(
        address,
        "weixin-serve-start",
        "weixin.serveStart",
        json!({ "maxPolls": 1, "maxMessagesPerPoll": 4 }),
    );
    assert_eq!(started["operation"], yunxi_weixin::SERVE_START_OPERATION);
    assert!(matches!(
        started["report"]["state"].as_str(),
        Some("running" | "completed")
    ));
    let generation = started["report"]["generation"]
        .as_u64()
        .expect("serve generation");
    assert!(generation > 0);
    assert!(
        lifecycle_started_at.elapsed() < Duration::from_secs(2),
        "serveStart blocked on the long-poll worker"
    );

    let repeated = rpc_call(
        address,
        "weixin-serve-start-repeat",
        "weixin.serveStart",
        json!({ "maxPolls": 1, "maxMessagesPerPoll": 4 }),
    );
    assert_eq!(repeated["report"]["generation"], generation);

    let lifecycle_status_at = Instant::now();
    let lifecycle = rpc_call(
        address,
        "weixin-serve-status",
        "weixin.serveStatus",
        json!({}),
    );
    assert_eq!(lifecycle["operation"], yunxi_weixin::SERVE_STATUS_OPERATION);
    assert!(lifecycle["report"]["messages"].is_array());
    assert!(
        lifecycle_status_at.elapsed() < Duration::from_secs(2),
        "serveStatus blocked on the long-poll worker"
    );

    let lifecycle_stop_at = Instant::now();
    let stopped = rpc_call(address, "weixin-serve-stop", "weixin.serveStop", json!({}));
    assert_eq!(stopped["operation"], yunxi_weixin::SERVE_STOP_OPERATION);
    assert_eq!(stopped["report"]["state"], "stopped");
    let repeated_stop = rpc_call(
        address,
        "weixin-serve-stop-repeat",
        "weixin.serveStop",
        json!({}),
    );
    assert_eq!(repeated_stop["report"]["state"], "stopped");
    assert!(
        lifecycle_stop_at.elapsed() < Duration::from_secs(2),
        "serveStop waited for the long-poll worker"
    );

    let served = rpc_call(
        address,
        "weixin-serve",
        "weixin.serve",
        json!({ "maxPolls": 1, "maxMessagesPerPoll": 4 }),
    );
    assert_eq!(served["operation"], "serve");
    assert_eq!(served["report"]["report"]["polls"], 1);
    assert_eq!(served["report"]["report"]["received_messages"], 0);
    assert!(served["report"]["messages"].is_array());

    let queued = rpc_call(
        address,
        "weixin-queued",
        "weixin.queued",
        json!({ "maximum": 4 }),
    );
    assert_eq!(queued["operation"], "queued_messages");
    assert!(queued["report"].is_array());
    assert!(
        queued["report"]
            .as_array()
            .expect("queued report")
            .is_empty()
    );

    let sent = rpc_call(
        address,
        "weixin-send",
        "weixin.send",
        json!({
            "message": {
                "message_id": "web-outbound-1",
                "from_user_id": "",
                "to_user_id": "peer-1",
                "client_id": "web-outbound-1",
                "message_type": 2,
                "message_state": 2,
                "item_list": [{
                    "type": 1,
                    "text_item": { "text": "hello from Web" },
                    "is_completed": true
                }],
                "context_token": "web-context"
            }
        }),
    );
    assert_eq!(sent["operation"], "send_message");
    assert_eq!(sent["report"]["statusCode"], 200);

    let pair = rpc_call(
        address,
        "weixin-pair-request",
        "weixin.pair",
        json!({ "action": "request", "peerId": "peer-1" }),
    );
    assert_eq!(pair["report"]["request"]["state"], "pending");
    let request_id = pair["report"]["request"]["request_id"]
        .as_str()
        .expect("pair request id")
        .to_string();
    let approved = rpc_call(
        address,
        "weixin-pair-approve",
        "weixin.pair",
        json!({ "action": "approve", "requestId": request_id }),
    );
    assert_eq!(approved["report"]["request"]["state"], "approved");

    let bound = rpc_call(
        address,
        "weixin-session-bind",
        "weixin.session",
        json!({
            "action": "bind",
            "bindingSessionId": "web-session-1",
            "peerId": "peer-1"
        }),
    );
    assert_eq!(bound["report"]["changed"], true);
    let listed = rpc_call(
        address,
        "weixin-session-list",
        "weixin.session",
        json!({ "action": "list" }),
    );
    assert_eq!(
        listed["report"]["bindings"][0]["session_id"],
        "web-session-1"
    );

    let logout = rpc_call(address, "weixin-logout", "weixin.logout", json!({}));
    assert_eq!(logout["report"]["state"], "logged_out");

    let invalid = rpc_result(
        address,
        "weixin-unknown-field",
        "weixin.status",
        json!({ "credential": "must-not-cross-boundary" }),
    );
    let RpcResult::Failure(error) = invalid else {
        panic!("unknown Weixin fields must fail at the Web boundary");
    };
    assert_eq!(error.code(), "invalid-payload");

    let disabled = rpc_call(
        address,
        "weixin-disable",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "weixin": false },
            "expectedRevision": 0,
        }),
    );
    assert_eq!(disabled["value"]["weixin"], false);
    let revoked = rpc_result(address, "weixin-revoked", "weixin.status", json!({}));
    let RpcResult::Failure(error) = revoked else {
        panic!("disabled Weixin route must fail closed");
    };
    assert_eq!(error.code(), "plugin-disabled");

    web.stop();
    drop(model_listener);
    remove_workspace(&workspace);
}

#[test]
#[ignore = "launched by the process-tree cancellation integration test"]
fn web_shell_cancellation_descendant_helper() {
    let root = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::env::current_dir().expect("helper working directory"));
    let _ = fs::write(root.join("tool-started.txt"), b"started");
    thread::sleep(Duration::from_secs(30));
    let _ = fs::write(root.join("tool-finished.txt"), b"finished");
}

#[test]
fn web_cancel_interrupts_an_inflight_root_shell_tool_and_its_process_tree() {
    let workspace = unique_temp_dir("yunxi-web-root-tool-cancel");
    fs::create_dir_all(&workspace).expect("create root tool cancellation workspace");

    // Run a copied test executable so the shell command itself stays
    // read-only-looking while the child process creates observable markers.
    // This exercises cancellation of both the shell wrapper and its child
    // without weakening the production shell write policy.
    let helper = workspace.join(format!("yunxi-cancel-helper-{}.exe", std::process::id()));
    fs::copy(
        std::env::current_exe().expect("current Web test executable"),
        &helper,
    )
    .expect("copy cancellation helper executable");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");
    let command = format!(
        "{} --ignored --exact web_shell_cancellation_descendant_helper --nocapture",
        helper.display()
    );
    let model_server = thread::spawn(move || {
        let (stream, body) = accept_request(&model_listener);
        assert!(body.contains("cancel the running shell tool"));
        let arguments = json!({
            "command": command,
            "timeout_millis": 10_000,
        })
        .to_string();
        let response = json!({
            "choices": [{
                "message": {
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "cancel-running-shell",
                        "type": "function",
                        "function": {
                            "name": "shell.execute",
                            "arguments": arguments,
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        write_response(stream, "200 OK", &response.to_string());
    });

    let mut web = WebChild::spawn_chat(&workspace, model_address);
    let address = web.wait_for_address();
    let session_id =
        rpc_call(address, "tool-cancel-create", "session.create", json!({}))["sessionId"]
            .as_str()
            .expect("session id")
            .to_string();
    assert_eq!(
        rpc_call(
            address,
            "tool-cancel-prompt",
            "session.prompt",
            json!({
                "sessionId": session_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "cancel the running shell tool" }],
            }),
        ),
        json!({ "accepted": true })
    );

    let approval_events = wait_for_mux_events(address, |events| {
        events
            .iter()
            .any(|event| event.payload["type"] == "approval/requested")
    });
    let approval = approval_events
        .iter()
        .find(|event| event.payload["type"] == "approval/requested")
        .expect("shell approval event");
    let response = RpcMessage::client_response(
        RpcId::new(approval.rpc_id.clone()).expect("approval response id"),
        RpcResult::success(json!({
            "sessionId": session_id,
            "approvalId": approval.payload["approvalId"],
            "outcome": "allowed-once",
        })),
    )
    .expect("approval response");
    assert_eq!(
        post_json(
            address,
            "/api/respond",
            response.encode().expect("encode approval response"),
        ),
        json!({ "accepted": true })
    );

    let started = workspace.join("tool-started.txt");
    let start_deadline = Instant::now() + Duration::from_secs(10);
    while !started.exists() && Instant::now() < start_deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(started.exists(), "the shell command never started");

    let cancelled_at = Instant::now();
    assert_eq!(
        rpc_call(
            address,
            "tool-cancel-active",
            "session.cancel",
            json!({ "sessionId": session_id }),
        ),
        json!({ "accepted": true })
    );
    wait_for_session_idle(address, &session_id);
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(2),
        "in-flight tool cancellation waited for the shell timeout"
    );
    wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["sessionId"] == session_id
                && event.payload["event"]["type"] == "turn/end"
                && event.payload["event"]["data"]["reason"]["kind"] == "cancelled"
        })
    });

    thread::sleep(Duration::from_millis(3_500));
    assert!(
        !workspace.join("tool-finished.txt").exists(),
        "a descendant shell process survived plugin cancellation"
    );

    web.stop();
    model_server
        .join()
        .expect("join cancellation model fixture");
    remove_workspace(&workspace);
}

#[test]
fn plugin_setting_rebuild_failure_rolls_back_without_losing_web_session() {
    let workspace = unique_temp_dir("yunxi-web-settings-rollback");
    fs::create_dir_all(&workspace).expect("create rollback workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut web = WebChild::spawn_settings(&workspace, model_address);
    let address = web.wait_for_address();
    let created = rpc_call(address, "rollback-session", "session.create", json!({}));
    let session_id = created["sessionId"]
        .as_str()
        .expect("rollback session id")
        .to_string();

    let RpcResult::Failure(error) = rpc_result(
        address,
        "storage-disable",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "plugins": { "yunxi.storage": false } },
            "expectedRevision": 0,
        }),
    ) else {
        panic!("disabling active session storage must fail the live rebuild");
    };
    assert_eq!(error.code(), "settings-apply-failed");
    assert_eq!(error.details()["rolledBack"], true);

    let settings = rpc_call(address, "rollback-settings", "settings.describe", json!({}));
    let namespace = settings_namespace(&settings);
    assert_eq!(namespace["revision"], 0);
    assert_eq!(namespace["value"]["storage"], true);
    assert!(namespace.get("user").is_none());

    let inventory = rpc_call(
        address,
        "rollback-inventory",
        "pluginInventory/list",
        json!({}),
    );
    assert_eq!(
        inventory_entry(&inventory, "yunxi.storage")["enabled"],
        true
    );
    let sessions = rpc_call(address, "rollback-sessions", "session.list", json!({}));
    assert!(
        sessions["items"]
            .as_array()
            .expect("rollback session list")
            .iter()
            .any(|item| item["sessionId"] == session_id)
    );

    web.stop();
    remove_workspace(&workspace);
}

#[test]
fn web_streams_before_completion_and_cancels_only_the_active_generation() {
    let workspace = unique_temp_dir("yunxi-web-stream-cancel");
    fs::create_dir_all(&workspace).expect("create stream workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");
    let model_server = thread::spawn(move || {
        let (first_stream, first_body) = accept_request(&model_listener);
        assert!(first_body.contains("stream then cancel"));
        let stalled = thread::spawn(move || {
            let mut stream = first_stream;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"visible-before-cancel\"}}]}\n\n",
                )
                .expect("write first SSE frame");
            stream.flush().expect("flush first SSE frame");
            thread::sleep(Duration::from_millis(750));
        });

        let (second_stream, second_body) = accept_request(&model_listener);
        assert!(second_body.contains("healthy retry"));
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"recovered after cancel"},"finish_reason":"stop"}]}"#,
        );
        stalled.join().expect("join stalled response");
    });

    let mut command = WebChild::command(&workspace, model_address);
    command
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .env("YUNXI_NEXT_MEMORY_ENABLED", "false")
        .env("YUNXI_NEXT_COMPANION_ENABLED", "false")
        .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
        .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
        .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
        .env("YUNXI_NEXT_SHELL_ENABLED", "false")
        .env("YUNXI_NEXT_PATCH_ENABLED", "false")
        .env("YUNXI_NEXT_FILES_ENABLED", "false")
        .env("YUNXI_NEXT_MCP_ENABLED", "false")
        .env("YUNXI_NEXT_SKILLS_ENABLED", "false")
        .env("YUNXI_NEXT_MULTI_AGENT_ENABLED", "false");
    let mut web = WebChild::start(command);
    let address = web.wait_for_address();
    let created = rpc_call(address, "stream-create", "session.create", json!({}));
    let session_id = created["sessionId"]
        .as_str()
        .expect("stream session id")
        .to_string();

    let started = Instant::now();
    assert_eq!(
        rpc_call(
            address,
            "stream-prompt",
            "session.prompt",
            json!({
                "sessionId": session_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "stream then cancel" }],
            }),
        ),
        json!({ "accepted": true })
    );
    assert!(started.elapsed() < Duration::from_millis(500));

    let stream_events = wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["event"]["type"] == "assistant/chunk"
                && event.payload["event"]["data"]["chunk"]["text"] == "visible-before-cancel"
        })
    });
    assert!(!stream_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
    }));

    let settings_while_running = rpc_call(
        address,
        "settings-during-stream",
        "settings.describe",
        json!({}),
    );
    assert_eq!(settings_while_running["writable"], true);
    assert!(
        settings_namespace(&settings_while_running)["plugins"].is_object(),
        "plugin inventory must remain readable while a turn is running"
    );

    let cancelled_at = Instant::now();
    assert_eq!(
        rpc_call(
            address,
            "stream-cancel",
            "session.cancel",
            json!({ "sessionId": session_id }),
        ),
        json!({ "accepted": true })
    );
    assert!(cancelled_at.elapsed() < Duration::from_millis(500));
    wait_for_session_idle(address, &session_id);
    let cancelled_events = wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["event"]["type"] == "turn/end"
                && event.payload["event"]["data"]["reason"]["kind"] == "cancelled"
        })
    });
    assert!(cancelled_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["data"]["reason"]["kind"] == "cancelled"
    }));

    let cancelled_history = rpc_call(
        address,
        "cancelled-history",
        "session.history",
        json!({ "sessionId": session_id, "maxMessages": 16 }),
    );
    let cancelled_history_events = cancelled_history["events"]
        .as_array()
        .expect("cancelled history events");
    assert!(cancelled_history_events.iter().any(|entry| {
        entry["event"]["type"] == "user/message"
            && entry["event"]["data"]["content"][0]["text"] == "stream then cancel"
    }));
    assert!(cancelled_history_events.iter().any(|entry| {
        entry["event"]["type"] == "turn/end"
            && entry["event"]["data"]["reason"]["kind"] == "cancelled"
    }));
    assert!(!cancelled_history_events.iter().any(|entry| {
        entry["event"]["type"] == "assistant/message"
            && entry["event"]["data"]["message"]["content"][0]["text"] == "visible-before-cancel"
    }));

    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        rpc_call(
            address,
            "retry-prompt",
            "session.prompt",
            json!({
                "sessionId": session_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "healthy retry" }],
            }),
        ),
        json!({ "accepted": true })
    );
    let recovered = wait_for_mux_events(address, |events| {
        events.iter().any(|event| {
            event.payload["type"] == "session/event"
                && event.payload["event"]["type"] == "assistant/message"
                && event.payload["event"]["data"]["message"]["content"][0]["text"]
                    == "recovered after cancel"
        })
    });
    assert!(recovered.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
    }));

    web.stop();
    model_server.join().expect("join stream model fixture");
    remove_workspace(&workspace);
}

struct WebChild {
    child: Child,
    stdout: BufReader<std::process::ChildStdout>,
}

fn configure_multi_agent_web(command: &mut Command) {
    command
        .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
        .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
        .env("YUNXI_NEXT_MEMORY_ENABLED", "false")
        .env("YUNXI_NEXT_COMPANION_ENABLED", "false")
        .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
        .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
        .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
        .env("YUNXI_NEXT_SHELL_ENABLED", "false")
        .env("YUNXI_NEXT_PATCH_ENABLED", "false")
        .env("YUNXI_NEXT_FILES_ENABLED", "false")
        .env("YUNXI_NEXT_MCP_ENABLED", "false")
        .env("YUNXI_NEXT_SKILLS_ENABLED", "false")
        .env("YUNXI_NEXT_MULTI_AGENT_ENABLED", "true");
}

impl WebChild {
    fn spawn_chat(workspace: &std::path::Path, model_address: SocketAddr) -> Self {
        let mut command = Self::command(workspace, model_address);
        command
            .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
            .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
            .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
            .env("YUNXI_NEXT_SHELL_ENABLED", "true");
        Self::start(command)
    }

    fn spawn_settings(workspace: &std::path::Path, model_address: SocketAddr) -> Self {
        let mut command = Self::command(workspace, model_address);
        command
            .env_remove("YUNXI_NEXT_CONTEXT_ENABLED")
            .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
            .env("YUNXI_NEXT_MEMORY_ENABLED", "false")
            .env("YUNXI_NEXT_COMPANION_ENABLED", "false")
            .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
            .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
            .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
            .env("YUNXI_NEXT_SHELL_ENABLED", "false")
            .env("YUNXI_NEXT_PATCH_ENABLED", "false")
            .env("YUNXI_NEXT_FILES_ENABLED", "false")
            .env("YUNXI_NEXT_MCP_ENABLED", "false")
            .env("YUNXI_NEXT_SKILLS_ENABLED", "false")
            .env("YUNXI_NEXT_MULTI_AGENT_ENABLED", "false");
        Self::start(command)
    }

    fn command(workspace: &std::path::Path, model_address: SocketAddr) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_yunxi-next"));
        command
            .arg("web")
            .args(["--bind", "127.0.0.1:0"])
            .current_dir(workspace)
            .env("YUNXI_NEXT_HOME", workspace.join("next-home"))
            .env("YUNXI_PROVIDER_PROFILE", "fixture")
            .env(
                "YUNXI_PROVIDER_BASE_URL",
                format!("http://{model_address}/v1"),
            )
            .env("YUNXI_PROVIDER_API_KEY", "fixture-secret")
            .env("YUNXI_AGENT_MODEL", "fixture-model")
            .env("YUNXI_PROVIDER_TIMEOUT_MILLIS", "3000")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        command
    }

    fn start(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("launch Web command");
        let stdout = BufReader::new(child.stdout.take().expect("Web stdout"));
        Self { child, stdout }
    }

    fn wait_for_address(&mut self) -> SocketAddr {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read Web startup line");
        line.trim()
            .strip_prefix("YunXi Next Web listening on http://")
            .expect("Web startup address")
            .parse()
            .expect("Web socket address")
    }

    fn stop(&mut self) {
        if self.child.try_wait().expect("poll Web command").is_none() {
            drop(self.child.stdin.take());
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if self.child.try_wait().expect("poll Web shutdown").is_some() {
                    return;
                }
                if Instant::now() >= deadline {
                    let _ignored = self.child.kill();
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        let _ignored = self.child.wait();
    }
}

impl Drop for WebChild {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_model_requests(listener: TcpListener) {
    let (first_stream, first_body) = accept_request(&listener);
    assert!(first_body.contains("hello from web"));
    write_response(
        first_stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":"web fixture reply"},"finish_reason":"stop"}]}"#,
    );

    let (second_stream, second_body) = accept_request(&listener);
    assert!(second_body.contains("shell.execute"));
    write_response(
        second_stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"web-call-1","type":"function","function":{"name":"shell.execute","arguments":"{\"command\":\"echo web-action\"}"}}]},"finish_reason":"tool_calls"}]}"#,
    );

    let (third_stream, third_body) = accept_request(&listener);
    assert!(third_body.contains("\"role\":\"tool\""));
    assert!(third_body.contains("web-action"));
    write_response(
        third_stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":"web approval complete"},"finish_reason":"stop"}]}"#,
    );
}

fn rpc_call(address: SocketAddr, id: &str, method: &str, payload: Value) -> Value {
    match rpc_result(address, id, method, payload) {
        RpcResult::Success(value) => value,
        RpcResult::Failure(error) => panic!("Web RPC failed: {error:?}"),
    }
}

fn rpc_result(address: SocketAddr, id: &str, method: &str, payload: Value) -> RpcResult<Value> {
    let request = RpcMessage::client_request(RpcId::new(id).expect("RPC id"), method, payload)
        .expect("client request")
        .encode()
        .expect("encode request");
    let body = post_json(address, &format!("/api/{method}"), request);
    let message = serde_json::from_value(body).expect("server response");
    let RpcMessage::ServerResponse(response) = message else {
        panic!("unary Web response must be a server response");
    };
    response.result().clone()
}

fn settings_namespace(document: &Value) -> &Value {
    document["namespaces"]
        .as_array()
        .expect("settings namespaces")
        .iter()
        .find(|namespace| namespace["ns"] == "yunxi-capabilities")
        .expect("capability settings namespace")
}

fn inventory_entry<'a>(inventory: &'a Value, entry_id: &str) -> &'a Value {
    inventory["entries"]
        .as_array()
        .expect("plugin inventory entries")
        .iter()
        .find(|entry| entry["entryId"] == entry_id)
        .expect("plugin inventory entry")
}

fn history_user_texts(history: &Value) -> Vec<String> {
    history["events"]
        .as_array()
        .expect("history events")
        .iter()
        .filter(|entry| entry["event"]["type"] == "user/message")
        .filter_map(|entry| {
            entry["event"]["data"]["content"][0]["text"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

struct EventFrame {
    rpc_id: String,
    payload: Value,
}

fn get_events(address: SocketAddr, path: &str) -> Vec<EventFrame> {
    get_events_for_channel(address, path, EventChannel::Mux)
}

fn wait_for_mux_events<F>(address: SocketAddr, mut ready: F) -> Vec<EventFrame>
where
    F: FnMut(&[EventFrame]) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut collected = Vec::new();
    loop {
        collected.extend(get_events(address, "/api/events.mux"));
        if ready(&collected) {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "expected Mux event was not published"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_session_idle(address: SocketAddr, session_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = get_events_for_channel(address, "/api/events.host", EventChannel::Host);
        let latest = events.iter().rev().find(|event| {
            event.payload["type"] == "host/session-status"
                && event.payload["sessionId"] == session_id
        });
        if latest.is_some_and(|event| event.payload["running"] == false) {
            return;
        }
        assert!(Instant::now() < deadline, "Web session remained busy");
        thread::sleep(Duration::from_millis(10));
    }
}

fn get_events_for_channel(
    address: SocketAddr,
    path: &str,
    expected_channel: EventChannel,
) -> Vec<EventFrame> {
    let body = request(address, "GET", path, &[]);
    let text = String::from_utf8(body).expect("SSE UTF-8");
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| {
            let message = RpcMessage::decode(line.as_bytes()).expect("SSE event message");
            let (channel, rpc_id, payload) = parse_event_message(&message).expect("event frame");
            assert_eq!(channel, expected_channel);
            EventFrame {
                rpc_id: rpc_id.as_str().to_string(),
                payload: payload.clone(),
            }
        })
        .collect()
}

fn post_json(address: SocketAddr, path: &str, body: Vec<u8>) -> Value {
    serde_json::from_slice(&request(address, "POST", path, &body)).expect("HTTP JSON")
}

fn request(address: SocketAddr, method: &str, path: &str, body: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).expect("connect Web command");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write HTTP request headers");
    stream.write_all(body).expect("write HTTP request body");
    stream
        .shutdown(Shutdown::Write)
        .expect("finish HTTP request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read HTTP response");
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response separator");
    let headers = String::from_utf8_lossy(&response[..separator]);
    assert!(
        headers.starts_with("HTTP/1.1 200 OK"),
        "HTTP response: {headers}"
    );
    response[separator + 4..].to_vec()
}

fn accept_request(listener: &TcpListener) -> (TcpStream, String) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let (stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "model fixture received no request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("model fixture accept failed: {error}"),
        }
    };
    stream
        .set_nonblocking(false)
        .expect("make accepted model stream blocking");
    let mut reader = BufReader::new(stream.try_clone().expect("clone model stream"));
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read model request line");
    assert!(request_line.starts_with("POST /v1/chat/completions "));

    let mut content_length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read model header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse::<usize>().expect("content length"));
            }
        }
    }
    let mut body = vec![0; content_length.expect("model content length")];
    reader
        .read_exact(&mut body)
        .expect("read model request body");
    (
        stream,
        String::from_utf8(body).expect("model request UTF-8"),
    )
}

fn write_response(mut stream: TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("write model response");
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()))
}

fn remove_workspace(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error)
                if error.kind() != std::io::ErrorKind::NotFound && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("remove Web workspace: {error}"),
        }
    }
}
