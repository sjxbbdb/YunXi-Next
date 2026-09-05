# `yunxi-plugin-host`

`yunxi-plugin-host` launches isolated plugin processes and maps their validated
declarations to capability providers. It detects duplicate plugin registration,
routes exact capability versions, and removes a failed connection without
changing sibling process state. Process primitives remain in `yunxi-kernel` and
wire types remain in `yunxi-protocol`.

Each registered plugin has a private slot containing its original
`PluginLaunch`, a loopback acceptor, and a generation-aware `RetryController`.
When `refresh()` observes a process or protocol failure, the slot removes its
routes and asks the kernel to start the next generation. The replacement must
complete the normal handshake and catalog registration before it is routable.
Automatic recovery is capped at three restarts per enable cycle. A repeated or
stale generation failure is ignored, and exhaustion disables the slot until an
explicit `enable`, `restart`, or `manual_restart` call. Transport failures are
marked synchronously; their recovery is deliberately performed by the next
bounded `refresh()` so an invocation cannot enter a hidden restart loop.

`disable` and the legacy `stop` method remove routes and stop the managed
process. Manual enable/restart resets the retry budget. Sibling slots have
independent supervisors and retry state.

Host grouping is represented as optional protocol/runtime metadata. A group is
a placement hint; the current kernel still supervises each plugin failure
domain independently. This crate does not claim to provide an OS sandbox.

The crate does not execute capability code, decide approvals, parse provider
responses, or render user interfaces.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Capability catalog, recovery slot, process host, and public facade |
| `Cargo.toml` | Serde, kernel, and protocol boundary dependencies |
