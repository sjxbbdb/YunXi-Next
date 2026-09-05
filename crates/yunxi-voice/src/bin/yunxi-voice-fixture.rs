//! Standalone process entry point for the voice contract fixture.

fn main() -> std::process::ExitCode {
    match yunxi_voice::run_voice_fixture() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("voice fixture failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
