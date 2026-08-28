use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use yunxi_tool_mcp::{McpClient, McpClientError, McpConfig};

fn config(mode: &str) -> McpConfig {
    let command = PathBuf::from(env!("CARGO_BIN_EXE_yunxi-mcp-fixture"));
    let environment = BTreeMap::from([("YUNXI_MCP_FIXTURE_MODE".to_string(), mode.to_string())]);
    McpConfig::new(command, Vec::new(), "fixture", environment).expect("fixture config")
}

#[test]
fn initialize_discover_and_call_follow_mcp_stdio_lifecycle() {
    let mut client = McpClient::start(&config("normal"), Duration::from_secs(2))
        .expect("start fixture MCP server");
    assert_eq!(client.server_name(), "fixture");
    assert_eq!(client.tools().len(), 1);
    assert_eq!(client.tools()[0].name(), "echo");
    let result = client
        .call_tool("echo", json!({"text": "hello"}), Duration::from_secs(2))
        .expect("call fixture tool");
    assert!(!result.is_error());
    assert_eq!(result.tool_name(), "echo");
}

#[test]
fn malformed_json_is_a_fatal_protocol_boundary() {
    let error = McpClient::start(&config("malformed"), Duration::from_secs(2))
        .expect_err("malformed MCP response must fail startup");
    assert!(matches!(error, McpClientError::MalformedJson { .. }));
    assert!(error.is_fatal());
}

#[test]
fn server_crash_is_reported_without_hanging_the_host() {
    let error = McpClient::start(&config("crash"), Duration::from_secs(2))
        .expect_err("crashed MCP server must fail startup");
    assert!(matches!(error, McpClientError::ChildStopped));
    assert!(error.is_fatal());
}
