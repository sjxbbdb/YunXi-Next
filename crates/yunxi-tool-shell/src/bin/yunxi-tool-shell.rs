//! Standalone host-approved shell plugin executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_tool_shell::run_shell_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shell plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
