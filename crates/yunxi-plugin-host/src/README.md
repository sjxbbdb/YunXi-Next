# Plugin Host Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable crate facade |
| `catalog.rs` | Plugin declarations, capability indexes, and deterministic resolution |
| `runtime.rs` | Multi-process launch, invocation, failure removal, and shutdown |
