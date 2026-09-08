//! Host-facing, bounded state machine for handing inbound messages to an Agent.
//!
//! The channel never runs an Agent itself. A Host claims work, invokes its
//! Agent, stages the exact outbound message, and commits it after transport
//! success. This keeps retries and duplicate deliveries deterministic.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use serde::Serialize;

use crate::IlinkMessage;

pub const MAX_AGENT_ATTEMPTS: u16 = 8;
pub const MAX_AGENT_ATTEMPT_ID_BYTES: usize = 128;
pub const MAX_AGENT_ERROR_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBridgeState {
    Pending,
    Running,
    ReplyReady,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentWorkItem {
    pub message_id: String,
    pub attempt_id: String,
    pub attempt: u16,
    pub message: IlinkMessage,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentBridgeSnapshot {
    pub message_id: String,
    pub state: AgentBridgeState,
    pub attempt: u16,
    pub max_attempts: u16,
    pub active_attempt_id: Option<String>,
    pub reply_message_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub enum AgentBridgeError {
    CapacityExceeded,
    InvalidAttemptId,
    InvalidError,
    UnknownMessage,
    AttemptConflict,
    InvalidTransition {
        from: AgentBridgeState,
        to: AgentBridgeState,
    },
    RetryExhausted,
    ReplyConflict,
}

impl fmt::Display for AgentBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded => formatter.write_str("Agent bridge queue is full"),
            Self::InvalidAttemptId => formatter.write_str("invalid Agent attempt id"),
            Self::InvalidError => formatter.write_str("invalid Agent bridge error"),
            Self::UnknownMessage => formatter.write_str("Agent bridge message is unknown"),
            Self::AttemptConflict => {
                formatter.write_str("Agent attempt conflicts with active work")
            }
            Self::InvalidTransition { from, to } => {
                write!(
                    formatter,
                    "invalid Agent bridge transition from {from:?} to {to:?}"
                )
            }
            Self::RetryExhausted => formatter.write_str("Agent bridge retry budget is exhausted"),
            Self::ReplyConflict => formatter.write_str("Agent reply conflicts with staged reply"),
        }
    }
}

impl std::error::Error for AgentBridgeError {}

#[derive(Clone)]
struct Entry {
    message: IlinkMessage,
    state: AgentBridgeState,
    attempt: u16,
    max_attempts: u16,
    active_attempt_id: Option<String>,
    last_attempt_id: Option<String>,
    reply: Option<IlinkMessage>,
    last_error: Option<String>,
}

/// A bounded idempotent queue that is deliberately independent of any Agent
/// implementation. It can be used by a CLI, Web host, or another adapter.
#[derive(Clone, Default)]
pub struct AgentBridge {
    entries: BTreeMap<String, Entry>,
    order: VecDeque<String>,
}

impl fmt::Debug for AgentBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentBridge")
            .field("entries", &self.entries.len())
            .field("messages", &"redacted")
            .finish()
    }
}

impl AgentBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn offer(&mut self, message: IlinkMessage) -> Result<bool, AgentBridgeError> {
        message
            .validate()
            .map_err(|_| AgentBridgeError::ReplyConflict)?;
        let key = message.message_id.clone();
        if let Some(existing) = self.entries.get(&key) {
            if existing.message == message {
                return Ok(true);
            }
            return Err(AgentBridgeError::ReplyConflict);
        }
        if self.entries.len() >= crate::runtime::MAX_QUEUE_ENTRIES {
            return Err(AgentBridgeError::CapacityExceeded);
        }
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            Entry {
                message,
                state: AgentBridgeState::Pending,
                attempt: 0,
                max_attempts: MAX_AGENT_ATTEMPTS,
                active_attempt_id: None,
                last_attempt_id: None,
                reply: None,
                last_error: None,
            },
        );
        Ok(false)
    }

    pub fn claim(
        &mut self,
        message_id: &str,
        attempt_id: impl Into<String>,
    ) -> Result<AgentWorkItem, AgentBridgeError> {
        let attempt_id = valid_attempt_id(attempt_id.into())?;
        let entry = self
            .entries
            .get_mut(message_id)
            .ok_or(AgentBridgeError::UnknownMessage)?;
        match entry.state {
            AgentBridgeState::Pending => {
                if entry.attempt >= entry.max_attempts {
                    return Err(AgentBridgeError::RetryExhausted);
                }
                entry.attempt += 1;
                entry.state = AgentBridgeState::Running;
                entry.active_attempt_id = Some(attempt_id.clone());
                entry.last_attempt_id = Some(attempt_id.clone());
                Ok(work_item(message_id, entry, attempt_id))
            }
            AgentBridgeState::Running
                if entry.active_attempt_id.as_deref() == Some(&attempt_id) =>
            {
                Ok(work_item(message_id, entry, attempt_id))
            }
            AgentBridgeState::Running => Err(AgentBridgeError::AttemptConflict),
            AgentBridgeState::ReplyReady
                if entry.active_attempt_id.as_deref() == Some(&attempt_id) =>
            {
                Ok(work_item(message_id, entry, attempt_id))
            }
            AgentBridgeState::Completed => Err(AgentBridgeError::InvalidTransition {
                from: entry.state,
                to: AgentBridgeState::Running,
            }),
            AgentBridgeState::Failed => Err(AgentBridgeError::RetryExhausted),
            AgentBridgeState::Cancelled => Err(AgentBridgeError::InvalidTransition {
                from: entry.state,
                to: AgentBridgeState::Running,
            }),
            AgentBridgeState::ReplyReady => Err(AgentBridgeError::AttemptConflict),
        }
    }

    pub fn claim_next(
        &mut self,
        attempt_id: impl Into<String>,
        maximum: usize,
    ) -> Result<Option<AgentWorkItem>, AgentBridgeError> {
        if maximum == 0 {
            return Ok(None);
        }
        let attempt_id = valid_attempt_id(attempt_id.into())?;
        let key = self
            .order
            .iter()
            .take(maximum)
            .find(|key| {
                self.entries.get(*key).is_some_and(|entry| {
                    entry.state == AgentBridgeState::Pending && entry.attempt < entry.max_attempts
                })
            })
            .cloned();
        key.map(|key| self.claim(&key, attempt_id)).transpose()
    }

    pub fn stage_reply(
        &mut self,
        message_id: &str,
        attempt_id: &str,
        reply: IlinkMessage,
    ) -> Result<bool, AgentBridgeError> {
        valid_attempt_id(attempt_id.to_owned())?;
        reply
            .validate()
            .map_err(|_| AgentBridgeError::ReplyConflict)?;
        let entry = self
            .entries
            .get_mut(message_id)
            .ok_or(AgentBridgeError::UnknownMessage)?;
        if entry.active_attempt_id.as_deref() != Some(attempt_id) {
            return Err(AgentBridgeError::AttemptConflict);
        }
        match entry.state {
            AgentBridgeState::Running => {
                entry.reply = Some(reply);
                entry.state = AgentBridgeState::ReplyReady;
                Ok(false)
            }
            AgentBridgeState::ReplyReady if entry.reply.as_ref() == Some(&reply) => Ok(true),
            AgentBridgeState::ReplyReady => Err(AgentBridgeError::ReplyConflict),
            state => Err(AgentBridgeError::InvalidTransition {
                from: state,
                to: AgentBridgeState::ReplyReady,
            }),
        }
    }

    pub fn staged_reply(
        &self,
        message_id: &str,
        attempt_id: &str,
    ) -> Result<Option<&IlinkMessage>, AgentBridgeError> {
        let entry = self
            .entries
            .get(message_id)
            .ok_or(AgentBridgeError::UnknownMessage)?;
        if entry.active_attempt_id.as_deref() != Some(attempt_id) {
            return Err(AgentBridgeError::AttemptConflict);
        }
        Ok(entry.reply.as_ref())
    }

    pub fn complete(
        &mut self,
        message_id: &str,
        attempt_id: &str,
    ) -> Result<bool, AgentBridgeError> {
        let entry = self
            .entries
            .get_mut(message_id)
            .ok_or(AgentBridgeError::UnknownMessage)?;
        if entry.active_attempt_id.as_deref() != Some(attempt_id) {
            return Err(AgentBridgeError::AttemptConflict);
        }
        match entry.state {
            AgentBridgeState::ReplyReady => {
                entry.state = AgentBridgeState::Completed;
                Ok(false)
            }
            AgentBridgeState::Completed => Ok(true),
            state => Err(AgentBridgeError::InvalidTransition {
                from: state,
                to: AgentBridgeState::Completed,
            }),
        }
    }

    pub fn fail(
        &mut self,
        message_id: &str,
        attempt_id: &str,
        reason: impl Into<String>,
        retryable: bool,
    ) -> Result<bool, AgentBridgeError> {
        let reason = valid_error(reason.into())?;
        let entry = self
            .entries
            .get_mut(message_id)
            .ok_or(AgentBridgeError::UnknownMessage)?;
        if entry.last_attempt_id.as_deref() == Some(attempt_id)
            && matches!(
                entry.state,
                AgentBridgeState::Pending | AgentBridgeState::Failed
            )
            && entry.last_error.as_deref() == Some(reason.as_str())
        {
            return Ok(true);
        }
        if entry.active_attempt_id.as_deref() != Some(attempt_id) {
            return Err(AgentBridgeError::AttemptConflict);
        }
        entry.last_error = Some(reason);
        entry.active_attempt_id = None;
        entry.reply = None;
        if retryable && entry.attempt < entry.max_attempts {
            entry.state = AgentBridgeState::Pending;
        } else if retryable {
            entry.state = AgentBridgeState::Failed;
            return Err(AgentBridgeError::RetryExhausted);
        } else {
            entry.state = AgentBridgeState::Failed;
        }
        Ok(false)
    }

    pub fn cancel(
        &mut self,
        message_id: &str,
        reason: impl Into<String>,
    ) -> Result<bool, AgentBridgeError> {
        let reason = valid_error(reason.into())?;
        let entry = self
            .entries
            .get_mut(message_id)
            .ok_or(AgentBridgeError::UnknownMessage)?;
        if entry.state == AgentBridgeState::Cancelled {
            return Ok(true);
        }
        if matches!(
            entry.state,
            AgentBridgeState::Completed | AgentBridgeState::Failed
        ) {
            return Err(AgentBridgeError::InvalidTransition {
                from: entry.state,
                to: AgentBridgeState::Cancelled,
            });
        }
        entry.state = AgentBridgeState::Cancelled;
        entry.active_attempt_id = None;
        entry.reply = None;
        entry.last_error = Some(reason);
        Ok(false)
    }

    pub fn snapshot(&self, message_id: &str) -> Result<AgentBridgeSnapshot, AgentBridgeError> {
        let entry = self
            .entries
            .get(message_id)
            .ok_or(AgentBridgeError::UnknownMessage)?;
        Ok(snapshot(message_id, entry))
    }

    pub fn snapshots(&self, maximum: usize) -> Vec<AgentBridgeSnapshot> {
        self.order
            .iter()
            .take(maximum)
            .filter_map(|key| self.entries.get(key).map(|entry| snapshot(key, entry)))
            .collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

fn work_item(message_id: &str, entry: &Entry, attempt_id: String) -> AgentWorkItem {
    AgentWorkItem {
        message_id: message_id.to_owned(),
        attempt_id,
        attempt: entry.attempt,
        message: entry.message.clone(),
    }
}

fn snapshot(message_id: &str, entry: &Entry) -> AgentBridgeSnapshot {
    AgentBridgeSnapshot {
        message_id: message_id.to_owned(),
        state: entry.state,
        attempt: entry.attempt,
        max_attempts: entry.max_attempts,
        active_attempt_id: entry.active_attempt_id.clone(),
        reply_message_id: entry.reply.as_ref().map(|reply| reply.message_id.clone()),
        last_error: entry.last_error.clone(),
    }
}

fn valid_attempt_id(value: String) -> Result<String, AgentBridgeError> {
    if value.is_empty()
        || value.len() > MAX_AGENT_ATTEMPT_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AgentBridgeError::InvalidAttemptId);
    }
    Ok(value)
}

fn valid_error(value: String) -> Result<String, AgentBridgeError> {
    if value.is_empty()
        || value.len() > MAX_AGENT_ERROR_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AgentBridgeError::InvalidError);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str) -> IlinkMessage {
        IlinkMessage::text(id, "peer", "hello").expect("message")
    }

    fn reply(id: &str) -> IlinkMessage {
        IlinkMessage::reply_text(id, "peer", "context", "reply").expect("reply")
    }

    #[test]
    fn claim_stage_complete_and_retry_are_idempotent() {
        let mut bridge = AgentBridge::new();
        assert!(!bridge.offer(message("in-1")).expect("offer"));
        let work = bridge.claim("in-1", "attempt-1").expect("claim");
        assert_eq!(work.attempt, 1);
        assert!(
            bridge
                .claim("in-1", "attempt-1")
                .expect("duplicate claim")
                .eq(&work)
        );
        assert!(
            !bridge
                .stage_reply("in-1", "attempt-1", reply("out-1"))
                .expect("stage")
        );
        assert!(
            bridge
                .stage_reply("in-1", "attempt-1", reply("out-1"))
                .expect("duplicate stage")
        );
        assert!(!bridge.complete("in-1", "attempt-1").expect("complete"));
        assert!(
            bridge
                .complete("in-1", "attempt-1")
                .expect("duplicate complete")
        );
    }

    #[test]
    fn retry_requires_a_new_attempt_and_is_bounded() {
        let mut bridge = AgentBridge::new();
        bridge.offer(message("in-1")).expect("offer");
        bridge.claim("in-1", "attempt-1").expect("claim");
        assert!(
            !bridge
                .fail("in-1", "attempt-1", "temporary", true)
                .expect("retry")
        );
        let work = bridge.claim("in-1", "attempt-2").expect("second claim");
        assert_eq!(work.attempt, 2);
        assert!(matches!(
            bridge.fail("in-1", "attempt-2", "permanent", false),
            Ok(false)
        ));
        assert!(matches!(
            bridge.claim("in-1", "attempt-3"),
            Err(AgentBridgeError::RetryExhausted)
        ));
    }
}
