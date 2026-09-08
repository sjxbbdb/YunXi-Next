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

Manifests may optionally carry `runtime` metadata containing a host-group name
and risk level. The field is omitted from legacy manifests, so protocol v2
messages remain backward compatible. It is a placement/default-startup hint,
not an OS sandbox or an additional permission grant.

Current typed contracts cover model chat, context, persona, memory recall and
write, sessions, companion policy, encrypted mailbox operations, and proactive
scheduling, plus bounded multi-agent graph/turn coordination. Capability
versions travel in both the readiness declaration and every invocation frame.

Skills retain a metadata-only `tools.json` contract. Executable Skill actions
use a separate typed contract: `SkillActionSpec` fixes a relative program and
arguments plus timeout/output limits, while `SkillActionRequest` and
`SkillActionResponse` use action protocol version 1. The protocol carries no
grant or secret; the Host must enforce an approved grant and workspace scope
before launching the child. Host cancellation is outside the child protocol
and must terminate the child process at the Host boundary.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Protocol messages, transport, and handshake logic |
| `Cargo.toml` | Serialization-only dependency boundary |
