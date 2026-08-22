# `yunxi-plugin-host`

`yunxi-plugin-host` launches isolated plugin processes and maps their validated
declarations to capability providers. It detects duplicate plugin registration,
routes exact capability versions, and removes a failed connection without
changing sibling process state. Process primitives remain in `yunxi-kernel` and
wire types remain in `yunxi-protocol`.

The crate does not execute capability code, decide approvals, parse provider
responses, or render user interfaces.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Capability catalog, process host, and public facade |
| `Cargo.toml` | Serde, kernel, and protocol boundary dependencies |
