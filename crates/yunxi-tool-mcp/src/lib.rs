#![doc = "Isolated stdio and HTTP MCP bridge for YunXi Next."]
#![forbid(unsafe_code)]

mod client;
mod config;
mod fixture;
mod http;
mod plugin;

pub use client::{MAX_MCP_FRAME_BYTES, McpClient, McpClientError};
pub use config::{
    ARGS_ENV, CHILD_ENV_ENV, COMMAND_ENV, HTTP_ENDPOINT_ENV, HTTP_HEADERS_ENV, HTTP_SECRETS_ENV,
    McpConfig, McpConfigError, McpTransportKind, NAME_ENV, NETWORK_GRANT_ENV, SECRET_GRANT_ENV,
    SECRET_REFERENCE_PREFIX, TRANSPORT_ENV,
};
pub use fixture::run_fixture;
pub use plugin::{MCP_PLUGIN_ID, McpPluginError, run_mcp_plugin};
