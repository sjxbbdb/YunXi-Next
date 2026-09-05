# Executable Entry Points

## `yunxi-weixin-fixture`

`yunxi-weixin-fixture` is a deterministic, single-process JSONL loopback for
the `channel.weixin@1` boundary and its `weixin.inbound@1` and
`weixin.outbound@1` contracts. It is useful for host integration and contract
tests without a Weixin account or external service.

Input is one JSON object per line. The process writes one JSON response per
command and keeps running after malformed input or a contract state error.

Supported commands:

```text
{"op":"describe"}
{"op":"fixture","direction":"inbound"}
{"op":"fixture","direction":"outbound"}
{"op":"inbound","message":{...}}
{"op":"outbound","message":{...}}
{"op":"ack","idempotency_key":"..."}
{"op":"cancel","idempotency_key":"...","reason":"..."}
{"op":"fail","idempotency_key":"...","reason":"..."}
{"op":"shutdown"}
```

The `fixture` commands use the canonical library fixtures. The explicit
`inbound` and `outbound` commands accept serialized contract messages. A
repeated immutable message with the same idempotency key is reported as a
duplicate even after its delivery state changes; a different message using
that key is rejected as a conflict.

The loopback deliberately does **not** implement Weixin login, encryption,
network transport, device integration, media bytes, or a Weixin SDK.

## Source Index

- `yunxi-weixin-fixture.rs`: bounded JSONL framing, command dispatch, in-memory
  idempotency registry, and delivery state transitions.
- `../lib.rs`: public contract facade and exports.
- `../message.rs`: typed inbound/outbound message contracts.
- `../state.rs`: ACK, retry, cancellation, backpressure, and delivery state
  machine.
- `../fixture.rs`: canonical inbound/outbound fixture messages.
- `../identifiers.rs`: bounded IDs and idempotency keys.
- `../media.rs`: bounded media metadata.
- `../error.rs`: contract validation errors.
