//! Standalone entry point for the isolated Skills plugin.

fn main() -> std::process::ExitCode {
    match yunxi_tool_skills::run_skills_plugin() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("skills plugin failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
