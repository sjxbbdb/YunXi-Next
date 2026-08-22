# Source Map

- `lib.rs` exposes the storage facade and plugin entry point.
- `record.rs` owns the Next session schema and legacy JSON projection.
- `store.rs` owns bounded filesystem access and state mutations.
- `plugin.rs` dispatches versioned protocol operations.
- `bin/` contains the standalone plugin executable.
