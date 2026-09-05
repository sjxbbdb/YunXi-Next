# Plugin Host Integration Tests

`process_runtime.rs` launches real test subprocesses through the kernel and
verifies manifest grant acceptance, rejection before route registration,
malformed-frame, crash, timeout isolation, generation-aware automatic
restarts, retry exhaustion, manual enable/restart, and sibling survival.

The crash fixtures use the same loopback handshake and invocation protocol as a
real plugin. Recovery is driven through `ProcessPluginHost::refresh()`; this
keeps transport failure handling bounded and observable in the synchronous API.
