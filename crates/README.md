# Workspace Crates

This directory contains production Rust packages. A crate belongs here only
when it owns a stable responsibility and can state its dependency direction.

| Crate | Responsibility |
| --- | --- |
| [`yunxi-cli`](yunxi-cli/README.md) | Terminal chat, conversation history, and model-plugin hosting |
| [`yunxi-kernel`](yunxi-kernel/README.md) | Process-isolated plugin lifecycle and kernel health |
| [`yunxi-launcher`](yunxi-launcher/README.md) | Native global-command routing between legacy YunXi and YunXi Next |
| [`yunxi-model-openai`](yunxi-model-openai/README.md) | OpenAI-compatible HTTP capability running as a child process |
| [`yunxi-protocol`](yunxi-protocol/README.md) | Versioned local messages, bounded transport, and readiness handshake |

Dependency direction is `yunxi-cli -> yunxi-kernel + yunxi-protocol +
yunxi-model-openai`, and `yunxi-model-openai -> yunxi-protocol`. The launcher
and kernel have no third-party dependencies. The kernel does not depend on any
capability implementation or serialization/HTTP package.
