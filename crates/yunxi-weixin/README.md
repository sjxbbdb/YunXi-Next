# yunxi-weixin

`yunxi-weixin` is a pure Rust contract crate plus a deterministic loopback
fixture for the first process-isolated Weixin migration baseline. It exercises the
`channel.weixin@1` boundary and defines the versioned capability contracts
`weixin.inbound@1` and `weixin.outbound@1`.

The crate contains no Weixin SDK, network client, credentials, device access,
or async runtime. Media is represented by bounded metadata only; this crate
does not carry media bytes or fetch remote content. Its process fixture is
launch-wired into the CLI/Web Host and appears in inventory when `weixin` is
enabled, proving the replaceable channel boundary without claiming a real
login or network integration.

## Contract

- `MessageId`, `SessionId`, `RequestId`, `IdempotencyKey`, `ParticipantId`, and
  `MediaId` are bounded ASCII tokens and are checked during construction and
  serde deserialization.
- `MessageEnvelope` carries the message/session/request identity, direction,
  sender and recipient, idempotency key, bounded text, bounded media metadata,
  and `DeliveryStatus`.
- `InboundMessage` and `OutboundMessage` are typed wrappers that reject a
  mismatched direction, so a Host cannot accidentally route one contract as
  the other.
- Delivery status models acknowledgement, bounded retries, cancellation, and
  backpressure. State transitions and capacity relationships are validated.
- All public structs use strict serde wire forms with `deny_unknown_fields`;
  malformed or oversized values are rejected on deserialization.
- Deterministic `inbound_fixture()` and `outbound_fixture()` values exercise
  JSON round trips and rejection cases in the test suite.

## Fixture boundary

`yunxi-weixin-fixture` is a single-process JSONL loopback for contract tests.
`yunxi-weixin-plugin-fixture` is the process-host fixture that exercises the
versioned handshake, required `Network`/`Secret` manifest declarations,
typed channel routing, idempotency, delivery mutation, and shutdown. Run the
standalone contract loop from the workspace with:

```powershell
cargo run -p yunxi-weixin --bin yunxi-weixin-fixture
```

It emits a `ready` frame and then accepts one JSON command per line:

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

Registering the same message again is reported as a duplicate. Reusing an
idempotency key for a different message is rejected as a conflict. ACK,
cancellation, and failure commands expose the contract state transitions;
invalid transitions return an error frame and keep the loop alive. Input and
output frames are bounded.

The Host integration test launches `yunxi-weixin-plugin-fixture` and checks the
exact `channel.weixin@1` capability contract, required grants, idempotent
inbound/ACK behavior, malformed payload handling, and route removal after
disable. The fixture's required grants describe what a real adapter would need;
they do not grant anything to the fixture and do not implement login or
network access.

This is deliberately not a real Weixin integration. It does not implement
login, encryption, network transport, device integration, media bytes, or a
Weixin SDK, and it performs no external side effect. The `real_weixin` field in
the fixture description is false by design.

## Source index

- `src/bin/yunxi-weixin-fixture.rs`: bounded JSONL framing, command dispatch,
  in-memory idempotency registry, and delivery state transitions.
- `src/fixture.rs`: canonical inbound/outbound fixture messages.
- `src/message.rs`: typed direction and versioned message contracts.
- `src/state.rs`: ACK, retry, cancellation, backpressure, and delivery state
  machine.
- `src/identifiers.rs`: bounded IDs and idempotency keys.
- `src/media.rs`: bounded media metadata.
- `src/error.rs`: contract validation errors.

The package remains independently checkable with package-scoped Cargo commands;
the parent workspace owns workspace membership and lockfile policy.

## Verify

Run these commands from this crate directory:

```powershell
cargo fmt --all -- --check
cargo check -p yunxi-weixin --all-targets
cargo clippy -p yunxi-weixin --all-targets -- -D warnings
cargo test -p yunxi-weixin --all-targets
```
