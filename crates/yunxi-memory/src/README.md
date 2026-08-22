# Memory Plugin Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable plugin facade |
| `record.rs` | Legacy-compatible stored record schema and validation helpers |
| `store.rs` | Bounded legacy/Next JSONL loading, Next-only append, and workspace fingerprinting |
| `recall.rs` | Boot and prompt-relevant memory selection |
| `write.rs` | Rule extraction, privacy policy, deduplication, persistence, and review |
| `plugin.rs` | `memory.recall@1` and `memory.write@1` protocol dispatch loop |
| [`bin/`](bin/README.md) | Standalone plugin executable |
