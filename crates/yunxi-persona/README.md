# `yunxi-persona`

`yunxi-persona` is the isolated provider for `persona.context@1`. It reads the
legacy persona settings, profile JSON, and optional `soul.txt`, then compiles a
bounded system-context block from that profile and host-routed memory records.

Persona output is expression guidance only. The compiler emits explicit
priority and memory-as-context notices and cannot grant tools, filesystem,
network, approval, or memory-write authority.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Settings/profile compatibility, safe compiler, and protocol loop |
| `Cargo.toml` | Serde, JSON, and protocol dependencies |
