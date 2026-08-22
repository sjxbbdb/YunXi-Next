#![doc = "Minimal process-isolated plugin kernel for YunXi Next."]
#![forbid(unsafe_code)]

mod error;
mod plugin;
mod runtime;
mod supervision;

pub use error::KernelError;
pub use plugin::{
    PluginCommand, PluginFailure, PluginId, PluginIdError, PluginSnapshot, PluginSpec, PluginState,
};
pub use runtime::{KernelSnapshot, KernelState, YunxiKernel};
