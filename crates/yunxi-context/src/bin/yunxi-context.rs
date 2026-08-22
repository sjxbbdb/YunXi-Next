//! Standalone entry point for the context plugin process.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_context::run_context_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("context plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
