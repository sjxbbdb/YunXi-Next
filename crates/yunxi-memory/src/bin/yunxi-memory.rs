//! Standalone entry point for the read-only memory plugin process.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_memory::run_memory_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("memory plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
