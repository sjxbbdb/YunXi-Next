# YunXi Next Development Instructions

## Repository Boundary

- Develop the new architecture only in this repository.
- Treat `D:\YunXi Agent` as a read-only legacy reference.
- Do not add path dependencies on the legacy repository.

## Kernel Invariants

- Third-party plugin code must not run inside the kernel process.
- A plugin crash must not change kernel health or stop another plugin.
- Plugin shutdown must be explicit and must not leave a supervised child process behind.
- Automatic restart must be bounded and observable before it is introduced.

## Rust

- Use Rust 2024.
- Keep the kernel dependency surface minimal.
- Give each source file one clear responsibility.
- Add a concise `README.md` when introducing a non-generated directory.
- Update `docs/project-structure.md` when files or directories move.
- Forbid unsafe code in the kernel workspace.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` before claiming completion.
