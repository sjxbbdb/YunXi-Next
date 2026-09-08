//! Library-only Weixin control plane.
//!
//! This module is the boundary that a later CLI or Web adapter can call. It
//! owns no threads and no runtime: callers decide where blocking iLink work
//! runs. Every loop has an explicit bound, and all persistent state is local to
//! one control-plane instance.

use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use crate::bridge::{
    AgentBridge, AgentBridgeError, AgentBridgeSnapshot, AgentBridgeState, AgentWorkItem,
};
use crate::control::RequestContext;
use crate::ilink::{IlinkError, IlinkMessage, IlinkTransport, PollBatch, QrChallenge, QrStatus};
use crate::poll_worker::PolledBatch;
use crate::secret_store::{SecretStore, SecretStoreError};
use crate::{ParticipantId, SecretMaterial, SecretRef, SessionId};

pub const MAX_QUEUE_ENTRIES: usize = 1024;
pub const MAX_SESSION_BINDINGS: usize = 1024;
pub const MAX_PAIR_REQUESTS: usize = 1024;
pub const MAX_PAIR_REASON_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginState {
    LoggedOut,
    AwaitingQr,
    LoggedIn,
    Loopback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginOptions {
    pub token_ref: SecretRef,
}

impl LoginOptions {
    pub fn new(token_ref: SecretRef) -> Self {
        Self { token_ref }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LoginReport {
    pub state: LoginState,
    pub qrcode: Option<String>,
    pub qr_status: Option<QrStatus>,
    pub credential_stored: bool,
    pub loopback: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StatusReport {
    pub account_alias: String,
    pub state: LoginState,
    pub authenticated: bool,
    pub credential_stored: bool,
    pub loopback: bool,
    pub real_weixin: bool,
    #[serde(rename = "productionReady")]
    pub production_ready: bool,
    pub queued_messages: usize,
    pub outbound_messages: usize,
    pub session_bindings: usize,
    pub pair_requests: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LogoutReport {
    pub state: LoginState,
    pub removed_token: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ServeOptions {
    /// Number of long-poll requests. It is mandatory and bounded to prevent a
    /// library caller from accidentally creating an unowned infinite loop.
    pub max_polls: usize,
    pub max_messages_per_poll: usize,
    pub require_approval: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            max_polls: 1,
            max_messages_per_poll: crate::ilink::MAX_ILINK_MESSAGES,
            require_approval: false,
        }
    }
}

impl ServeOptions {
    pub fn validate(&self) -> Result<(), WeixinRuntimeError> {
        if self.max_polls == 0 || self.max_polls > 10_000 {
            return Err(WeixinRuntimeError::InvalidOptions("max_polls"));
        }
        if self.max_messages_per_poll == 0
            || self.max_messages_per_poll > crate::ilink::MAX_ILINK_MESSAGES
        {
            return Err(WeixinRuntimeError::InvalidOptions("max_messages_per_poll"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ServeReport {
    pub polls: usize,
    pub received_messages: usize,
    pub enqueued_messages: usize,
    pub duplicate_messages: usize,
    pub cursor: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum SessionCommand {
    Bind {
        session_id: SessionId,
        peer_id: ParticipantId,
    },
    Unbind {
        session_id: SessionId,
    },
    List,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionBinding {
    pub session_id: SessionId,
    pub peer_id: ParticipantId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionReport {
    pub changed: bool,
    pub bindings: Vec<SessionBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum PairAction {
    Request { peer_id: ParticipantId },
    Approve { request_id: String },
    Deny { request_id: String, reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairState {
    Pending,
    Approved,
    Denied,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PairRequest {
    pub request_id: String,
    pub peer_id: ParticipantId,
    pub state: PairState,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PairReport {
    pub duplicate: bool,
    pub request: PairRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum RemoteCommand {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteControlState {
    Queued,
    ApprovalRequired,
    Approved,
    Acknowledged,
    Cancelled,
    Denied,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ControlResult {
    pub idempotency_key: String,
    pub state: RemoteControlState,
    pub duplicate: bool,
}

/// Bounded, read-only projection of one message retained by the channel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QueuedMessageSnapshot {
    pub message: IlinkMessage,
    pub state: RemoteControlState,
}

#[derive(Clone)]
struct QueueEntry {
    message: IlinkMessage,
    state: RemoteControlState,
    last_reason: Option<String>,
}

#[derive(Clone, Copy)]
struct OutboundQueueEntry {
    status_code: u16,
}

#[derive(Default)]
struct InboundQueue {
    entries: BTreeMap<String, QueueEntry>,
    order: VecDeque<String>,
}

impl InboundQueue {
    fn enqueue(
        &mut self,
        message: IlinkMessage,
        require_approval: bool,
    ) -> Result<(bool, RemoteControlState), WeixinRuntimeError> {
        let key = message.message_id.clone();
        if let Some(entry) = self.entries.get(&key) {
            if entry.message == message {
                return Ok((true, entry.state));
            }
            return Err(WeixinRuntimeError::IdempotencyConflict);
        }
        if self.entries.len() >= MAX_QUEUE_ENTRIES {
            return Err(WeixinRuntimeError::QueueCapacityExceeded);
        }
        let state = if require_approval {
            RemoteControlState::ApprovalRequired
        } else {
            RemoteControlState::Queued
        };
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            QueueEntry {
                message,
                state,
                last_reason: None,
            },
        );
        Ok((false, state))
    }

    fn apply(&mut self, command: &RemoteCommand) -> Result<ControlResult, WeixinRuntimeError> {
        let (key, next, reason) = command.parts()?;
        let entry = self
            .entries
            .get_mut(&key)
            .ok_or(WeixinRuntimeError::UnknownMessage)?;
        let current = entry.state;
        if current == next {
            if entry.last_reason != reason {
                return Err(WeixinRuntimeError::IdempotencyConflict);
            }
            return Ok(ControlResult {
                idempotency_key: key,
                state: current,
                duplicate: true,
            });
        }
        let allowed = matches!(
            (current, next),
            (RemoteControlState::Queued, RemoteControlState::Acknowledged)
                | (RemoteControlState::Queued, RemoteControlState::Cancelled)
                | (
                    RemoteControlState::Queued,
                    RemoteControlState::ApprovalRequired
                )
                | (
                    RemoteControlState::ApprovalRequired,
                    RemoteControlState::Approved
                )
                | (
                    RemoteControlState::ApprovalRequired,
                    RemoteControlState::Denied
                )
                | (
                    RemoteControlState::ApprovalRequired,
                    RemoteControlState::Cancelled
                )
                | (
                    RemoteControlState::Approved,
                    RemoteControlState::Acknowledged
                )
                | (RemoteControlState::Approved, RemoteControlState::Cancelled)
        );
        if !allowed {
            return Err(WeixinRuntimeError::InvalidTransition {
                from: current,
                to: next,
            });
        }
        entry.state = next;
        entry.last_reason = reason;
        Ok(ControlResult {
            idempotency_key: key,
            state: next,
            duplicate: false,
        })
    }

    fn state(&self, key: &str) -> Option<RemoteControlState> {
        self.entries.get(key).map(|entry| entry.state)
    }
}

impl RemoteCommand {
    fn parts(&self) -> Result<(String, RemoteControlState, Option<String>), WeixinRuntimeError> {
        let (key, state, reason) = match self {
            Self::Acknowledge { idempotency_key } => {
                (idempotency_key, RemoteControlState::Acknowledged, None)
            }
            Self::Cancel {
                idempotency_key,
                reason,
            } => (idempotency_key, RemoteControlState::Cancelled, Some(reason)),
            Self::RequestApproval { idempotency_key } => {
                (idempotency_key, RemoteControlState::ApprovalRequired, None)
            }
            Self::Approve { idempotency_key } => {
                (idempotency_key, RemoteControlState::Approved, None)
            }
            Self::Deny {
                idempotency_key,
                reason,
            } => (idempotency_key, RemoteControlState::Denied, Some(reason)),
        };
        if key.is_empty()
            || key.len() > crate::ilink::MAX_ILINK_ID_BYTES
            || key.chars().any(char::is_control)
        {
            return Err(WeixinRuntimeError::InvalidOptions("idempotency_key"));
        }
        if reason.is_some_and(|reason| {
            reason.is_empty()
                || reason.len() > MAX_PAIR_REASON_BYTES
                || reason.chars().any(char::is_control)
        }) {
            return Err(WeixinRuntimeError::InvalidOptions("reason"));
        }
        Ok((key.clone(), state, reason.cloned()))
    }
}

/// Facade for login, status, doctor, serving, pairing, sessions, and logout.
/// `T` is replaceable, so the same API works with real HTTP and loopback.
pub struct WeixinControlPlane<T, S> {
    account_alias: String,
    transport: T,
    store: S,
    state: LoginState,
    pending_login: Option<PendingLogin>,
    token_ref: Option<SecretRef>,
    cursor: String,
    queue: InboundQueue,
    agent_bridge: AgentBridge,
    outbound: BTreeMap<String, (IlinkMessage, OutboundQueueEntry)>,
    sessions: BTreeMap<SessionId, ParticipantId>,
    pairs: BTreeMap<String, PairRequest>,
    next_pair_id: u64,
}

struct PendingLogin {
    challenge: QrChallenge,
    token_ref: SecretRef,
}

impl<T, S> fmt::Debug for WeixinControlPlane<T, S>
where
    T: fmt::Debug,
    S: fmt::Debug,
    T: IlinkTransport,
    S: SecretStore,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WeixinControlPlane")
            .field("account_alias", &self.account_alias)
            .field("transport", &self.transport)
            .field("store", &self.store)
            .field("state", &self.state)
            .field("authenticated", &self.transport_is_authenticated())
            .field("queued_messages", &self.queue.entries.len())
            .field("outbound_messages", &self.outbound.len())
            .field("session_bindings", &self.sessions.len())
            .finish()
    }
}

impl<T, S> WeixinControlPlane<T, S>
where
    T: IlinkTransport,
    S: SecretStore,
{
    pub fn new(
        account_alias: impl Into<String>,
        transport: T,
        store: S,
    ) -> Result<Self, WeixinRuntimeError> {
        Self::new_with_token_ref(account_alias, transport, store, None)
    }

    /// Builds a control plane while retaining the reference used to load the
    /// transport credential.  This is needed by short-lived CLI invocations:
    /// a later `logout` must be able to remove the same durable secret.
    pub fn new_with_token_ref(
        account_alias: impl Into<String>,
        transport: T,
        store: S,
        token_ref: Option<SecretRef>,
    ) -> Result<Self, WeixinRuntimeError> {
        let account_alias = account_alias.into();
        if account_alias.is_empty()
            || account_alias.len() > crate::ilink::MAX_ILINK_ID_BYTES
            || account_alias.chars().any(char::is_control)
        {
            return Err(WeixinRuntimeError::InvalidOptions("account_alias"));
        }
        let credential_stored = token_ref
            .as_ref()
            .map(|reference| store.contains(reference))
            .transpose()?
            .unwrap_or(false);
        let authenticated = transport.is_authenticated() && credential_stored;
        let state = if authenticated && transport.is_loopback() {
            LoginState::Loopback
        } else if authenticated {
            LoginState::LoggedIn
        } else {
            LoginState::LoggedOut
        };
        Ok(Self {
            account_alias,
            transport,
            store,
            state,
            pending_login: None,
            token_ref: token_ref.filter(|_| credential_stored),
            cursor: String::new(),
            queue: InboundQueue::default(),
            agent_bridge: AgentBridge::new(),
            outbound: BTreeMap::new(),
            sessions: BTreeMap::new(),
            pairs: BTreeMap::new(),
            next_pair_id: 1,
        })
    }

    /// Restores the non-secret part of a pending QR login after a short-lived
    /// process has been restarted.  The QR image is intentionally not stored;
    /// only the bounded challenge identifier is needed by iLink polling.
    pub fn restore_pending_login(
        &mut self,
        qrcode: impl Into<String>,
        token_ref: SecretRef,
    ) -> Result<(), WeixinRuntimeError> {
        let challenge = QrChallenge::new(qrcode, "restored")?;
        self.pending_login = Some(PendingLogin {
            challenge,
            token_ref,
        });
        self.state = LoginState::AwaitingQr;
        Ok(())
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Returns the last committed iLink cursor for a background poll worker.
    pub fn cursor(&self) -> &str {
        &self.cursor
    }

    pub fn secret_store(&self) -> &S {
        &self.store
    }

    pub fn login(
        &mut self,
        options: LoginOptions,
        context: &RequestContext,
    ) -> Result<LoginReport, WeixinRuntimeError> {
        context.check()?;
        let challenge = self.transport.fetch_qr(context)?;
        self.pending_login = Some(PendingLogin {
            challenge: challenge.clone(),
            token_ref: options.token_ref,
        });
        self.state = LoginState::AwaitingQr;
        Ok(LoginReport {
            state: self.state,
            qrcode: Some(challenge.qrcode),
            qr_status: None,
            credential_stored: false,
            loopback: self.transport.is_loopback(),
        })
    }

    pub fn poll_login(
        &mut self,
        context: &RequestContext,
    ) -> Result<LoginReport, WeixinRuntimeError> {
        self.poll_login_with_verify_code(None, context)
    }

    pub fn poll_login_with_verify_code(
        &mut self,
        verify_code: Option<&str>,
        context: &RequestContext,
    ) -> Result<LoginReport, WeixinRuntimeError> {
        let pending = self
            .pending_login
            .as_ref()
            .ok_or(WeixinRuntimeError::NoPendingLogin)?;
        let poll =
            self.transport
                .poll_qr_status(&pending.challenge.qrcode, verify_code, context)?;
        match poll.status {
            QrStatus::Confirmed => {
                let token = poll
                    .auth_token
                    .ok_or(IlinkError::Unavailable("login credential"))?;
                let token_bytes = token.as_bytes().to_vec();
                let token_ref = pending.token_ref.clone();
                self.store.put(
                    token_ref.clone(),
                    SecretMaterial::from_bytes(token_bytes.clone())
                        .map_err(SecretStoreError::Secret)?,
                )?;
                if let Err(error) = self.transport.set_auth_token(
                    SecretMaterial::from_bytes(token_bytes).map_err(SecretStoreError::Secret)?,
                ) {
                    let _ = self.store.remove(&token_ref);
                    self.transport.clear_auth_token();
                    return Err(error.into());
                }
                self.token_ref = Some(token_ref);
                self.pending_login = None;
                self.state = if self.transport.is_loopback() {
                    LoginState::Loopback
                } else {
                    LoginState::LoggedIn
                };
                Ok(LoginReport {
                    state: self.state,
                    qrcode: None,
                    qr_status: Some(poll.status),
                    credential_stored: true,
                    loopback: self.transport.is_loopback(),
                })
            }
            QrStatus::Expired => {
                self.pending_login = None;
                self.state = LoginState::LoggedOut;
                Ok(LoginReport {
                    state: self.state,
                    qrcode: None,
                    qr_status: Some(poll.status),
                    credential_stored: false,
                    loopback: self.transport.is_loopback(),
                })
            }
            status => Ok(LoginReport {
                state: self.state,
                qrcode: Some(pending.challenge.qrcode.clone()),
                qr_status: Some(status),
                credential_stored: false,
                loopback: self.transport.is_loopback(),
            }),
        }
    }

    pub fn status(&self) -> StatusReport {
        let credential_stored = self.credential_stored();
        let authenticated = self.transport.is_authenticated() && credential_stored;
        let production_ready =
            authenticated && !self.transport.is_loopback() && self.state == LoginState::LoggedIn;
        StatusReport {
            account_alias: self.account_alias.clone(),
            state: self.state,
            authenticated,
            credential_stored,
            loopback: self.transport.is_loopback(),
            real_weixin: production_ready,
            production_ready,
            queued_messages: self.queue.entries.len(),
            outbound_messages: self.outbound.len(),
            session_bindings: self.sessions.len(),
            pair_requests: self.pairs.len(),
        }
    }

    pub fn doctor(&self) -> DoctorReport {
        let credential_stored = self.credential_stored();
        let authenticated = self.transport.is_authenticated() && credential_stored;
        let production_ready =
            authenticated && !self.transport.is_loopback() && self.state == LoginState::LoggedIn;
        let checks = vec![
            DoctorCheck {
                name: "transport".to_owned(),
                ok: true,
                detail: if self.transport.is_loopback() {
                    "loopback transport; no external side effect".to_owned()
                } else {
                    "iLink HTTP transport configured".to_owned()
                },
            },
            DoctorCheck {
                name: "credential".to_owned(),
                ok: authenticated,
                detail: if authenticated && credential_stored {
                    "credential is available to the transport and SecretStore".to_owned()
                } else if self.transport.is_authenticated() {
                    "transport is authenticated but the SecretStore credential is missing"
                        .to_owned()
                } else {
                    "not authenticated; login is required".to_owned()
                },
            },
            DoctorCheck {
                name: "runtime".to_owned(),
                ok: matches!(self.state, LoginState::LoggedIn | LoginState::Loopback)
                    && authenticated,
                detail: format!("state={:?}", self.state),
            },
            DoctorCheck {
                name: "production".to_owned(),
                ok: self.transport.is_loopback() || production_ready,
                detail: if self.transport.is_loopback() {
                    "loopback mode; productionReady=false by design".to_owned()
                } else if production_ready {
                    "real transport, Runtime, and SecretStore are ready".to_owned()
                } else {
                    "real Weixin is not ready; login and a stored credential are required"
                        .to_owned()
                },
            },
        ];
        DoctorReport {
            healthy: checks.iter().all(|check| check.ok),
            checks,
        }
    }

    pub fn serve(
        &mut self,
        options: ServeOptions,
        context: &RequestContext,
    ) -> Result<ServeReport, WeixinRuntimeError> {
        options.validate()?;
        self.ensure_authenticated()?;
        let mut report = ServeReport {
            polls: 0,
            received_messages: 0,
            enqueued_messages: 0,
            duplicate_messages: 0,
            cursor: self.cursor.clone(),
        };
        for _ in 0..options.max_polls {
            context.check()?;
            let batch = self
                .transport
                .get_updates(&self.cursor, context)
                .map_err(|error| self.handle_transport_error(error))?;
            self.accept_batch(batch, &options, &mut report)?;
            report.polls += 1;
        }
        report.cursor = self.cursor.clone();
        Ok(report)
    }

    /// Imports one batch produced by [`crate::LongPollWorker`]. The requested
    /// cursor is checked so a stale worker cannot overwrite newer state. A
    /// fully duplicated batch is accepted as a no-op, which makes Host event
    /// redelivery safe.
    pub fn accept_polled_batch(
        &mut self,
        polled: PolledBatch,
        options: &ServeOptions,
    ) -> Result<ServeReport, WeixinRuntimeError> {
        options.validate()?;
        self.ensure_authenticated()?;
        let mut report = ServeReport {
            polls: 0,
            received_messages: 0,
            enqueued_messages: 0,
            duplicate_messages: 0,
            cursor: self.cursor.clone(),
        };
        if polled.requested_cursor != self.cursor {
            let duplicate = polled.batch.messages.iter().all(|message| {
                self.queue
                    .entries
                    .get(&message.message_id)
                    .is_some_and(|entry| entry.message == *message)
            });
            if !duplicate || polled.batch.cursor != self.cursor {
                return Err(WeixinRuntimeError::CursorConflict);
            }
            report.polls = 1;
            report.received_messages = polled.batch.messages.len();
            report.duplicate_messages = polled.batch.messages.len();
            return Ok(report);
        }
        self.accept_batch(polled.batch, options, &mut report)?;
        report.polls = 1;
        report.cursor = self.cursor.clone();
        Ok(report)
    }

    pub fn send_message(
        &mut self,
        message: &IlinkMessage,
        context: &RequestContext,
    ) -> Result<crate::ilink::SendResult, WeixinRuntimeError> {
        context.check()?;
        self.ensure_authenticated()?;
        message.validate()?;
        self.validate_outbound_session(message)?;

        if let Some((existing, result)) = self.outbound.get(&message.message_id) {
            if existing != message {
                return Err(WeixinRuntimeError::IdempotencyConflict);
            }
            return Ok(crate::ilink::SendResult {
                status_code: result.status_code,
            });
        }
        if self.queue.entries.contains_key(&message.message_id) {
            return Err(WeixinRuntimeError::IdempotencyConflict);
        }
        if self.outbound.len() >= MAX_QUEUE_ENTRIES {
            return Err(WeixinRuntimeError::OutboundQueueCapacityExceeded);
        }

        let result = self
            .transport
            .send_message(message, context)
            .map_err(|error| self.handle_transport_error(error))?;
        self.outbound.insert(
            message.message_id.clone(),
            (
                message.clone(),
                OutboundQueueEntry {
                    status_code: result.status_code,
                },
            ),
        );
        Ok(result)
    }

    /// Sends a text reply using the inbound message's recipient and opaque
    /// context token. Approval and cancellation state is checked before any
    /// transport call so a remote control decision cannot be bypassed.
    pub fn send_reply_text(
        &mut self,
        idempotency_key: &str,
        reply_message_id: impl Into<String>,
        text: impl Into<String>,
        context: &RequestContext,
    ) -> Result<crate::ilink::SendResult, WeixinRuntimeError> {
        let (to_user_id, context_token, session_id, group_id) = {
            let entry = self
                .queue
                .entries
                .get(idempotency_key)
                .ok_or(WeixinRuntimeError::UnknownMessage)?;
            if matches!(
                entry.state,
                RemoteControlState::ApprovalRequired
                    | RemoteControlState::Cancelled
                    | RemoteControlState::Denied
            ) {
                return Err(WeixinRuntimeError::MessageNotSendable(entry.state));
            }
            (
                entry.message.from_user_id.clone(),
                entry.message.context_token.clone(),
                entry.message.session_id.clone(),
                entry.message.group_id.clone(),
            )
        };
        let mut reply = IlinkMessage::reply_text(
            reply_message_id,
            to_user_id,
            context_token.ok_or(WeixinRuntimeError::Ilink(IlinkError::InvalidInput(
                "context_token",
            )))?,
            text,
        )?;
        reply.session_id = session_id;
        reply.group_id = group_id;
        self.send_message(&reply, context)
    }

    pub fn apply_remote_command(
        &mut self,
        command: RemoteCommand,
    ) -> Result<ControlResult, WeixinRuntimeError> {
        let cancellation = match &command {
            RemoteCommand::Cancel {
                idempotency_key,
                reason,
            } => Some((idempotency_key.as_str(), reason.as_str())),
            _ => None,
        };
        let result = self.queue.apply(&command)?;
        if let Some((message_id, reason)) = cancellation {
            self.agent_bridge.cancel(message_id, reason)?;
        }
        Ok(result)
    }

    /// Returns the Agent-facing state without exposing credentials or mutable
    /// channel internals.
    pub fn agent_bridge_snapshots(&self, maximum: usize) -> Vec<AgentBridgeSnapshot> {
        self.agent_bridge.snapshots(maximum)
    }

    /// Claims one inbound message for an Agent turn. Reusing the same attempt
    /// id is a duplicate and returns the same work item; another active id is
    /// rejected. Approval-required messages remain unavailable until approved.
    pub fn claim_agent_work(
        &mut self,
        idempotency_key: &str,
        attempt_id: impl Into<String>,
    ) -> Result<AgentWorkItem, WeixinRuntimeError> {
        match self.queue.state(idempotency_key) {
            Some(RemoteControlState::Queued | RemoteControlState::Approved) => {}
            Some(state) => return Err(WeixinRuntimeError::MessageNotSendable(state)),
            None => return Err(WeixinRuntimeError::UnknownMessage),
        }
        self.agent_bridge
            .claim(idempotency_key, attempt_id)
            .map_err(WeixinRuntimeError::AgentBridge)
    }

    pub fn stage_agent_reply(
        &mut self,
        idempotency_key: &str,
        attempt_id: &str,
        reply: IlinkMessage,
    ) -> Result<bool, WeixinRuntimeError> {
        self.agent_bridge
            .stage_reply(idempotency_key, attempt_id, reply)
            .map_err(WeixinRuntimeError::AgentBridge)
    }

    /// Sends the exact staged reply and commits the Agent item only after the
    /// transport succeeds. A retry is safe because `send_message` is itself
    /// idempotent by message id.
    pub fn send_staged_agent_reply(
        &mut self,
        idempotency_key: &str,
        attempt_id: &str,
        context: &RequestContext,
    ) -> Result<crate::ilink::SendResult, WeixinRuntimeError> {
        let reply = self
            .agent_bridge
            .staged_reply(idempotency_key, attempt_id)
            .map_err(WeixinRuntimeError::AgentBridge)?
            .cloned()
            .ok_or(WeixinRuntimeError::AgentBridge(
                AgentBridgeError::InvalidTransition {
                    from: AgentBridgeState::Running,
                    to: AgentBridgeState::ReplyReady,
                },
            ))?;
        let result = self.send_message(&reply, context)?;
        self.agent_bridge
            .complete(idempotency_key, attempt_id)
            .map_err(WeixinRuntimeError::AgentBridge)?;
        if matches!(
            self.queue.state(idempotency_key),
            Some(RemoteControlState::Queued | RemoteControlState::Approved)
        ) {
            let _ = self.queue.apply(&RemoteCommand::Acknowledge {
                idempotency_key: idempotency_key.to_owned(),
            });
        }
        Ok(result)
    }

    pub fn fail_agent_work(
        &mut self,
        idempotency_key: &str,
        attempt_id: &str,
        reason: impl Into<String>,
        retryable: bool,
    ) -> Result<bool, WeixinRuntimeError> {
        self.agent_bridge
            .fail(idempotency_key, attempt_id, reason, retryable)
            .map_err(WeixinRuntimeError::AgentBridge)
    }

    pub fn cancel_agent_work(
        &mut self,
        idempotency_key: &str,
        reason: impl Into<String>,
    ) -> Result<bool, WeixinRuntimeError> {
        let reason = reason.into();
        let result = self.agent_bridge.cancel(idempotency_key, reason.clone())?;
        if matches!(
            self.queue.state(idempotency_key),
            Some(RemoteControlState::Queued | RemoteControlState::Approved)
        ) {
            let _ = self.queue.apply(&RemoteCommand::Cancel {
                idempotency_key: idempotency_key.to_owned(),
                reason,
            });
        }
        Ok(result)
    }

    pub fn queued_message(&self, idempotency_key: &str) -> Option<&IlinkMessage> {
        self.queue
            .entries
            .get(idempotency_key)
            .map(|entry| &entry.message)
    }

    /// Returns queued messages in arrival order without consuming them.
    ///
    /// A caller acknowledges or cancels an item explicitly through
    /// [`Self::apply_remote_command`], which keeps retries idempotent across a
    /// failed Agent turn.
    pub fn queued_messages(
        &self,
        maximum: usize,
    ) -> Result<Vec<QueuedMessageSnapshot>, WeixinRuntimeError> {
        if maximum == 0 || maximum > crate::ilink::MAX_ILINK_MESSAGES {
            return Err(WeixinRuntimeError::InvalidOptions("maximum"));
        }
        Ok(self
            .queue
            .order
            .iter()
            .take(maximum)
            .filter_map(|key| {
                self.queue
                    .entries
                    .get(key)
                    .map(|entry| QueuedMessageSnapshot {
                        message: entry.message.clone(),
                        state: entry.state,
                    })
            })
            .collect())
    }

    pub fn remote_state(&self, idempotency_key: &str) -> Option<RemoteControlState> {
        self.queue.state(idempotency_key)
    }

    pub fn pair(&mut self, action: PairAction) -> Result<PairReport, WeixinRuntimeError> {
        match action {
            PairAction::Request { peer_id } => {
                if let Some(existing) = self.pairs.values().find(|request| {
                    request.peer_id == peer_id && request.state == PairState::Pending
                }) {
                    return Ok(PairReport {
                        duplicate: true,
                        request: existing.clone(),
                    });
                }
                if self.pairs.len() >= MAX_PAIR_REQUESTS {
                    return Err(WeixinRuntimeError::PairCapacityExceeded);
                }
                let request_id = format!("pair-{}", self.next_pair_id);
                self.next_pair_id = self.next_pair_id.saturating_add(1);
                let request = PairRequest {
                    request_id: request_id.clone(),
                    peer_id,
                    state: PairState::Pending,
                    reason: None,
                };
                self.pairs.insert(request_id, request.clone());
                Ok(PairReport {
                    duplicate: false,
                    request,
                })
            }
            PairAction::Approve { request_id } => {
                self.change_pair(request_id, PairState::Approved, None)
            }
            PairAction::Deny { request_id, reason } => {
                if reason.is_empty()
                    || reason.len() > MAX_PAIR_REASON_BYTES
                    || reason.chars().any(char::is_control)
                {
                    return Err(WeixinRuntimeError::InvalidOptions("reason"));
                }
                self.change_pair(request_id, PairState::Denied, Some(reason))
            }
        }
    }

    pub fn session(
        &mut self,
        command: SessionCommand,
    ) -> Result<SessionReport, WeixinRuntimeError> {
        let mut changed = false;
        match command {
            SessionCommand::Bind {
                session_id,
                peer_id,
            } => {
                if let Some(existing) = self.sessions.get(&session_id) {
                    if existing != &peer_id {
                        return Err(WeixinRuntimeError::SessionBindingConflict);
                    }
                } else {
                    if self.sessions.len() >= MAX_SESSION_BINDINGS {
                        return Err(WeixinRuntimeError::SessionCapacityExceeded);
                    }
                    self.sessions.insert(session_id, peer_id);
                    changed = true;
                }
            }
            SessionCommand::Unbind { session_id } => {
                changed = self.sessions.remove(&session_id).is_some();
            }
            SessionCommand::List => {}
        }
        Ok(SessionReport {
            changed,
            bindings: self
                .sessions
                .iter()
                .map(|(session_id, peer_id)| SessionBinding {
                    session_id: session_id.clone(),
                    peer_id: peer_id.clone(),
                })
                .collect(),
        })
    }

    pub fn logout(&mut self) -> Result<LogoutReport, WeixinRuntimeError> {
        // Keep the reference until removal succeeds so a transient store error
        // leaves the authenticated runtime retryable rather than half-logged-out.
        let removed_token = if let Some(reference) = self.token_ref.as_ref() {
            self.store.remove(reference)?
        } else {
            false
        };
        self.token_ref = None;
        self.pending_login = None;
        self.transport.clear_auth_token();
        self.clear_channel_state();
        self.state = LoginState::LoggedOut;
        Ok(LogoutReport {
            state: self.state,
            removed_token,
        })
    }

    fn accept_batch(
        &mut self,
        batch: PollBatch,
        options: &ServeOptions,
        report: &mut ServeReport,
    ) -> Result<(), WeixinRuntimeError> {
        if batch.messages.len() > options.max_messages_per_poll {
            return Err(WeixinRuntimeError::TooManyMessages {
                count: batch.messages.len(),
                maximum: options.max_messages_per_poll,
            });
        }

        // Validate the whole batch before mutating queue, bindings, or cursor.
        // iLink is at-least-once, so a failed batch must be safe to retry.
        let mut new_keys = BTreeMap::new();
        let mut new_sessions = BTreeMap::new();
        for message in &batch.messages {
            message.validate()?;
            if let Some(entry) = self.queue.entries.get(&message.message_id) {
                if entry.message != *message {
                    return Err(WeixinRuntimeError::IdempotencyConflict);
                }
            } else if let Some(existing) = new_keys.get(&message.message_id) {
                if existing != message {
                    return Err(WeixinRuntimeError::IdempotencyConflict);
                }
            } else {
                new_keys.insert(message.message_id.clone(), message.clone());
            }

            let Some(session_id) = &message.session_id else {
                continue;
            };
            let session_id =
                SessionId::new(session_id.clone()).map_err(WeixinRuntimeError::Contract)?;
            let peer_id = ParticipantId::new(message.from_user_id.clone())
                .map_err(WeixinRuntimeError::Contract)?;
            if let Some(existing) = self.sessions.get(&session_id) {
                if existing != &peer_id {
                    return Err(WeixinRuntimeError::SessionBindingConflict);
                }
            } else if let Some(existing) = new_sessions.get(&session_id) {
                if existing != &peer_id {
                    return Err(WeixinRuntimeError::SessionBindingConflict);
                }
            } else {
                new_sessions.insert(session_id, peer_id);
            }
        }
        if self.queue.entries.len().saturating_add(new_keys.len()) > MAX_QUEUE_ENTRIES {
            return Err(WeixinRuntimeError::QueueCapacityExceeded);
        }
        if self.sessions.len().saturating_add(new_sessions.len()) > MAX_SESSION_BINDINGS {
            return Err(WeixinRuntimeError::SessionCapacityExceeded);
        }

        for message in batch.messages {
            report.received_messages += 1;
            self.bind_message_session(&message)?;
            let bridge_duplicate = self.agent_bridge.offer(message.clone())?;
            let (duplicate, _) = self.queue.enqueue(message, options.require_approval)?;
            debug_assert_eq!(bridge_duplicate, duplicate);
            if duplicate {
                report.duplicate_messages += 1;
            } else {
                report.enqueued_messages += 1;
            }
        }
        self.cursor = batch.cursor;
        Ok(())
    }

    fn bind_message_session(&mut self, message: &IlinkMessage) -> Result<(), WeixinRuntimeError> {
        let Some(session_id) = &message.session_id else {
            return Ok(());
        };
        let session_id =
            SessionId::new(session_id.clone()).map_err(WeixinRuntimeError::Contract)?;
        let peer_id = ParticipantId::new(message.from_user_id.clone())
            .map_err(WeixinRuntimeError::Contract)?;
        if let Some(existing) = self.sessions.get(&session_id) {
            if existing != &peer_id {
                return Err(WeixinRuntimeError::SessionBindingConflict);
            }
        } else {
            if self.sessions.len() >= MAX_SESSION_BINDINGS {
                return Err(WeixinRuntimeError::SessionCapacityExceeded);
            }
            self.sessions.insert(session_id, peer_id);
        }
        Ok(())
    }

    fn validate_outbound_session(&self, message: &IlinkMessage) -> Result<(), WeixinRuntimeError> {
        let Some(session_id) = &message.session_id else {
            return Ok(());
        };
        let session_id =
            SessionId::new(session_id.clone()).map_err(WeixinRuntimeError::Contract)?;
        let recipient = message
            .to_user_id
            .as_deref()
            .ok_or(WeixinRuntimeError::InvalidOptions("to_user_id"))?;
        let recipient =
            ParticipantId::new(recipient.to_owned()).map_err(WeixinRuntimeError::Contract)?;
        let bound_peer = self
            .sessions
            .get(&session_id)
            .ok_or(WeixinRuntimeError::SessionBindingRequired)?;
        if bound_peer != &recipient {
            return Err(WeixinRuntimeError::SessionBindingConflict);
        }
        Ok(())
    }

    fn change_pair(
        &mut self,
        request_id: String,
        state: PairState,
        _reason: Option<String>,
    ) -> Result<PairReport, WeixinRuntimeError> {
        let request = self
            .pairs
            .get_mut(&request_id)
            .ok_or(WeixinRuntimeError::UnknownPairRequest)?;
        if request.state == state {
            return Ok(PairReport {
                duplicate: true,
                request: request.clone(),
            });
        }
        if request.state != PairState::Pending {
            return Err(WeixinRuntimeError::PairStateConflict);
        }
        request.state = state;
        request.reason = _reason;
        Ok(PairReport {
            duplicate: false,
            request: request.clone(),
        })
    }

    pub(crate) fn ensure_authenticated(&self) -> Result<(), WeixinRuntimeError> {
        if matches!(self.state, LoginState::LoggedIn | LoginState::Loopback)
            && self.transport.is_authenticated()
            && self.credential_stored()
        {
            Ok(())
        } else {
            Err(WeixinRuntimeError::NotAuthenticated)
        }
    }

    fn transport_is_authenticated(&self) -> bool {
        self.transport.is_authenticated() && self.credential_stored()
    }

    fn credential_stored(&self) -> bool {
        self.token_ref
            .as_ref()
            .and_then(|reference| self.store.contains(reference).ok())
            .unwrap_or(false)
    }

    fn handle_transport_error(&mut self, error: IlinkError) -> WeixinRuntimeError {
        if matches!(error, IlinkError::SessionExpired) {
            self.expire_session();
        }
        error.into()
    }

    fn expire_session(&mut self) {
        let reference = self.token_ref.clone();
        if let Some(reference) = reference.as_ref() {
            if self.store.remove(reference).is_ok() {
                self.token_ref = None;
            }
        } else {
            self.token_ref = None;
        }
        self.pending_login = None;
        self.transport.clear_auth_token();
        self.clear_channel_state();
        self.state = LoginState::LoggedOut;
    }

    fn clear_channel_state(&mut self) {
        self.cursor.clear();
        self.queue = InboundQueue::default();
        self.agent_bridge.clear();
        self.outbound.clear();
        self.sessions.clear();
        self.pairs.clear();
        self.next_pair_id = 1;
    }
}

#[derive(Debug)]
pub enum WeixinRuntimeError {
    Contract(crate::WeixinContractError),
    AgentBridge(AgentBridgeError),
    Ilink(IlinkError),
    Secret(SecretStoreError),
    InvalidOptions(&'static str),
    CursorConflict,
    NotAuthenticated,
    NoPendingLogin,
    QueueCapacityExceeded,
    OutboundQueueCapacityExceeded,
    PairCapacityExceeded,
    SessionCapacityExceeded,
    UnknownMessage,
    UnknownPairRequest,
    IdempotencyConflict,
    SessionBindingConflict,
    SessionBindingRequired,
    MessageNotSendable(RemoteControlState),
    PairStateConflict,
    TooManyMessages {
        count: usize,
        maximum: usize,
    },
    InvalidTransition {
        from: RemoteControlState,
        to: RemoteControlState,
    },
}

impl fmt::Display for WeixinRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => error.fmt(formatter),
            Self::AgentBridge(error) => error.fmt(formatter),
            Self::Ilink(error) => error.fmt(formatter),
            Self::Secret(error) => error.fmt(formatter),
            Self::InvalidOptions(field) => write!(formatter, "invalid Weixin option: {field}"),
            Self::CursorConflict => {
                formatter.write_str("Weixin poll cursor is stale or conflicting")
            }
            Self::NotAuthenticated => formatter.write_str("Weixin account is not authenticated"),
            Self::NoPendingLogin => formatter.write_str("no Weixin login is pending"),
            Self::QueueCapacityExceeded => formatter.write_str("Weixin inbound queue is full"),
            Self::OutboundQueueCapacityExceeded => {
                formatter.write_str("Weixin outbound queue is full")
            }
            Self::PairCapacityExceeded => {
                formatter.write_str("Weixin pair request capacity is full")
            }
            Self::SessionCapacityExceeded => formatter.write_str("Weixin session capacity is full"),
            Self::UnknownMessage => formatter.write_str("Weixin message is unknown"),
            Self::UnknownPairRequest => formatter.write_str("Weixin pair request is unknown"),
            Self::IdempotencyConflict => {
                formatter.write_str("Weixin idempotency key conflicts with another message")
            }
            Self::SessionBindingConflict => {
                formatter.write_str("Weixin session is bound to another peer")
            }
            Self::SessionBindingRequired => {
                formatter.write_str("Weixin outbound session is not bound to a peer")
            }
            Self::MessageNotSendable(state) => {
                write!(
                    formatter,
                    "Weixin message cannot be replied to in state {state:?}"
                )
            }
            Self::PairStateConflict => {
                formatter.write_str("Weixin pair request state cannot be changed")
            }
            Self::TooManyMessages { count, maximum } => {
                write!(
                    formatter,
                    "Weixin poll returned {count} messages, maximum is {maximum}"
                )
            }
            Self::InvalidTransition { from, to } => {
                write!(
                    formatter,
                    "invalid remote control transition from {from:?} to {to:?}"
                )
            }
        }
    }
}

impl std::error::Error for WeixinRuntimeError {}

impl From<IlinkError> for WeixinRuntimeError {
    fn from(error: IlinkError) -> Self {
        Self::Ilink(error)
    }
}

impl From<AgentBridgeError> for WeixinRuntimeError {
    fn from(error: AgentBridgeError) -> Self {
        Self::AgentBridge(error)
    }
}

impl From<SecretStoreError> for WeixinRuntimeError {
    fn from(error: SecretStoreError) -> Self {
        Self::Secret(error)
    }
}

impl From<crate::WeixinContractError> for WeixinRuntimeError {
    fn from(error: crate::WeixinContractError) -> Self {
        Self::Contract(error)
    }
}

impl From<crate::RequestControlError> for WeixinRuntimeError {
    fn from(error: crate::RequestControlError) -> Self {
        Self::Ilink(error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ilink::{
        IlinkHttpTransport, IlinkTransport, LoopbackIlinkTransport, PollBatch, QrPoll,
    };
    use crate::{MemorySecretStore, SecretRef};

    fn fixture_plane() -> WeixinControlPlane<LoopbackIlinkTransport, MemorySecretStore> {
        WeixinControlPlane::new(
            "fixture",
            LoopbackIlinkTransport::new(),
            MemorySecretStore::new(),
        )
        .expect("plane")
    }

    fn login(plane: &mut WeixinControlPlane<LoopbackIlinkTransport, MemorySecretStore>) {
        plane
            .transport
            .queue_qr(QrChallenge::new("qr", "image").expect("challenge"));
        plane.transport.queue_login_poll(Ok(QrPoll::confirmed(
            SecretMaterial::from_text("loopback-token").expect("token"),
        )));
        let reference = SecretRef::new("host:weixin/token").expect("reference");
        plane
            .login(LoginOptions::new(reference), &RequestContext::new())
            .expect("login start");
        plane
            .poll_login(&RequestContext::new())
            .expect("login finish");
    }

    #[test]
    fn login_never_claims_success_without_a_credential() {
        let mut plane = fixture_plane();
        plane
            .transport
            .queue_qr(QrChallenge::new("qr", "image").expect("challenge"));
        plane.transport.queue_qr_status(Ok(QrStatus::Confirmed));
        let reference = SecretRef::new("host:weixin/token").expect("reference");
        plane
            .login(LoginOptions::new(reference), &RequestContext::new())
            .expect("start");
        let error = plane
            .poll_login(&RequestContext::new())
            .expect_err("missing token");
        assert!(matches!(
            error,
            WeixinRuntimeError::Ilink(IlinkError::Unavailable(_))
        ));
        assert_eq!(plane.status().state, LoginState::AwaitingQr);
        assert!(!plane.status().authenticated);
    }

    #[test]
    fn serve_is_long_poll_bounded_and_remote_controls_are_idempotent() {
        let mut plane = fixture_plane();
        login(&mut plane);
        assert_eq!(plane.status().state, LoginState::Loopback);
        assert!(!plane.status().real_weixin);
        assert!(!plane.status().production_ready);
        let message = IlinkMessage::text("message-1", "peer-1", "hello").expect("message");
        plane
            .transport
            .queue_poll(Ok(PollBatch::new("cursor-1", vec![message]).expect("batch")));
        let report = plane
            .serve(
                ServeOptions {
                    max_polls: 1,
                    ..ServeOptions::default()
                },
                &RequestContext::new(),
            )
            .expect("serve");
        assert_eq!(report.enqueued_messages, 1);
        assert_eq!(report.cursor, "cursor-1");
        let command = RemoteCommand::Acknowledge {
            idempotency_key: "message-1".to_owned(),
        };
        assert!(
            !plane
                .apply_remote_command(command.clone())
                .expect("ack")
                .duplicate
        );
        assert!(
            plane
                .apply_remote_command(command)
                .expect("duplicate ack")
                .duplicate
        );
    }

    #[test]
    fn reply_path_reuses_context_and_cannot_bypass_remote_approval() {
        let mut plane = fixture_plane();
        login(&mut plane);
        let mut message = IlinkMessage::text("message-reply", "peer-1", "hello").expect("message");
        message.to_user_id = Some("bot-1".to_owned());
        message.context_token = Some("context-1".to_owned());
        plane
            .transport
            .queue_poll(Ok(PollBatch::new("reply", vec![message]).expect("batch")));
        plane
            .serve(ServeOptions::default(), &RequestContext::new())
            .expect("serve");
        plane
            .send_reply_text("message-reply", "reply-1", "answer", &RequestContext::new())
            .expect("reply");
        let sent = &plane.transport.sent_messages()[0];
        assert_eq!(sent.to_user_id.as_deref(), Some("peer-1"));
        assert_eq!(sent.context_token.as_deref(), Some("context-1"));
        assert_eq!(sent.session_id.as_deref(), None);

        let mut approval_plane = fixture_plane();
        login(&mut approval_plane);
        let mut approval_message =
            IlinkMessage::text("message-approval-reply", "peer-1", "hello").expect("message");
        approval_message.context_token = Some("context-2".to_owned());
        approval_plane.transport.queue_poll(Ok(PollBatch::new(
            "approval-reply",
            vec![approval_message],
        )
        .expect("batch")));
        approval_plane
            .serve(
                ServeOptions {
                    require_approval: true,
                    ..ServeOptions::default()
                },
                &RequestContext::new(),
            )
            .expect("serve");
        assert!(matches!(
            approval_plane.send_reply_text(
                "message-approval-reply",
                "reply-2",
                "blocked",
                &RequestContext::new(),
            ),
            Err(WeixinRuntimeError::MessageNotSendable(
                RemoteControlState::ApprovalRequired
            ))
        ));
        approval_plane
            .apply_remote_command(RemoteCommand::Approve {
                idempotency_key: "message-approval-reply".to_owned(),
            })
            .expect("approve");
        approval_plane
            .send_reply_text(
                "message-approval-reply",
                "reply-2",
                "approved",
                &RequestContext::new(),
            )
            .expect("approved reply");
    }

    #[test]
    fn approval_and_session_binding_reject_cross_peer_messages() {
        let mut plane = fixture_plane();
        login(&mut plane);
        let mut first = IlinkMessage::text("message-1", "peer-1", "hello").expect("message");
        first.session_id = Some("session-1".to_owned());
        let mut second = IlinkMessage::text("message-2", "peer-2", "hello").expect("message");
        second.session_id = Some("session-1".to_owned());
        plane
            .transport
            .queue_poll(Ok(PollBatch::new("one", vec![first]).expect("batch")));
        plane
            .serve(
                ServeOptions {
                    require_approval: true,
                    ..ServeOptions::default()
                },
                &RequestContext::new(),
            )
            .expect("first serve");
        assert_eq!(
            plane.remote_state("message-1"),
            Some(RemoteControlState::ApprovalRequired)
        );
        plane
            .transport
            .queue_poll(Ok(PollBatch::new("two", vec![second]).expect("batch")));
        assert!(matches!(
            plane.serve(ServeOptions::default(), &RequestContext::new()),
            Err(WeixinRuntimeError::SessionBindingConflict)
        ));
    }

    #[test]
    fn outbound_session_binding_is_required_and_idempotent() {
        let mut plane = fixture_plane();
        login(&mut plane);
        let session_id = SessionId::new("session-1").expect("session");
        let peer_id = ParticipantId::new("peer-1").expect("peer");
        plane
            .session(SessionCommand::Bind {
                session_id: session_id.clone(),
                peer_id,
            })
            .expect("bind");

        let mut reply =
            IlinkMessage::reply_text("reply-1", "peer-1", "context-1", "answer").expect("reply");
        reply.session_id = Some(session_id.to_string());
        let first = plane
            .send_message(&reply, &RequestContext::new())
            .expect("first send");
        let second = plane
            .send_message(&reply, &RequestContext::new())
            .expect("duplicate send");
        assert_eq!(first, second);
        assert_eq!(plane.transport.sent_messages().len(), 1);
        assert_eq!(plane.status().outbound_messages, 1);

        let mut wrong_peer = reply.clone();
        wrong_peer.message_id = "reply-2".to_owned();
        wrong_peer.client_id = Some("reply-2".to_owned());
        wrong_peer.to_user_id = Some("peer-2".to_owned());
        assert!(matches!(
            plane.send_message(&wrong_peer, &RequestContext::new()),
            Err(WeixinRuntimeError::SessionBindingConflict)
        ));

        let mut unbound = reply;
        unbound.message_id = "reply-3".to_owned();
        unbound.client_id = Some("reply-3".to_owned());
        unbound.session_id = Some("session-2".to_owned());
        assert!(matches!(
            plane.send_message(&unbound, &RequestContext::new()),
            Err(WeixinRuntimeError::SessionBindingRequired)
        ));
    }

    #[test]
    fn failed_batch_does_not_partially_mutate_queue_bindings_or_cursor() {
        let mut plane = fixture_plane();
        login(&mut plane);
        let mut first = IlinkMessage::text("message-1", "peer-1", "hello").expect("message");
        first.session_id = Some("session-1".to_owned());
        let mut second = IlinkMessage::text("message-2", "peer-2", "hello").expect("message");
        second.session_id = Some("session-1".to_owned());
        plane.transport.queue_poll(Ok(
            PollBatch::new("failed-cursor", vec![first, second]).expect("batch")
        ));

        assert!(matches!(
            plane.serve(ServeOptions::default(), &RequestContext::new()),
            Err(WeixinRuntimeError::SessionBindingConflict)
        ));
        assert_eq!(plane.status().queued_messages, 0);
        assert_eq!(plane.status().session_bindings, 0);
        assert_eq!(plane.status().state, LoginState::Loopback);
    }

    #[test]
    fn remote_approval_and_cancellation_are_idempotent_and_fail_closed() {
        let mut plane = fixture_plane();
        login(&mut plane);
        let message = IlinkMessage::text("message-approval", "peer-1", "hello").expect("message");
        plane
            .transport
            .queue_poll(Ok(PollBatch::new("approval", vec![message]).expect("batch")));
        plane
            .serve(
                ServeOptions {
                    require_approval: true,
                    ..ServeOptions::default()
                },
                &RequestContext::new(),
            )
            .expect("serve");

        let approve = RemoteCommand::Approve {
            idempotency_key: "message-approval".to_owned(),
        };
        assert!(
            !plane
                .apply_remote_command(approve.clone())
                .expect("approve")
                .duplicate
        );
        assert!(
            plane
                .apply_remote_command(approve)
                .expect("duplicate approve")
                .duplicate
        );
        assert!(matches!(
            plane.apply_remote_command(RemoteCommand::Deny {
                idempotency_key: "message-approval".to_owned(),
                reason: "late".to_owned(),
            }),
            Err(WeixinRuntimeError::InvalidTransition { .. })
        ));

        let message = IlinkMessage::text("message-cancel", "peer-1", "cancel me").expect("message");
        plane
            .transport
            .queue_poll(Ok(PollBatch::new("cancel", vec![message]).expect("batch")));
        plane
            .serve(ServeOptions::default(), &RequestContext::new())
            .expect("serve");
        let cancel = RemoteCommand::Cancel {
            idempotency_key: "message-cancel".to_owned(),
            reason: "operator request".to_owned(),
        };
        assert!(
            !plane
                .apply_remote_command(cancel.clone())
                .expect("cancel")
                .duplicate
        );
        assert!(
            plane
                .apply_remote_command(cancel)
                .expect("duplicate cancel")
                .duplicate
        );
        assert!(matches!(
            plane.apply_remote_command(RemoteCommand::Cancel {
                idempotency_key: "message-cancel".to_owned(),
                reason: "a different operator request".to_owned(),
            }),
            Err(WeixinRuntimeError::IdempotencyConflict)
        ));
    }

    #[test]
    fn pair_denial_preserves_a_bounded_operator_reason() {
        let mut plane = fixture_plane();
        let peer_id = ParticipantId::new("peer-1").expect("peer");
        let pending = plane
            .pair(PairAction::Request { peer_id })
            .expect("request")
            .request;
        let denied = plane
            .pair(PairAction::Deny {
                request_id: pending.request_id.clone(),
                reason: "not recognized".to_owned(),
            })
            .expect("deny")
            .request;
        assert_eq!(denied.state, PairState::Denied);
        assert_eq!(denied.reason.as_deref(), Some("not recognized"));
    }

    #[test]
    fn loopback_honors_cancellation_before_network_like_operations() {
        let mut plane = fixture_plane();
        let context = RequestContext::new();
        context.cancel();
        let error = plane
            .login(
                LoginOptions::new(SecretRef::new("host:weixin/token").expect("reference")),
                &context,
            )
            .expect_err("cancelled login");
        assert!(matches!(
            error,
            WeixinRuntimeError::Ilink(IlinkError::Cancelled)
        ));
    }

    #[test]
    fn logout_removes_the_in_memory_credential_and_disables_send() {
        let mut plane = fixture_plane();
        login(&mut plane);
        assert!(plane.status().authenticated);
        plane
            .session(SessionCommand::Bind {
                session_id: SessionId::new("session-1").expect("session"),
                peer_id: ParticipantId::new("peer-1").expect("peer"),
            })
            .expect("bind");
        plane
            .send_message(
                &IlinkMessage::reply_text("reply-1", "peer-1", "context-1", "answer")
                    .expect("reply"),
                &RequestContext::new(),
            )
            .expect("send");
        assert_eq!(plane.status().outbound_messages, 1);
        let report = plane.logout().expect("logout");
        assert!(report.removed_token);
        assert!(!plane.status().authenticated);
        assert_eq!(plane.status().queued_messages, 0);
        assert_eq!(plane.status().outbound_messages, 0);
        assert_eq!(plane.status().session_bindings, 0);
        assert!(matches!(
            plane.serve(ServeOptions::default(), &RequestContext::new()),
            Err(WeixinRuntimeError::NotAuthenticated)
        ));
    }

    #[test]
    fn production_ready_requires_transport_and_secret_store_credential() {
        let token_ref = SecretRef::new("host:weixin/token").expect("reference");
        let store = MemorySecretStore::new();
        let mut transport = IlinkHttpTransport::production("real", Some(token_ref.clone()), &store)
            .expect("transport");
        transport
            .set_auth_token(SecretMaterial::from_text("transient-token").expect("token"))
            .expect("transport auth");

        let plane = WeixinControlPlane::new_with_token_ref(
            "real",
            transport,
            store.clone(),
            Some(token_ref.clone()),
        )
        .expect("plane");
        let status = plane.status();
        assert!(!status.authenticated);
        assert!(!status.credential_stored);
        assert!(!status.real_weixin);
        assert!(!status.production_ready);
        assert!(!plane.doctor().healthy);

        store
            .put(
                token_ref.clone(),
                SecretMaterial::from_text("stored-token").expect("token"),
            )
            .expect("store credential");
        let transport = IlinkHttpTransport::production("real", Some(token_ref.clone()), &store)
            .expect("transport with credential");
        let plane =
            WeixinControlPlane::new_with_token_ref("real", transport, store, Some(token_ref))
                .expect("plane with credential");
        let status = plane.status();
        assert!(status.authenticated);
        assert!(status.credential_stored);
        assert!(status.real_weixin);
        assert!(status.production_ready);
        assert_eq!(
            serde_json::to_value(status).expect("status JSON")["productionReady"],
            serde_json::Value::Bool(true)
        );
        assert!(plane.doctor().healthy);
    }

    #[test]
    fn removing_secret_revokes_runtime_authentication_and_production_ready() {
        let token_ref = SecretRef::new("host:weixin/token").expect("reference");
        let store = MemorySecretStore::new();
        store
            .put(
                token_ref.clone(),
                SecretMaterial::from_text("stored-token").expect("token"),
            )
            .expect("store credential");
        let transport = IlinkHttpTransport::production("real", Some(token_ref.clone()), &store)
            .expect("transport");
        let mut plane = WeixinControlPlane::new_with_token_ref(
            "real",
            transport,
            store.clone(),
            Some(token_ref),
        )
        .expect("plane");
        assert!(plane.status().production_ready);

        store
            .remove(&SecretRef::new("host:weixin/token").expect("reference"))
            .expect("remove credential");
        let status = plane.status();
        assert!(!status.authenticated);
        assert!(!status.credential_stored);
        assert!(!status.production_ready);
        assert!(!plane.doctor().healthy);
        assert!(matches!(
            plane.serve(ServeOptions::default(), &RequestContext::new()),
            Err(WeixinRuntimeError::NotAuthenticated)
        ));
    }

    #[test]
    fn session_expiry_clears_cursor_and_revokes_loopback_credential() {
        let mut plane = fixture_plane();
        login(&mut plane);
        plane.transport.queue_poll(Err(IlinkError::SessionExpired));
        let error = plane
            .serve(ServeOptions::default(), &RequestContext::new())
            .expect_err("expired session");
        assert!(matches!(
            error,
            WeixinRuntimeError::Ilink(IlinkError::SessionExpired)
        ));
        assert_eq!(plane.status().state, LoginState::LoggedOut);
        assert!(!plane.status().authenticated);
        assert_eq!(plane.status().queued_messages, 0);
        assert!(plane.secret_store().is_empty().expect("store state"));
    }

    #[test]
    fn agent_bridge_commits_only_after_send_and_accepts_batch_replay() {
        let mut plane = fixture_plane();
        login(&mut plane);
        let inbound = IlinkMessage::text("agent-in", "peer-1", "hello").expect("message");
        let batch = PollBatch::new("agent-cursor", vec![inbound]).expect("batch");
        plane
            .accept_polled_batch(
                crate::PolledBatch {
                    requested_cursor: String::new(),
                    batch: batch.clone(),
                },
                &ServeOptions::default(),
            )
            .expect("import");

        let replay = plane
            .accept_polled_batch(
                crate::PolledBatch {
                    requested_cursor: String::new(),
                    batch,
                },
                &ServeOptions::default(),
            )
            .expect("replay");
        assert_eq!(replay.enqueued_messages, 0);
        assert_eq!(replay.duplicate_messages, 1);

        let work = plane.claim_agent_work("agent-in", "turn-1").expect("claim");
        assert_eq!(work.attempt, 1);
        let reply =
            IlinkMessage::reply_text("agent-out", "peer-1", "context", "answer").expect("reply");
        assert!(
            !plane
                .stage_agent_reply("agent-in", "turn-1", reply)
                .expect("stage")
        );
        assert_eq!(
            plane
                .agent_bridge_snapshots(8)
                .first()
                .expect("snapshot")
                .state,
            crate::AgentBridgeState::ReplyReady
        );
        plane
            .send_staged_agent_reply("agent-in", "turn-1", &RequestContext::new())
            .expect("send and commit");
        assert_eq!(
            plane.remote_state("agent-in"),
            Some(RemoteControlState::Acknowledged)
        );
        assert_eq!(
            plane
                .agent_bridge_snapshots(8)
                .first()
                .expect("snapshot")
                .state,
            crate::AgentBridgeState::Completed
        );
    }

    #[test]
    fn agent_bridge_failure_returns_to_a_new_attempt_and_cancel_stops_it() {
        let mut plane = fixture_plane();
        login(&mut plane);
        plane.transport.queue_poll(Ok(PollBatch::new(
            "retry-cursor",
            vec![IlinkMessage::text("retry-in", "peer-1", "hello").expect("message")],
        )
        .expect("batch")));
        plane
            .serve(ServeOptions::default(), &RequestContext::new())
            .expect("serve");
        plane.claim_agent_work("retry-in", "turn-1").expect("claim");
        assert!(
            !plane
                .fail_agent_work("retry-in", "turn-1", "model unavailable", true)
                .expect("retry")
        );
        plane
            .claim_agent_work("retry-in", "turn-2")
            .expect("new attempt");
        plane
            .cancel_agent_work("retry-in", "operator stopped")
            .expect("cancel");
        assert!(matches!(
            plane.claim_agent_work("retry-in", "turn-3"),
            Err(WeixinRuntimeError::MessageNotSendable(
                RemoteControlState::Cancelled
            ))
        ));
    }

    #[test]
    fn stale_polled_batch_is_rejected_when_it_is_not_a_duplicate() {
        let mut plane = fixture_plane();
        login(&mut plane);
        let batch = PollBatch::new(
            "new-cursor",
            vec![IlinkMessage::text("stale-in", "peer-1", "hello").expect("message")],
        )
        .expect("batch");
        assert!(matches!(
            plane.accept_polled_batch(
                crate::PolledBatch {
                    requested_cursor: "wrong-cursor".to_owned(),
                    batch,
                },
                &ServeOptions::default(),
            ),
            Err(WeixinRuntimeError::CursorConflict)
        ));
    }
}
