#![doc = "Isolated persona and memory-context compiler for YunXi Next."]
#![forbid(unsafe_code)]

mod compiler;
mod plugin;
mod profile;
mod settings;

pub use compiler::{PersonaCompileError, compile_context};
pub use plugin::{PERSONA_PLUGIN_ID, PersonaPluginError, run_persona_plugin};
