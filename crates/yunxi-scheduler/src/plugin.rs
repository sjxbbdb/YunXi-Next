//! Plugin handshake and dispatch for proactive scheduling decisions.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, HostMessage, InvocationCodecError, InvocationResponse,
    PluginMessage, ProactiveSchedulerRequest, ProtocolError,
    SCHEDULER_PROACTIVE_EVALUATE_OPERATION, capabilities, connect_plugin,
};

use crate::evaluate;

pub const SCHEDULER_PLUGIN_ID: &str = "yunxi.scheduler";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_scheduler_plugin() -> Result<(), SchedulerPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::SCHEDULER_PROACTIVE,
        capabilities::SCHEDULER_PROACTIVE_VERSION,
    )?;
    let mut session = connect_plugin(
        SCHEDULER_PLUGIN_ID,
        "Bounded proactive scheduler",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;
    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::SCHEDULER_PROACTIVE
                    || request.capability().version() != capabilities::SCHEDULER_PROACTIVE_VERSION
                    || request.operation() != SCHEDULER_PROACTIVE_EVALUATE_OPERATION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "scheduler plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let payload = match request.decode_payload::<ProactiveSchedulerRequest>() {
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
                let response = InvocationResponse::encode(request_id, &evaluate(&payload))?;
                session.send(&PluginMessage::InvocationCompleted { response })?;
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(SchedulerPluginError::UnexpectedHostMessage(
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
pub enum SchedulerPluginError {
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for SchedulerPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for SchedulerPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for SchedulerPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for SchedulerPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for SchedulerPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
