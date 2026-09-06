#![doc = "Capability catalog for process-isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod catalog;
mod discovery;
mod manager;
mod retry;
mod runtime;

pub use catalog::{CapabilityCatalog, CatalogError, PluginRecord};
pub use discovery::{
    DiscoveredPlugin, DiscoveryError, DiscoveryFailure, DiscoveryLimits, DiscoveryReport,
    ExecutableDefinition, MAX_DISCOVERY_PLUGINS, MAX_EXECUTABLE_ARG_BYTES, MAX_EXECUTABLE_ARGS,
    MAX_MANIFEST_DEPENDENCIES, MAX_MANIFEST_FILE_BYTES, MAX_PLUGIN_PATH_BYTES,
    MAX_PLUGIN_VERSION_BYTES, PLUGIN_MANIFEST_FILE, PLUGIN_MANIFEST_SCHEMA_VERSION,
    PluginDependency, PluginDirectory, PluginDiscovery, PluginPackageManifest, PluginVersion,
    VersionError,
};
pub use manager::{
    MAX_DYNAMIC_PLUGIN_DIAGNOSTICS, PluginDiscoveryManager, PluginDiscoveryManagerError,
    PluginLifecycleFailure, PluginReloadReport,
};
pub use retry::{
    DEFAULT_MAX_AUTOMATIC_RESTARTS, MAX_AUTOMATIC_RESTARTS, RetryAction, RetryController,
    RetryPolicy, RetrySnapshot,
};
pub use runtime::{PluginCallError, PluginHostError, PluginLaunch, ProcessPluginHost};
