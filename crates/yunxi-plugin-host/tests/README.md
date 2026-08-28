# Plugin Host Integration Tests

`process_runtime.rs` launches real test subprocesses through the kernel and
verifies manifest grant acceptance, rejection before route registration,
malformed-frame, crash, and timeout isolation without disrupting a sibling
provider.
