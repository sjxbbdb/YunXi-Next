# Source Map

| File | Responsibility |
| --- | --- |
| `entry.rs` | Validated entry IDs, module names, bounded JSON config, and optional manifest attachment |
| `error.rs` | Composition and validation errors |
| `inventory.rs` | dsh-compatible plugin inventory projection |
| `layer.rs` | Serializable layer operations and layer validation |
| `manifest.rs` | Plugin role, coarse risk, and default enablement policy |
| `profile.rs` | Ordered profile composition and immutable snapshots |
| `lib.rs` | Public facade and shared limits |
