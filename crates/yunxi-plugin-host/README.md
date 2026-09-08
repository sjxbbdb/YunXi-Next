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

`disable` and the legacy `stop` method remove routes, stop the managed process,
and revoke pending host-secret references. A failed generation is also removed
from routing and its secret access is revoked before automatic recovery starts.
Manual enable/restart resets the retry budget and grants access only after a
new generation passes its handshake. Each process connection allows one
invocation at a time (`MAX_CONCURRENT_INVOCATIONS_PER_PLUGIN == 1`); stream
progress is bounded by the protocol event limit, and invocation wall-clock,
read, write, and cumulative invocation output work is bounded by the resource
policy. These are host-side
cooperative limits, not CPU accounting or an OS sandbox.

Host grouping is represented as optional protocol/runtime metadata. A group is
a placement hint; the current kernel still supervises each plugin failure
domain independently. This crate does not claim to provide an OS sandbox.

The crate does not execute capability code, decide approvals, parse provider
responses, or render user interfaces.

## Package discovery

`PluginDirectory` is the explicit, opt-in package boundary. It scans only
immediate child directories and reads `plugin.json`; it never searches PATH,
loads a dylib, or executes a package during discovery. A package is accepted
only after all of these checks pass:

- the manifest is valid JSON, uses schema `1`, and is at most 64 KiB;
- the plugin id, semantic version, capabilities, grants, dependency list, and
  executable declaration are valid and bounded;
- the executable is a regular, non-symlink file below the package directory;
- the executable protocol version matches the host protocol version; and
- an unsafe (`external` or `high`) plugin is not enabled by default.

The accepted manifest shape is:

```json
{
  "schema_version": 1,
  "plugin_id": "yunxi.example",
  "display_name": "Example",
  "plugin_version": "1.0.0",
  "executable": {
    "path": "bin/example.exe",
    "version": "1.0.0",
    "protocol_version": 2,
    "args": []
  },
  "capabilities": [{ "id": "example.run", "version": 1 }],
  "dependencies": [{ "id": "yunxi.base", "version": "1.0.0" }],
  "grants": [],
  "runtime": { "host_group": "default", "risk": "safe" },
  "default_enabled": true
}
```

`PluginDirectory::discover()` returns a `DiscoveryReport`. Filesystem/root
errors are returned as `Result` errors; an invalid package is instead a
bounded `DiscoveryFailure`, so a bad package cannot hide or start a healthy
sibling. `DiscoveryReport::load_order()` performs deterministic topological
ordering and reports missing, mismatched, or cyclic exact dependencies.

`PluginDiscoveryManager` layers a stable directory rescan and per-package
reconciliation over the process host. It applies dependency order and user
enablement, and reconciles package changes at a bounded refresh boundary. A
replacement stops and unregisters the old Host slot before the new package is
launched; if cleanup fails, the old metadata is retained and the replacement
is blocked until a later refresh can retry. Its `unload_all` operation is used
when a configured directory disappears or is replaced, so dropping the manager
cannot silently leak registrations. A missing root is reported as a bounded
discovery failure with an empty package set; malformed siblings do not abort
healthy packages. This remains package-based Rust executable loading, not an
unsafe in-place `cdylib`/WASM loader or an OS resource sandbox. Discovery also
records a filesystem snapshot of each executable and the host checks it again
before a discovered launch, so ordinary same-manifest file replacement is not
silently retained. This is an integrity-change detector, not a cryptographic
signature or an OS resource sandbox.

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Capability catalog, package discovery/manager, recovery slot, process host, and public facade |
| `Cargo.toml` | Serde/JSON, kernel, and protocol boundary dependencies |
