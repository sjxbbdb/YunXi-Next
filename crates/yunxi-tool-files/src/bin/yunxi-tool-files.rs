//! Standalone read-only file tool plugin executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_tool_files::run_file_tool_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("file tool plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
