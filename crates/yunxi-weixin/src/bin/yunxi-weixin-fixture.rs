//! Deterministic JSONL loopback for the versioned Weixin channel contracts.
//!
//! This executable is intentionally a fixture boundary, not a Weixin client. It
//! has no login, encryption, network, device, media transport, or SDK code.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use serde::Deserialize;
use serde_json::{Value, json};
use yunxi_weixin::{
    IdempotencyKey, InboundMessage, OutboundMessage, WeixinContractError, inbound_fixture,
    outbound_fixture,
};

const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_ERROR_BYTES: usize = 4096;
const CHANNEL_CONTRACT: &str = "channel.weixin@1";
const PROTOCOL: &str = "yunxi.weixin.fixture@1";

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Describe {},
    Fixture {
        direction: FixtureDirection,
    },
    Inbound {
        message: InboundMessage,
    },
    Outbound {
        message: OutboundMessage,
    },
    Ack {
        idempotency_key: IdempotencyKey,
    },
    Cancel {
        idempotency_key: IdempotencyKey,
        reason: String,
    },
    Fail {
        idempotency_key: IdempotencyKey,
        reason: String,
    },
    Shutdown {},
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StoredMessage {
    Inbound(InboundMessage),
    Outbound(OutboundMessage),
}

impl StoredMessage {
    fn idempotency_key(&self) -> &str {
        match self {
            Self::Inbound(message) => message.envelope().idempotency_key.as_str(),
            Self::Outbound(message) => message.envelope().idempotency_key.as_str(),
        }
    }

    fn direction(&self) -> &'static str {
        match self {
            Self::Inbound(_) => "inbound",
            Self::Outbound(_) => "outbound",
        }
    }

    fn as_value(&self) -> Value {
        match self {
            Self::Inbound(message) => json!({
                "direction": "inbound",
                "message": message,
            }),
            Self::Outbound(message) => json!({
                "direction": "outbound",
                "message": message,
            }),
        }
    }

    fn same_message(&self, other: &Self) -> bool {
        let (left, right) = match (self, other) {
            (Self::Inbound(left), Self::Inbound(right)) => (left.envelope(), right.envelope()),
            (Self::Outbound(left), Self::Outbound(right)) => (left.envelope(), right.envelope()),
            _ => return false,
        };
        left.message_id == right.message_id
            && left.session_id == right.session_id
            && left.request_id == right.request_id
            && left.idempotency_key == right.idempotency_key
            && left.direction == right.direction
            && left.sender_id == right.sender_id
            && left.recipient_id == right.recipient_id
            && left.sent_at_ms == right.sent_at_ms
            && left.content == right.content
    }

    fn acknowledge(&mut self) -> Result<(), WeixinContractError> {
        match self {
            Self::Inbound(message) => {
                let mut envelope = message.clone().into_envelope();
                envelope.delivery.acknowledge()?;
                *message = InboundMessage::new(envelope)?;
            }
            Self::Outbound(message) => {
                let mut envelope = message.clone().into_envelope();
                envelope.delivery.acknowledge()?;
                *message = OutboundMessage::new(envelope)?;
            }
        }
        Ok(())
    }

    fn cancel(&mut self, reason: String) -> Result<(), WeixinContractError> {
        match self {
            Self::Inbound(message) => {
                let mut envelope = message.clone().into_envelope();
                envelope.delivery.request_cancel(reason)?;
                envelope.delivery.mark_cancelled()?;
                *message = InboundMessage::new(envelope)?;
            }
            Self::Outbound(message) => {
                let mut envelope = message.clone().into_envelope();
                envelope.delivery.request_cancel(reason)?;
                envelope.delivery.mark_cancelled()?;
                *message = OutboundMessage::new(envelope)?;
            }
        }
        Ok(())
    }

    fn fail(&mut self, reason: String) -> Result<(), WeixinContractError> {
        match self {
            Self::Inbound(message) => {
                let mut envelope = message.clone().into_envelope();
                envelope.delivery.mark_failed(reason)?;
                *message = InboundMessage::new(envelope)?;
            }
            Self::Outbound(message) => {
                let mut envelope = message.clone().into_envelope();
                envelope.delivery.mark_failed(reason)?;
                *message = OutboundMessage::new(envelope)?;
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct FixtureRuntime {
    messages: BTreeMap<String, StoredMessage>,
}

impl FixtureRuntime {
    fn dispatch(&mut self, command: Command) -> (Value, bool) {
        match command {
            Command::Describe {} => (describe_response(), false),
            Command::Fixture { direction } => {
                let message = match direction {
                    FixtureDirection::Inbound => match inbound_fixture() {
                        Ok(fixture) => StoredMessage::Inbound(fixture.message),
                        Err(error) => return (contract_error("fixture", error), false),
                    },
                    FixtureDirection::Outbound => match outbound_fixture() {
                        Ok(fixture) => StoredMessage::Outbound(fixture.message),
                        Err(error) => return (contract_error("fixture", error), false),
                    },
                };
                (self.register(message), false)
            }
            Command::Inbound { message } => (self.register(StoredMessage::Inbound(message)), false),
            Command::Outbound { message } => {
                (self.register(StoredMessage::Outbound(message)), false)
            }
            Command::Ack { idempotency_key } => (
                self.mutate(&idempotency_key, |message| message.acknowledge(), "ack"),
                false,
            ),
            Command::Cancel {
                idempotency_key,
                reason,
            } => (
                self.mutate(&idempotency_key, |message| message.cancel(reason), "cancel"),
                false,
            ),
            Command::Fail {
                idempotency_key,
                reason,
            } => (
                self.mutate(&idempotency_key, |message| message.fail(reason), "fail"),
                false,
            ),
            Command::Shutdown {} => (
                json!({
                    "ok": true,
                    "event": "shutdown",
                    "protocol": PROTOCOL,
                    "stored_messages": self.messages.len(),
                }),
                true,
            ),
        }
    }

    fn register(&mut self, message: StoredMessage) -> Value {
        let key = message.idempotency_key().to_owned();
        if let Some(existing) = self.messages.get(&key) {
            if existing.same_message(&message) {
                return json!({
                    "ok": true,
                    "event": "message.duplicate",
                    "duplicate": true,
                    "idempotency_key": key,
                    "message": existing.as_value(),
                });
            }
            return json!({
                "ok": false,
                "error": {
                    "code": "idempotency_conflict",
                    "message": "idempotency key is already bound to a different message",
                    "idempotency_key": key,
                },
            });
        }

        let direction = message.direction();
        let value = message.as_value();
        self.messages.insert(key.clone(), message);
        json!({
            "ok": true,
            "event": "message.accepted",
            "duplicate": false,
            "direction": direction,
            "idempotency_key": key,
            "message": value,
        })
    }

    fn mutate<F>(
        &mut self,
        key: &IdempotencyKey,
        operation: F,
        operation_name: &'static str,
    ) -> Value
    where
        F: FnOnce(&mut StoredMessage) -> Result<(), WeixinContractError>,
    {
        let Some(message) = self.messages.get_mut(key.as_str()) else {
            return json!({
                "ok": false,
                "error": {
                    "code": "unknown_idempotency_key",
                    "message": "no message is stored for this idempotency key",
                    "idempotency_key": key.as_str(),
                },
            });
        };

        if let Err(error) = operation(message) {
            return contract_error(operation_name, error);
        }
        json!({
            "ok": true,
            "event": format!("message.{operation_name}"),
            "idempotency_key": key.as_str(),
            "message": message.as_value(),
        })
    }
}

fn describe_response() -> Value {
    json!({
            "ok": true,
            "event": "describe",
            "plugin": "yunxi-weixin-fixture",
            "channel": CHANNEL_CONTRACT,
            "protocol": PROTOCOL,
        "mode": "loopback",
        "capabilities": ["weixin.inbound@1", "weixin.outbound@1"],
        "supports": ["ack", "idempotency", "cancel", "failure"],
        "real_weixin": false,
        "boundary": [
            "no login",
            "no encryption",
            "no network",
            "no device",
            "no Weixin SDK",
        ],
    })
}

fn contract_error(operation: &str, error: WeixinContractError) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": "contract_error",
            "operation": operation,
            "message": bounded_error(error.to_string()),
        },
    })
}

fn bounded_error(message: String) -> String {
    message.chars().take(MAX_ERROR_BYTES).collect()
}

enum Frame {
    Line(Vec<u8>),
    Oversized,
}

fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<Frame>> {
    let mut frame = Vec::with_capacity(4096);
    let mut oversized = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if frame.is_empty() && !oversized {
                return Ok(None);
            }
            return Ok(Some(if oversized {
                Frame::Oversized
            } else {
                Frame::Line(frame)
            }));
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        if !oversized {
            if frame.len().saturating_add(take) > MAX_FRAME_BYTES {
                oversized = true;
            } else {
                frame.extend_from_slice(&available[..take]);
            }
        }
        reader.consume(take);
        if newline.is_some() {
            return Ok(Some(if oversized {
                Frame::Oversized
            } else {
                Frame::Line(frame)
            }));
        }
    }
}

fn parse_frame(frame: Vec<u8>) -> Result<Option<Command>, Value> {
    let mut frame = frame;
    while matches!(frame.last(), Some(b'\n' | b'\r')) {
        frame.pop();
    }
    if frame.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let text = std::str::from_utf8(&frame).map_err(|_| {
        json!({
            "ok": false,
            "error": {"code": "invalid_utf8", "message": "input frame is not UTF-8"},
        })
    })?;
    serde_json::from_str(text).map(Some).map_err(|error| {
        json!({
            "ok": false,
            "error": {
                "code": "invalid_command",
                "message": bounded_error(error.to_string()),
            },
        })
    })
}

fn write_response<W: Write>(writer: &mut W, response: Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&response)
        .map_err(|error| io::Error::other(format!("serialize response: {error}")))?;
    if bytes.len() > MAX_FRAME_BYTES {
        bytes = br#"{"ok":false,"error":{"code":"response_too_large","message":"response exceeds fixture frame bound"}}"#.to_vec();
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn run<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    write_response(
        writer,
        json!({
            "ok": true,
            "event": "ready",
            "plugin": "yunxi-weixin-fixture",
            "channel": CHANNEL_CONTRACT,
            "protocol": PROTOCOL,
        }),
    )?;
    let mut runtime = FixtureRuntime::default();
    loop {
        let Some(frame) = read_frame(reader)? else {
            return Ok(());
        };
        let response = match frame {
            Frame::Oversized => json!({
                "ok": false,
                "error": {
                    "code": "frame_too_large",
                    "message": format!("input frame exceeds {MAX_FRAME_BYTES} bytes"),
                },
            }),
            Frame::Line(frame) => match parse_frame(frame) {
                Ok(None) => continue,
                Ok(Some(command)) => {
                    let (response, shutdown) = runtime.dispatch(command);
                    write_response(writer, response)?;
                    if shutdown {
                        return Ok(());
                    }
                    continue;
                }
                Err(response) => response,
            },
        };
        write_response(writer, response)?;
    }
}

fn main() -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    match run(&mut reader, &mut writer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("yunxi-weixin-fixture: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn output_for(input: &str) -> Vec<Value> {
        let mut output = Vec::new();
        run(&mut Cursor::new(input.as_bytes()), &mut output).expect("fixture loop runs");
        String::from_utf8(output)
            .expect("fixture output is UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("fixture output is JSON"))
            .collect()
    }

    #[test]
    fn jsonl_loopback_covers_inbound_ack_and_idempotency() {
        let output = output_for(
            r#"{"op":"fixture","direction":"inbound"}
{"op":"fixture","direction":"inbound"}
{"op":"ack","idempotency_key":"fixture-inbound-key"}
{"op":"shutdown"}
"#,
        );
        assert_eq!(output[0]["event"], "ready");
        assert_eq!(output[1]["event"], "message.accepted");
        assert_eq!(output[2]["event"], "message.duplicate");
        assert_eq!(output[2]["duplicate"], true);
        assert_eq!(output[3]["event"], "message.ack");
        assert_eq!(
            output[3]["message"]["message"]["delivery"]["acknowledgement"],
            "acknowledged"
        );
        assert_eq!(output[4]["event"], "shutdown");
    }

    #[test]
    fn cancellation_and_failure_are_explicit_boundaries() {
        let output = output_for(
            r#"{"op":"fixture","direction":"outbound"}
{"op":"fail","idempotency_key":"fixture-outbound-key","reason":"fixture failure"}
{"op":"cancel","idempotency_key":"fixture-outbound-key","reason":"user cancelled"}
{"op":"cancel","idempotency_key":"fixture-outbound-key","reason":"duplicate cancel"}
{"op":"shutdown"}
"#,
        );
        assert_eq!(
            output[2]["message"]["message"]["delivery"]["state"],
            "failed"
        );
        assert_eq!(
            output[3]["message"]["message"]["delivery"]["cancellation"],
            "cancelled"
        );
        assert_eq!(output[4]["ok"], false);
        assert_eq!(output[4]["error"]["code"], "contract_error");
    }

    #[test]
    fn oversized_frames_do_not_stop_the_loop() {
        let oversized = "x".repeat(MAX_FRAME_BYTES);
        let input = format!("{oversized}\n{{\"op\":\"describe\"}}\n{{\"op\":\"shutdown\"}}\n");
        let output = output_for(&input);
        assert_eq!(output[1]["error"]["code"], "frame_too_large");
        assert_eq!(output[2]["event"], "describe");
        assert_eq!(output[3]["event"], "shutdown");
    }
}
