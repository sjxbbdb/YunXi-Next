# `yunxi-memory`

`yunxi-memory` provides `memory.recall@1` and `memory.write@1`. Recall consumes
legacy and YunXi Next JSONL records, filters them by state, privacy, expiry, and
workspace scope, then returns bounded boot and dynamic context records.

Write calls perform deterministic rule extraction, privacy classification,
deduplication, conflict staging, and explicit approve/reject review. New data
is written only beneath `.yunxi-next` or `YUNXI_NEXT_HOME`; legacy `.yunxi`
files remain read-only. Provider-based extraction and bulk clear are not yet
implemented.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Schema, bounded store, recall/write policy, and protocol loop |
| `Cargo.toml` | Serde, JSON, and protocol dependencies |
