# Protocol Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable protocol facade and version constants |
| `capability.rs` | Validated capability ids, contract versions, and built-in names |
| `identity.rs` | Context, persona, and memory recall capability payloads |
| `invocation.rs` | Generic request/response envelopes and typed payload codecs |
| `message.rs` | Host, plugin, handshake, and model-chat payload messages |
| `transport.rs` | Size-bounded JSONL over an established TCP stream |
| `handshake.rs` | Loopback listener, version negotiation, and readiness |

Transport knows how to move typed frames. Handshake knows which frames must be
exchanged before a plugin is ready. Neither layer knows model API semantics.
