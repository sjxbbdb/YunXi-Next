//! Plugin-side handshake and request dispatch for read-only memory recall.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, HostMessage, InvocationCodecError, InvocationResponse,
    MEMORY_RECALL_OPERATION, MemoryRecallRequest, PluginMessage, ProtocolError, capabilities,
    connect_plugin,
};

use crate::{MemoryRecallError, recall};

pub const MEMORY_PLUGIN_ID: &str = "yunxi.memory";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_memory_plugin() -> Result<(), MemoryPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::MEMORY_RECALL,
        capabilities::MEMORY_RECALL_VERSION,
    )?;
    let mut session = connect_plugin(
        MEMORY_PLUGIN_ID,
        "Read-only long-term memory",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::MEMORY_RECALL
                    || request.capability().version() != capabilities::MEMORY_RECALL_VERSION
                    || request.operation() != MEMORY_RECALL_OPERATION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "memory plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let payload = match request.decode_payload::<MemoryRecallRequest>() {
                    Ok(payload) => payload,
                    Err(error) => {
                        send_failure(
                            &mut session,
                            request_id,
                            "invalid_request",
                            error.to_string(),
                        )?;
                        continue;
                    }
                };
                match recall(&payload) {
                    Ok(result) => {
                        let response = InvocationResponse::encode(request_id, &result)?;
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => send_failure(
                        &mut session,
                        request_id,
                        "memory_recall_error",
                        error.to_string(),
                    )?,
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(MemoryPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn send_failure(
    session: &mut yunxi_protocol::PluginSession,
    request_id: u64,
    code: &str,
    message: String,
) -> Result<(), ProtocolError> {
    session.send(&PluginMessage::InvocationFailed {
        request_id,
        code: code.to_string(),
        message,
        retryable: false,
    })
}

#[derive(Debug)]
pub enum MemoryPluginError {
    Capability(CapabilityError),
    Recall(MemoryRecallError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for MemoryPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Recall(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for MemoryPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Recall(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for MemoryPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<MemoryRecallError> for MemoryPluginError {
    fn from(error: MemoryRecallError) -> Self {
        Self::Recall(error)
    }
}

impl From<InvocationCodecError> for MemoryPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for MemoryPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
