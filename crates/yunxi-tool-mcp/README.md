# `yunxi-tool-mcp`

`yunxi-tool-mcp` is the isolated MCP bridge for YunXi Next. It owns one
configured MCP Server launched through stdio or an opt-in streamable HTTP
transport and exposes only the bounded `tool.mcp@1` plugin contract to the
YunXi host.

The bridge does not pass the parent process environment through to the MCP
Server. `YUNXI_NEXT_MCP_ENV_JSON` is an explicit object-shaped allowlist; the
host `PATH` is copied only to make command lookup and child launch practical.
The MCP command is executed directly, without a shell.

| Path | Responsibility |
| --- | --- |
| `src/config.rs` | Environment configuration and allowlist validation |
| `src/client.rs` | Bounded JSON-RPC client and MCP lifecycle |
| `src/http.rs` | Bounded HTTP/HTTPS JSON/SSE transport, session reuse, cancellation, and redaction |
| `src/plugin.rs` | YunXi handshake and typed `tool.mcp@1` dispatch |
| `src/lib.rs` | Public plugin facade |
| `src/bin/yunxi-tool-mcp.rs` | Standalone plugin entry point |
| `src/bin/yunxi-mcp-fixture.rs` | Deterministic test MCP Server |

Required configuration:

- `YUNXI_NEXT_MCP_COMMAND`: executable path or executable name; it is not
  parsed as a shell command line.
- `YUNXI_NEXT_MCP_ARGS_JSON`: optional JSON array of string arguments.
- `YUNXI_NEXT_MCP_NAME`: optional lowercase server name, defaulting to
  `default`.
- `YUNXI_NEXT_MCP_ENV_JSON`: optional JSON object of explicit child
  environment variables.

HTTP configuration is opt-in with `YUNXI_NEXT_MCP_TRANSPORT=http` (or
`https`/`streamable-http`) and uses `YUNXI_NEXT_MCP_URL`. The CLI requires
`YUNXI_NEXT_MCP_NETWORK_GRANT_JSON` to include the endpoint's exact
scheme/host/port. `YUNXI_NEXT_MCP_HEADERS_JSON` accepts ordinary headers, but
credential-bearing headers must use `secret://reference`; the corresponding
local values belong in `YUNXI_NEXT_MCP_SECRETS_JSON`, while
`YUNXI_NEXT_MCP_SECRET_GRANT_JSON` contains only allowed references.

The HTTP transport accepts bounded JSON or SSE responses, does not follow
redirects, reuses a valid `Mcp-Session-Id`, and attempts
`notifications/cancelled` after a timeout. That cancellation is best effort
and is not a remote execution guarantee. Network scopes are Host/plugin
authority checks, not an OS network sandbox. Secret values are redacted from
MCP errors and results before they return to the host.
