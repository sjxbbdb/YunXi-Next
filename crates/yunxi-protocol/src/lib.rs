#![doc = "Versioned local protocol for isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod handshake;
mod message;
mod transport;

pub use handshake::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, HostPluginSession, PluginAcceptor,
    PluginConnectionInfo, PluginSession, connect_plugin,
};
pub use message::{
    CHAT_CAPABILITY, ChatMessage, ChatRole, HostMessage, PROTOCOL_VERSION, PluginMessage,
};
pub use transport::{DEFAULT_MAX_FRAME_BYTES, JsonLineTransport, ProtocolError};
