//! Model plugin request loop and per-request API failure containment.

use std::error::Error;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, ChatRequest, ChatResult, GrantKind, GrantRequirement,
    HostMessage, InvocationCodecError, InvocationResponse, MODEL_CHAT_COMPLETE_OPERATION,
    ModelStreamEvent, PluginMessage, PluginSession, ProtocolError, StreamProtocolError,
    capabilities, connect_plugin_with_grants,
};

use crate::{
    ApiError, ChatCompletion, ChatStreamEvent, OpenAiChatClient, ProviderConfig,
    ProviderConfigError, StreamObserverError, StreamOptions,
};

pub const MODEL_PLUGIN_ID: &str = "yunxi.model.openai-compatible";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_POLL: Duration = Duration::from_millis(50);
const CANCEL_GRACE: Duration = Duration::from_millis(300);
const WORKER_QUEUE_CAPACITY: usize = 32;

pub fn run_model_plugin_from_env() -> Result<(), ModelPluginError> {
    let config = ProviderConfig::from_env()?;
    run_model_plugin(config)
}

pub fn run_model_plugin(config: ProviderConfig) -> Result<(), ModelPluginError> {
    let client = Arc::new(OpenAiChatClient::new(config)?);
    let model_chat =
        CapabilityDescriptor::new(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)?;
    let mut session = connect_plugin_with_grants(
        MODEL_PLUGIN_ID,
        "OpenAI-compatible chat model",
        env!("CARGO_PKG_VERSION"),
        vec![model_chat],
        vec![
            GrantRequirement::required(GrantKind::Network),
            GrantRequirement::required(GrantKind::ProviderCredential),
        ],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::MODEL_CHAT
                    || request.capability().version() != capabilities::MODEL_CHAT_VERSION
                    || request.operation() != MODEL_CHAT_COMPLETE_OPERATION
                {
                    session.send(&PluginMessage::InvocationFailed {
                        request_id,
                        code: "unsupported_operation".to_string(),
                        message: format!(
                            "plugin does not support {}@{}:{}",
                            request.capability().id(),
                            request.capability().version(),
                            request.operation()
                        ),
                        retryable: false,
                    })?;
                    continue;
                }

                let chat = match request.decode_payload::<ChatRequest>() {
                    Ok(chat) => chat,
                    Err(error) => {
                        session.send(&PluginMessage::InvocationFailed {
                            request_id,
                            code: "invalid_request".to_string(),
                            message: error.to_string(),
                            retryable: false,
                        })?;
                        continue;
                    }
                };
                match run_model_invocation(&mut session, Arc::clone(&client), request_id, chat)? {
                    InvocationRunResult::Continue => {}
                    InvocationRunResult::Shutdown => return Ok(()),
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Cancel { .. } => {
                // A cancel without an active invocation is stale. The active
                // invocation consumes its own request-scoped control frames.
            }
            HostMessage::Welcome { .. } => {
                return Err(ModelPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

#[derive(Debug)]
enum WorkerMessage {
    Progress(ChatStreamEvent),
    Finished(Result<ChatCompletion, ApiError>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvocationRunResult {
    Continue,
    Shutdown,
}

fn run_model_invocation(
    session: &mut PluginSession,
    client: Arc<OpenAiChatClient>,
    request_id: u64,
    chat: ChatRequest,
) -> Result<InvocationRunResult, ModelPluginError> {
    session.set_timeouts(Some(CONTROL_POLL), Some(Duration::from_secs(10)))?;

    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
    let worker = thread::spawn(move || {
        let result = client.stream_with_tools_cancelable(
            chat.messages(),
            chat.tools(),
            StreamOptions::default(),
            || worker_cancelled.load(Ordering::Acquire),
            |event| {
                sender
                    .send(WorkerMessage::Progress(event))
                    .map_err(|_| StreamObserverError::new("model worker output receiver closed"))
            },
        );
        let _ = sender.send(WorkerMessage::Finished(result));
    });

    let result = drive_model_invocation(session, request_id, cancelled, receiver, worker);
    let reset_result = session.set_timeouts(None, None);
    match (result, reset_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(result), Ok(())) => Ok(result),
    }
}

fn drive_model_invocation(
    session: &mut PluginSession,
    request_id: u64,
    cancelled: Arc<AtomicBool>,
    receiver: Receiver<WorkerMessage>,
    worker: JoinHandle<()>,
) -> Result<InvocationRunResult, ModelPluginError> {
    let mut cancellation_deadline = None;

    loop {
        if cancellation_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(ModelPluginError::Worker(format!(
                "model worker did not stop within {} milliseconds",
                CANCEL_GRACE.as_millis()
            )));
        }

        match session.receive() {
            Ok(HostMessage::Cancel {
                request_id: cancelled_request_id,
            }) if cancelled_request_id == request_id => {
                cancelled.store(true, Ordering::Release);
                cancellation_deadline
                    .get_or_insert_with(|| std::time::Instant::now() + CANCEL_GRACE);
            }
            Ok(HostMessage::Cancel { .. }) => {}
            Ok(HostMessage::Shutdown) => {
                cancelled.store(true, Ordering::Release);
                return Ok(InvocationRunResult::Shutdown);
            }
            Ok(message) => {
                return Err(ModelPluginError::UnexpectedHostMessage(format!(
                    "received {message:?} while invocation {request_id} was active"
                )));
            }
            Err(error) if protocol_read_timeout(&error) => {}
            Err(error) => return Err(error.into()),
        }

        loop {
            match receiver.try_recv() {
                Ok(WorkerMessage::Progress(event)) => {
                    if cancellation_deadline.is_some() {
                        continue;
                    }
                    let event = protocol_stream_event(event).map_err(|error| {
                        ModelPluginError::Worker(format!(
                            "provider produced an invalid stream event: {error}"
                        ))
                    })?;
                    session.send(&PluginMessage::InvocationProgress { request_id, event })?;
                }
                Ok(WorkerMessage::Finished(result)) => {
                    if worker.join().is_err() {
                        return Err(ModelPluginError::Worker(
                            "model worker panicked".to_string(),
                        ));
                    }
                    if cancellation_deadline.is_some() {
                        session.send(&PluginMessage::InvocationFailed {
                            request_id,
                            code: "cancelled".to_string(),
                            message: "invocation cancelled by host".to_string(),
                            retryable: false,
                        })?;
                        return Ok(InvocationRunResult::Continue);
                    }
                    match result {
                        Ok(completion) => {
                            let result = ChatResult::new(
                                completion.content(),
                                completion.finish_reason().map(str::to_string),
                            )
                            .with_tool_calls(completion.tool_calls().to_vec());
                            let response = InvocationResponse::encode(request_id, &result)?;
                            session.send(&PluginMessage::InvocationCompleted { response })?;
                        }
                        Err(error) => {
                            session.send(&PluginMessage::InvocationFailed {
                                request_id,
                                code: error.code().to_string(),
                                message: error.to_string(),
                                retryable: error.retryable(),
                            })?;
                        }
                    }
                    return Ok(InvocationRunResult::Continue);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(ModelPluginError::Worker(
                        "model worker output channel disconnected".to_string(),
                    ));
                }
            }
        }

        if worker.is_finished() {
            return Err(ModelPluginError::Worker(
                "model worker stopped without a completion message".to_string(),
            ));
        }
    }
}

fn protocol_read_timeout(error: &ProtocolError) -> bool {
    matches!(
        error,
        ProtocolError::Io(io_error)
            if matches!(
                io_error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            )
    )
}

fn protocol_stream_event(event: ChatStreamEvent) -> Result<ModelStreamEvent, StreamProtocolError> {
    match event {
        ChatStreamEvent::TextDelta { text } => ModelStreamEvent::text_delta(text),
        ChatStreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments,
        } => ModelStreamEvent::tool_call_delta(index, id, name, arguments),
        ChatStreamEvent::Finished { reason } => ModelStreamEvent::finished(reason),
    }
}

#[derive(Debug)]
pub enum ModelPluginError {
    Config(ProviderConfigError),
    Api(ApiError),
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    Worker(String),
    UnexpectedHostMessage(String),
}

impl fmt::Display for ModelPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "model plugin configuration failed: {error}"),
            Self::Api(error) => write!(formatter, "model plugin HTTP client failed: {error}"),
            Self::Capability(error) => {
                write!(
                    formatter,
                    "model plugin capability declaration failed: {error}"
                )
            }
            Self::Invocation(error) => write!(formatter, "model plugin invocation failed: {error}"),
            Self::Protocol(error) => write!(formatter, "model plugin protocol failed: {error}"),
            Self::Worker(message) => write!(formatter, "model plugin worker failed: {message}"),
            Self::UnexpectedHostMessage(message) => {
                write!(
                    formatter,
                    "model plugin received invalid host message: {message}"
                )
            }
        }
    }
}

impl Error for ModelPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Api(error) => Some(error),
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Worker(_) => None,
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<ProviderConfigError> for ModelPluginError {
    fn from(error: ProviderConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<ApiError> for ModelPluginError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<CapabilityError> for ModelPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for ModelPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for ModelPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
