//! Standalone proactive scheduler plugin executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_scheduler::run_scheduler_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("scheduler plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
