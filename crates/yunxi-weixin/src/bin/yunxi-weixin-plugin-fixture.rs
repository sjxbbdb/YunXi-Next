//! Entrypoint for the process-isolated Weixin channel contract fixture.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_weixin::run_weixin_plugin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("yunxi-weixin-plugin-fixture: {error}");
            ExitCode::FAILURE
        }
    }
}
