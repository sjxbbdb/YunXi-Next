# yunxi-protocol

`yunxi-protocol` defines the versioned local wire contract between a YunXi host
and an isolated capability plugin. It owns message shapes, bounded JSONL
transport, and readiness handshakes.

It does not launch processes, call model APIs, maintain chat history, or render
terminal output.

Protocol version 1 uses newline-delimited JSON over an IPv4 loopback TCP
connection. Frames are bounded at 16 MiB. The launch token correlates a child
with its host; it must not be treated as authentication or sandboxing.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Protocol messages, transport, and handshake logic |
| `Cargo.toml` | Serialization-only dependency boundary |
