# Source Map

- `lib.rs` exposes the mailbox facade and plugin entry point.
- `store.rs` owns encrypted records, keys, bounds, and idempotency.
- `plugin.rs` dispatches versioned mailbox operations.
- `bin/` contains the standalone plugin executable.
