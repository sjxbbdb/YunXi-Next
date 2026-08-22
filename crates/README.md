# Workspace Crates

This directory contains production Rust packages. A crate belongs here only
when it owns a stable responsibility and can state its dependency direction.

| Crate | Responsibility |
| --- | --- |
| [`yunxi-companion`](yunxi-companion/README.md) | Isolated deterministic companion response policy |
| [`yunxi-companion-mailbox`](yunxi-companion-mailbox/README.md) | Isolated encrypted proactive-message mailbox |
| [`yunxi-composition`](yunxi-composition/README.md) | Pure dsh-style profile layers and Web plugin inventory projection |
| [`yunxi-cli`](yunxi-cli/README.md) | Terminal chat, conversation history, and model-plugin hosting |
| [`yunxi-context`](yunxi-context/README.md) | Isolated root-to-cwd `AGENTS.md` context composition |
| [`yunxi-kernel`](yunxi-kernel/README.md) | Process-isolated plugin lifecycle and kernel health |
| [`yunxi-memory`](yunxi-memory/README.md) | Legacy-compatible recall and Next-only memory write plugin |
| [`yunxi-model-openai`](yunxi-model-openai/README.md) | OpenAI-compatible HTTP capability running as a child process |
| [`yunxi-persona`](yunxi-persona/README.md) | Isolated persona and memory-context compiler plugin |
| [`yunxi-plugin-host`](yunxi-plugin-host/README.md) | Capability provider catalog and deterministic routing checks |
| [`yunxi-protocol`](yunxi-protocol/README.md) | Versioned local messages, bounded transport, and readiness handshake |
| [`yunxi-scheduler`](yunxi-scheduler/README.md) | Isolated bounded proactive scheduling policy |
| [`yunxi-storage`](yunxi-storage/README.md) | Isolated persistent sessions and legacy session projection |
| [`yunxi-web-contract`](yunxi-web-contract/README.md) | Bounded dsh-compatible browser RPC and event envelopes |

Dependency direction is `yunxi-cli -> capability crates + yunxi-composition +
yunxi-plugin-host + yunxi-kernel + yunxi-protocol`, `yunxi-plugin-host ->
yunxi-kernel + yunxi-protocol`, `yunxi-composition` depends only on serde and
serde_json, and every capability crate depends inward on
`yunxi-protocol`. Capability crates do not depend on one another. The kernel has
no third-party dependencies and does not depend on any capability
implementation, catalog, serialization, or HTTP package.
