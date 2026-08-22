# Plugin Domain

This directory describes plugins as data. It does not own child processes or
the kernel registry.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Module boundary and internal re-exports |
| `id.rs` | Stable plugin identifiers and validation errors |
| `command.rs` | Executable, arguments, environment, and working directory |
| `spec.rs` | User-facing plugin description assembled for registration |
| `state.rs` | Failures, lifecycle states, and immutable snapshots |

New plugin metadata belongs here only when it is independent of a running
kernel instance.
