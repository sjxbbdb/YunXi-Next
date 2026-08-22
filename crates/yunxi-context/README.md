# `yunxi-context`

`yunxi-context` is the isolated provider for `context.compose@1`. Given an
explicit working directory, it reads `AGENTS.md` files from the filesystem root
to that directory and returns their ordered text and source paths.

The plugin has no model, network, memory, persona, or tool responsibilities.
One file is limited to 1 MiB and the combined context is limited to 4 MiB.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Context loader, protocol loop, and crate facade |
| `Cargo.toml` | Protocol-only dependency boundary |
