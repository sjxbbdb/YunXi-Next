# Kernel Source Layout

This directory is divided by ownership boundary rather than by feature count.

| Path | Responsibility |
| --- | --- |
| `lib.rs` | Stable public facade; contains no lifecycle implementation |
| `error.rs` | Public errors returned by kernel operations |
| [`plugin/`](plugin/README.md) | Plugin identity, launch description, and observable state |
| [`runtime/`](runtime/README.md) | Registry and lifecycle coordination |
| [`supervision/`](supervision/README.md) | Private operating-system child-process boundary |
| [`bin/`](bin/README.md) | Minimal executable entry points |

Dependency direction is `runtime -> supervision` and `runtime -> plugin`.
`plugin` never depends on runtime or process supervision.
