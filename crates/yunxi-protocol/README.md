# yunxi-protocol

`yunxi-protocol` defines the versioned local wire contract between a YunXi host
and an isolated capability plugin. It owns message shapes, bounded JSONL
transport, and readiness handshakes.

It does not launch processes, call model APIs, maintain chat history, or render
terminal output.

Protocol version 2 uses newline-delimited JSON over an IPv4 loopback TCP
connection. Plugins announce validated, versioned capability ids. Runtime calls
use a capability-neutral invocation envelope with typed payloads at each
capability boundary. Frames are bounded at 16 MiB. The launch token correlates
a child with its host; it must not be treated as authentication or sandboxing.

Current typed contracts are `model.chat@1`, `context.compose@1`,
`memory.recall@1`, and `persona.context@1`. Capability versions travel in both
the readiness declaration and every invocation frame.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Protocol messages, transport, and handshake logic |
| `Cargo.toml` | Serialization-only dependency boundary |
