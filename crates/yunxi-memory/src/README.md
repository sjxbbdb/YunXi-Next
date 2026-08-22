# Memory Plugin Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable plugin facade |
| `record.rs` | Legacy-compatible stored record schema and validation helpers |
| `store.rs` | Bounded, read-only JSONL loading and workspace fingerprinting |
| `recall.rs` | Boot and prompt-relevant memory selection |
| `plugin.rs` | `memory.recall@1` protocol dispatch loop |
| [`bin/`](bin/README.md) | Standalone plugin executable |
