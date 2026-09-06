# Plugin Host Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable crate facade |
| `catalog.rs` | Plugin declarations, capability indexes, and deterministic resolution |
| `discovery.rs` | Explicit package-directory scanning, bounded manifest parsing, executable/path validation, and dependency ordering |
| `manager.rs` | Directory rescans, enablement reconciliation, generation-safe replacement, and manager-owned unload |
| `runtime.rs` | Launch slots, handshake/re-handshake, invocation, bounded recovery, lifecycle, and shutdown |
| `retry.rs` | Generation-aware, bounded retry policy used by each launch slot |
| `bin/yunxi-plugin-fixture.rs` | Deterministic process fixture used by package discovery and lifecycle tests |
