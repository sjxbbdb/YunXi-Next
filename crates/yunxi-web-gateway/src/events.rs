//! Bounded in-memory carriers for the two dsh event streams.

use std::collections::VecDeque;

use serde_json::Value;
use yunxi_web_contract::{EventChannel, RpcId, RpcMessage, event_message};

use crate::GatewayError;

pub const MAX_PENDING_EVENTS: usize = 256;

#[derive(Debug)]
pub(crate) struct EventBuffer {
    mux: VecDeque<RpcMessage>,
    host: VecDeque<RpcMessage>,
    next_id: u64,
}

impl EventBuffer {
    pub(crate) fn new() -> Self {
        Self {
            mux: VecDeque::new(),
            host: VecDeque::new(),
            next_id: 1,
        }
    }

    pub(crate) fn publish(
        &mut self,
        channel: EventChannel,
        payload: Value,
    ) -> Result<(), GatewayError> {
        let rpc_id = RpcId::new(format!("event-{}", self.next_id))?;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.publish_with_id(channel, rpc_id, payload)
    }

    pub(crate) fn publish_with_id(
        &mut self,
        channel: EventChannel,
        rpc_id: RpcId,
        payload: Value,
    ) -> Result<(), GatewayError> {
        if self.queue(channel).len() >= MAX_PENDING_EVENTS {
            return Err(GatewayError::EventQueueFull {
                channel,
                capacity: MAX_PENDING_EVENTS,
            });
        }
        let message = event_message(channel, rpc_id, payload)?;
        self.queue(channel).push_back(message);
        Ok(())
    }

    pub(crate) fn drain(&mut self, channel: EventChannel) -> Vec<RpcMessage> {
        self.queue(channel).drain(..).collect()
    }

    pub(crate) fn has_events(&self, channel: EventChannel) -> bool {
        match channel {
            EventChannel::Mux => !self.mux.is_empty(),
            EventChannel::Host => !self.host.is_empty(),
        }
    }

    pub(crate) fn take_bounded(
        &mut self,
        channel: EventChannel,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> Result<Vec<RpcMessage>, GatewayError> {
        let mut events = Vec::new();
        let mut encoded_bytes = 0usize;
        while events.len() < maximum_events {
            let Some(next_bytes) = self
                .queue(channel)
                .front()
                .map(RpcMessage::encode)
                .transpose()?
            else {
                break;
            };
            let Some(next_total) = encoded_bytes.checked_add(next_bytes.len()) else {
                break;
            };
            if next_total > maximum_encoded_bytes {
                break;
            }
            let Some(message) = self.queue(channel).pop_front() else {
                break;
            };
            encoded_bytes = next_total;
            events.push(message);
        }
        Ok(events)
    }

    fn queue(&mut self, channel: EventChannel) -> &mut VecDeque<RpcMessage> {
        match channel {
            EventChannel::Mux => &mut self.mux,
            EventChannel::Host => &mut self.host,
        }
    }
}
