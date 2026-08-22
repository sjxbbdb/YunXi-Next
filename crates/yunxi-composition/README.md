# yunxi-composition

Pure Rust composition primitives inspired by DeepSeek Harness (dsh) profiles.
The crate models an ordered set of bundle and overlay layers, applies explicit
insert/replace/enable/disable/remove operations, and produces a bounded plugin
inventory snapshot suitable for a Web API adapter.

This crate does not load code, spawn processes, read credentials, or make
network calls. Process ownership remains with `yunxi-kernel` and
`yunxi-plugin-host`; a composition failure is data returned to the caller.

## Ownership

- `entry.rs` validates stable entry identities, module names, and JSON config.
- `layer.rs` defines serializable patch operations and layer bounds.
- `profile.rs` applies layers in dsh-compatible order and preserves entry order.
- `inventory.rs` projects composition state into the dsh Web plugin inventory
  shape, including optional lifecycle phases.

The current Web adapter is intentionally not part of this crate. It will map
the snapshot to dsh's `pluginInventory.list()` RPC while Rust implements the
remaining `/api/*` and event-stream contract.
