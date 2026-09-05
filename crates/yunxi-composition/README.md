# yunxi-composition

Pure Rust composition primitives inspired by DeepSeek Harness (dsh) profiles.
The crate models an ordered set of bundle and overlay layers, applies explicit
insert/replace/enable/disable/remove operations, and produces a bounded plugin
inventory snapshot suitable for a Web API adapter.

Entries may carry a small `PluginManifest` with a role (`core`, `agent-spine`,
or `optional`), coarse risk (`none` or `external`), and a default enablement
strategy. A safe optional entry starts enabled; an external optional entry
starts disabled. Core and Agent-spine entries start enabled. Existing entries
without a manifest retain their original JSON and enabled-state behavior.

The inventory uses the existing `fiberPhase` field for runtime state and adds a
`disabled` phase when a manifest-backed entry is disabled before it is loaded.
This is descriptive metadata only; it is not an operating-system sandbox.

This crate does not load code, spawn processes, read credentials, or make
network calls. Process ownership remains with `yunxi-kernel` and
`yunxi-plugin-host`; a composition failure is data returned to the caller.

## Ownership

- `entry.rs` validates stable entry identities, module names, JSON config, and
  attaches optional manifest metadata.
- `layer.rs` defines serializable patch operations and layer bounds.
- `profile.rs` applies layers in dsh-compatible order and preserves entry order.
- `inventory.rs` projects composition state into the dsh Web plugin inventory
  shape, including optional lifecycle phases.
- `manifest.rs` defines the small role, risk, and default-enablement policy.

The current Web adapter is intentionally not part of this crate. It will map
the snapshot to dsh's `pluginInventory.list()` RPC while Rust implements the
remaining `/api/*` and event-stream contract.
