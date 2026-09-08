//! Host process entry point for the configured Weixin channel runtime.
//!
//! The process keeps credentials and blocking iLink I/O outside the Agent
//! kernel. Production is selected only by an explicit environment mode. A bad
//! configuration falls back to a deterministic control plane while retaining
//! the same external-risk manifest, so text chat and healthy plugins remain
//! available.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement, HostMessage,
    InvocationCodecError, InvocationRequest, InvocationResponse, PluginManifest, PluginMessage,
    PluginRiskLevel, PluginRuntimeMetadata, ProtocolError, capabilities,
    connect_plugin_with_manifest,
};

use crate::plugin::{ChannelRuntime, dispatch_with_mode};
use crate::{
    DoctorReport, EmptyRequest, FileSecretStore, IlinkError, IlinkHttpConfig, IlinkHttpTransport,
    IlinkMessage, IlinkTransport, LoginOptions, LoginReport, LogoutReport, LongPollError,
    LongPollOptions, LongPollSnapshot, MemorySecretStore, PairAction, PairReport, ParticipantId,
    PolledBatch, QrChallenge, QrPoll, QueuedMessageSnapshot, RemoteCommand, RequestContext,
    SecretMaterial, SecretRef, SecretStore, SendResult, ServeOptions, ServeReport, SessionCommand,
    SessionId, SessionReport, StatusReport, WEIXIN_PLUGIN_ID, WeixinControlPlane,
    WeixinPluginError, WeixinPluginResponse, WeixinRuntimeError,
};

pub const WEIXIN_MODE_ENV: &str = "YUNXI_WEIXIN_MODE";
pub const WEIXIN_ACCOUNT_ENV: &str = "YUNXI_WEIXIN_ACCOUNT";
pub const WEIXIN_MASTER_KEY_HEX_ENV: &str = "YUNXI_WEIXIN_MASTER_KEY_HEX";
pub const WEIXIN_MASTER_KEY_ENV: &str = "YUNXI_WEIXIN_MASTER_KEY";
pub const WEIXIN_SECRET_STORE_ENV: &str = "YUNXI_WEIXIN_SECRET_STORE";
pub const WEIXIN_TOKEN_REF_ENV: &str = "YUNXI_WEIXIN_TOKEN_REF";

pub const STATUS_OPERATION: &str = "status";
pub const DOCTOR_OPERATION: &str = "doctor";
pub const LOGIN_OPERATION: &str = "login";
pub const POLL_LOGIN_OPERATION: &str = "poll_login";
pub const SERVE_OPERATION: &str = "serve";
pub const SERVE_START_OPERATION: &str = "serve_start";
pub const SERVE_STATUS_OPERATION: &str = "serve_status";
pub const SERVE_STOP_OPERATION: &str = "serve_stop";
pub const QUEUED_MESSAGES_OPERATION: &str = "queued_messages";
pub const SEND_MESSAGE_OPERATION: &str = "send_message";
pub const REPLY_TEXT_OPERATION: &str = "reply_text";
pub const REMOTE_CONTROL_OPERATION: &str = "remote_control";
pub const PAIR_OPERATION: &str = "pair";
pub const SESSION_OPERATION: &str = "session";
pub const LOGOUT_OPERATION: &str = "logout";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PRODUCTION_OPERATION_TIMEOUT: Duration = Duration::from_secs(125);
const FALLBACK_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_QUEUED_MESSAGES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeixinPollLoginRequest {
    #[serde(default)]
    pub verify_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeixinServeRequest {
    #[serde(default)]
    pub max_polls: Option<usize>,
    #[serde(default)]
    pub max_messages_per_poll: Option<usize>,
    #[serde(default)]
    pub require_approval: bool,
}

impl WeixinServeRequest {
    fn options(&self) -> ServeOptions {
        let defaults = ServeOptions::default();
        ServeOptions {
            max_polls: self.max_polls.unwrap_or(defaults.max_polls),
            max_messages_per_poll: self
                .max_messages_per_poll
                .unwrap_or(defaults.max_messages_per_poll),
            require_approval: self.require_approval,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeixinQueuedMessagesRequest {
    #[serde(default)]
    pub maximum: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeixinSendMessageRequest {
    pub message: IlinkMessage,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeixinReplyTextRequest {
    pub idempotency_key: String,
    pub reply_message_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WeixinPairRequest {
    Request { peer_id: String },
    Approve { request_id: String },
    Deny { request_id: String, reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WeixinSessionRequest {
    Bind { session_id: String, peer_id: String },
    Unbind { session_id: String },
    List,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WeixinRemoteControlRequest {
    Acknowledge {
        idempotency_key: String,
    },
    Cancel {
        idempotency_key: String,
        reason: String,
    },
    RequestApproval {
        idempotency_key: String,
    },
    Approve {
        idempotency_key: String,
    },
    Deny {
        idempotency_key: String,
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WeixinServeSnapshot {
    pub report: ServeReport,
    pub messages: Vec<QueuedMessageSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WeixinServeLifecycleState {
    Stopped,
    Running,
    CancellationRequested,
    Completed,
    Failed,
}

/// A bounded, secret-free projection of the asynchronous poll lifecycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WeixinServeLifecycleSnapshot {
    pub generation: u64,
    pub state: WeixinServeLifecycleState,
    pub worker: Option<LongPollSnapshot>,
    pub report: ServeReport,
    pub imported_batches: usize,
    pub messages: Vec<QueuedMessageSnapshot>,
    pub last_error: Option<String>,
}

trait PollWorkerHandle {
    fn try_next_batch(&mut self) -> Option<PolledBatch>;
    fn snapshot(&self) -> LongPollSnapshot;
    fn cancel(&self);
    fn try_finish(&mut self) -> Result<Option<Result<ServeReport, String>>, String>;
}

impl PollWorkerHandle for crate::LongPollWorker<Box<dyn crate::IlinkTransport>> {
    fn try_next_batch(&mut self) -> Option<PolledBatch> {
        crate::LongPollWorker::try_next_batch(self)
    }

    fn snapshot(&self) -> LongPollSnapshot {
        crate::LongPollWorker::snapshot(self)
    }

    fn cancel(&self) {
        crate::LongPollWorker::cancel(self);
    }

    fn try_finish(&mut self) -> Result<Option<Result<ServeReport, String>>, String> {
        crate::LongPollWorker::try_finish(self)
            .map_err(|error| error.to_string())
            .map(|completion| {
                completion.map(|completion| completion.report.map_err(|error| error.to_string()))
            })
    }
}

struct ActiveServe {
    generation: u64,
    worker: Option<Box<dyn PollWorkerHandle>>,
    options: ServeOptions,
    state: WeixinServeLifecycleState,
    worker_snapshot: Option<LongPollSnapshot>,
    report: ServeReport,
    imported_batches: usize,
    last_error: Option<String>,
    stop_requested: bool,
}

impl ActiveServe {
    fn snapshot(&self, messages: Vec<QueuedMessageSnapshot>) -> WeixinServeLifecycleSnapshot {
        WeixinServeLifecycleSnapshot {
            generation: self.generation,
            state: self.state,
            worker: self.worker_snapshot.clone(),
            report: self.report.clone(),
            imported_batches: self.imported_batches,
            messages,
            last_error: self.last_error.clone(),
        }
    }
}

/// Returns whether the operator explicitly selected the real iLink boundary.
pub fn weixin_production_requested() -> bool {
    std::env::var(WEIXIN_MODE_ENV)
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case("production"))
}

/// Starts the built-in Host plugin, selecting the real iLink transport only
/// when the complete production configuration is valid.
pub fn run_weixin_plugin_from_env() -> Result<(), WeixinProcessPluginError> {
    if !weixin_production_requested() {
        // The dedicated fixture binary keeps the old contract for focused
        // protocol tests.  The Host-installed default, however, must expose
        // the same lifecycle operations as production so Web/CLI callers do
        // not silently fall back to an operation-less stub.
        return ConfiguredRuntime::deterministic_loopback()?.run();
    }

    let runtime = ConfiguredRuntime::production_from_environment()
        .or_else(|_| ConfiguredRuntime::deterministic_fallback())?;
    runtime.run()
}

struct ConfiguredRuntime {
    channel: ChannelRuntime,
    control: Box<dyn ControlPlaneBackend>,
    serve: Option<ActiveServe>,
    next_serve_generation: u64,
    token_ref: SecretRef,
    mode: &'static str,
    adapter: &'static str,
    operation_timeout: Duration,
}

impl ConfiguredRuntime {
    fn production_from_environment() -> Result<Self, WeixinProcessPluginError> {
        let account = std::env::var(WEIXIN_ACCOUNT_ENV).unwrap_or_else(|_| "default".to_owned());
        let key_text = std::env::var(WEIXIN_MASTER_KEY_HEX_ENV)
            .or_else(|_| std::env::var(WEIXIN_MASTER_KEY_ENV))
            .map_err(|_| WeixinProcessPluginError::Configuration("master key is missing"))?;
        let mut master_key = parse_master_key(&key_text)?;
        let secret_path = std::env::var_os(WEIXIN_SECRET_STORE_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .ok_or(WeixinProcessPluginError::Configuration(
                "secret store path is missing",
            ))?;
        let token_ref = SecretRef::new(
            std::env::var(WEIXIN_TOKEN_REF_ENV)
                .unwrap_or_else(|_| format!("host:weixin/{account}/token")),
        )
        .map_err(|_| WeixinProcessPluginError::Configuration("token reference is invalid"))?;
        let store = FileSecretStore::new(&secret_path, master_key)
            .map_err(WeixinProcessPluginError::Secret);
        master_key.fill(0);
        let store = store?;
        let transport = IlinkHttpTransport::new(
            IlinkHttpConfig::production(account.clone())?
                .with_token_ref(token_ref.clone())
                .with_request_timeout(PRODUCTION_OPERATION_TIMEOUT)?,
            &store,
        )?;
        let plane = WeixinControlPlane::new_with_token_ref(
            account,
            transport,
            store,
            Some(token_ref.clone()),
        )?;
        Ok(Self {
            channel: ChannelRuntime::default(),
            control: Box::new(plane),
            serve: None,
            next_serve_generation: 1,
            token_ref,
            mode: "production",
            adapter: "weixin-ilink",
            operation_timeout: PRODUCTION_OPERATION_TIMEOUT,
        })
    }

    fn deterministic_loopback() -> Result<Self, WeixinProcessPluginError> {
        Self::deterministic("loopback", "weixin-ilink-loopback", "loopback")
    }

    fn deterministic_fallback() -> Result<Self, WeixinProcessPluginError> {
        Self::deterministic(
            "production-unavailable",
            "weixin-ilink-fallback",
            "fallback",
        )
    }

    fn deterministic(
        mode: &'static str,
        adapter: &'static str,
        default_account: &'static str,
    ) -> Result<Self, WeixinProcessPluginError> {
        let account = std::env::var(WEIXIN_ACCOUNT_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default_account.to_owned());
        let token_ref = SecretRef::new(format!("host:weixin/{account}/token"))
            .map_err(|_| WeixinProcessPluginError::Configuration("fallback token reference"))?;
        let mut transport = crate::LoopbackIlinkTransport::new();
        transport.queue_qr(QrChallenge::new("loopback-qr", "loopback-image")?);
        let token = SecretMaterial::from_text("loopback-token")
            .map_err(|_| WeixinProcessPluginError::Configuration("fallback token is invalid"))?;
        transport.queue_login_poll(Ok(QrPoll::confirmed(token)));
        let plane = WeixinControlPlane::new(account, transport, MemorySecretStore::new())?;
        Ok(Self {
            channel: ChannelRuntime::default(),
            control: Box::new(plane),
            serve: None,
            next_serve_generation: 1,
            token_ref,
            mode,
            adapter,
            operation_timeout: FALLBACK_OPERATION_TIMEOUT,
        })
    }

    fn run(mut self) -> Result<(), WeixinProcessPluginError> {
        let capability = CapabilityDescriptor::new(
            capabilities::CHANNEL_WEIXIN,
            capabilities::CHANNEL_WEIXIN_VERSION,
        )?;
        let manifest = PluginManifest::new(
            WEIXIN_PLUGIN_ID,
            "YunXi Weixin channel runtime",
            env!("CARGO_PKG_VERSION"),
            vec![capability],
        )
        .with_grants(vec![
            GrantRequirement::required(GrantKind::Network),
            GrantRequirement::required(GrantKind::Secret),
        ])
        .with_runtime_metadata(PluginRuntimeMetadata::new(
            self.adapter,
            PluginRiskLevel::External,
        ));
        let mut session = connect_plugin_with_manifest(manifest, CONNECT_TIMEOUT)?;

        loop {
            self.pump_serve();
            match session.receive()? {
                HostMessage::Invoke { request } => {
                    let request_id = request.request_id();
                    match self.dispatch(&request) {
                        Ok(result) => {
                            let response = InvocationResponse::encode(request_id, &result)?;
                            session.send(&PluginMessage::InvocationCompleted { response })?;
                        }
                        Err(error) => {
                            session.send(&PluginMessage::InvocationFailed {
                                request_id,
                                code: error.code().to_owned(),
                                message: error.to_string(),
                                retryable: error.retryable(),
                            })?;
                        }
                    }
                }
                // Blocking iLink calls are bounded by the request deadline.
                // Host timeout recovery terminates only this plugin process.
                HostMessage::Cancel { .. } => {}
                HostMessage::Shutdown => {
                    self.stop_serve();
                    return Ok(());
                }
                HostMessage::Welcome { .. } => {
                    return Err(WeixinProcessPluginError::UnexpectedHostMessage);
                }
            }
        }
    }

    fn dispatch(
        &mut self,
        request: &InvocationRequest,
    ) -> Result<WeixinPluginResponse, WeixinProcessPluginError> {
        if request.capability().id().as_str() != capabilities::CHANNEL_WEIXIN
            || request.capability().version() != capabilities::CHANNEL_WEIXIN_VERSION
        {
            return Err(WeixinProcessPluginError::UnsupportedOperation);
        }
        self.pump_serve();
        if matches!(
            request.operation(),
            crate::INBOUND_OPERATION
                | crate::OUTBOUND_OPERATION
                | crate::FIXTURE_OPERATION
                | crate::ACK_OPERATION
                | crate::CANCEL_OPERATION
                | crate::FAIL_OPERATION
                | crate::DESCRIBE_OPERATION
        ) {
            return dispatch_with_mode(
                &mut self.channel,
                request,
                self.mode,
                self.control.status().real_weixin,
            )
            .map_err(WeixinProcessPluginError::Legacy);
        }

        let context = RequestContext::with_timeout(self.operation_timeout);
        let report = match request.operation() {
            STATUS_OPERATION => {
                decode::<EmptyRequest>(request)?;
                serde_json::to_value(self.control.status())?
            }
            DOCTOR_OPERATION => {
                decode::<EmptyRequest>(request)?;
                serde_json::to_value(self.control.doctor())?
            }
            LOGIN_OPERATION => {
                decode::<EmptyRequest>(request)?;
                serde_json::to_value(
                    self.control
                        .login(LoginOptions::new(self.token_ref.clone()), &context)?,
                )?
            }
            POLL_LOGIN_OPERATION => {
                let input = decode::<WeixinPollLoginRequest>(request)?;
                serde_json::to_value(
                    self.control
                        .poll_login(input.verify_code.as_deref(), &context)?,
                )?
            }
            SERVE_OPERATION => {
                let input = decode::<WeixinServeRequest>(request)?;
                // Keep the established synchronous contract available when no
                // asynchronous worker owns the channel. A running worker is
                // deliberately not raced with the legacy cursor-mutating API.
                if self.serve.as_ref().is_some_and(|active| {
                    matches!(
                        active.state,
                        WeixinServeLifecycleState::Running
                            | WeixinServeLifecycleState::CancellationRequested
                    )
                }) {
                    return Err(WeixinProcessPluginError::Runtime(
                        WeixinRuntimeError::InvalidOptions("serve worker active"),
                    ));
                }
                let report = self.control.serve(input.options(), &context)?;
                let messages = self.control.queued_messages(
                    input
                        .max_messages_per_poll
                        .unwrap_or(crate::MAX_ILINK_MESSAGES.min(DEFAULT_QUEUED_MESSAGES)),
                )?;
                serde_json::to_value(WeixinServeSnapshot { report, messages })?
            }
            SERVE_START_OPERATION => {
                let input = decode::<WeixinServeRequest>(request)?;
                serde_json::to_value(self.start_serve(input)?)?
            }
            SERVE_STATUS_OPERATION => {
                decode::<EmptyRequest>(request)?;
                serde_json::to_value(self.lifecycle_snapshot()?)?
            }
            SERVE_STOP_OPERATION => {
                decode::<EmptyRequest>(request)?;
                self.stop_serve();
                serde_json::to_value(self.lifecycle_snapshot()?)?
            }
            QUEUED_MESSAGES_OPERATION => {
                let input = decode::<WeixinQueuedMessagesRequest>(request)?;
                serde_json::to_value(
                    self.control.queued_messages(
                        input
                            .maximum
                            .unwrap_or(crate::MAX_ILINK_MESSAGES.min(DEFAULT_QUEUED_MESSAGES)),
                    )?,
                )?
            }
            SEND_MESSAGE_OPERATION => {
                let input = decode::<WeixinSendMessageRequest>(request)?;
                let result = self.control.send_message(&input.message, &context)?;
                json!({ "statusCode": result.status_code })
            }
            REPLY_TEXT_OPERATION => {
                let input = decode::<WeixinReplyTextRequest>(request)?;
                let result = self.control.send_reply_text(
                    &input.idempotency_key,
                    input.reply_message_id,
                    input.text,
                    &context,
                )?;
                json!({ "statusCode": result.status_code })
            }
            REMOTE_CONTROL_OPERATION => {
                let input = decode::<WeixinRemoteControlRequest>(request)?;
                serde_json::to_value(self.control.apply_remote_command(input.into())?)?
            }
            PAIR_OPERATION => {
                let input = decode::<WeixinPairRequest>(request)?;
                serde_json::to_value(self.control.pair(input.try_into()?)?)?
            }
            SESSION_OPERATION => {
                let input = decode::<WeixinSessionRequest>(request)?;
                serde_json::to_value(self.control.session(input.try_into()?)?)?
            }
            LOGOUT_OPERATION => {
                decode::<EmptyRequest>(request)?;
                self.stop_serve();
                serde_json::to_value(self.control.logout()?)?
            }
            _ => return Err(WeixinProcessPluginError::UnsupportedOperation),
        };
        Ok(WeixinPluginResponse::Runtime {
            operation: request.operation().to_owned(),
            mode: self.mode.to_owned(),
            report,
        })
    }

    fn pump_serve(&mut self) {
        let Some(mut active) = self.serve.take() else {
            return;
        };
        let Some(mut worker) = active.worker.take() else {
            self.serve = Some(active);
            return;
        };

        while let Some(batch) = worker.try_next_batch() {
            match self.control.accept_polled_batch(batch, &active.options) {
                Ok(report) => {
                    active.report.polls = active.report.polls.saturating_add(report.polls);
                    active.report.received_messages = active
                        .report
                        .received_messages
                        .saturating_add(report.received_messages);
                    active.report.enqueued_messages = active
                        .report
                        .enqueued_messages
                        .saturating_add(report.enqueued_messages);
                    active.report.duplicate_messages = active
                        .report
                        .duplicate_messages
                        .saturating_add(report.duplicate_messages);
                    active.report.cursor = report.cursor;
                    active.imported_batches = active.imported_batches.saturating_add(1);
                }
                Err(error) => {
                    active.state = WeixinServeLifecycleState::Failed;
                    active.last_error = Some(bound_error(error.to_string()));
                    active.stop_requested = true;
                    worker.cancel();
                    break;
                }
            }
        }

        active.worker_snapshot = Some(worker.snapshot());
        match worker.try_finish() {
            Ok(Some(Ok(completion))) => {
                active.report.cursor = completion.cursor;
                active.state = if active.stop_requested {
                    WeixinServeLifecycleState::Stopped
                } else {
                    WeixinServeLifecycleState::Completed
                };
                active.worker = None;
            }
            Ok(Some(Err(error))) => {
                if active.stop_requested {
                    active.state = WeixinServeLifecycleState::Stopped;
                } else {
                    active.state = WeixinServeLifecycleState::Failed;
                    active.last_error = Some(bound_error(error));
                }
                active.worker = None;
            }
            Ok(None) => {
                active.worker = Some(worker);
            }
            Err(error) => {
                active.state = WeixinServeLifecycleState::Failed;
                active.last_error = Some(bound_error(error));
                active.worker = None;
            }
        }
        self.serve = Some(active);
    }

    fn lifecycle_snapshot(
        &mut self,
    ) -> Result<WeixinServeLifecycleSnapshot, WeixinProcessPluginError> {
        self.pump_serve();
        let messages = self.control.queued_messages(DEFAULT_QUEUED_MESSAGES)?;
        Ok(if let Some(active) = self.serve.as_ref() {
            active.snapshot(messages)
        } else {
            WeixinServeLifecycleSnapshot {
                generation: 0,
                state: WeixinServeLifecycleState::Stopped,
                worker: None,
                report: empty_report(&self.control.cursor()),
                imported_batches: 0,
                messages,
                last_error: None,
            }
        })
    }

    fn start_serve(
        &mut self,
        input: WeixinServeRequest,
    ) -> Result<WeixinServeLifecycleSnapshot, WeixinProcessPluginError> {
        self.pump_serve();
        if let Some(active) = self
            .serve
            .as_ref()
            .filter(|active| active.state != WeixinServeLifecycleState::Stopped)
        {
            return Ok(active.snapshot(self.control.queued_messages(DEFAULT_QUEUED_MESSAGES)?));
        }
        let options = input.options();
        options.validate()?;
        let worker = self.control.start_poll_worker(options.clone())?;
        let report = empty_report(&self.control.cursor());
        let generation = self.next_serve_generation;
        self.next_serve_generation = self.next_serve_generation.saturating_add(1);
        self.serve = Some(ActiveServe {
            generation,
            worker: Some(worker),
            options,
            state: WeixinServeLifecycleState::Running,
            worker_snapshot: None,
            report,
            imported_batches: 0,
            last_error: None,
            stop_requested: false,
        });
        self.pump_serve();
        self.lifecycle_snapshot()
    }

    fn stop_serve(&mut self) {
        if let Some(active) = self.serve.as_mut() {
            active.stop_requested = true;
            if let Some(worker) = active.worker.as_ref() {
                worker.cancel();
                if matches!(active.state, WeixinServeLifecycleState::Running) {
                    active.state = WeixinServeLifecycleState::CancellationRequested;
                }
            } else {
                active.state = WeixinServeLifecycleState::Stopped;
            }
        }
        self.pump_serve();
    }
}

trait ControlPlaneBackend {
    fn status(&self) -> StatusReport;
    fn doctor(&self) -> DoctorReport;
    fn login(
        &mut self,
        options: LoginOptions,
        context: &RequestContext,
    ) -> Result<LoginReport, WeixinRuntimeError>;
    fn poll_login(
        &mut self,
        verify_code: Option<&str>,
        context: &RequestContext,
    ) -> Result<LoginReport, WeixinRuntimeError>;
    fn serve(
        &mut self,
        options: ServeOptions,
        context: &RequestContext,
    ) -> Result<ServeReport, WeixinRuntimeError>;
    fn cursor(&self) -> String;
    fn start_poll_worker(
        &self,
        options: ServeOptions,
    ) -> Result<Box<dyn PollWorkerHandle>, WeixinProcessPluginError>;
    fn accept_polled_batch(
        &mut self,
        polled: PolledBatch,
        options: &ServeOptions,
    ) -> Result<ServeReport, WeixinRuntimeError>;
    fn queued_messages(
        &self,
        maximum: usize,
    ) -> Result<Vec<QueuedMessageSnapshot>, WeixinRuntimeError>;
    fn send_message(
        &mut self,
        message: &IlinkMessage,
        context: &RequestContext,
    ) -> Result<SendResult, WeixinRuntimeError>;
    fn send_reply_text(
        &mut self,
        idempotency_key: &str,
        reply_message_id: String,
        text: String,
        context: &RequestContext,
    ) -> Result<SendResult, WeixinRuntimeError>;
    fn apply_remote_command(
        &mut self,
        command: RemoteCommand,
    ) -> Result<crate::ControlResult, WeixinRuntimeError>;
    fn pair(&mut self, action: PairAction) -> Result<PairReport, WeixinRuntimeError>;
    fn session(&mut self, command: SessionCommand) -> Result<SessionReport, WeixinRuntimeError>;
    fn logout(&mut self) -> Result<LogoutReport, WeixinRuntimeError>;
}

impl<T, S> ControlPlaneBackend for WeixinControlPlane<T, S>
where
    T: IlinkTransport + 'static,
    S: SecretStore + 'static,
{
    fn status(&self) -> StatusReport {
        WeixinControlPlane::status(self)
    }

    fn doctor(&self) -> DoctorReport {
        WeixinControlPlane::doctor(self)
    }

    fn login(
        &mut self,
        options: LoginOptions,
        context: &RequestContext,
    ) -> Result<LoginReport, WeixinRuntimeError> {
        WeixinControlPlane::login(self, options, context)
    }

    fn poll_login(
        &mut self,
        verify_code: Option<&str>,
        context: &RequestContext,
    ) -> Result<LoginReport, WeixinRuntimeError> {
        WeixinControlPlane::poll_login_with_verify_code(self, verify_code, context)
    }

    fn serve(
        &mut self,
        options: ServeOptions,
        context: &RequestContext,
    ) -> Result<ServeReport, WeixinRuntimeError> {
        WeixinControlPlane::serve(self, options, context)
    }

    fn cursor(&self) -> String {
        WeixinControlPlane::cursor(self).to_owned()
    }

    fn start_poll_worker(
        &self,
        options: ServeOptions,
    ) -> Result<Box<dyn PollWorkerHandle>, WeixinProcessPluginError> {
        options.validate()?;
        self.ensure_authenticated()?;
        let transport = self.transport().fork_for_worker()?;
        let worker = crate::LongPollWorker::spawn(
            transport,
            self.cursor().to_owned(),
            LongPollOptions::from_serve(&options),
            RequestContext::new(),
        )
        .map_err(WeixinProcessPluginError::Poll)?;
        Ok(Box::new(worker))
    }

    fn accept_polled_batch(
        &mut self,
        polled: PolledBatch,
        options: &ServeOptions,
    ) -> Result<ServeReport, WeixinRuntimeError> {
        WeixinControlPlane::accept_polled_batch(self, polled, options)
    }

    fn queued_messages(
        &self,
        maximum: usize,
    ) -> Result<Vec<QueuedMessageSnapshot>, WeixinRuntimeError> {
        WeixinControlPlane::queued_messages(self, maximum)
    }

    fn send_message(
        &mut self,
        message: &IlinkMessage,
        context: &RequestContext,
    ) -> Result<SendResult, WeixinRuntimeError> {
        WeixinControlPlane::send_message(self, message, context)
    }

    fn send_reply_text(
        &mut self,
        idempotency_key: &str,
        reply_message_id: String,
        text: String,
        context: &RequestContext,
    ) -> Result<SendResult, WeixinRuntimeError> {
        WeixinControlPlane::send_reply_text(self, idempotency_key, reply_message_id, text, context)
    }

    fn apply_remote_command(
        &mut self,
        command: RemoteCommand,
    ) -> Result<crate::ControlResult, WeixinRuntimeError> {
        WeixinControlPlane::apply_remote_command(self, command)
    }

    fn pair(&mut self, action: PairAction) -> Result<PairReport, WeixinRuntimeError> {
        WeixinControlPlane::pair(self, action)
    }

    fn session(&mut self, command: SessionCommand) -> Result<SessionReport, WeixinRuntimeError> {
        WeixinControlPlane::session(self, command)
    }

    fn logout(&mut self) -> Result<LogoutReport, WeixinRuntimeError> {
        WeixinControlPlane::logout(self)
    }
}

impl TryFrom<WeixinPairRequest> for PairAction {
    type Error = WeixinProcessPluginError;

    fn try_from(request: WeixinPairRequest) -> Result<Self, Self::Error> {
        match request {
            WeixinPairRequest::Request { peer_id } => Ok(Self::Request {
                peer_id: ParticipantId::new(peer_id)
                    .map_err(|_| WeixinProcessPluginError::InvalidRequest("peer_id"))?,
            }),
            WeixinPairRequest::Approve { request_id } => Ok(Self::Approve { request_id }),
            WeixinPairRequest::Deny { request_id, reason } => Ok(Self::Deny { request_id, reason }),
        }
    }
}

fn empty_report(cursor: &str) -> ServeReport {
    ServeReport {
        polls: 0,
        received_messages: 0,
        enqueued_messages: 0,
        duplicate_messages: 0,
        cursor: cursor.to_owned(),
    }
}

fn bound_error(error: String) -> String {
    error.chars().take(512).collect()
}

impl TryFrom<WeixinSessionRequest> for SessionCommand {
    type Error = WeixinProcessPluginError;

    fn try_from(request: WeixinSessionRequest) -> Result<Self, Self::Error> {
        match request {
            WeixinSessionRequest::Bind {
                session_id,
                peer_id,
            } => Ok(Self::Bind {
                session_id: SessionId::new(session_id)
                    .map_err(|_| WeixinProcessPluginError::InvalidRequest("session_id"))?,
                peer_id: ParticipantId::new(peer_id)
                    .map_err(|_| WeixinProcessPluginError::InvalidRequest("peer_id"))?,
            }),
            WeixinSessionRequest::Unbind { session_id } => Ok(Self::Unbind {
                session_id: SessionId::new(session_id)
                    .map_err(|_| WeixinProcessPluginError::InvalidRequest("session_id"))?,
            }),
            WeixinSessionRequest::List => Ok(Self::List),
        }
    }
}

impl From<WeixinRemoteControlRequest> for RemoteCommand {
    fn from(request: WeixinRemoteControlRequest) -> Self {
        match request {
            WeixinRemoteControlRequest::Acknowledge { idempotency_key } => {
                Self::Acknowledge { idempotency_key }
            }
            WeixinRemoteControlRequest::Cancel {
                idempotency_key,
                reason,
            } => Self::Cancel {
                idempotency_key,
                reason,
            },
            WeixinRemoteControlRequest::RequestApproval { idempotency_key } => {
                Self::RequestApproval { idempotency_key }
            }
            WeixinRemoteControlRequest::Approve { idempotency_key } => {
                Self::Approve { idempotency_key }
            }
            WeixinRemoteControlRequest::Deny {
                idempotency_key,
                reason,
            } => Self::Deny {
                idempotency_key,
                reason,
            },
        }
    }
}

fn decode<T: for<'de> Deserialize<'de>>(
    request: &InvocationRequest,
) -> Result<T, WeixinProcessPluginError> {
    request
        .decode_payload()
        .map_err(WeixinProcessPluginError::Invocation)
}

fn parse_master_key(
    value: &str,
) -> Result<[u8; crate::MASTER_KEY_BYTES], WeixinProcessPluginError> {
    if value.len() != crate::MASTER_KEY_BYTES * 2 {
        return Err(WeixinProcessPluginError::Configuration(
            "master key must be 32-byte hexadecimal",
        ));
    }
    let mut key = [0_u8; crate::MASTER_KEY_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        key[index] = (hex_digit(pair[0]).ok_or(WeixinProcessPluginError::Configuration(
            "master key is not hexadecimal",
        ))? << 4)
            | hex_digit(pair[1]).ok_or(WeixinProcessPluginError::Configuration(
                "master key is not hexadecimal",
            ))?;
    }
    Ok(key)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug)]
pub enum WeixinProcessPluginError {
    Capability(CapabilityError),
    Configuration(&'static str),
    Ilink(IlinkError),
    InvalidRequest(&'static str),
    Invocation(InvocationCodecError),
    Legacy(WeixinPluginError),
    Poll(LongPollError),
    Protocol(ProtocolError),
    Runtime(WeixinRuntimeError),
    Secret(crate::SecretStoreError),
    Serialization(serde_json::Error),
    UnsupportedOperation,
    UnexpectedHostMessage,
}

impl WeixinProcessPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) | Self::Invocation(_) => "invalid_request",
            Self::Runtime(_) | Self::Ilink(_) | Self::Poll(_) | Self::Secret(_) => {
                "weixin_runtime_error"
            }
            Self::UnsupportedOperation => "unsupported_operation",
            Self::Serialization(_) => "serialization_error",
            Self::Capability(_)
            | Self::Configuration(_)
            | Self::Legacy(_)
            | Self::Protocol(_)
            | Self::UnexpectedHostMessage => "plugin_error",
        }
    }

    fn retryable(&self) -> bool {
        let ilink = match self {
            Self::Ilink(error) => Some(error),
            Self::Runtime(WeixinRuntimeError::Ilink(error)) => Some(error),
            _ => None,
        };
        matches!(
            ilink,
            Some(
                IlinkError::TimedOut
                    | IlinkError::Transport(_)
                    | IlinkError::Unavailable(_)
                    | IlinkError::HttpStatus {
                        status: 429 | 500..=599
                    }
            )
        )
    }
}

impl fmt::Display for WeixinProcessPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => error.fmt(formatter),
            Self::Configuration(message) => {
                write!(formatter, "invalid Weixin configuration: {message}")
            }
            Self::Ilink(error) => error.fmt(formatter),
            Self::InvalidRequest(field) => {
                write!(formatter, "invalid Weixin request field: {field}")
            }
            Self::Invocation(error) => error.fmt(formatter),
            Self::Legacy(error) => error.fmt(formatter),
            Self::Poll(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::Secret(error) => error.fmt(formatter),
            Self::Serialization(_) => formatter.write_str("Weixin response serialization failed"),
            Self::UnsupportedOperation => formatter.write_str("unsupported Weixin operation"),
            Self::UnexpectedHostMessage => formatter.write_str("unexpected Host message"),
        }
    }
}

impl Error for WeixinProcessPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Ilink(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Legacy(error) => Some(error),
            Self::Poll(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Secret(error) => Some(error),
            Self::Serialization(error) => Some(error),
            Self::Configuration(_)
            | Self::InvalidRequest(_)
            | Self::UnsupportedOperation
            | Self::UnexpectedHostMessage => None,
        }
    }
}

impl From<CapabilityError> for WeixinProcessPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<IlinkError> for WeixinProcessPluginError {
    fn from(error: IlinkError) -> Self {
        Self::Ilink(error)
    }
}

impl From<LongPollError> for WeixinProcessPluginError {
    fn from(error: LongPollError) -> Self {
        Self::Poll(error)
    }
}

impl From<InvocationCodecError> for WeixinProcessPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for WeixinProcessPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<WeixinRuntimeError> for WeixinProcessPluginError {
    fn from(error: WeixinRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<serde_json::Error> for WeixinProcessPluginError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_key_parser_accepts_exact_hex_and_rejects_secret_text() {
        assert_eq!(
            parse_master_key(&"01".repeat(crate::MASTER_KEY_BYTES)).unwrap()[0],
            1
        );
        assert!(parse_master_key("not-a-key").is_err());
    }

    #[test]
    fn request_conversions_validate_channel_identifiers() {
        assert!(
            SessionCommand::try_from(WeixinSessionRequest::Bind {
                session_id: "session-1".to_owned(),
                peer_id: "peer-1".to_owned(),
            })
            .is_ok()
        );
        assert!(
            SessionCommand::try_from(WeixinSessionRequest::Bind {
                session_id: "bad session".to_owned(),
                peer_id: "peer-1".to_owned(),
            })
            .is_err()
        );
    }
}
