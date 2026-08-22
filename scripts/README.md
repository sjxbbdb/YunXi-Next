# Repository scripts

This directory contains explicit developer and installation operations that do
not belong in a Rust crate.

| File | Responsibility |
| --- | --- |
| `install-windows.ps1` | Build `yunxi-next`, install it beside Cargo, migrate prior PATH entries, and preserve legacy `yunxi` |
