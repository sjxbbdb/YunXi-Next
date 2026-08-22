//! Executable entry point and private child-process mode switch.

use std::env;
use std::ffi::OsStr;
use std::process::ExitCode;

use yunxi_cli::INTERNAL_MODEL_PLUGIN_ARGUMENT;

fn main() -> ExitCode {
    if env::args_os().nth(1).as_deref() == Some(OsStr::new(INTERNAL_MODEL_PLUGIN_ARGUMENT)) {
        return match yunxi_model_openai::run_model_plugin_from_env() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("model plugin failed: {error}");
                ExitCode::FAILURE
            }
        };
    }

    match yunxi_cli::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
