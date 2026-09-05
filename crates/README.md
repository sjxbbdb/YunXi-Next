# Workspace Crates

This directory contains production Rust packages. A crate belongs here only
when it owns a stable responsibility and can state its dependency direction.

| Crate | Responsibility |
| --- | --- |
| [`yunxi-cordis-core`](yunxi-cordis-core/README.md) | Minimal trusted Rust Context, Service, inject, Event, Effect, and Fiber primitives |
| [`yunxi-cordis-runtime`](yunxi-cordis-runtime/README.md) | Static Cordis-style plugin registry, enable/disable lifecycle, and failure isolation |
| [`yunxi-agent-spine`](yunxi-agent-spine/README.md) | Minimal replaceable Rust Agent loop, context seam, model/tool contracts, and budgets |
| [`yunxi-companion`](yunxi-companion/README.md) | Isolated deterministic companion response policy |
| [`yunxi-companion-mailbox`](yunxi-companion-mailbox/README.md) | Isolated encrypted proactive-message mailbox |
| [`yunxi-composition`](yunxi-composition/README.md) | Pure dsh-style profile layers and Web plugin inventory projection |
| [`yunxi-cli`](yunxi-cli/README.md) | Terminal chat, conversation history, and model-plugin hosting |
| [`yunxi-context`](yunxi-context/README.md) | Isolated root-to-cwd `AGENTS.md` context composition |
| [`yunxi-kernel`](yunxi-kernel/README.md) | Process-isolated plugin lifecycle and kernel health |
| [`yunxi-memory`](yunxi-memory/README.md) | Legacy-compatible recall and Next-only memory write plugin |
| [`yunxi-model-openai`](yunxi-model-openai/README.md) | OpenAI-compatible HTTP capability running as a child process |
| [`yunxi-multi-agent`](yunxi-multi-agent/README.md) | Isolated agent graph, budget, transcript, and lifecycle coordination |
| [`yunxi-persona`](yunxi-persona/README.md) | Isolated persona and memory-context compiler plugin |
| [`yunxi-plugin-host`](yunxi-plugin-host/README.md) | Capability provider catalog, routing checks, isolated processes, and bounded restart recovery |
| [`yunxi-protocol`](yunxi-protocol/README.md) | Versioned local messages, bounded transport, and readiness handshake |
| [`yunxi-scheduler`](yunxi-scheduler/README.md) | Isolated bounded proactive scheduling policy |
| [`yunxi-settings`](yunxi-settings/README.md) | Persistent restart-scoped capability switches and environment overrides |
| [`yunxi-storage`](yunxi-storage/README.md) | Isolated persistent sessions and legacy session projection |
| [`yunxi-tool-files`](yunxi-tool-files/README.md) | Read-only workspace file search and bounded file viewing |
| [`yunxi-tool-mcp`](yunxi-tool-mcp/README.md) | Isolated stdio MCP Server bridge, discovery, and Host-approved calls |
| [`yunxi-tool-patch`](yunxi-tool-patch/README.md) | Isolated, Host-approved workspace patch application |
| [`yunxi-tool-shell`](yunxi-tool-shell/README.md) | Isolated, Host-approved bounded shell execution |
| [`yunxi-tool-skills`](yunxi-tool-skills/README.md) | Isolated Skill discovery, bounded context, and metadata-only tool declarations |
| [`yunxi-voice`](yunxi-voice/README.md) | Versioned voice transcribe/synthesize contracts and deterministic process-host fixture; no device runtime |
| [`yunxi-weixin`](yunxi-weixin/README.md) | Versioned Weixin channel contract and deterministic process-host fixture; no Weixin SDK or network runtime |
| [`yunxi-web-gateway`](yunxi-web-gateway/README.md) | Bounded in-memory dsh RPC dispatcher, projections, and event buffers |
| [`yunxi-web-contract`](yunxi-web-contract/README.md) | Bounded dsh-compatible browser RPC and event envelopes |

Dependency direction is `yunxi-cli -> capability crates + yunxi-composition +
yunxi-plugin-host + yunxi-kernel + yunxi-protocol`, `yunxi-plugin-host ->
yunxi-kernel + yunxi-protocol`, `yunxi-composition` depends only on serde and
serde_json, and every capability crate depends inward on
`yunxi-protocol`; `yunxi-cli` also exposes the optional Web Host facade through
`yunxi-web-gateway` and `yunxi-web-contract`. Capability crates do not depend
on one another. The gateway depends only on composition and Web contract data;
it does not depend on the CLI, kernel, or provider implementation. The kernel
has no third-party dependencies and does not depend on any capability
implementation, catalog, serialization, or HTTP package. The Cordis core/runtime
and Agent spine are integrated foundation crates. The default CLI turn path uses
the spine-backed Host adapter, while application storage and Web projection
remain in the CLI facade. Dynamic Rust plugin loading is intentionally not part
of v1. Voice and Weixin are launch-wired contract/fixture crates and appear in
the user-facing inventory when enabled, but they do not yet contain real device,
login, or network backends.
