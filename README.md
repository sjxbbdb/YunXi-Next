# YunXi Next

YunXi Next is the next-generation Rust architecture for YunXi, built around a
plugin-first capability runtime.

## Repository Boundary

This repository is intentionally independent from the legacy project at
`D:\YunXi Agent`. The legacy project remains a read-only reference and a
runnable fallback while the new architecture is developed and verified.

## Kernel Baseline

The first kernel intentionally has no model, tool, memory, voice, or Web UI
integration. It owns only the minimum lifecycle needed to run plugins safely:

- register a process-isolated plugin;
- start and observe it;
- contain an unexpected process exit;
- keep the kernel and sibling plugins running;
- stop supervised processes during shutdown.

Plugin failures remain visible until an explicit restart. The kernel does not
automatically restart a crashing plugin.

Process isolation protects the kernel from plugin crashes. It is not yet a
filesystem, network, or resource-usage sandbox.

## Repository Map

- [`crates/`](crates/README.md) contains production Rust packages.
- [`docs/`](docs/README.md) contains architecture and repository maps.
- [`AGENTS.md`](AGENTS.md) defines repository boundaries and verification rules.
- [`docs/project-structure.md`](docs/project-structure.md) explains every tracked
  directory and source file.

## Verify

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -p yunxi-kernel
```
