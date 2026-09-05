#![doc = "A bounded, synchronous Cordis meta-runtime for YunXi Next."]
#![forbid(unsafe_code)]

mod error;
mod manifest;
mod registry;
mod runtime;
mod snapshot;

pub use error::RuntimeError;
pub use manifest::{
    DefaultEnablement, MAX_DISPLAY_NAME_BYTES, MAX_PLUGIN_ID_BYTES, ManifestError, PluginManifest,
    PluginRisk, PluginRole,
};
pub use registry::{
    MAX_STATIC_PLUGINS, PluginDefinition, PluginFactory, PluginFactoryFn, PluginRegistry,
};
pub use runtime::CordisRuntime;
pub use snapshot::{
    FailureInfo, FailurePhase, PluginRuntimeState, PluginSnapshot, RuntimeSnapshot, StartupFailure,
    StartupReport,
};

pub use yunxi_cordis_core::{
    Context, CordisError, Fiber, FiberId, FiberSnapshot, FiberState, Plugin, PluginId, ScopeId,
};
