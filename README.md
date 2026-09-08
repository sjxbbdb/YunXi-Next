# YunXi Next

YunXi Next is a Rust rebuild of YunXi around a small trusted kernel and
process-isolated capabilities. The current baseline composes project
instructions, persona, recalled memory, and companion response policy; calls
an OpenAI-compatible model; then persists sessions, reviewed memory, and
encrypted proactive mailbox items through separate plugin processes.

## Repository Boundary

This repository is intentionally independent from the legacy project at
`D:\YunXi Agent`. The legacy project remains a read-only reference and a
runnable fallback while the new architecture is developed and verified.

## Current Baseline

The repository now has three explicit layers, plus a Cordis runtime foundation.
The default CLI and Web turn path uses the Rust Agent spine. `ChatSession`
retains application lifecycle, durable storage, post-response hooks, and Web
projection responsibilities around that loop; the old compatibility loop is
kept only as an explicit recovery path.

`yunxi-cordis-core` is the small trusted, dependency-free Rust meta-kernel:
scoped Context/Service lookup, typed events, reversible Effects, and Fiber
lifecycle. `yunxi-cordis-runtime` adds a bounded static registry and coarse
enable/disable lifecycle. `yunxi-agent-spine` is a separate replaceable Agent
loop with bounded sessions, context/model/tool seams, cancellation, budgets,
and fail-closed tool approval. The CLI supplies these seams through the process
Host, so model/tool turns use the spine while application hooks remain outside
  the trusted loop.

The plugin Host also has a package-based dynamic boundary. An explicit plugin
directory is scanned for validated `plugin.json` packages, dependencies are
ordered deterministically, and enabled packages are launched through the same
handshake, grant, retry, and route-removal path as built-ins. Directory changes
are reconciled at Host refresh/inventory boundaries; replacement is
generation-safe, and `unload_all` removes manager-owned routes and processes
when a directory is removed or replaced. This is dynamic Rust executable
loading, not arbitrary `cdylib`/WASM loading or an operating-system sandbox.

The L0 kernel owns only the minimum lifecycle needed to run plugins safely:

- register a process-isolated plugin;
- start and observe it;
- contain an unexpected process exit;
- keep the kernel and sibling plugins running;
- stop supervised processes during shutdown;
- retry an unexpectedly failed optional Host with bounded backoff, up to three
  times per enable cycle, then leave it disabled until the user enables it
  again.

The L1 capability platform adds:

- validated, versioned capability declarations;
- generic typed invocations over a size-bounded loopback protocol;
- a provider catalog outside the trusted kernel;
- deterministic rejection of missing or ambiguous capability routes.

The connected identity and stateful paths add:

- an isolated OpenAI-compatible Chat Completions plugin using `model.chat@1`;
- isolated `context.compose@1`, `memory.recall@1`, and `persona.context@1`
  providers before each model request;
- compatible reads of legacy `AGENTS.md`, persona profiles, `soul.txt`, JSONL
  memory, and session records without modifying legacy files;
- `memory.write@1` rule extraction with privacy discard, pending review,
  deduplication, and Next-only JSONL writes;
- `storage.sessions@1` persistence and resume, with legacy sessions imported
  on first write instead of overwritten;
- `companion.decide@1`, `scheduler.proactive@1`, and an encrypted
  `companion.mailbox@1` path;
- persisted plugin launch switches: Model/Agent spine are always on, Context,
  Persona, Memory, Companion, and Storage are enabled by default, while
  externally connected or side-effecting capabilities are disabled by default;
- inherited YunXi/DeepSeek/OpenAI environment configuration, with the Host
  broker limiting built-in model credential exposure to the final child
  process boundary;
- an interactive CLI with bounded working history backed by persistent
  sessions;
- session list/resume/new, memory review, mailbox, status, clear, and quit
  commands;
- `--once` mode for scripts and health checks;
- the pinned DeepSeek Harness Web client, served from the Rust binary with a
  bounded HTTP/SSE adapter, real session chat, and composition-scoped capability
  switches.

Voice and Weixin have launch-wired Rust process plugins rather than in-process
SDK dependencies. Voice selects a deterministic loopback provider by default
or a bounded external JSONL sidecar when `YUNXI_VOICE_SIDECAR_PROGRAM` is set;
the latter requires the Host-issued `Device` grant and is used by the same CLI,
TUI, and Web route. Weixin likewise selects deterministic loopback behavior by
default or the bounded HTTPS iLink control plane, encrypted `FileSecretStore`,
and non-blocking long-poll worker when production mode is explicitly configured.
Neither path certifies a physical audio device or real Weixin account without
the external integration checks documented below.

When enabled, the Files tool exposes bounded `file.search` and `file.read`
model calls under a read-only workspace grant. Shell and Patch model calls
remain behind the Host approval boundary; any tool failure is visible in the
CLI and returned to the model for recovery.

The optional MCP bridge discovers one external stdio or opt-in HTTP Server and
projects its tools behind Host approval. The optional Skills process discovers
bounded workspace-local `SKILL.md` files, injects their instructions, and keeps
metadata-only `tools.json` declarations inert. A separate `actions.json`
allowlist can expose fixed executables only when Skill actions are explicitly
enabled; every action remains approval-gated, grant-bounded, cancellable, and
process-isolated. The optional Multi-agent coordinator provides bounded agent
graphs and Host-approved child turns, each using a separate Model plugin
process. Child turns receive only the file/patch tool subset justified by their
stored grants and the parent sandbox. WebHost adds bounded continuable child
prompts, concurrent background workers, per-turn model selection, streamed
events, immediate targeted interruption, and persisted worker recovery when a
root session is reattached; the CLI model-tool path remains a synchronous
one-shot interaction.

Model SSE is converted into bounded Agent events. The REPL, TUI, `run
--jsonl`, and Web mux event path can render text/tool progress before a turn
completes and can request cancellation. This is local end-to-end event
streaming, not a claim of multi-user or remotely authenticated Web service.

API failures are returned per request and do not terminate the model plugin.
An optional capability failure produces a visible warning and falls back to
the remaining route set. `yunxi-plugin-host` performs bounded automatic
recovery for an unexpectedly failed plugin, at most three retries per enable
cycle. Recovery advances at its synchronous `refresh()` boundary; after the
limit, the plugin is disabled until explicit user enable or manual restart.

Process isolation protects the kernel from plugin crashes. Host grants bound
the intended workspace/network/secret authority, and the model credential is
held by the Host Secret Broker until the final model-child boundary. The broker
is process-local by default and can use an explicitly keyed authenticated file
store for durable rotation and removal. Protocol frame, request, output,
duration, plugin-count, and per-plugin concurrency limits are enforced, but
none of this is an OS keychain or a filesystem, network, CPU, memory, handle,
or child-process sandbox.

The remaining external acceptance work for real audio hardware/providers, a
real Weixin account/media path, platform keychain policy, OS-level resource
sandboxing, remote multi-host workers, and authenticated non-loopback Web is tracked in
[`docs/capability-migration.md`](docs/capability-migration.md). A capability is
counted as migrated only after it has a real process boundary, versioned
contract, explicit grants, failure-containment tests, and any required external
provider/device/account validation.

The dsh-inspired composition layer and the initial Cordis primitives are now
represented by
[`yunxi-composition`](crates/yunxi-composition/README.md). It keeps ordered
bundle/profile/overlay configuration separate from process supervision and
projects the current plugin set into the inventory shape needed by the dsh Web
client. `yunxi-cordis-core` provides generic Context, Service, Event, Effect,
and Fiber primitives; `yunxi-cordis-runtime` adds a static registry and
enable/disable lifecycle for in-process composition. The runtime is the trusted
bootstrap, the spine is the default turn loop, and `yunxi-plugin-host` now
provides validated package discovery, reload, and unload for external Rust
executables. Web switch changes rebuild the current Host rather than performing
a true in-place single-plugin unmount. The upstream record and reuse boundary
are documented in
[`docs/dsh-web-compatibility.md`](docs/dsh-web-compatibility.md).
User capability choices are stored by
[`yunxi-settings`](crates/yunxi-settings/README.md) in a bounded, versioned
document. An explicit `settings.plugins` value is resolved first; legacy
capability environment/file settings are retained as compatibility fallbacks.
The settings crate knows 15 built-in optional keys, including `voice` and
`weixin`, and the current CLI Host and Web schema expose all 15 launch-wired
optional entries alongside the required Model entry. Voice and Weixin remain
off by default; enabling them grants their declared Host authority and launches
their process plugin, while disabling them removes the route and process.

## Run

Install the independent `yunxi-next` command beside Cargo without replacing an
existing legacy YunXi binary:

```powershell
.\scripts\install-windows.ps1
```

Open a new terminal, set a provider credential, then start the CLI:

```powershell
$env:DEEPSEEK_API_KEY = "your-key"
yunxi-next
```

The installer uses the command directory that already contains `cargo.exe`, so
terminals with Cargo available can resolve `yunxi-next` immediately. If needed,
refresh that directory in the current process once:

```powershell
$commandBin = Split-Path (Get-Command cargo.exe).Source
$env:Path = "$commandBin;$env:Path"
```

Send one prompt without entering interactive mode:

```powershell
yunxi-next --once "你好"
```

Emit bounded lifecycle, text, tool, warning, and final-result records:

```powershell
yunxi-next run --once "你好" --jsonl
```

Start the bounded Web HTTP/SSE carrier on loopback:

```powershell
yunxi-next web
```

The default listener is `127.0.0.1:8787`. Use `yunxi-next web --bind
127.0.0.1:0` for an available port during local testing. Closing the Web
command's standard input performs an explicit Host shutdown. Open
`http://127.0.0.1:8787` to use the embedded dsh workbench. The current Web
surface supports bounded multi-session text chat, per-session history and
cancellation, model projection, Host approvals, plugin inventory, and
capability switches in Settings > Plugins. Switch changes are persisted
immediately; WebHost applies them by rebuilding its current Host, while a
standalone CLI applies them on its next Host launch. Provider credentials are
not Web payloads; the Host broker exposes them only at the model child boundary.

The Web server is loopback-only and unauthenticated. It is a local development
surface, not a remotely exposed service. Voice management defaults to a
deterministic loopback provider; setting `YUNXI_VOICE_SIDECAR_PROGRAM` selects
the explicit sidecar boundary but still reports readiness only from its doctor
and device results. Weixin management defaults to loopback; setting
`YUNXI_WEIXIN_MODE=production` selects the HTTPS iLink path and requires an
operator-managed 32-byte master key, secret-store path, and account login.
Both selections are consumed by the same process plugin represented in Host
inventory; real device/account readiness still depends on their doctor and
external acceptance results.

Migration is explicit and writes only YunXi Next state:

```powershell
yunxi-next migrate sessions plan --json
yunxi-next migrate sessions apply --json
yunxi-next migrate rollback <migration-id> --json
```
The plan is read-only, apply preserves the legacy source, and rollback removes
only unchanged files recorded as created by that migration.

See [`docs/provider-configuration.md`](docs/provider-configuration.md) for
custom OpenAI-compatible endpoints and the complete resolution order. The
runtime does not load `.env` files automatically and never writes credentials
to repository files or local protocol messages.

## Repository Map

- [`crates/`](crates/README.md) contains production Rust packages.
- [`web/`](web/README.md) contains the pinned dsh Web import, YunXi adapter,
  licenses, and generated browser distribution.
- [`docs/`](docs/README.md) contains architecture and repository maps.
- [`AGENTS.md`](AGENTS.md) defines repository boundaries and verification rules.
- [`docs/project-structure.md`](docs/project-structure.md) explains every tracked
  directory and source file.
- [`docs/chat-runtime.md`](docs/chat-runtime.md) explains the end-to-end process
  and failure boundaries.

## Verify

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --release
```
