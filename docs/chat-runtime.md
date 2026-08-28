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

## Readiness

1. The host binds an ephemeral IPv4 loopback port and launches the child.
2. The child sends its protocol version, stable plugin id, display metadata,
   versioned capabilities, and a launch-correlation token.
3. The host validates the id, token, version, declarations, optional manifest,
   and the exact capability expected from that process. Launch policies can
   require specific manifest grants before a route is registered.
4. `Welcome` and `Ready` complete the handshake before the CLI accepts input.

The model plugin is required. Context, persona, and session storage are enabled
by default. Memory and companion behavior are disabled by default to preserve
the legacy opt-in policy; mailbox and scheduler follow the companion switch
unless explicitly overridden. Files, MCP, and Skills are also disabled by
default. A disabled capability is never launched or registered.

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
   MCP, or Skills is enabled, the request also carries a bounded tool catalog.
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
message. Skill declarations are metadata-only in this phase, so calls are
rejected as `skill_tool_unavailable` without approval or execution. Every
isolated tool result is then appended as a tool message and sent back to the
model. The loop is bounded to eight rounds and eight calls per round. Automatic
Shell calls start with read-only, no-network authority; Patch calls receive the
separate workspace-write authority. Manual `/shell` and `/patch` remain
available as diagnostic fallback commands.

After a successful model response, the host invokes these side-effect routes in
order when enabled:

1. `storage.sessions@1:append` saves the turn beneath
   `.yunxi-next/sessions` and returns the active session id.
2. `memory.write@1:extract` applies privacy/write policy and writes accepted or
   pending records beneath `.yunxi-next`.
3. `scheduler.proactive@1:evaluate` applies signal, quiet-hour, and frequency
   policy.
4. `companion.mailbox@1:enqueue` stores emitted plans with encrypted content.

The model path currently uses complete responses rather than token streaming.
The REPL keeps at most 32 user/assistant turns in working memory. `/clear`
clears only that working context; `/new` starts a new persistent session, and
`/resume <id>` restores a saved conversation. Legacy sessions are read-only and
are imported into a new Next record on the first appended turn.

API credentials are read from inherited environment variables. The CLI
preflights configuration for useful startup errors, but credentials are never
serialized into local protocol frames or diagnostic output.

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
- Automatic restart is intentionally absent. A future policy must be bounded,
  observable, and owned above the kernel primitive.

Process isolation does not restrict filesystem, network, CPU, or memory access.
A separate sandbox design is required before running untrusted plugins.

Stateful request types carry an explicit `WorkspaceGrant`. Built-in plugins
validate the granted root and write only into its `.yunxi-next` namespace. This
is an authority contract for trusted built-ins, not an operating-system sandbox.
