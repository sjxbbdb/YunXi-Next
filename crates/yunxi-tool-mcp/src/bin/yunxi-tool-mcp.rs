//! Thin standalone entry point for the isolated MCP bridge.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_tool_mcp::run_mcp_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("MCP plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
