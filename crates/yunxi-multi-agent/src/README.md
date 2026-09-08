# Multi-agent Sources

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable crate facade |
| `store.rs` | Bounded graph, budget, transcript, event, and atomic persistence logic |
| `plugin.rs` | Manifest handshake and typed operation dispatch |
| `runtime.rs` | Async worker execution, cooperative cancellation, resumable turns, and bounded event projection |
| `bin/yunxi-multi-agent.rs` | Standalone plugin entry point |
