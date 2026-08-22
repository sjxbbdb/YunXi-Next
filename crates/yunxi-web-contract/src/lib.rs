#![doc = "Bounded Rust wire types for the dsh-compatible YunXi Web contract."]
#![forbid(unsafe_code)]

mod bounds;
mod error;
mod events;
mod rpc;

pub use bounds::{
    MAX_DETAILS_BYTES, MAX_ERROR_MESSAGE_BYTES, MAX_FRAME_BYTES, MAX_METHOD_BYTES,
    MAX_PAYLOAD_BYTES, MAX_RPC_ID_BYTES,
};
pub use error::WebContractError;
pub use events::{
    EVENTS_HOST_METHOD, EVENTS_MUX_METHOD, EventChannel, event_message, parse_event_message,
};
pub use rpc::{
    ClientRequest, ClientResponse, RpcError, RpcId, RpcMessage, RpcResult, ServerRequest,
    ServerResponse,
};
