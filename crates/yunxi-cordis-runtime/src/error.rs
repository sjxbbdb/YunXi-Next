//! Structured errors returned by the meta-runtime.

use std::error::Error;
use std::fmt;

use yunxi_cordis_core::CordisError;

use crate::manifest::{ManifestError, PluginRole};
use crate::snapshot::PluginRuntimeState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    RegistryTooLarge {
        count: usize,
        maximum: usize,
    },
    DuplicatePluginRegistration {
        plugin_id: String,
    },
    InvalidManifest {
        plugin_id: String,
        source: ManifestError,
    },
    UnknownPlugin {
        plugin_id: String,
    },
    RuntimeClosed {
        operation: &'static str,
    },
    CorePluginCannotDisable {
        plugin_id: String,
        role: PluginRole,
    },
    InvalidPluginState {
        plugin_id: String,
        state: PluginRuntimeState,
        operation: &'static str,
    },
    PluginFactoryPanicked {
        plugin_id: String,
    },
    PluginIdentityPanicked {
        plugin_id: String,
    },
    PluginIdentityMismatch {
        expected: String,
        actual: String,
    },
    PluginMountFailed {
        plugin_id: String,
        cause: CordisError,
    },
    PluginUnmountFailed {
        plugin_id: String,
        cause: CordisError,
    },
    RootContextFailed(CordisError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegistryTooLarge { count, maximum } => {
                write!(
                    formatter,
                    "static plugin registry has {count} entries; maximum is {maximum}"
                )
            }
            Self::DuplicatePluginRegistration { plugin_id } => {
                write!(
                    formatter,
                    "plugin `{plugin_id}` is registered more than once"
                )
            }
            Self::InvalidManifest { plugin_id, source } => {
                write!(formatter, "manifest for `{plugin_id}` is invalid: {source}")
            }
            Self::UnknownPlugin { plugin_id } => {
                write!(
                    formatter,
                    "plugin `{plugin_id}` is not in the static registry"
                )
            }
            Self::RuntimeClosed { operation } => {
                write!(formatter, "cannot {operation}: Cordis runtime is closed")
            }
            Self::CorePluginCannotDisable { plugin_id, role } => write!(
                formatter,
                "cannot disable {role} plugin `{plugin_id}` while the runtime is running"
            ),
            Self::InvalidPluginState {
                plugin_id,
                state,
                operation,
            } => write!(
                formatter,
                "plugin `{plugin_id}` cannot {operation} while it is {state}"
            ),
            Self::PluginFactoryPanicked { plugin_id } => {
                write!(formatter, "factory for plugin `{plugin_id}` panicked")
            }
            Self::PluginIdentityPanicked { plugin_id } => {
                write!(
                    formatter,
                    "identity lookup for plugin `{plugin_id}` panicked"
                )
            }
            Self::PluginIdentityMismatch { expected, actual } => write!(
                formatter,
                "plugin factory identity mismatch: expected `{expected}`, got `{actual}`"
            ),
            Self::PluginMountFailed { plugin_id, cause } => {
                write!(formatter, "plugin `{plugin_id}` failed to mount: {cause}")
            }
            Self::PluginUnmountFailed { plugin_id, cause } => {
                write!(formatter, "plugin `{plugin_id}` failed to unmount: {cause}")
            }
            Self::RootContextFailed(cause) => {
                write!(formatter, "root context operation failed: {cause}")
            }
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidManifest { source, .. } => Some(source),
            Self::PluginMountFailed { cause, .. }
            | Self::PluginUnmountFailed { cause, .. }
            | Self::RootContextFailed(cause) => Some(cause),
            _ => None,
        }
    }
}

impl From<CordisError> for RuntimeError {
    fn from(error: CordisError) -> Self {
        Self::RootContextFailed(error)
    }
}
