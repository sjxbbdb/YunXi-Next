#![doc = "Isolated read-only Skills metadata and context capability for YunXi Next."]
#![forbid(unsafe_code)]

mod action;
mod config;
mod discovery;
mod plugin;

pub use action::{
    SkillActionDeclaration, SkillActionError, SkillActionExecution, SkillActionExecutor,
};
pub use config::{
    SKILLS_ACTIONS_ENABLED_ENV, SKILLS_DISABLED_ENV, SKILLS_MODE_ENV, SKILLS_ROOT_ENV,
    SkillsConfig, SkillsConfigError,
};
pub use discovery::DiscoveryError;
pub use plugin::{SKILLS_PLUGIN_ID, SkillsPluginError, run_skills_plugin};
