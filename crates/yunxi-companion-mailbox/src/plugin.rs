//! Plugin handshake and dispatch for encrypted mailbox operations.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    COMPANION_MAILBOX_ENQUEUE_OPERATION, COMPANION_MAILBOX_GET_OPERATION,
    COMPANION_MAILBOX_LIST_OPERATION, COMPANION_MAILBOX_MARK_READ_OPERATION, CapabilityDescriptor,
    CapabilityError, HostMessage, InvocationCodecError, InvocationResponse, MailboxEnqueueRequest,
    MailboxGetRequest, MailboxListRequest, MailboxMarkReadRequest, PluginMessage, ProtocolError,
    capabilities, connect_plugin,
};

use crate::{MailboxError, MailboxStore};

pub const MAILBOX_PLUGIN_ID: &str = "yunxi.companion-mailbox";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_mailbox_plugin() -> Result<(), MailboxPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::COMPANION_MAILBOX,
        capabilities::COMPANION_MAILBOX_VERSION,
    )?;
    let mut session = connect_plugin(
        MAILBOX_PLUGIN_ID,
        "Encrypted companion mailbox",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;
    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::COMPANION_MAILBOX
                    || request.capability().version() != capabilities::COMPANION_MAILBOX_VERSION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_capability",
                        "mailbox plugin does not support the requested capability".to_string(),
                    )?;
                    continue;
                }
                let response = match request.operation() {
                    COMPANION_MAILBOX_ENQUEUE_OPERATION => request
                        .decode_payload::<MailboxEnqueueRequest>()
                        .map_err(MailboxPluginError::Invocation)
                        .and_then(|payload| {
                            MailboxStore::from_grant(payload.grant())
                                .and_then(|store| store.enqueue(&payload))
                                .map_err(MailboxPluginError::Mailbox)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    COMPANION_MAILBOX_LIST_OPERATION => request
                        .decode_payload::<MailboxListRequest>()
                        .map_err(MailboxPluginError::Invocation)
                        .and_then(|payload| {
                            MailboxStore::from_grant(payload.grant())
                                .and_then(|store| store.list(&payload))
                                .map_err(MailboxPluginError::Mailbox)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    COMPANION_MAILBOX_GET_OPERATION => request
                        .decode_payload::<MailboxGetRequest>()
                        .map_err(MailboxPluginError::Invocation)
                        .and_then(|payload| {
                            MailboxStore::from_grant(payload.grant())
                                .and_then(|store| store.get(&payload))
                                .map_err(MailboxPluginError::Mailbox)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    COMPANION_MAILBOX_MARK_READ_OPERATION => request
                        .decode_payload::<MailboxMarkReadRequest>()
                        .map_err(MailboxPluginError::Invocation)
                        .and_then(|payload| {
                            MailboxStore::from_grant(payload.grant())
                                .and_then(|store| store.mark_read(&payload))
                                .map_err(MailboxPluginError::Mailbox)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    _ => {
                        send_failure(
                            &mut session,
                            request_id,
                            "unsupported_operation",
                            "mailbox plugin does not support the requested operation".to_string(),
                        )?;
                        continue;
                    }
                };
                match response {
                    Ok(response) => {
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => {
                        send_failure(&mut session, request_id, error.code(), error.to_string())?
                    }
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(MailboxPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn encode<T: serde::Serialize>(
    request_id: u64,
    value: &T,
) -> Result<InvocationResponse, MailboxPluginError> {
    InvocationResponse::encode(request_id, value).map_err(MailboxPluginError::Invocation)
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
pub enum MailboxPluginError {
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Mailbox(MailboxError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl MailboxPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invocation(_) => "invalid_request",
            Self::Mailbox(MailboxError::WriteNotGranted) => "write_not_granted",
            Self::Mailbox(_) => "mailbox_error",
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for MailboxPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Mailbox(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for MailboxPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Mailbox(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for MailboxPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<ProtocolError> for MailboxPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
