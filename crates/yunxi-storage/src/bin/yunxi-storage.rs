//! Standalone session storage plugin executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_storage::run_storage_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("storage plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
