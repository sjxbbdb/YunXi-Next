# `yunxi-memory`

`yunxi-memory` is the read-only provider for `memory.recall@1`. It consumes the
legacy YunXi JSONL memory layout without rewriting it, filters records by state,
privacy, expiry, and workspace scope, then returns bounded boot and dynamic
context records.

This first inheritance wave cannot write, approve, reject, merge, or clear
memories. Those operations belong to the later `memory.write` contract.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Legacy schema, bounded store, recall policy, and protocol loop |
| `Cargo.toml` | Serde, JSON, and protocol dependencies |
