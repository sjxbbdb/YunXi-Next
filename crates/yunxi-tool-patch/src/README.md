# Source Map

- `applier.rs` parses, validates, plans, and applies bounded file changes.
- `plugin.rs` performs the handshake and dispatches `tool.patch@1`.
- `lib.rs` exposes the testable patch applier and plugin facade.
- `bin/` contains the standalone executable.
