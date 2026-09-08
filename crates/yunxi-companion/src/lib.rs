#![doc = "Deterministic companion response policy for YunXi Next."]
#![forbid(unsafe_code)]

mod decision;
mod management;
mod plugin;

pub use decision::decide;
pub use management::{
    CompanionCheckResult, CompanionClearResult, CompanionHistoryRecord, CompanionHistoryResult,
    CompanionManagementError, CompanionStatus, check_with_grant, clear_with_grant, enabled,
    history_with_grant, set_enabled, set_enabled_with_grant, status, status_with_grant,
};
pub use plugin::{COMPANION_PLUGIN_ID, CompanionPluginError, run_companion_plugin};
