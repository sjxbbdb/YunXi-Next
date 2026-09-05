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
  Persona, and Storage are enabled by default, while external or side-effecting
  capabilities are disabled by default;
- inherited YunXi/DeepSeek/OpenAI environment configuration;
- an interactive CLI with bounded working history backed by persistent
  sessions;
- session list/resume/new, memory review, mailbox, status, clear, and quit
  commands;
- `--once` mode for scripts and health checks;
- the pinned DeepSeek Harness Web client, served from the Rust binary with a
  bounded HTTP/SSE adapter, real session chat, and composition-scoped capability
  switches.

Voice and Weixin now have Rust contract crates, process-host fixtures, and
launch-wired optional entries. When enabled, `yunxi-voice` exposes bounded
transcribe/synthesize routes and `yunxi-weixin` exposes a bounded
`channel.weixin@1` route with idempotent inbound/outbound delivery state. The
current implementations are deterministic fixtures: they do not provide a
real microphone, speaker, Weixin login, or Weixin network transport.

When enabled, the Files tool exposes bounded `file.search` and `file.read`
model calls under a read-only workspace grant. Shell and Patch model calls
remain behind the Host approval boundary; any tool failure is visible in the
CLI and returned to the model for recovery.

The optional MCP bridge discovers one external stdio Server and projects its
tools behind Host approval. The optional Skills process discovers bounded
workspace-local `SKILL.md` files, injects their instructions, and projects
`tools.json` entries as metadata-only declarations that cannot execute yet.
The optional Multi-agent coordinator provides bounded agent graphs and
Host-approved child turns, each using a separate Model plugin process. Its
current baseline is synchronous; background parallelism and live interruption
remain Phase 4 work.

API failures are returned per request and do not terminate the model plugin.
An optional capability failure produces a visible warning and falls back to
the remaining route set. `yunxi-plugin-host` performs bounded automatic
recovery for an unexpectedly failed plugin, at most three retries per enable
cycle. Recovery advances at its synchronous `refresh()` boundary; after the
limit, the plugin is disabled until explicit user enable or manual restart.

Process isolation protects the kernel from plugin crashes. It is not yet a
filesystem, network, or resource-usage sandbox.

The remaining inheritance plan for richer model-based memory extraction,
companion automation, production Voice/Weixin adapters, streaming, and
management parity is tracked in
[`docs/capability-migration.md`](docs/capability-migration.md). A capability is
counted as migrated only after it has a real process boundary, versioned
contract, explicit grants, and failure-containment tests.

The dsh-inspired composition layer and the initial Cordis primitives are now
represented by
[`yunxi-composition`](crates/yunxi-composition/README.md). It keeps ordered
bundle/profile/overlay configuration separate from process supervision and
projects the current plugin set into the inventory shape needed by the dsh Web
client. `yunxi-cordis-core` provides generic Context, Service, Event, Effect,
and Fiber primitives; `yunxi-cordis-runtime` adds a static registry and
enable/disable lifecycle for in-process composition. The runtime is the trusted
bootstrap and the spine is the default turn loop, while dynamic Rust plugin
loading and live in-place Web unmounting remain later work. The upstream record
and reuse boundary are documented in
[`docs/dsh-web-compatibility.md`](docs/dsh-web-compatibility.md).
User capability choices are stored by
[`yunxi-settings`](crates/yunxi-settings/README.md) in a bounded, versioned
document. An explicit `settings.plugins` value is resolved first; legacy
capability environment/file settings are retained as compatibility fallbacks.
The settings crate knows 15 built-in optional keys, including `voice` and
`weixin`, and the current CLI Host and Web schema expose all 15 launch-wired
optional entries alongside the required Model entry. Voice and Weixin remain
off by default and use fixture implementations until their real adapters land.

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

Start the bounded Web HTTP/SSE carrier on loopback:

```powershell
yunxi-next web
```

The default listener is `127.0.0.1:8787`. Use `yunxi-next web --bind
127.0.0.1:0` for an available port during local testing. Closing the Web
command's standard input performs an explicit Host shutdown. Open
`http://127.0.0.1:8787` to use the embedded dsh workbench. The current Web
surface supports text sessions, model selection projection, history, real
chat replies, Host approvals, plugin inventory, and capability switches in
Settings > Plugins. Switch changes are persisted immediately; WebHost applies
them by rebuilding its current Host, while a standalone CLI applies them on its
next Host launch. Provider credentials stay in the Host process environment.

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
