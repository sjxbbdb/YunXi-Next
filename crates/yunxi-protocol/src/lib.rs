#![doc = "Versioned local protocol for isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod capability;
mod handshake;
mod invocation;
mod message;
mod transport;

pub use capability::{
    CapabilityDescriptor, CapabilityError, CapabilityId, CapabilityIdError, capabilities,
};
pub use handshake::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, HostPluginSession, PluginAcceptor,
    PluginConnectionInfo, PluginSession, connect_plugin,
};
pub use invocation::{InvocationCodecError, InvocationRequest, InvocationResponse};
pub use message::{
    ChatMessage, ChatRequest, ChatResult, ChatRole, HostMessage, MODEL_CHAT_COMPLETE_OPERATION,
    PROTOCOL_VERSION, PluginMessage,
};
pub use transport::{DEFAULT_MAX_FRAME_BYTES, JsonLineTransport, ProtocolError};
