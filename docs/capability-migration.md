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
| Model completion | `yunxi-agent-provider` | `model.chat@1:complete` | `yunxi-model-openai` | Transport integrated; manifest and grants pending |
| Prompt and AGENTS context | `yunxi-agent-context` | `context.compose@1` | `yunxi-context` | Planned: inheritance wave 1 |
| Persona and soul | `yunxi-agent-persona` | `persona.context@1` | `yunxi-persona` | Planned: inheritance wave 1 |
| Memory recall | `yunxi-agent-persona`, `yunxi-agent-storage` | `memory.recall@1` | `yunxi-memory` | Planned: inheritance wave 1 |
| Memory extraction and writes | `yunxi-agent-persona`, `yunxi-agent-storage` | `memory.write@1` | `yunxi-memory` | Planned: inheritance wave 2 |
| Companion response policy | `yunxi-agent-companion` | `companion.decide@1` | `yunxi-companion` | Planned: inheritance wave 2 |
| Relationship mailbox | `yunxi-agent-companion`, `yunxi-agent-storage` | `companion.mailbox@1` | `yunxi-companion-mailbox` | Planned: inheritance wave 2 |
| Proactive scheduling | `yunxi-agent-runtime`, `yunxi-agent-companion` | `scheduler.proactive@1` | `yunxi-scheduler` | Planned: inheritance wave 2 |
| Session history and resume | `yunxi-agent-storage` | `storage.sessions@1` | `yunxi-storage` | Planned: inheritance wave 2 |
| Shell execution | `yunxi-agent-tools`, `yunxi-agent-exec`, `yunxi-agent-sandbox` | `tool.shell@1` | `yunxi-tool-shell` | Planned after host grants |
| Patch application | `yunxi-agent-tools`, `yunxi-agent-patch` | `tool.patch@1` | `yunxi-tool-patch` | Planned after host grants |
| MCP bridge | `yunxi-agent-mcp`, `yunxi-agent-tools` | `tool.mcp@1` | `yunxi-tool-mcp` | Planned: inheritance wave 3 |
| Skills and dynamic tools | `yunxi-agent-skills`, `yunxi-agent-tools` | `tool.skills@1` | `yunxi-tool-skills` | Planned: inheritance wave 3 |
| Multi-agent coordination | `yunxi-agent-multi-agent` | `tool.multi-agent@1` | `yunxi-multi-agent` | Planned: inheritance wave 3 |
| Weixin channel | `yunxi-agent-weixin` | `channel.weixin@1` | `yunxi-channel-weixin` | Planned: inheritance wave 4 |
| Speech recognition | `yunxi-agent-voice` | `voice.transcribe@1` | `yunxi-voice` | Planned: inheritance wave 4 |
| Speech synthesis | `yunxi-agent-voice` | `voice.synthesize@1` | `yunxi-voice` | Planned: inheritance wave 4 |

Legacy `yunxi-agent-core`, `yunxi-agent-protocol`, and
`yunxi-agent-runtime` contain mixed contracts and orchestration. Their behavior
is redistributed across the YunXi Next protocol, plugin host, and narrow
capability contracts rather than migrated as one runtime plugin.

Legacy `yunxi-agent-cli`, `yunxi-agent-tui`, and the embedded Web server are
interaction surfaces. They will consume the same plugin catalog. The future Web
settings page will render installed plugin manifests and persist enable/disable
state without loading disabled plugin code.

`yunxi-agent-eval` remains development infrastructure. Its scenarios become
cross-plugin acceptance fixtures instead of a runtime capability.

## Migration Order

1. **Foundation:** generic invocation protocol, validated capability ids, and
   deterministic provider catalog. The model plugin proves the complete path.
2. **Read-only identity path:** context, persona, and memory recall compose the
   system context before a model request. Failure falls back to plain chat.
3. **Stateful companion path:** memory writes, storage, companion policy,
   mailbox, and scheduler gain explicit workspace-scoped storage grants.
4. **Action path:** shell, patch, MCP, skills, and multi-agent run only after
   host-issued approval and resource grants. A plugin result never upgrades its
   own authority.
5. **Channel and media path:** Weixin and voice become optional adapters over
   the same runtime calls. Their failure cannot stop terminal chat.
6. **Management surface:** Web settings reads manifests, changes persisted
   enable state, and shows lifecycle health from the kernel and plugin host.

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

The migration ledger status changes only after these gates pass; source copied
into the new repository but still called in-process remains `planned`.
