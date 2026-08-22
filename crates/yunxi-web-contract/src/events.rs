//! Event-stream channel helpers for dsh's two WebSocket routes.

use serde_json::Value;

use crate::{RpcId, RpcMessage, WebContractError};

pub const EVENTS_MUX_METHOD: &str = "events.mux";
pub const EVENTS_HOST_METHOD: &str = "events.host";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventChannel {
    Mux,
    Host,
}

impl EventChannel {
    pub fn method(self) -> &'static str {
        match self {
            Self::Mux => EVENTS_MUX_METHOD,
            Self::Host => EVENTS_HOST_METHOD,
        }
    }
}

pub fn event_message(
    channel: EventChannel,
    rpc_id: RpcId,
    payload: Value,
) -> Result<RpcMessage, WebContractError> {
    RpcMessage::server_request(rpc_id, channel.method(), payload)
}

pub fn parse_event_message(
    message: &RpcMessage,
) -> Result<(EventChannel, &RpcId, &Value), WebContractError> {
    let RpcMessage::ServerRequest(request) = message else {
        return Err(WebContractError::InvalidMessage {
            message: "event stream message must be a server-request".to_string(),
        });
    };
    let rpc_id = request.rpc_id();
    let method = request.method();
    let payload = request.payload();
    let channel = match method {
        EVENTS_MUX_METHOD => EventChannel::Mux,
        EVENTS_HOST_METHOD => EventChannel::Host,
        _ => {
            return Err(WebContractError::InvalidEventChannel {
                method: method.to_string(),
            });
        }
    };
    Ok((channel, rpc_id, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_helpers_preserve_the_dsh_stream_method() {
        let message = event_message(
            EventChannel::Mux,
            RpcId::new("event-1").expect("id"),
            json!({ "type": "stream/error" }),
        )
        .expect("event");
        let (channel, id, payload) = parse_event_message(&message).expect("parse event");
        assert_eq!(channel, EventChannel::Mux);
        assert_eq!(id.as_str(), "event-1");
        assert_eq!(payload["type"], "stream/error");
    }

    #[test]
    fn ordinary_server_requests_are_not_misidentified_as_events() {
        let message = RpcMessage::server_request(
            RpcId::new("rpc-1").expect("id"),
            "approval.request",
            json!({}),
        )
        .expect("request");
        let error = parse_event_message(&message).expect_err("not an event");
        assert!(matches!(
            error,
            WebContractError::InvalidEventChannel { .. }
        ));
    }
}
