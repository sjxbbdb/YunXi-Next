//! Standalone entry point for the isolated multi-agent coordinator.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_multi_agent::run_multi_agent_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("multi-agent plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
