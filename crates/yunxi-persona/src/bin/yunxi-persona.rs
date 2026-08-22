//! Standalone entry point for the persona context plugin process.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_persona::run_persona_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("persona plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
