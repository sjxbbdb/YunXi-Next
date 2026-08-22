# Source Map

- `executor.rs` validates action grants and owns bounded child-process execution.
- `plugin.rs` performs the handshake and dispatches `tool.shell@1`.
- `lib.rs` exposes the plugin facade and testable executor.
- `bin/` contains the standalone executable.
