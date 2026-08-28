# Integration Tests

`stdio.rs` launches the deterministic MCP fixture as a real child process and
verifies initialize, tool discovery, normal calls, malformed JSON, and child
crash boundaries without sharing the YunXi host process. `http.rs` verifies
opt-in HTTP JSON/SSE discovery and calls, exact network denial, per-call Secret
grant header injection, session id reuse, bounded malformed/oversized
responses, remote-error redaction, and timeout cancellation notifications.
