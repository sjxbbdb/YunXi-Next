#![doc = "Host-approved, transactional patch application for YunXi Next."]
#![forbid(unsafe_code)]

mod applier;
mod plugin;

pub use applier::{PatchError, apply_patch};
pub use plugin::{PATCH_PLUGIN_ID, PatchPluginError, run_patch_plugin};
