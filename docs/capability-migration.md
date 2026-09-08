# Legacy Capability Migration

## Objective

YunXi Next will inherit the useful behavior of the legacy YunXi Agent without
copying its monolithic runtime assembly. Business capabilities run in isolated
child processes and communicate through versioned capability calls. The legacy
repository remains a read-only behavioral reference during migration.

Pluginization means a process boundary, a declared wire contract, explicit
grants, and independent failure state. Splitting code into another Rust crate is
not sufficient by itself.

Status vocabulary used below:

- `integrated`: usable through the current product path and covered by the
  required process, contract, grant, disable, and failure tests;
- `baseline`: the Rust contract and isolation evidence exists, but a real
  provider, product integration, or parity surface is still missing;
- `planned`: the migration has not reached a usable implementation.

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
| Model completion | `yunxi-agent-provider` | `model.chat@1:complete` | `yunxi-model-openai` | Integrated local path: isolated request loop, required network/provider-secret declarations, API failure containment, bounded tool-call response, provider SSE parsing, and bounded Agent events consumed by REPL, TUI, `run --jsonl`, and Web mux polling |
| Prompt and AGENTS context | `yunxi-agent-context` | `context.compose@1:compose` | `yunxi-context` | Integrated: bounded root-to-cwd read path |
| Persona and soul | `yunxi-agent-persona` | `persona.context@1:compile` | `yunxi-persona` | Integrated: default/custom profile and soul read path |
| Memory recall | `yunxi-agent-persona`, `yunxi-agent-storage` | `memory.recall@1:recall` | `yunxi-memory` | Integrated: bounded legacy + Next JSONL recall |
| Memory extraction and writes | `yunxi-agent-persona`, `yunxi-agent-storage` | `memory.write@1:extract/review`, `memory.management@1` | `yunxi-memory` | Integrated local path: rule extraction, privacy policy, dedup, pending review, search/list/show/approve/reject/delete/clear/on/off, and Next-only writes through the process Host facade |
| Companion response policy | `yunxi-agent-companion` | `companion.decide@1:decide` | `yunxi-companion` | Integrated: deterministic emotion/tone/follow-up policy reaches model context |
| Relationship mailbox | `yunxi-agent-companion`, `yunxi-agent-storage` | `companion.mailbox@1` | `yunxi-companion-mailbox` | Integrated local path: encrypted Next mailbox, idempotency, list/get/read, Web projection, and safe migration reporting; opaque legacy credential-store payloads remain unsupported rather than guessed |
| Proactive scheduling | `yunxi-agent-runtime`, `yunxi-agent-companion` | `scheduler.proactive@1:evaluate` | `yunxi-scheduler` | Integrated local path: explicit signals, quiet hours, limits, encrypted mailbox enqueue, bounded background worker, cancellation, and session-owned shutdown |
| Session history and resume | `yunxi-agent-storage` | `storage.sessions@1`, `session.management@1` | `yunxi-storage` | Integrated local path: append/list/load/resume/show/history/rollout/graph/fork/archive/pin, read-only legacy projection, explicit migration, and bounded legacy event replay/import |
| Shell execution | `yunxi-agent-tools`, `yunxi-agent-exec`, `yunxi-agent-sandbox` | `tool.shell@1` | `yunxi-tool-shell` | Baseline integrated: isolated execution, required approval/workspace-read manifest grants, bounded output/timeout, and Host-controlled model loop; OS sandbox and optional write/network policy expansion pending |
| Patch application | `yunxi-agent-tools`, `yunxi-agent-patch` | `tool.patch@1` | `yunxi-tool-patch` | Baseline integrated: isolated apply, required approval/workspace-read/workspace-write manifest grants, path validation, rollback, and Host-controlled model loop |
| File search and viewing | `yunxi-agent-tools`, `yunxi-agent-context` | `tool.files@1:search/read` | `yunxi-tool-files` | Baseline integrated: isolated name search and bounded UTF-8 reads under a required read-only workspace grant; write, rename, indexing, and richer ignore policy remain out of scope |
| MCP bridge | `yunxi-agent-mcp`, `yunxi-agent-tools` | `tool.mcp@1:list/call/cancel/status` | `yunxi-tool-mcp` | Baseline integrated: one explicit stdio or opt-in HTTP Server, bounded JSON-RPC initialize/list/call/cancel, JSON/SSE responses, session reuse, dynamic model projection, Host Approval, exact network scopes, reference-only Secret grants, redaction, and isolated crash recovery |
| Skills and dynamic tools | `yunxi-agent-skills`, `yunxi-agent-tools` | `tool.skills@1:list/context/status` plus Host-side action facade | `yunxi-tool-skills` | Integrated local path: isolated discovery/injection, inert metadata-only declarations, and separately declared fixed executable actions that are opt-in, approval-gated, grant-bounded, cancellable, and process-isolated |
| Multi-agent coordination | `yunxi-agent-multi-agent` | `tool.multi-agent@1` | `yunxi-multi-agent` | Integrated local path: isolated coordinator, grant subsets, budgets, exact child file/patch tool catalogs, per-turn model selection, CLI one-shot turns, concurrent Web continuations, streaming, targeted interruption, persisted restart recovery, and sibling failure isolation; remote/multi-host scheduling remains outside the local product |
| Weixin channel | `yunxi-agent-weixin` | `channel.weixin@1` | `yunxi-weixin` | Integrated local boundary: one Host process plugin selects loopback or explicit production iLink, with QR login polling, non-blocking long poll, send/reply, encrypted SecretStore, session binding, idempotent queue/ACK/cancel/approval, and Agent bridge contracts; real-account/media E2E remains manual |
| Voice input (speech recognition) | `yunxi-agent-voice` | `voice.transcribe@1` | `yunxi-voice` | Integrated local boundary: one Host process plugin selects loopback or an explicit bounded sidecar, with Device grant, doctor/devices/transcribe/chat/talk, cancellation/backpressure, panic/crash/timeout isolation, and text fallback; physical microphone/provider evidence remains manual |
| Voice output (speech synthesis) | `yunxi-agent-voice` | `voice.synthesize@1` | `yunxi-voice` | Integrated local boundary: speak/playback/save, bounded playable WAV/PCM file adapter, Device grant, external sidecar transport, hot replacement, cancellation, and text fallback; physical speaker/provider evidence remains manual |

The Voice and Weixin rows are integrated as local, replaceable process
boundaries. Their loopback modes remain fixtures, and only real devices,
providers, accounts, and media traffic can close the external integration gate.
Creating or launching a fixture is evidence for the process boundary only.

Legacy `yunxi-agent-core`, `yunxi-agent-protocol`, and
`yunxi-agent-runtime` contain mixed contracts and orchestration. Their behavior
is redistributed across the YunXi Next protocol, plugin host, and narrow
capability contracts rather than migrated as one runtime plugin.

Legacy `yunxi-agent-cli`, `yunxi-agent-tui`, and the embedded Web server are
interaction surfaces. They consume the same plugin catalog. The current Web
settings page renders the bounded built-in and discovered package inventory and
persists enable/disable state without loading disabled plugin code. A successful
Web write rebuilds the current Host composition; the WebHost does not hot-unmount
a single running plugin in place. The dynamic package manager rescans its
explicit directory at refresh/inventory boundaries and can replace or unload
manager-owned processes and routes. A standalone CLI process reads the setting
when its next Host is created. `yunxi-settings` has 15 built-in keys, and the
current CLI/Web launch path exposes all 15 connected optional entries;
`voice` and `weixin` use the same Host routes for loopback and explicitly
configured external adapters; readiness is reported without pretending that
an untested device or account is online.

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
4. **Action path:** shell, patch, MCP, executable Skill actions, and
   multi-agent child turns run only after host-issued approval and resource
   grants. Read-only agent listing and stored-state interruption do not acquire
   execution authority. Metadata-only Skill declarations remain inert; an
   executable action requires a separate fixed allowlist entry and approval. A
   plugin result never upgrades its own authority.
5. **Channel and media path:** Voice and Weixin process plugins select either a
   deterministic loopback or an explicitly configured sidecar/iLink adapter.
   Host-issued device/network/secret authority, failure fallback, and bounded
   lifecycle are automated; real hardware/account/media validation is external.
6. **Management surface:** Web settings reads the bounded Host inventory,
   changes persisted enable state, and shows lifecycle health from the kernel
   and plugin host. Manifest metadata remains Host-owned at the dsh wire
   boundary; WebHost rebuilds its current composition after a successful write.
   An explicit dynamic package directory is discovered, dependency-ordered,
   reconciled, and unloaded through `yunxi-plugin-host`; invalid or crashing
   packages remain isolated from healthy siblings.

## Operator Prerequisites

The repository supplies contracts, fixtures, and local loopback behavior. The
operator must supply the following for real integrations:

| Capability | Required outside this repository | Current local behavior |
| --- | --- | --- |
| Model chat | A valid `DEEPSEEK_API_KEY` or compatible provider credential, endpoint, and model | Real OpenAI-compatible HTTP model path; credentials are not stored in settings or protocol frames |
| Files/Shell/Patch/MCP/Skills | An intentional workspace path, external command/server, and approval decisions | Side-effecting/external capabilities are off by default; grants are Host-controlled, not OS sandboxing |
| Voice input/output | Microphone, speaker, codec/provider SDK, and a separate sidecar implementing the Voice contract | Loopback by default; an explicit JSONL sidecar can be selected, but the repository does not certify its devices or audio |
| Weixin | Real account, QR scan/device confirmation, network access, account/session material, and media implementation | Loopback by default; explicit production mode uses bounded HTTPS iLink and encrypted file storage, but requires real-account validation |
| Legacy migration | A reviewed workspace and, optionally, explicit `YUNXI_MIGRATION_LEGACY_HOME`/`YUNXI_HOME` and `YUNXI_NEXT_HOME` roots | `plan` is read-only; `apply` writes only new Next files/manifests; rollback removes only unchanged generated targets |

Enabling a plugin is the only v1 permission control. Enabled means the Host
starts that plugin and supplies its declared grants; disabled means no process,
route, or grant. This is an application-level choice, not an OS sandbox.

## Current Completion Boundary

The current branch may claim the following, and no more:

| Area | Completed in-repository | Still outside completion |
| --- | --- | --- |
| Streaming | Provider SSE parsing, protocol events, Agent backpressure, REPL/TUI/JSONL output, Web mux projection, cancellation tests | Remote multi-user delivery guarantees and provider-specific production soak testing |
| Multi-agent | Persisted coordinator, isolated child models, exact child file/patch grants, concurrent Web jobs, per-turn models, stream projection, targeted interruption, reattach recovery, and sibling isolation | Remote workers, multi-host scheduling, and production load/soak evidence |
| Voice | Typed contracts, loopback and external JSONL sidecar through one Host route, WAV/PCM file IO, Device grants, fallback, replacement, and process-failure tests | Real microphone/speaker/codec/provider implementation and manual device/permission/playback evidence |
| Weixin | Loopback and production iLink through one Host route, encrypted file store, QR/status/serve/send/reply/session/remote-control operations, non-blocking polling, Agent bridge contracts, and local HTTP/store tests | Real-account QR/login/send/receive/reconnect evidence and real media byte handling |
| Secrets | Host-scoped one-shot references, redacted provider failures, optional authenticated durable broker storage, and separate encrypted Weixin store | OS keychain/HSM integration and production rotation/redaction audit |
| Sandbox | Process failure isolation, application-level grants, path checks, request/time/output bounds | OS-enforced filesystem/network/CPU/memory/handle/child-process isolation |
| Migration | Explicit plan/apply/rollback plus lazy session import-on-first-write; legacy sources remain read-only | Full semantic parity for opaque or unsupported legacy formats and operator backup/restore acceptance |

Passing deterministic fixtures or unit tests does not close any item in the
right-hand column.

## Migration Boundary

`yunxi-next migrate sessions plan|apply` and `yunxi-next migrate rollback
<migration-id>` expose the current explicit migration facade. Despite the
historical `sessions` command name, the plan can include workspace sessions and
memory plus supported persona/control/memory items from an explicitly selected
legacy user home. It does not guess or decode opaque mailbox formats.

`plan` performs no write. `apply` revalidates source fingerprints, never
replaces an existing Next target, writes only Next files and a bounded manifest,
and leaves every legacy byte unchanged. `rollback` removes only targets still
matching that manifest and preserves files changed after migration. Normal
legacy-session resume remains a separate lazy import-on-first-write path.

## Voice Migration Boundary

Voice input and voice output are both part of the inheritance scope. The current
repository has one Host-supervised process plugin with deterministic loopback,
a usable external sidecar transport, and a bounded WAV/PCM file adapter, but no
repository-owned OS microphone/speaker or provider SDK. Input and output remain
separate versioned capabilities inside the Voice plugin; the user-facing v1
permission is the single Voice plugin switch, and text chat remains independent.

The current Host route and provider boundaries have these properties:

- `voice.transcribe@1` accepts bounded audio chunks, emits partial and final
  transcripts, and supports cancellation and backpressure;
- `voice.synthesize@1` accepts bounded text, emits audio chunks, and supports
  cancellation and backpressure;
- loopback requests no `Device` grant because it has no device access; an
  external sidecar declares feature support and uses Host-issued `DeviceGrant`
  values rather than authority inferred by a voice plugin;
- raw audio is not persisted by default; an explicit user action is required
  for recording or diagnostic retention;
- a missing device, provider timeout, malformed audio frame, or plugin crash
  produces a visible warning and leaves the text CLI/model route available;
- channel adapters such as Weixin consume these same contracts instead of
  embedding a second speech implementation.

Loopback covers partial/final input, synthetic output, cancellation, bounds,
malformed payloads, and process disable/removal. Sidecar and Host tests cover
bounded JSONL, Device grants, timeout, cancellation, panic/crash quarantine,
malformed/oversized response, replacement, restart, and text fallback. WAV/PCM
tests prove real bounded file IO. Physical device permission denial, playable
speaker output, provider credentials, and SDK integration still require
external manual evidence. A
provider-specific speech SDK is an implementation detail and must not leak into
the host protocol.

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

The release-level proof is `scripts\acceptance-audit.ps1`. It runs the exact
bad-frame, crash, timeout, bounded-restart, disable/re-enable, replacement,
unload/ownership, and migration rollback filters in addition to the aggregate
workspace tests. The script installs only `yunxi-next.exe` into a private
temporary directory and protects the old executable with before/after SHA-256
and a read-only legacy-tree fingerprint. It has no permission to modify PATH,
the registry, services, the old repository, or the current checkout.

## Phase 0 Evidence

The current baseline records plugin grant declarations in the versioned
handshake manifest. `yunxi-plugin-host` rejects a launch before route
registration when a host-required grant is absent, while per-call `ActionGrant`
validation remains the authority for approval, workspace scope, write, network,
timeout, and output limits. A manifest declaration is not a secret broker or an
OS sandbox. The Host stores the built-in model credential in a scoped broker and
resolves a single-use reference before injecting the credential at the final
model-child environment boundary. The default broker is in-memory; an explicit
32-byte master key and path enable authenticated durable storage with rotation,
removal, and redacted audit tests. The child necessarily receives the credential
needed by the provider; no OS keychain/HSM is claimed.

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
MCP's configured reference/value map is not the Host's model Secret Broker or a
platform secret store.

`yunxi-tool-skills` scans only immediate directories beneath a Host-approved
workspace root. It accepts bounded UTF-8 `SKILL.md` instructions and optional
metadata-only `tools.json` declarations, validates paths and schemas, and
provides typed list/context/status responses through `tool.skills@1`. The child
environment is cleared and receives no Provider credential.

The CLI injects available Skill instruction blocks as system messages and
projects declarations as `skill.<skill-id>.<tool>`. A metadata-only declaration
receives `skill_tool_unavailable` without approval or side effect. An explicitly
separate `actions.json` entry may bind the same tool to one fixed relative
program and fixed arguments when `YUNXI_NEXT_SKILLS_ACTIONS_ENABLED=true`.
Execution still requires Host approval and a bounded ActionGrant, clears the
child environment, rejects network/secret authority, propagates cancellation,
and confines failures to that action. Disabled Skills are omitted, and a
discovery or action failure does not stop model chat.

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
Model plugin process, receives its own transcript, and receives only the
file/patch catalog justified by its stored child-grant subset and the current
parent sandbox. It is explicitly shut down afterward. A child API failure marks only that
branch failed and is returned to the parent model as a tool result; the main
model route remains usable. Restart recovery requeues stale Running branches,
preserves transcript/model/budget, excludes cancelled branches, and is
idempotent; recursive interruption spares siblings, and process fixtures verify
that Provider credentials are absent from persisted coordinator state.

The CLI model-tool path above remains synchronous and one-shot. Separately,
WebHost wires `AsyncMultiAgentRuntime` to `subagent.prompt` and
`subagent.interrupt`: a bounded set of continuable child jobs can run while the
Host services other requests, each child uses its own model process, model
deltas become bounded Web events, and interruption propagates to the active
provider stream. Multiple continuations issue provider requests in parallel;
reattaching a persisted root session automatically resumes recoverable workers.
The runtime supports per-turn model selection and validates child-grant subsets.
Remote/multi-host scheduling is not implemented.

## Channel and Media Baseline Evidence

`yunxi-voice` provides bounded `voice.transcribe@1` and
`voice.synthesize@1` request/event types behind one launch-wired process plugin.
Without external configuration it selects deterministic loopback behavior and
synthetic audio. With `YUNXI_VOICE_SIDECAR_PROGRAM` it announces and requires a
Host `Device` grant and forwards doctor/devices/transcribe/speak/chat/talk/
playback/save/cancel through a bounded JSONL child. The same Host route enforces
deadlines, backpressure, process termination, restart, panic quarantine, and
text fallback. The included sidecar binary remains a transport fixture; the
WAV/PCM file adapter is real bounded data IO but not an OS microphone/speaker.

`yunxi-weixin` provides a bounded `channel.weixin@1` contract behind one
launch-wired process plugin. Loopback mode is deterministic; explicit
`YUNXI_WEIXIN_MODE=production` selects the bounded HTTPS iLink transport and
authenticated encrypted file store in that same route. The plugin exposes
login/poll-login/status/doctor, synchronous or non-blocking serve, queued
messages, send/reply, pair/session, remote ACK/cancel/approval, and logout.
Long-poll batches enter an idempotent Agent bridge queue, and stop/logout/plugin
shutdown request cancellation without blocking the Host loop. Host tests cover
exact grants/capabilities, malformed configuration fallback, real iLink request
shapes, route removal, and non-blocking lifecycle.

Voice and Weixin external acceptance is intentionally manual. A local fixture,
loopback transport, iLink control-plane test, or `production_ready` field does
not prove a real microphone/speaker or account/network/media integration.

Both process plugins are launched by `yunxi-cli` and projected as live Host/Web
inventory routes when enabled. Their default loopback providers are not evidence
of real devices or a real Weixin account. External completion requires supplying
the sidecar/account/media prerequisites and attaching the manual evidence listed
in `acceptance-audit.md`.
