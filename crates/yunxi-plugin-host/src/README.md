# Plugin Host Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable crate facade |
| `catalog.rs` | Plugin declarations, capability indexes, and deterministic resolution |
| `runtime.rs` | Launch slots, handshake/re-handshake, invocation, bounded recovery, lifecycle, and shutdown |
| `retry.rs` | Generation-aware, bounded retry policy used by each launch slot |
