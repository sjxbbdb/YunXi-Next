# Kernel Runtime

This directory owns the in-memory registry and lifecycle state machine. It
coordinates plugins but delegates operating-system process work to
`supervision`.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Runtime module facade |
| `kernel.rs` | Registration, start, stop, refresh, restart, and shutdown |
| `snapshot.rs` | Kernel health state and immutable aggregate snapshots |

Runtime code may depend on plugin-domain types and the private supervisor API.
Neither dependency may call back into runtime implementation types.
