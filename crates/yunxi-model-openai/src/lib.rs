#![doc = "OpenAI-compatible chat capability plugin for YunXi Next."]
#![forbid(unsafe_code)]

mod client;
mod config;
mod plugin;
mod streaming;

pub use client::{ApiError, ChatCompletion, OpenAiChatClient};
pub use config::{ProviderConfig, ProviderConfigError};
pub use plugin::{MODEL_PLUGIN_ID, ModelPluginError, run_model_plugin, run_model_plugin_from_env};
pub use streaming::{
    ChatStreamEvent, MAX_STREAM_DELTA_BYTES, MAX_STREAM_EVENTS, MAX_STREAM_LINE_BYTES,
    MAX_STREAM_RESPONSE_BYTES, MAX_STREAM_TOOL_CALLS, StreamObserverError, StreamOptions,
    StreamOptionsError, atomic_cancellation,
};
