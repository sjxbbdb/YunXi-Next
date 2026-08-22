# `yunxi-plugin-host`

`yunxi-plugin-host` maps validated plugin declarations to capability providers.
It detects duplicate plugin registration and ambiguous capability routing while
keeping process supervision in `yunxi-kernel` and wire types in
`yunxi-protocol`.

The crate does not execute capability code, decide approvals, parse provider
responses, or render user interfaces.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | In-memory capability catalog and public facade |
| `Cargo.toml` | Kernel and protocol boundary dependencies |
