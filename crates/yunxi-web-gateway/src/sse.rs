//! Bounded Server-Sent Events framing for the dsh event channels.

use serde_json::json;
use yunxi_web_contract::{EventChannel, RpcId, event_message};

use crate::GatewayError;
use crate::events::{EventRecord, ReplayGap, ReplayPage};

pub const MAX_SSE_EVENTS: usize = 256;
pub const MAX_SSE_RESPONSE_BYTES: usize = 512 * 1024;

const CONNECTED_COMMENT: &[u8] = b": connected\n\n";
const END_COMMENT: &[u8] = b": end\n\n";
const SSE_DATA_PREFIX: &[u8] = b"data: ";
const SSE_DATA_SUFFIX: &[u8] = b"\n\n";

pub(crate) fn encode_page(
    channel: EventChannel,
    page: &ReplayPage,
) -> Result<Vec<u8>, GatewayError> {
    let mut output = Vec::new();
    append_bytes(&mut output, CONNECTED_COMMENT)?;
    if let Some(gap) = &page.replay_gap {
        append_record(&mut output, &replay_gap_event(channel, gap)?)?;
    }
    for record in &page.events {
        append_record(&mut output, record)?;
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
    let record = EventRecord {
        sequence: 0,
        encoded_bytes: event.encode()?.len(),
        message: event,
    };
    let mut output = Vec::new();
    append_bytes(&mut output, CONNECTED_COMMENT)?;
    append_record(&mut output, &record)?;
    append_bytes(&mut output, END_COMMENT)?;
    Ok(output)
}

fn append_record(output: &mut Vec<u8>, record: &EventRecord) -> Result<(), GatewayError> {
    let encoded = record.message.encode()?;
    if record.sequence > 0 {
        append_bytes(output, b"id: ")?;
        append_bytes(output, record.sequence.to_string().as_bytes())?;
        append_bytes(output, b"\n")?;
    }
    append_bytes(output, SSE_DATA_PREFIX)?;
    append_bytes(output, &encoded)?;
    append_bytes(output, SSE_DATA_SUFFIX)
}

fn replay_gap_event(channel: EventChannel, gap: &ReplayGap) -> Result<EventRecord, GatewayError> {
    let event = event_message(
        channel,
        RpcId::new("event-replay-gap")?,
        json!({
            "type": "stream/error",
            "error": {
                // The public RPC error union is closed. Keep the transport
                // diagnostic in the message and response headers instead of
                // emitting a frame the browser schema must discard.
                "code": "internal",
                "message": format!(
                    "replay gap on {}: afterSeq={}, oldestSeq={}, latestSeq={}",
                    channel.method(),
                    gap.after_sequence,
                    gap.oldest_sequence,
                    gap.latest_sequence,
                ),
                "details": {},
            },
        }),
    )?;
    Ok(EventRecord {
        // Keep SSE ids monotonic: the synthetic diagnostic occupies the last
        // missing position, then retained records continue at oldestSeq.
        // When the journal has no retained records, latestSeq is the last
        // available position and remains the correct cursor.
        sequence: gap
            .oldest_sequence
            .checked_sub(1)
            .unwrap_or(gap.latest_sequence),
        encoded_bytes: event.encode()?.len(),
        message: event,
    })
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
