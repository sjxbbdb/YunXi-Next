# Source Map

- `executor.rs` owns canonical path checks, bounded directory traversal, and
  UTF-8 file reads under a read-only workspace grant.
- `plugin.rs` owns the `tool.files@1` manifest handshake and typed operation
  dispatch.
- `lib.rs` exposes the stable plugin facade and executor functions.
- `bin/` contains the thin standalone executable entry point.

The executor has focused unit tests because its path and size limits are the
security-sensitive behavior of this crate. It never writes files or persists
file contents.
