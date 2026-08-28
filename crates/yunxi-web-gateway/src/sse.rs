//! Bounded Server-Sent Events framing for the dsh event channels.

use serde_json::json;
use yunxi_web_contract::{EventChannel, RpcId, RpcMessage, event_message};

use crate::GatewayError;

pub const MAX_SSE_EVENTS: usize = 256;
pub const MAX_SSE_RESPONSE_BYTES: usize = 512 * 1024;

const CONNECTED_COMMENT: &[u8] = b": connected\n\n";
const END_COMMENT: &[u8] = b": end\n\n";
const SSE_DATA_PREFIX: &[u8] = b"data: ";
const SSE_DATA_SUFFIX: &[u8] = b"\n\n";

pub(crate) fn encode_events(messages: &[RpcMessage]) -> Result<Vec<u8>, GatewayError> {
    let mut output = Vec::new();
    append_bytes(&mut output, CONNECTED_COMMENT)?;
    for message in messages {
        let encoded = message.encode()?;
        append_bytes(&mut output, SSE_DATA_PREFIX)?;
        append_bytes(&mut output, &encoded)?;
        append_bytes(&mut output, SSE_DATA_SUFFIX)?;
    }
    append_bytes(&mut output, END_COMMENT)?;
    Ok(output)
}

pub(crate) fn error_event(
    channel: EventChannel,
    message: impl Into<String>,
) -> Result<Vec<u8>, GatewayError> {
    let event = event_message(
        channel,
        RpcId::new("event-error")?,
        json!({
            "type": "stream/error",
            "error": {
                "code": "internal",
                "message": message.into(),
                "details": {},
            },
        }),
    )?;
    encode_events(&[event])
}

fn append_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), GatewayError> {
    let Some(length) = output.len().checked_add(bytes.len()) else {
        return Err(GatewayError::ResponseTooLarge {
            kind: "SSE",
            length: usize::MAX,
            maximum: MAX_SSE_RESPONSE_BYTES,
        });
    };
    if length > MAX_SSE_RESPONSE_BYTES {
        return Err(GatewayError::ResponseTooLarge {
            kind: "SSE",
            length,
            maximum: MAX_SSE_RESPONSE_BYTES,
        });
    }
    output.extend_from_slice(bytes);
    Ok(())
}
