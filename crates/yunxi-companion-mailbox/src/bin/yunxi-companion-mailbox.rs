//! Standalone encrypted companion mailbox plugin executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_companion_mailbox::run_mailbox_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mailbox plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
