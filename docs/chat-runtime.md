# Chat Runtime

## Process Topology

```text
terminal user
    |
    v
yunxi-next CLI process
  - rolling conversation history
  - YunxiKernel lifecycle coordinator
  - capability provider catalog
  - ordered capability orchestration
    |
    | separate loopback JSONL connection per process
    |
    +--> yunxi-context process --> bounded AGENTS.md reads
    +--> yunxi-memory process  --> legacy/Next recall + Next-only writes
    +--> yunxi-persona process --> profile/soul + safe context compiler
    +--> yunxi-companion process --> deterministic response policy
    +--> yunxi-storage process --> persistent sessions + legacy projection
    +--> yunxi-scheduler process --> bounded proactive plans
    +--> yunxi-companion-mailbox process --> encrypted messages
    +--> yunxi-tool-files process --> read-only workspace search and reads
    +--> yunxi-tool-mcp process --> external MCP Server over stdio or HTTP/SSE
    +--> yunxi-tool-skills process --> bounded Skill metadata and instructions
  +--> yunxi-multi-agent process --> graph, budgets, transcripts, and events
  |       `--> one supervised model plugin process per child turn
  +--> yunxi-voice process --> bounded loopback or explicit sidecar routes
  +--> yunxi-weixin process --> bounded loopback or explicit iLink routes
  `--> model plugin process --> HTTPS --> model API
```

The default distribution launches the same `yunxi-next` executable in a private
child mode for each built-in capability. This avoids extra installation
artifacts without weakening process isolation: capability code executes only
after its child process starts. Every capability crate also builds a standalone
plugin binary for later packaging. An external compatible model executable can
be selected with `--plugin`; optional plugin paths have development environment
overrides. Windows installs `yunxi-next` independently and leaves the existing
legacy `yunxi` command untouched.

`yunxi-voice` and `yunxi-weixin` are part of this topology when their switches
are enabled. They are launch-wired and visible in Host inventory. Voice uses a
deterministic loopback provider by default or an explicitly configured JSONL
sidecar; Weixin uses a deterministic loopback control plane by default or the
explicit HTTPS iLink transport when production configuration is supplied. The
loopback paths are useful local adapters, not evidence that a physical audio
device or real account is ready.

## Readiness

1. The host binds an ephemeral IPv4 loopback port and launches the child.
2. The child sends its protocol version, stable plugin id, display metadata,
   versioned capabilities, and a launch-correlation token.
3. The host validates the id, token, version, declarations, optional manifest,
   and the exact capability expected from that process. Launch policies can
   require specific manifest grants before a route is registered.
4. `Welcome` and `Ready` complete the handshake before the CLI accepts input.

The model plugin and the Cordis Agent-spine bootstrap are required. The live
CLI turn uses the standalone `yunxi-agent-spine` loop through its Host adapter;
`ChatSession` owns durable application hooks around it. Context, persona,
memory, storage, and the deterministic companion policy are enabled by
default. Mailbox and scheduler follow the companion switch. Shell, Patch,
Files, MCP, Skills, and Multi-agent are disabled by default because they have
external effects or require additional grants. A disabled capability is never
launched or registered; explicit `settings.plugins` values override the
manifest default and legacy capability settings.
The settings crate accepts 15 built-in optional keys, and the current CLI/Web
launch composition has all 15 connected optional entries. `voice` and `weixin`
are off by default; enabling them starts their bounded local adapter or the
explicitly configured external transport in the Host/session path.

The correlation token prevents accidental attachment to the wrong launched
process. It is not a security or sandbox boundary. Frames are limited to 16
MiB, and model HTTP responses are limited to 32 MiB.

## Chat Requests

For each turn the CLI uses protocol-v2 `Invoke` frames in this order:

1. `context.compose@1:compose` loads ordered project instructions.
2. `tool.skills@1:context`, when enabled, returns bounded instruction blocks for
   the Skills selected during startup discovery.
3. `memory.recall@1:recall`, when enabled, returns bounded boot and dynamic
   records from legacy and Next storage.
4. `persona.context@1:compile` safely combines persona and routed memory into a
   system context.
5. `companion.decide@1:decide`, when enabled, adds deterministic tone and
   optional follow-up guidance.
6. `model.chat@1:complete` receives those system messages followed by rolling
   conversation history and the current user message. When Shell, Patch, Files,
   MCP, Skills, or Multi-agent is enabled, the request also carries a bounded tool catalog.
   MCP descriptors are projected as `mcp.fixture.echo`; metadata-only Skill
   descriptors are projected as `skill.review.check`.

If the model returns tool calls, the Host appends an assistant tool-call
message. Shell and Patch calls pause at the user approval boundary and expose
`/approve`, `/deny`, and `/cancel`; approval creates a new Host-issued
`ActionGrant`. File search and file reads are dispatched automatically with a
read-only workspace grant and never receive an approval or write grant. MCP
calls are treated as potential side effects and always require approval before
the Host creates their grant. HTTP MCP calls additionally carry the configured
exact scheme/host/port network scope and only the granted Secret references;
header values are resolved inside the MCP bridge and never enter the protocol
message. Metadata-only Skill declarations are rejected as
`skill_tool_unavailable` without approval or execution. A separately declared
`actions.json` Skill action is instead routed through the bounded executable
action host and pauses for Host approval before it receives its exact grant.
Every isolated tool result is then appended as a tool message and sent back to
the model. The loop is bounded to eight rounds and eight calls per round.
Automatic Shell calls start with read-only, no-network authority; Patch calls
receive the separate workspace-write authority. Manual `/shell` and `/patch`
remain available as diagnostic fallback commands.

Multi-agent exposes `agent.spawn`, `agent.list`, `agent.message`, and
`agent.interrupt`. Spawn and message pause at the same Host approval boundary.
The coordinator enforces parent-child grant subsets and fixed agent/depth/turn
budgets, then persists graph state beneath `.yunxi-next/multi-agent`. The CLI
model-tool path launches a new isolated Model plugin process for each one-shot
child turn, with no parent tool catalog. List reads bounded state and the CLI
interrupt path updates the persisted branch between turns.

The WebHost additionally exposes dsh-compatible `subagent.list` and
`subagent.history` plus continuable `subagent.prompt` and `subagent.interrupt`.
It runs a bounded number of child workers concurrently, streams child model
deltas and runtime events through the mux channel, propagates interruption to
an active child provider stream, and supplies the child the explicitly granted
file/patch tool catalog. Child-grant subsets and model selection are validated;
each child remains bounded and isolated. This is a local worker path, not a
remote task scheduler.

After a successful model response, the host invokes these side-effect routes in
order when enabled:

1. `storage.sessions@1:append` saves the turn beneath
   `.yunxi-next/sessions` and returns the active session id.
2. `memory.write@1:extract` applies privacy/write policy and writes accepted or
   pending records beneath `.yunxi-next`.
3. `scheduler.proactive@1:evaluate` applies signal, quiet-hour, and frequency
   policy.
4. `companion.mailbox@1:enqueue` stores emitted plans with encrypted content.

The model capability and Agent spine expose a bounded streaming adapter. The
CLI REPL/TUI render text and tool progress as events arrive, `run --jsonl`
emits event/warning/result records, and WebHost projects incremental chunks and
terminal state over recoverable finite SSE polls. Web session cancellation
stops the active generation and clears queued prompts. The carrier remains
loopback-only and unauthenticated; this does not claim remote multi-user stream
semantics.
The REPL keeps at most 32 user/assistant turns in working memory. `/clear`
clears only that working context; `/new` starts a new persistent session, and
`/resume <id>` restores a saved conversation. Legacy sessions are read-only and
are imported into a new Next record on the first appended turn. Multi-agent
state follows the active Next session id across new and resumed sessions.

The Host stores the built-in model credential in an in-memory Secret Broker and
issues a single-use plugin-scoped reference before injecting the value at the
final model-child boundary. The provider child still receives the credential it
needs for HTTP. Weixin production management uses its separate encrypted local
`FileSecretStore`. Neither path is an OS keychain or a durable common broker;
credentials are never serialized into local protocol frames or normal
diagnostic projections.

## Failure Semantics

- An HTTP/API failure becomes `InvocationFailed`; the model process stays alive
  and can serve the next request.
- A malformed frame, mismatched response, timeout, or process exit removes only
  that plugin's routes and records a failed lifecycle state.
- Optional plugin failures produce one user-visible warning per distinct
  failure. Context failures fall back to remaining context; post-response state
  failures do not discard the successful model reply.
- A model process crash makes model chat unavailable, while kernel health and
  sibling plugin processes remain unchanged.
- A file-tool failure is reported as a tool result; the model can continue the
  turn or fall back to text without affecting the model, kernel, or action
  plugins.
- An MCP Server failure is reported as an MCP tool result when the call has
  already crossed the Host boundary; the MCP route and child are removed while
  the model, Files, and Shell routes remain available.
- An MCP transport timeout attempts a bounded `notifications/cancelled`
  request. This is best effort: it does not provide a guarantee that a remote
  server stopped work after receiving the original request.
- HTTP MCP uses the configured endpoint's network grant only; a missing or
  mismatched scheme, host, or port is denied before the request is sent. A
  configured Secret value is redacted from MCP errors and results before they
  leave the bridge.
- A Skills discovery or context failure removes its route and dynamic Skill
  declarations. The current model turn continues without Skill context; no
  metadata-only declaration can execute a command or acquire a grant.
- A child model API failure is recorded only on that agent branch and returned
  to the main model as a tool result. The parent model route, kernel, coordinator,
  and sibling agent records remain available.
- Coordinator restart marks only stale Running branches failed. CLI child turns
  remain synchronous and one-shot, while WebHost reattaches persisted Running
  workers, supports bounded parallel jobs, and can stop an active child
  provider stream without stopping siblings. A provider that ignores
  cooperative cancellation still requires the process timeout/shutdown
  boundary.
- An unexpected optional Host failure is retried with bounded backoff up to
  three times for the current enable cycle. The synchronous implementation
  advances recovery at `refresh()`/inspection boundaries; after exhaustion it
  removes the plugin route and disables the plugin until explicit re-enable or
  manual restart. A failed optional plugin does not stop sibling plugins or the
  Agent spine.

Process isolation does not restrict filesystem, network, CPU, or memory access.
A separate sandbox design is required before running untrusted plugins.

Stateful request types carry an explicit `WorkspaceGrant`. Built-in plugins
validate the granted root and write only into its `.yunxi-next` namespace. This
is an authority contract for trusted built-ins, not an operating-system sandbox.
