//! Deterministic MCP stdio Server used by integration tests.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

pub fn run_fixture() {
    let mode = std::env::var("YUNXI_MCP_FIXTURE_MODE").unwrap_or_else(|_| "normal".to_string());
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            return;
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            continue;
        };
        if method == "notifications/initialized" {
            continue;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if mode == "crash" && method == "tools/list" {
            return;
        }
        if mode == "call-crash" && method == "tools/call" {
            return;
        }
        if mode == "malformed" && method == "tools/list" {
            let _ignored = writeln!(stdout, "this is not json");
            let _ignored = stdout.flush();
            return;
        }
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "1.0.0"}
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": "echo",
                        "description": "Return the provided arguments.",
                        "inputSchema": {"type": "object", "additionalProperties": true}
                    }]
                }
            }),
            "tools/call" if mode == "rpc-error" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32001, "message": "fixture rejected the call"}
            }),
            "tools/call" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": request
                        .get("params")
                        .and_then(|params| params.get("arguments"))
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".to_string())}],
                    "isError": false
                }
            }),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not found"}
            }),
        };
        if serde_json::to_writer(&mut stdout, &response).is_err() {
            return;
        }
        if writeln!(stdout).is_err() || stdout.flush().is_err() {
            return;
        }
    }
}
