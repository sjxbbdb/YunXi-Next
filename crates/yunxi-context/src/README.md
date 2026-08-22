# Context Plugin Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable plugin facade and error surface |
| `compose.rs` | Bounded root-to-cwd `AGENTS.md` loading |
| `plugin.rs` | `context.compose@1` protocol dispatch loop |
| [`bin/`](bin/README.md) | Standalone plugin executable |
