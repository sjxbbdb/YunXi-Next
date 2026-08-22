# Workspace Crates

This directory contains production Rust packages. A crate belongs here only
when it owns a stable responsibility and can state its dependency direction.

| Crate | Responsibility |
| --- | --- |
| [`yunxi-kernel`](yunxi-kernel/README.md) | Process-isolated plugin lifecycle and kernel health |

Tests that span one crate stay inside that crate. Cross-crate integration tests
may receive their own workspace package after a second production crate exists.
