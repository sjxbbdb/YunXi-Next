#![doc = "OpenAI-compatible chat capability plugin for YunXi Next."]
#![forbid(unsafe_code)]

mod client;
mod config;
mod plugin;

pub use client::{ApiError, ChatCompletion, OpenAiChatClient};
pub use config::{ProviderConfig, ProviderConfigError};
pub use plugin::{MODEL_PLUGIN_ID, ModelPluginError, run_model_plugin, run_model_plugin_from_env};
