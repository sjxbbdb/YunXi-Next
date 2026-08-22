# Protocol Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable protocol facade and version constants |
| `capability.rs` | Validated capability ids, contract versions, and built-in names |
| `identity.rs` | Context, persona, and memory recall capability payloads |
| `grant.rs` | Explicit workspace read/write grant carried by stateful calls |
| `memory_write.rs` | Memory extraction and review payloads |
| `sessions.rs` | Session append, list, load, and mutation payloads |
| `companion.rs` | Companion tone and emotional policy payloads |
| `scheduler.rs` | Proactive signal, limit, and plan payloads |
| `mailbox.rs` | Encrypted mailbox management payloads |
| `invocation.rs` | Generic request/response envelopes and typed payload codecs |
| `message.rs` | Host, plugin, handshake, and model-chat payload messages |
| `transport.rs` | Size-bounded JSONL over an established TCP stream |
| `handshake.rs` | Loopback listener, version negotiation, and readiness |

Transport knows how to move typed frames. Handshake knows which frames must be
exchanged before a plugin is ready. Neither layer knows model API semantics.
