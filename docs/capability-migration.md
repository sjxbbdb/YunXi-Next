# Legacy Capability Migration

## Objective

YunXi Next will inherit the useful behavior of the legacy YunXi Agent without
copying its monolithic runtime assembly. Business capabilities run in isolated
child processes and communicate through versioned capability calls. The legacy
repository remains a read-only behavioral reference during migration.

Pluginization means a process boundary, a declared wire contract, explicit
grants, and independent failure state. Splitting code into another Rust crate is
not sufficient by itself.

## Trusted Host Boundary

The following responsibilities remain outside capability plugins:

| Host responsibility | Reason |
| --- | --- |
| Process supervision and shutdown | A plugin cannot supervise or certify itself |
| Protocol validation and capability routing | Malformed or ambiguous declarations must fail closed |
| Approval and sandbox policy decisions | Action plugins must not approve their own requests |
| Secret and filesystem grant brokering | Plugins receive only the values and paths they were granted |
| Enable/disable state | A disabled plugin must not execute code to report that it is disabled |
| User-visible failure reporting | Plugin failure must remain observable without taking down YunXi |

The terminal CLI, future TUI, Web UI, and channel adapters are replaceable host
surfaces. They are not allowed to bypass the trusted approval or routing layer.

## Capability Ledger

| Domain | Legacy owner | YunXi Next contract | Target process | Status |
| --- | --- | --- | --- | --- |
| Model completion | `yunxi-agent-provider` | `model.chat@1:complete` | `yunxi-model-openai` | Baseline integrated: isolated request loop, required network/provider-credential manifest grants, API failure containment, and optional bounded tool-call response; token streaming and credential brokering still pending |
| Prompt and AGENTS context | `yunxi-agent-context` | `context.compose@1:compose` | `yunxi-context` | Integrated: bounded root-to-cwd read path |
| Persona and soul | `yunxi-agent-persona` | `persona.context@1:compile` | `yunxi-persona` | Integrated: default/custom profile and soul read path |
| Memory recall | `yunxi-agent-persona`, `yunxi-agent-storage` | `memory.recall@1:recall` | `yunxi-memory` | Integrated: bounded legacy + Next JSONL recall |
| Memory extraction and writes | `yunxi-agent-persona`, `yunxi-agent-storage` | `memory.write@1:extract/review` | `yunxi-memory` | Baseline integrated: rule extraction, privacy policy, dedup, pending review, Next-only writes; provider extractor and bulk management pending |
| Companion response policy | `yunxi-agent-companion` | `companion.decide@1:decide` | `yunxi-companion` | Integrated: deterministic emotion/tone/follow-up policy reaches model context |
| Relationship mailbox | `yunxi-agent-companion`, `yunxi-agent-storage` | `companion.mailbox@1` | `yunxi-companion-mailbox` | Baseline integrated: encrypted Next mailbox, idempotency, list/get/read; legacy credential-store mailbox import pending |
| Proactive scheduling | `yunxi-agent-runtime`, `yunxi-agent-companion` | `scheduler.proactive@1:evaluate` | `yunxi-scheduler` | Baseline integrated: explicit signals, quiet hours, limits, mailbox enqueue; background daemon and love-letter generation pending |
| Session history and resume | `yunxi-agent-storage` | `storage.sessions@1` | `yunxi-storage` | Baseline integrated: append/list/load/resume and read-only legacy projection; full legacy event replay pending |
| Shell execution | `yunxi-agent-tools`, `yunxi-agent-exec`, `yunxi-agent-sandbox` | `tool.shell@1` | `yunxi-tool-shell` | Baseline integrated: isolated execution, required approval/workspace-read manifest grants, bounded output/timeout, and Host-controlled model loop; OS sandbox and optional write/network policy expansion pending |
| Patch application | `yunxi-agent-tools`, `yunxi-agent-patch` | `tool.patch@1` | `yunxi-tool-patch` | Baseline integrated: isolated apply, required approval/workspace-read/workspace-write manifest grants, path validation, rollback, and Host-controlled model loop |
| File search and viewing | `yunxi-agent-tools`, `yunxi-agent-context` | `tool.files@1:search/read` | `yunxi-tool-files` | Baseline integrated: isolated name search and bounded UTF-8 reads under a required read-only workspace grant; write, rename, indexing, and richer ignore policy remain out of scope |
| MCP bridge | `yunxi-agent-mcp`, `yunxi-agent-tools` | `tool.mcp@1:list/call/cancel/status` | `yunxi-tool-mcp` | Baseline integrated: one explicit stdio or opt-in HTTP Server, bounded JSON-RPC initialize/list/call/cancel, JSON/SSE responses, session reuse, dynamic model projection, Host Approval, exact network scopes, reference-only Secret grants, redaction, and isolated crash recovery |
| Skills and dynamic tools | `yunxi-agent-skills`, `yunxi-agent-tools` | `tool.skills@1:list/context/status` | `yunxi-tool-skills` | Baseline integrated: isolated workspace-bounded discovery, validated metadata, bounded instruction injection, disabled filtering, and metadata-only model tool projection; executable Skill tools and richer lifecycle management remain pending |
| Multi-agent coordination | `yunxi-agent-multi-agent` | `tool.multi-agent@1` | `yunxi-multi-agent` | Baseline integrated: isolated coordinator, parent-child grant subset, fixed graph/turn budgets, persisted transcripts/events, restart recovery, Host-approved spawn/message, and a separate Model plugin process per child turn; background parallelism, in-flight cancellation, child tool grants, and Web graph UI remain pending |
| Weixin channel | `yunxi-agent-weixin` | `channel.weixin@1` | `yunxi-weixin` | Baseline launch-wired contract/fixture: process-host handshake, required Network/Secret declarations, typed inbound/outbound routing, idempotency, ACK/cancel/fail state transitions, malformed-payload handling, disable route removal, and CLI/Web inventory projection; no real login, SDK, or network transport |
| Voice input (speech recognition) | `yunxi-agent-voice` | `voice.transcribe@1` | `yunxi-voice` | Baseline launch-wired contract/fixture: bounded chunks, partial/final transcript events, cancellation, backpressure, process-host handshake, exact capability check, disable route removal, and CLI/Web inventory projection; no Device grant or device runtime |
| Voice output (speech synthesis) | `yunxi-agent-voice` | `voice.synthesize@1` | `yunxi-voice` | Baseline launch-wired contract/fixture: bounded text/audio chunks, cancellation, backpressure, process-host handshake, exact capability check, disable route removal, and CLI/Web inventory projection; output bytes are synthetic and no speaker runtime exists |

The Voice and Weixin rows remain `baseline`, not full product `integrated`.
Their crates prove typed contracts and an isolated test process, and the current
`yunxi-cli` Host composition launches them and exposes their inventory routes
when enabled. Creating a fixture is evidence for the process boundary only; it
is not evidence of microphone, speaker, login, or network support.

Legacy `yunxi-agent-core`, `yunxi-agent-protocol`, and
`yunxi-agent-runtime` contain mixed contracts and orchestration. Their behavior
is redistributed across the YunXi Next protocol, plugin host, and narrow
capability contracts rather than migrated as one runtime plugin.

Legacy `yunxi-agent-cli`, `yunxi-agent-tui`, and the embedded Web server are
interaction surfaces. They consume the same plugin catalog. The current Web
settings page renders the bounded built-in inventory and persists enable/disable
state without loading disabled plugin code. A successful Web write rebuilds the
current Host composition; the WebHost does not hot-unmount a single running
plugin in place. A standalone CLI process reads the setting when its next Host
is created. `yunxi-settings` has 15 built-in keys, and the current CLI/Web launch
path exposes all 15 connected optional entries; `voice` and `weixin` remain
fixture routes until their production adapters exist.

`yunxi-agent-eval` remains development infrastructure. Its scenarios become
cross-plugin acceptance fixtures instead of a runtime capability.

## Migration Order

1. **Foundation:** generic invocation protocol, validated capability ids, and
   deterministic provider catalog. The model plugin proves the complete path.
2. **Read-only identity path (complete):** context, persona, and memory recall
   compose the system context before a model request. Failure falls back to the
   remaining capabilities and model chat.
3. **Stateful companion path (baseline complete):** memory writes, storage,
   companion policy, mailbox, and scheduler use process boundaries and explicit
   workspace-scoped grants. Remaining parity items are recorded in the ledger.
4. **Action path:** shell, patch, MCP, future executable Skill tools, and
   multi-agent child turns run only after host-issued approval and resource
   grants. Read-only agent listing and stored-state interruption do not acquire
   execution authority. Current Skill declarations are metadata-only and cannot
   execute. A plugin result never upgrades its own authority.
5. **Channel and media path (baseline contracts):** Voice and Weixin fixtures
   now exercise the same versioned process boundary, but real adapters still
   need CLI/Web composition, host-issued device/network/secret authority, and
   product-level fallback before this phase is integrated.
6. **Management surface:** Web settings reads the bounded Host inventory,
   changes persisted enable state, and shows lifecycle health from the kernel
   and plugin host. Manifest metadata remains Host-owned at the dsh wire
   boundary; WebHost rebuilds its current composition after a successful write.

## Voice Migration Boundary

Voice input and voice output are both part of the inheritance scope. The current
repository has a contract and process fixture baseline, but no usable device
adapter. They are
separate capabilities even when one executable provides both, so either side
can be disabled, replaced, or restarted without taking down text chat.

The current contracts and fixture have these properties:

- `voice.transcribe@1` accepts bounded audio chunks, emits partial and final
  transcripts, and supports cancellation and backpressure;
- `voice.synthesize@1` accepts bounded text, emits audio chunks, and supports
  cancellation and backpressure;
- the fixture requests no `Device` grant because it has no device access;
  production microphone/speaker permissions must be host-issued grants rather
  than authority inferred by a voice plugin;
- raw audio is not persisted by default; an explicit user action is required
  for recording or diagnostic retention;
- a missing device, provider timeout, malformed audio frame, or plugin crash
  produces a visible warning and leaves the text CLI/model route available;
- channel adapters such as Weixin consume these same contracts instead of
  embedding a second speech implementation.

The current fixture covers partial/final input, synthetic output,
cancellation, bounds, malformed payloads, and process disable/removal. Device
permission denial, playable streaming output, real failure fallback, and SDK
integration remain required for the production voice wave. A provider-specific
speech SDK is an implementation detail of the plugin and must not leak into the
host protocol.

## Per-Plugin Acceptance Gate

A legacy capability is marked integrated only when all of these are true:

1. It runs outside the host process and has a stable plugin id.
2. Every provided capability declares a nonzero contract version.
3. Typed request and response fixtures cover its wire payloads.
4. A crash, timeout, malformed frame, or API failure is contained and visible.
5. Disabling it prevents process launch and removes its routes.
6. Filesystem, network, secret, and approval requirements are explicit.
7. Its sibling plugin and the kernel stay healthy during a failure test.
8. Its directory has ownership documentation and focused tests.

Voice plugins additionally prove that device grants are enforced, streamed
audio can be cancelled without hanging the host, and raw audio is absent from
default persistent state.

The migration ledger status changes only after these gates pass. A typed
contract or process fixture alone is `baseline`; source copied into the new
repository but still called in-process remains `planned`.

## Phase 0 Evidence

The current baseline records plugin grant declarations in the versioned
handshake manifest. `yunxi-plugin-host` rejects a launch before route
registration when a host-required grant is absent, while per-call `ActionGrant`
validation remains the authority for approval, workspace scope, write, network,
timeout, and output limits. A manifest declaration is not a secret broker or an
OS sandbox; the built-in model still reads its provider credential from its
child-process environment until the later credential-broker work.

The process acceptance fixture covers accepted manifests, missing required
grants, malformed frames, crashes, and read timeouts. The CLI fixture disables
every optional capability and verifies that only the required Model plugin and
its route are launched.

## Phase 1 Evidence

The model-tool fixture covers Shell/Patch approval, denial, cancellation,
bounded timeout, plugin rejection, and loop-round recovery. Automatic Shell
calls receive read-only/no-network grants; Patch calls receive the separate
workspace-write grant.

The read-only file-tool fixture covers `file.search` and `file.read` across the
plugin boundary, confirms that the model receives both tools only when the
Files switch is enabled, and verifies that a file result is returned without
an approval prompt or write grant. Executor unit tests cover workspace escape,
UTF-8 validation, and bounded reads. `YUNXI_NEXT_FILES_ENABLED=false` is also
included in the disabled-no-launch matrix.

## Phase 2 Evidence

`yunxi-tool-mcp` owns a second process boundary around one external MCP Server.
For stdio, its command is executed without a shell, its environment is cleared
except for `PATH` and an explicit JSON allowlist, and stdout is consumed through
a bounded newline-delimited JSON-RPC reader. `initialize`,
`notifications/initialized`, `tools/list`, and `tools/call` require matching
request ids and bounded responses; malformed JSON, oversized frames, timeout,
or child exit are fatal to the MCP route only.

The CLI projects discovered tools as `mcp.<server>.<tool>`. These calls are
always paused at the existing Host approval boundary and carry a Host-issued
`ActionGrant`; an MCP child failure is returned as a tool result and clears only
the MCP route. `YUNXI_NEXT_MCP_ENABLED=false` prevents launch and model tool
exposure. Stdio remains the default transport. HTTP is opt-in and requires an
exact scheme/host/port network scope. Header credentials use `secret://reference`
and a Secret grant contains references only; the configured value is injected
inside the MCP bridge only after the Host grant permits that reference. HTTP
responses support JSON and bounded SSE, cache `Mcp-Session-Id`, and send
`notifications/cancelled` after a transport timeout on a best-effort basis.

P2-04 is complete at the bridge boundary. The scope is an authority check in
the Host/plugin path, not an operating-system network sandbox. It does not
claim to stop a remote server that has already received a request, and an
external Secret broker is still future work.

`yunxi-tool-skills` scans only immediate directories beneath a Host-approved
workspace root. It accepts bounded UTF-8 `SKILL.md` instructions and optional
metadata-only `tools.json` declarations, validates paths and schemas, and
provides typed list/context/status responses through `tool.skills@1`. The child
environment is cleared and receives no Provider credential.

The CLI injects available Skill instruction blocks as system messages and
projects declarations as `skill.<skill-id>.<tool>`. These declarations do not
carry executable fields: a model call receives `skill_tool_unavailable` without
an approval prompt or side effect. Disabled Skills are omitted, and a discovery
or context-process failure removes only the Skills route while model chat
continues.

## Phase 4 Baseline Evidence

`yunxi-multi-agent` runs as a separate coordinator process with the stable
`tool.multi-agent@1` contract. Its Host-issued `AgentDelegationGrant` binds a
workspace, session, approval ticket, graph/turn budget, and allowed child-grant
set. Spawn rejects depth, count, turn, transcript, or grant escalation before
state is written. Graphs, bounded events, and child transcripts use recoverable
same-directory replacement beneath `.yunxi-next/multi-agent`.

The CLI exposes `agent.spawn`, `agent.list`, `agent.message`, and
`agent.interrupt` only when the capability is enabled. Spawn and message reuse
the existing user approval continuation. Each child turn starts a separate
Model plugin process, receives its own transcript and no parent tool catalog,
and is explicitly shut down afterward. A child API failure marks only that
branch failed and is returned to the parent model as a tool result; the main
model route remains usable. Restart recovery marks only stale Running branches
failed, recursive interruption spares siblings, and process fixtures verify
that Provider credentials are absent from persisted coordinator state.

This is a baseline rather than full integration. Child turns are synchronous,
so `agent.interrupt` updates stored cancellation between turns but cannot yet
terminate an in-flight model HTTP request. Parallel/background workers, live
cancellation, delegated child tools, model selection, and a Web graph/event
view remain Phase 4 work.

## Channel and Media Baseline Evidence

`yunxi-voice` provides bounded `voice.transcribe@1` and
`voice.synthesize@1` request/event types and a Rust child-process fixture. The
fixture announces both capabilities through the protocol-v2 handshake and
returns deterministic transcript events or synthetic audio markers. Host
integration tests verify the expected capability set, malformed requests,
cancellation, and route removal after disable. It intentionally has no
microphone, speaker, codec, SDK, network, or `Device` grant.

`yunxi-weixin` provides a bounded `channel.weixin@1` contract and a separate
`yunxi-weixin-plugin-fixture`. The fixture declares required `Network` and
`Secret` grants so the manifest shape is testable, then performs only local
in-memory message registration and delivery-state transitions. Host tests
verify exact capability matching, idempotent inbound/ACK behavior, malformed
payload handling, and route removal after disable. The grants are declarations
for a future adapter, not a credential broker or permission to access Weixin.

Both fixtures are now launched by `yunxi-cli` and projected as live Host/Web
inventory routes when enabled. They are not yet connected to real devices or a
Weixin service. The next production steps are to replace the fixture handlers
behind the same Host boundary and add failure fallback tests showing text chat
remains available.
