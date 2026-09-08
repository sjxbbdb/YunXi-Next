# Source Map

- `lib.rs` exposes the storage facade and plugin entry point.
- `record.rs` owns the Next session schema and legacy JSON projection.
- `store.rs` owns bounded filesystem access and state mutations.
- `event_log.rs` owns read-only legacy JSONL normalization, cursor replay, and
  fingerprinted reversible event import under the Next migration namespace.
- `migration.rs` owns explicit legacy plan/apply/rollback and migration
  manifests without writing legacy sources.
- `plugin.rs` dispatches versioned protocol operations.
- `bin/` contains the standalone plugin executable.
