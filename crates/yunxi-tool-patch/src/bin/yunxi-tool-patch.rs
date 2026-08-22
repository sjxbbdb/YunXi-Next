//! Standalone host-approved patch plugin executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_tool_patch::run_patch_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("patch plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
