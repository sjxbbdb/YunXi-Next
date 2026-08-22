# yunxi-kernel

`yunxi-kernel` is the smallest trusted process in YunXi Next. It registers
plugin descriptions, starts isolated plugin processes, records lifecycle state,
and stops supervised children during shutdown.

It does not implement models, tools, memory, voice, Web UI, plugin protocols, or
security sandboxing.

## Layout

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Production library and smoke binary |
| [`tests/`](tests/README.md) | Black-box process isolation tests |
| `Cargo.toml` | Crate identity and inherited workspace policy |

The public API is exported only from `src/lib.rs`. Internal module paths are not
part of the compatibility contract.
