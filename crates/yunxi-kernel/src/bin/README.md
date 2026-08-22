# Kernel Binaries

This directory contains executable entry points for the kernel crate.

| File | Responsibility |
| --- | --- |
| `yunxi-kernel.rs` | Starts an empty kernel and reports readiness for smoke checks |

Business logic must remain in the library. Binaries only parse host input,
construct library types, and report outcomes.
