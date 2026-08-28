# Source Map

- `config.rs` validates the single MCP Server launch configuration and keeps
  its environment explicit.
- `client.rs` implements newline-delimited JSON-RPC with bounded frames,
  matching request IDs, response timeouts, and child cleanup.
- `http.rs` implements bounded HTTP/HTTPS JSON and SSE requests, session
  reuse, scoped header secrets, timeout cancellation, and response redaction.
- `plugin.rs` translates the client into the isolated YunXi plugin protocol.
- `lib.rs` exposes the stable bridge facade.
- `bin/` contains the plugin and test-server executables.
