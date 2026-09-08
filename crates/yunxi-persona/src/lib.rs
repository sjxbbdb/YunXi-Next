#![doc = "Isolated persona and memory-context compiler for YunXi Next."]
#![forbid(unsafe_code)]

mod compiler;
mod management;
mod plugin;
mod profile;
mod settings;

pub use compiler::{PersonaCompileError, compile_context};
pub use management::{
    PersonaManagementError, PersonaProfileSummary, PersonaStatus, import_profile,
    import_profile_json_with_grant, list_profiles, list_profiles_with_grant, profile_with_grant,
    reset_with_grant, set_active_profile, set_active_profile_with_grant, set_enabled,
    set_enabled_with_grant, status, status_with_grant,
};
pub use plugin::{PERSONA_PLUGIN_ID, PersonaPluginError, run_persona_plugin};
