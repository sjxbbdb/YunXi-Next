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
- `session.history`
- `session.models`
- `session.prompt`
- read-only startup projections for credentials, providers, presets, commands,
  skills, subagents, and Cordis inventory
- `respond` (the carrier-level approval response route)

The generic `Gateway` owns the projection-only methods; `yunxi-cli::WebHost`
adds the session methods and the three revision-fenced capability-settings
writes while keeping the same ChatSession and plugin Host.
Unknown methods and the two event-stream methods used as unary calls return a
structured dsh `RpcResult` failure. The carrier is dependency-light and uses
bounded HTTP/1.1 request parsing plus bounded SSE responses; callers own the
listener lifecycle and can use `ShutdownToken` for explicit stop control.

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
- `src/events.rs` owns the two bounded in-memory event queues.
- `src/http.rs` owns HTTP/1.1 parsing, `/api` routing, and TCP serving helpers.
- `src/assets.rs` owns lookup and cache policy for generated embedded assets.
- `src/projection.rs` owns browser-facing health and session projections.
- `src/sse.rs` owns bounded SSE framing and stream error events.
- `build.rs` validates `web/dist` and generates the static-resource table.
- `src/lib.rs` exports the stable facade.
- `tests/` verifies the facade, HTTP wire shape, SSE isolation, and TCP serving.

The imported browser source, adapter, licenses, and rebuild script are owned by
the repository-level [`../../web`](../../web/README.md) directory.
