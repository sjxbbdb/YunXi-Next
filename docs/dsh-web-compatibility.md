# dsh Web Compatibility

YunXi Next uses DeepSeek Harness (dsh) as an architectural and Web contract
reference. The reference checkout is `D:\deepseek-harness-reference`; it is
read-only and is not a path dependency of this repository.

## Upstream Record

- Repository: `https://github.com/deepseek-ai/deepseek-harness`
- Reviewed commit: `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`
- License: MIT
- Copyright notice: `Copyright (c) 2026 DeepSeek`

The dsh source and any reused frontend package must retain the upstream MIT
notice and be tracked separately from YunXi-owned Rust code. This record pins
the behavior reviewed by the current compatibility work; future upstream
updates require a new review and commit entry.

## Reuse Boundary

The dsh Web client is a browser-side composition of modules, remotes, session
projection, settings, and React UI. YunXi can reuse that client because the
Rust side can provide the same wire contract. The first compatibility surface
is deliberately small:

- unary RPCs use `POST /api/<method>`;
- requests use `{type: "client-request", rpcId, method, payload}`;
- responses use `{type: "server-response", rpcId, result}`;
- event streams use `/api/events.mux` and `/api/events.host`;
- plugin settings can consume a `pluginInventory.list()` result shaped as
  `{entries: [{entryId, moduleName, enabled, fiberPhase}]}`;
- browser module boot uses the dsh `__DSH_BOOT__` graph and `/plugins` bundle
  routes imported from the pinned upstream build.

The repository-level `web/` directory owns the source record, MIT notice,
third-party notices, YunXi transport adapter, reproducible import script, and
generated distribution. The import runs with an isolated temporary `DSH_HOME`
and captures 42 upstream client bundles, preventing user-installed dsh plugins
from entering the executable. The YunXi overlays replace the WebSocket
downlink with cancellation-aware 250 ms bounded SSE polling, change the
visible product title to `YunXi Next`, and bind the upstream Plugins tab to the
bounded capability-settings namespace.

`yunxi-composition` owns the Rust data model for ordered bundle layers and
overlays. `yunxi-web-contract` owns the bounded RPC and event envelope types;
it does not open sockets or start a server. Neither crate loads frontend code,
spawns processes, or makes network calls. `yunxi-kernel` and
`yunxi-plugin-host` remain the owners of lifecycle, process isolation,
readiness, and capability routing. A dsh-style settings toggle therefore
changes composition state before launch; disabled entries do not execute
plugin code.

The Gateway facade lives in `yunxi-web-gateway`. It maps a refreshed CLI Host
projection to `health.status`, `pluginInventory/list`, and `session.list`, and
keeps `events.mux` and `events.host` in separate bounded in-memory queues.
`yunxi-cli::WebHost` adds `session.create`, `session.history`, and
`session.prompt` over the same `ChatSession`, plugin Host, approval boundary,
and storage plugin used by the terminal path. Approval answers use the dsh
`client-response` envelope on `POST /api/respond`; the requested event's
`rpcId` is the only accepted correlation id. `HttpCarrier<WebHost>` is the
transport adapter around that same facade; it does not create a second runtime
or expose credentials.

`yunxi-cli::WebHost` also owns writable `settings.describe`,
`settings.update`, `settings.replace`, and `settings.mutate` behavior for the
single `yunxi-capabilities` namespace. Only one-level known boolean fields are
accepted, writes are revision-fenced and bounded to 64 mutations, and a
successful change emits `settings/document-updated` on `events.host`.
`yunxi-settings` persists the target composition to `settings.json`; the live
Host keeps its current children and routes until the next Host composition.
Explicit environment switches override that document for the process. The
required Model entry is read-only. The settings domain has 15 built-in keys,
and the current CLI/Web launch schema exposes all 15 connected composition-scoped
optional switches, including Multi-agent. `voice` and `weixin` launch
deterministic fixture routes when enabled and remain off by default.

`yunxi-web-gateway/build.rs` recursively validates `web/dist` and generates an
exact embedded resource table. It admits only the required document, script,
style, manifest, image, and font MIME types, caps each file at 1 MiB, gives
hashed assets immutable caching, keeps entry documents no-cache, and provides
no filesystem fallback for unknown or traversal paths.

## Current Carrier

The independent Rust carrier currently provides:

- `POST /api/health.status`, `/api/pluginInventory/list`, `/api/session.list`,
  `/api/session.create`, `/api/session.history`, `/api/session.models`, and
  `/api/session.prompt` with dsh `client-request` and `server-response` JSON
  envelopes;
- dsh startup projections for `host.describe`, `workspace.list`,
  `settings.describe`, `credentials.describe`, `llm.providers`, presets,
  commands, skills, subagents, and Cordis inventory;
- `settings.update`, `settings.replace`, and `settings.mutate` for the
  composition-scoped optional capability switches;
- `POST /api/respond` with a bounded `client-response` body and a JSON receipt;
- `GET /api/events.mux` and `GET /api/events.host` as bounded SSE responses;
- complete `turn/start`, message, step, and `turn/end` event sequences for
  history and live text replies;
- the imported dsh shell, boot manifest, fonts, language bundles, and 42 client
  plugin bundles from the Rust executable;
- explicit HTTP/1.1 header and body limits, JSON content-type validation,
  duplicate-length and chunked-transfer rejection, and no HTTP pipelining;
- caller-owned `TcpListener` serving through `HttpCarrier::serve_until` and
  explicit `ShutdownToken` stop control.

The `yunxi-next web` command is the current executable carrier entry point. It
binds `127.0.0.1:8787` by default, accepts `--bind <ADDR>`, and treats
standard-input EOF as an explicit shutdown signal so the shared Host can close
all supervised plugin children.

The carrier remains a local first-stage Web host rather than a production Web
server. Only capability booleans are writable; provider configuration and
credentials remain environment-owned. Authentication and non-loopback listener
policy are not implemented, and one Host instance serializes its active Web session.
Image input, token streaming, session cancellation, model mutation, and true
multi-client event fan-out remain later work. A malformed JSON envelope that
is syntactically valid JSON returns a bounded structured `bad-request` response;
invalid HTTP framing remains a normal HTTP 400/413 response. Credential
descriptions expose only configured state and never return secret values.

The imported browser adapter source contains Voice and Weixin inventory-field
mappings, and the Rust Web schema and CLI composition launch their fixture
routes when enabled. The UI is evidence of route/inventory wiring only; it is
not evidence of real microphone, speaker, login, or channel transport support.

## Migration Order

1. Keep dsh's Web/client packages in a separately tracked compatibility input.
2. [x] Implement the independent HTTP/SSE carrier around the stable Rust
   Gateway envelopes and event carriers, with bounded JSON and no credentials
   in messages or logs.
3. [x] Extend the CLI Host projection with history, prompt, approval, and
   failure events while keeping the same plugin Host and session ownership.
4. [x] Add a thin TypeScript adapter only for the bounded-SSE carrier
   difference.
5. [x] Import and serve the pinned dsh browser shell and client graph.
6. [x] Persist bounded capability switches and connect them to the dsh Plugins
   tab with composition-scoped state.
7. Replace the remaining read-only compatibility projections with native Rust
   services incrementally while retaining the browser UI.

The frontend is not copied into the Rust kernel. This keeps the old
`D:\YunXi Agent` fallback untouched and keeps a failed optional Web or plugin
surface from changing kernel health.
