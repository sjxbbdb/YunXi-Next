# Plugin Host Integration Tests

`process_runtime.rs` launches real test subprocesses through the kernel and
verifies manifest grant acceptance, rejection before route registration,
malformed-frame, crash, timeout isolation, generation-aware automatic
restarts, retry exhaustion, manual enable/restart, and sibling survival.

`discovery.rs` exercises the public `PluginDirectory` facade. It verifies
deterministic dependency ordering, exact dependency-version rejection,
duplicate-id isolation, traversal/path rejection, sibling preservation, and
manifest-size bounding without starting any discovered executable.

`manager_runtime.rs` launches a discovered package through
`PluginDiscoveryManager` and verifies enable/disable reconciliation,
generation-safe replacement, route removal, and cleanup when the package
directory is unloaded or disappears.

The crash fixtures use the same loopback handshake and invocation protocol as a
real plugin. Recovery is driven through `ProcessPluginHost::refresh()`; this
keeps transport failure handling bounded and observable in the synchronous API.
