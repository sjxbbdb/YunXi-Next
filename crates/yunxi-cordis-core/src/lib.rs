#![doc = "Small, dependency-free Rust Cordis semantics for YunXi Next."]
#![forbid(unsafe_code)]

mod context;
mod effect;
mod error;
mod event;
mod plugin;
mod service;

pub use context::{Context, ContextSnapshot, EffectSnapshot, ScopeId, ScopeState, ServiceSnapshot};
pub use effect::{Effect, EffectId, EffectState};
pub use error::{CordisError, IdentifierKind};
pub use event::{
    BailDecision, BailResult, EventBus, EventBusSnapshot, EventError, EventKey, EventMode,
    EventSnapshot, HandlerError, HandlerId, MAX_EVENT_DEFINITIONS, MAX_EVENT_HANDLERS,
    MAX_PARALLEL_WORKERS, Subscription,
};
pub use plugin::{Fiber, FiberId, FiberSnapshot, FiberState, Plugin, PluginId, PluginIdError};
pub use service::{MAX_PLUGIN_DEPENDENCIES, ServiceDependency, ServiceKey};
