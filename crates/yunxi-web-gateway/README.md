# yunxi-web-gateway

Bounded Web Gateway facade and optional std HTTP/SSE carrier for the
dsh-compatible YunXi contract. The `Gateway` owns unary method dispatch,
browser-safe projections, and bounded event buffering. `HttpCarrier` translates
those envelopes to `POST /api/<method>` and `GET /api/events.*` without moving
plugin lifecycle, credentials, or kernel logic into the Web surface.

The stable method set currently covers:

- `host.describe`
- `workspace.list`
- `settings.describe`
- `settings.update`
- `settings.replace`
- `settings.mutate`
- `health.status`
- `pluginInventory/list`
- `session.list`
- `session.create`
- `session.cancel`
- `session.history`
- `session.models`
- `session.prompt`
- read-only startup projections for credentials, providers, presets, commands,
  skills, subagents, and Cordis inventory; the concrete `WebHost` also serves
  dsh `subagent.list` and one-shot `subagent.history` projections
- `respond` (the carrier-level approval response route)

The generic `Gateway` owns the projection-only methods and an empty fallback
for subagent history; `yunxi-cli::WebHost` adds the session methods, the
three revision-fenced capability-settings writes, and the real read-only
subagent graph/history projection while keeping the same ChatSession and
plugin Host.
Unknown methods and the two event-stream methods used as unary calls return a
structured dsh `RpcResult` failure. The carrier is dependency-light and uses
bounded HTTP/1.1 request parsing plus bounded SSE responses; callers own the
listener lifecycle and can use `ShutdownToken` for explicit stop control.

Each HTTP event channel has an independent, bounded replay journal. Responses
carry a monotonically increasing SSE `id`; reconnects may send either
`Last-Event-ID` or the `afterSeq` query parameter. When both are present they
must match; a conflicting pair is rejected with HTTP 400. The
optional `waitMs` query parameter is capped at 40 ms for finite long-polling.
When the requested cursor predates the retained window, the response includes
retained frames followed by a bounded replay-gap `stream/error` control frame.
The control frame uses the public `internal` error shape and a synthetic SSE
id immediately before the oldest retained id, allowing the existing browser
cursor repair to resume at the retained window. The response also includes
`X-Yunxi-Event-After`, `X-Yunxi-Event-Oldest`, and `X-Yunxi-Event-Latest`
headers. Events already evicted from this journal must be recovered through
the existing session history repair path.

The journal is in-memory by default for backwards compatibility. Durable
replay is opt-in and uses the following interface:

```rust
let carrier = HttpCarrier::new(backend)
    .with_event_journal_path("state/web-events.jsonl")?;
```

`EventJournal::open`, `EventJournal::from_path`, and `attach_path` provide the
same file adapter directly; `path` reports the attached location. The JSONL
record contains `version`, `channel`, `sequence`, and a validated Web event.
Every append is flushed with `sync_data`. A malformed final partial line is
truncated during startup, but a complete malformed line, invalid channel,
invalid RPC event, or non-monotonic sequence is reported. Loading and
attaching the same path is idempotent. Memory remains capped at
`MAX_PENDING_EVENTS` and `MAX_REPLAY_BYTES`; once the file reaches
`MAX_PERSISTED_EVENT_LOG_BYTES`, it is atomically rewritten from that retained
window, preserving the next sequence number. Persisted payloads redact
credential-like fields and audio/sample fields, and all messages continue to
pass the Web contract bounds before being accepted.

`Gateway` remains a projection/test backend: it does not create sessions or
run prompts. Real session lifecycle and prompt/cancel behavior belong to the
serial `WebHost` backend supplied by the CLI; the carrier only guarantees that
concurrent HTTP requests enter that backend one at a time and never share HTTP
cursor state across event channels.

The `yunxi-next web` command is the thin executable entry point around this
carrier. It binds loopback by default and treats standard-input EOF as an
explicit shutdown signal so the shared Host can close supervised plugins.

The carrier embeds the pinned dsh production build from the repository-level
`web/dist` directory. `build.rs` generates an exact static-resource table at
compile time; unknown paths and traversal attempts return 404, each embedded
file is capped at 1 MiB, hashed assets receive immutable caching, and root
documents remain no-cache. The browser can create and restore sessions, send
text prompts, render complete dsh turn logs, consume mux/host events, and
answer pending Host approvals. It does not change plugin settings or carry
provider credentials. Capability writes are the only exception: the WebHost
accepts known boolean fields in the `yunxi-capabilities` namespace, stores
them through `yunxi-settings`, emits `settings/document-updated`, and leaves
the running plugin set unchanged until restart.

## Layout

- `src/dispatch.rs` owns method names and bounded unary dispatch.
- `src/error.rs` owns gateway and event-buffer errors.
- `src/events.rs` owns the two bounded event queues and the optional durable
  JSONL replay adapter.
- `src/http.rs` owns HTTP/1.1 parsing, `/api` routing, and TCP serving helpers.
- `src/assets.rs` owns lookup and cache policy for generated embedded assets.
- `src/projection.rs` owns browser-facing health and session projections.
- `src/sse.rs` owns bounded SSE framing and stream error events.
- `build.rs` validates `web/dist` and generates the static-resource table.
- `src/lib.rs` exports the stable facade.
- `tests/` verifies the facade, HTTP wire shape, SSE isolation, and TCP serving.

The imported browser source, adapter, licenses, and rebuild script are owned by
the repository-level [`../../web`](../../web/README.md) directory.
