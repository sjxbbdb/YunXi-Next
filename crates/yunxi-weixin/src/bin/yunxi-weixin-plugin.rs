//! Entrypoint for the environment-selected Weixin Host plugin.

use std::process::ExitCode;

fn main() -> ExitCode {
    match yunxi_weixin::run_weixin_plugin_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("yunxi-weixin-plugin: {error}");
            ExitCode::FAILURE
        }
    }
}
