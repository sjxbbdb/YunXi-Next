//! Errors shared by the small Cordis runtime.

use std::error::Error;
use std::fmt;

use crate::context::{ScopeId, ScopeState};
use crate::effect::{EffectId, EffectState};
use crate::event::EventError;
use crate::plugin::{FiberId, FiberState, PluginId, PluginIdError};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IdentifierKind {
    Service,
    Event,
}

impl fmt::Display for IdentifierKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service => formatter.write_str("service"),
            Self::Event => formatter.write_str("event"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CordisError {
    InvalidIdentifier {
        kind: IdentifierKind,
        value: String,
    },
    InvalidPluginId(PluginIdError),
    ContextClosed {
        scope_id: ScopeId,
        state: ScopeState,
    },
    DuplicateService {
        scope_id: ScopeId,
        name: String,
    },
    MissingService {
        name: String,
    },
    ServiceTypeMismatch {
        name: String,
        expected: &'static str,
        actual: &'static str,
    },
    MissingDependency {
        plugin: PluginId,
        service: String,
    },
    DependencyLimit {
        plugin: PluginId,
        maximum: usize,
    },
    DuplicatePlugin {
        plugin: PluginId,
    },
    PluginMountFailed {
        plugin: PluginId,
        message: String,
    },
    InvalidFiberState {
        fiber: FiberId,
        state: FiberState,
        operation: &'static str,
    },
    EffectNotFound {
        id: EffectId,
    },
    EffectUnavailable {
        id: EffectId,
        state: EffectState,
    },
    DisposerFailed {
        id: EffectId,
        message: String,
    },
    Event(EventError),
    Poisoned {
        component: &'static str,
    },
}

impl fmt::Display for CordisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier { kind, value } => {
                write!(formatter, "invalid {kind} identifier `{value}`")
            }
            Self::InvalidPluginId(error) => error.fmt(formatter),
            Self::ContextClosed { scope_id, state } => {
                write!(formatter, "scope {scope_id} is {state}")
            }
            Self::DuplicateService { scope_id, name } => {
                write!(
                    formatter,
                    "service `{name}` is already registered in scope {scope_id}"
                )
            }
            Self::MissingService { name } => write!(formatter, "service `{name}` is unavailable"),
            Self::ServiceTypeMismatch {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "service `{name}` has type `{actual}`, requested `{expected}`"
            ),
            Self::MissingDependency { plugin, service } => write!(
                formatter,
                "plugin `{plugin}` requires missing service `{service}`"
            ),
            Self::DependencyLimit { plugin, maximum } => write!(
                formatter,
                "plugin `{plugin}` declares more than the maximum of {maximum} dependencies"
            ),
            Self::DuplicatePlugin { plugin } => {
                write!(formatter, "plugin `{plugin}` is already mounted")
            }
            Self::PluginMountFailed { plugin, message } => {
                write!(formatter, "plugin `{plugin}` failed to mount: {message}")
            }
            Self::InvalidFiberState {
                fiber,
                state,
                operation,
            } => write!(
                formatter,
                "fiber {fiber} cannot {operation} while it is {state}"
            ),
            Self::EffectNotFound { id } => write!(formatter, "effect {id} is not registered"),
            Self::EffectUnavailable { id, state } => {
                write!(
                    formatter,
                    "effect {id} cannot be disposed while it is {state}"
                )
            }
            Self::DisposerFailed { id, message } => {
                write!(formatter, "disposer for effect {id} failed: {message}")
            }
            Self::Event(error) => error.fmt(formatter),
            Self::Poisoned { component } => {
                write!(formatter, "Cordis {component} lock is poisoned")
            }
        }
    }
}

impl Error for CordisError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPluginId(error) => Some(error),
            Self::Event(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EventError> for CordisError {
    fn from(error: EventError) -> Self {
        Self::Event(error)
    }
}
