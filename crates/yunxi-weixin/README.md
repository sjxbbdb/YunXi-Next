# yunxi-weixin

`yunxi-weixin` is a pure Rust contract crate plus a replaceable iLink control
plane for the process-isolated Weixin migration. It exercises the
`channel.weixin@1` boundary and defines the versioned capability contracts
`weixin.inbound@1` and `weixin.outbound@1`.

The production boundary is intentionally dependency-light and blocking:
`IlinkHttpTransport` implements the current iLink QR (`GET`), long-poll, and
send endpoints with bounded request/response bodies, the required bearer
headers, and a fresh `X-WECHAT-UIN` value per request. Its default request
budget covers the protocol's 120-second long poll, while callers can impose a
shorter request or cancellation deadline. It accepts numeric message IDs and
the observed `messages`/`next_key` update aliases, and keeps the opaque update
cursor across long-poll retries. `SecretStore` keeps credential
bytes outside configuration and allows a Host to provide durable storage.
`MemorySecretStore` is a bounded process-local implementation. For durable
storage, `FileSecretStore::new(path, master_key)` requires an explicit 32-byte
master key and writes one bounded ChaCha20-Poly1305 authenticated snapshot;
references and secret values are both encrypted, and an invalid key, tampered
file, or corrupt file is rejected without returning file contents. Writes use a
same-directory temporary file followed by a platform-aware replacement, and
removing the last entry removes the store file. `FileSecretStore` serializes
operations made through one instance; separate instances should still be
coordinated by the owning Host. `LoopbackIlinkTransport` remains deterministic
and local; none of these implementations imply that a real Weixin account is
logged in.
Media is represented by bounded metadata only; this crate does not carry media
bytes or fetch remote content.

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
network access. The fixture remains independent from the production iLink
transport.

## External integration boundary

`WeixinControlPlane<T, S>` is the library facade for `login`, `poll_login`,
`status`, `doctor`, bounded long-poll `serve`, `pair`, `session`, remote
`ack/cancel/approval`, context-bound `send_reply_text`, raw `send_message`, and
`logout`. `T` is an
`IlinkTransport`; use `IlinkHttpTransport` for the production endpoint or
`LoopbackIlinkTransport` for deterministic tests.

## Non-blocking Host integration

The synchronous `serve` method remains for API compatibility. The built-in
process plugin also exposes a non-blocking lifecycle over the same
`LongPollWorker<T>` boundary:

```text
serve_start: { "max_polls": 1, "max_messages_per_poll": 128, "require_approval": false }
serve_status: {}
serve_stop: {}
```

`serve_start` is idempotent until `serve_stop`: it returns the existing
`generation` rather than creating another worker. `serve_status` and
`serve_stop` are bounded Host requests; stop requests cancellation and never
waits for a provider call. Each response is a `WeixinPluginResponse::Runtime`
whose `report` has this secret-free shape:

```text
{
  "generation": 1,
  "state": "running | cancellation_requested | completed | failed | stopped",
  "worker": { "state": "...", "polls": 0, "received_messages": 0,
              "retries": 0, "pending_batches": 0, "cursor": "", "last_error": null },
  "report": { "polls": 0, "received_messages": 0, "enqueued_messages": 0,
              "duplicate_messages": 0, "cursor": "" },
  "imported_batches": 0,
  "messages": [],
  "last_error": null
}
```

The worker uses an independent transport copy, so the Host control plane does
not share a network-call mutex with a long poll. On each Host operation the
plugin drains its bounded worker channel and passes every `PolledBatch` to
`WeixinControlPlane::accept_polled_batch`; this validates the cursor, updates
the idempotent inbound queue, and offers new messages to `AgentBridge`. A
worker completion, failed import, explicit stop, logout, or plugin shutdown
requests cancellation and leaves no Host request waiting for long-poll I/O.

A custom Host that must keep its request loop responsive can use
`LongPollWorker<T>` directly instead:

1. Construct it with `LongPollWorker::spawn(transport, cursor, options, context)`.
2. Call `try_next_batch()` from the Host loop and pass each `PolledBatch` to
   `WeixinControlPlane::accept_polled_batch`.
3. Read `snapshot()` for progress and call `cancel()` to request stop without
   waiting on the provider call. Call `try_finish()` later to reclaim the
   transport after the worker exits.

The event channel is bounded to eight batches. It fails closed when the Host
does not drain it, rather than growing memory without limit. Transient iLink
transport failures are retried with a bounded retry count and cancellable
backoff; cursors advance only after a successful batch is emitted. The worker
does not mutate a control plane, so Agent processing and provider polling can
be independently scheduled. The current blocking HTTP transport can only
observe cancellation before and after an in-flight request; its request
timeout is the hard upper bound for a provider that never returns. This is
intentional and documented, not presented as forced socket interruption.

`WeixinControlPlane::accept_polled_batch` validates the worker cursor and
accepts a fully duplicated batch as a no-op. A stale conflicting cursor is
rejected, preventing an old worker from overwriting newer channel state.

`AgentBridge` and the matching control-plane methods provide the Agent handoff
without embedding an Agent implementation in this channel crate. The Host
claims an `AgentWorkItem` with an attempt id, invokes its Agent, stages the
exact `IlinkMessage` reply, then calls `send_staged_agent_reply`. A successful
transport call commits the item; a failed call leaves the staged reply for a
safe retry. `fail_agent_work(..., retryable: true)` returns the item to the
bounded retry budget, while `cancel_agent_work` and remote cancellation remove
it from runnable work. Reusing an attempt id or reply is idempotent; conflicting
payloads are rejected.

Inbound messages are retained in a bounded idempotency queue. Successful
outbound sends are retained by message ID as well: retrying the same message
returns the original transport status without sending it twice, while reusing
the ID for different content is rejected. An outbound message carrying a
session ID must target the peer bound to that session; an unbound or mismatched
session is rejected before any network call. This makes the facade safe for a
long-lived Host to call from both inbound and outbound channel routes.

The control plane stores only a `SecretRef`. A confirmed QR response without a
credential is an explicit error and leaves the state at `AwaitingQr`. No API
reports a real login for the loopback transport. Pair requests (including a
bounded denial reason), session bindings, inbound messages, and remote control
transitions are bounded and idempotent. A failed poll batch is rejected before
changing its queue, session bindings, or cursor. `productionReady` is true
only for an authenticated non-loopback transport whose credential is present
in the configured store. Logout removes the credential before changing the
runtime state, so a SecretStore failure remains retryable.

The Host plugin and CLI management facade can select this path with
`YUNXI_WEIXIN_MODE=production`; it requires an operator-managed master key and
stores account material through `FileSecretStore`. Both paths use the typed
control-plane and transport boundary; the loopback mode remains the deterministic
local adapter. This does not implement device automation,
media byte uploads, or a Weixin-hosted voice codec. Voice reuse is therefore
currently at the contract boundary: audio can be represented by
`MediaKind::Audio` metadata, and direct iLink messages preserve bounded voice
CDN metadata and server transcription for an upstream Voice plugin to consume.
`IlinkMessage::reply_text` provides the context-bound text reply shape. The
adapter does not download/decrypt media or perform iLink CDN uploads yet;
alternate update records that only expose a text field are normalized as text.
Real-account QR confirmation,
inbound/outbound delivery, reconnect, logout, and account-policy behavior still
require external manual validation. A computed `productionReady` value reports
local control-plane state only; it is not release acceptance by itself.

## Source index

- `src/bin/yunxi-weixin-fixture.rs`: bounded JSONL framing, command dispatch,
  in-memory idempotency registry, and delivery state transitions.
- `src/fixture.rs`: canonical inbound/outbound fixture messages.
- `src/ilink.rs`: bounded HTTP and loopback iLink transports and wire models.
- `src/poll_worker.rs`: bounded background long-poll worker, cancellation,
  retry backoff, event buffering, and transport reclamation.
- `src/runtime.rs`: library control plane, long polling, sessions, pairing,
  remote controls, and approval-aware context-bound replies.
- `src/bridge.rs`: reusable Agent handoff state machine with attempt leases,
  staged replies, bounded retries, cancellation, and idempotent completion.
- `src/secret_store.rs`: replaceable secret store, bounded in-memory
  implementation, and authenticated encrypted file implementation.
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
