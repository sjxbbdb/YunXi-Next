# Protocol Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable protocol facade and version constants |
| `message.rs` | Host, plugin, and chat wire messages |
| `transport.rs` | Size-bounded JSONL over an established TCP stream |
| `handshake.rs` | Loopback listener, version negotiation, and readiness |

Transport knows how to move typed frames. Handshake knows which frames must be
exchanged before a plugin is ready. Neither layer knows model API semantics.
