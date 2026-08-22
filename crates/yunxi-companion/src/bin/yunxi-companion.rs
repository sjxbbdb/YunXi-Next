//! Standalone companion decision plugin executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_companion::run_companion_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("companion plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
