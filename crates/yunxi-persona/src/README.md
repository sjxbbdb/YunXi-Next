# Persona Plugin Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable plugin facade |
| `settings.rs` | Legacy settings path and environment compatibility |
| `profile.rs` | Built-in and custom persona profile loading |
| `compiler.rs` | Bounded, escaped persona and memory context rendering |
| `plugin.rs` | `persona.context@1` protocol dispatch loop |
| [`bin/`](bin/README.md) | Standalone plugin executable |
