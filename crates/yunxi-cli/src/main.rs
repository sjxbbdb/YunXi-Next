//! Executable entry point and private child-process mode switch.

use std::env;
use std::ffi::OsStr;
use std::process::ExitCode;

use yunxi_cli::{
    INTERNAL_COMPANION_PLUGIN_ARGUMENT, INTERNAL_CONTEXT_PLUGIN_ARGUMENT,
    INTERNAL_MAILBOX_PLUGIN_ARGUMENT, INTERNAL_MEMORY_PLUGIN_ARGUMENT,
    INTERNAL_MODEL_PLUGIN_ARGUMENT, INTERNAL_PATCH_PLUGIN_ARGUMENT,
    INTERNAL_PERSONA_PLUGIN_ARGUMENT, INTERNAL_SCHEDULER_PLUGIN_ARGUMENT,
    INTERNAL_SHELL_PLUGIN_ARGUMENT, INTERNAL_STORAGE_PLUGIN_ARGUMENT,
};

fn main() -> ExitCode {
    match env::args_os().nth(1).as_deref() {
        Some(argument) if argument == OsStr::new(INTERNAL_MODEL_PLUGIN_ARGUMENT) => {
            return plugin_exit("model", yunxi_model_openai::run_model_plugin_from_env());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_COMPANION_PLUGIN_ARGUMENT) => {
            return plugin_exit("companion", yunxi_companion::run_companion_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_MAILBOX_PLUGIN_ARGUMENT) => {
            return plugin_exit("mailbox", yunxi_companion_mailbox::run_mailbox_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_CONTEXT_PLUGIN_ARGUMENT) => {
            return plugin_exit("context", yunxi_context::run_context_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_MEMORY_PLUGIN_ARGUMENT) => {
            return plugin_exit("memory", yunxi_memory::run_memory_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_PERSONA_PLUGIN_ARGUMENT) => {
            return plugin_exit("persona", yunxi_persona::run_persona_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_SCHEDULER_PLUGIN_ARGUMENT) => {
            return plugin_exit("scheduler", yunxi_scheduler::run_scheduler_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_STORAGE_PLUGIN_ARGUMENT) => {
            return plugin_exit("storage", yunxi_storage::run_storage_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_SHELL_PLUGIN_ARGUMENT) => {
            return plugin_exit("shell", yunxi_tool_shell::run_shell_plugin());
        }
        Some(argument) if argument == OsStr::new(INTERNAL_PATCH_PLUGIN_ARGUMENT) => {
            return plugin_exit("patch", yunxi_tool_patch::run_patch_plugin());
        }
        _ => {}
    }

    match yunxi_cli::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn plugin_exit<E>(name: &str, result: Result<(), E>) -> ExitCode
where
    E: std::fmt::Display,
{
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name} plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}
