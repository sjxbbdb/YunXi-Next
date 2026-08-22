#![doc = "Minimal process-isolated plugin kernel for YunXi Next."]
#![forbid(unsafe_code)]

mod error;
mod kernel;
mod plugin;
mod supervisor;

pub use error::KernelError;
pub use kernel::{KernelSnapshot, KernelState, YunxiKernel};
pub use plugin::{
    PluginCommand, PluginFailure, PluginId, PluginIdError, PluginSnapshot, PluginSpec, PluginState,
};
