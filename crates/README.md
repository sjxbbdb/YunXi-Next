# Workspace Crates

This directory contains production Rust packages. A crate belongs here only
when it owns a stable responsibility and can state its dependency direction.

| Crate | Responsibility |
| --- | --- |
| [`yunxi-cli`](yunxi-cli/README.md) | Terminal chat, conversation history, and model-plugin hosting |
| [`yunxi-context`](yunxi-context/README.md) | Isolated root-to-cwd `AGENTS.md` context composition |
| [`yunxi-kernel`](yunxi-kernel/README.md) | Process-isolated plugin lifecycle and kernel health |
| [`yunxi-memory`](yunxi-memory/README.md) | Read-only legacy-compatible memory recall plugin |
| [`yunxi-model-openai`](yunxi-model-openai/README.md) | OpenAI-compatible HTTP capability running as a child process |
| [`yunxi-persona`](yunxi-persona/README.md) | Isolated persona and memory-context compiler plugin |
| [`yunxi-plugin-host`](yunxi-plugin-host/README.md) | Capability provider catalog and deterministic routing checks |
| [`yunxi-protocol`](yunxi-protocol/README.md) | Versioned local messages, bounded transport, and readiness handshake |

Dependency direction is `yunxi-cli -> capability crates + yunxi-plugin-host +
yunxi-kernel + yunxi-protocol`, `yunxi-plugin-host -> yunxi-kernel +
yunxi-protocol`, and every capability crate depends inward on
`yunxi-protocol`. Capability crates do not depend on one another. The kernel has
no third-party dependencies and does not depend on any capability
implementation, catalog, serialization, or HTTP package.
