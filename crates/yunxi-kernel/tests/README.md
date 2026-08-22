# Kernel Integration Tests

Tests in this directory exercise behavior across the public crate boundary.

| File | Responsibility |
| --- | --- |
| `process_isolation.rs` | Runs real child processes to prove crash and spawn-failure containment |

Pure value validation stays beside its source module as a unit test. A test
belongs here when it must launch a process or observe the kernel as an external
consumer would.
