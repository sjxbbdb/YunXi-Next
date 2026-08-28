# Provider Configuration

The first model plugin targets OpenAI-compatible Chat Completions endpoints.
Configuration is inherited from the parent process; YunXi Next does not load a
`.env` file or persist credentials.

## Variables

| Variable | Purpose |
| --- | --- |
| `YUNXI_PROVIDER_PROFILE` | Provider label; use `deepseek` for DeepSeek defaults |
| `YUNXI_PROVIDER_BASE_URL` | API base URL, without `/chat/completions` |
| `YUNXI_PROVIDER_API_KEY` | Direct credential override |
| `YUNXI_PROVIDER_API_KEY_ENV` | Name of another environment variable holding the credential |
| `YUNXI_AGENT_MODEL` | Model identifier sent to the endpoint |
| `YUNXI_PROVIDER_TIMEOUT_MILLIS` | Positive per-request timeout; default `120000` |
| `DEEPSEEK_API_KEY` | DeepSeek credential and automatic profile signal |
| `OPENAI_API_KEY` | Default OpenAI-compatible credential |
| `OPENAI_BASE_URL` | Compatible fallback when the YunXi base URL is absent |

## Resolution Order

1. `YUNXI_PROVIDER_PROFILE` wins. Without it, the presence of
   `DEEPSEEK_API_KEY` selects `deepseek`; otherwise the profile is
   `openai-compatible`.
2. `YUNXI_AGENT_MODEL` wins. Defaults are `deepseek-v4-flash` for DeepSeek and
   `gpt-4.1` otherwise.
3. `YUNXI_PROVIDER_BASE_URL`, then `OPENAI_BASE_URL`, then the profile default.
4. `YUNXI_PROVIDER_API_KEY`, then the variable named by
   `YUNXI_PROVIDER_API_KEY_ENV`, then `DEEPSEEK_API_KEY` for DeepSeek or
   `OPENAI_API_KEY` otherwise.

Trailing slashes are removed from the base URL, and the plugin appends
`/chat/completions`.

## Examples

DeepSeek using inherited defaults:

```powershell
$env:DEEPSEEK_API_KEY = "your-key"
yunxi-next
```

Custom compatible endpoint:

```powershell
$env:YUNXI_PROVIDER_PROFILE = "local"
$env:YUNXI_PROVIDER_BASE_URL = "http://127.0.0.1:8000/v1"
$env:YUNXI_PROVIDER_API_KEY = "local-key"
$env:YUNXI_AGENT_MODEL = "local-model"
yunxi-next
```

Use the shell or a secret manager to inject real credentials. Do not commit
them to this repository.

## Capability Switches

The twelve optional capabilities can be changed in Web Settings > Plugins.
Those choices are written to `YUNXI_NEXT_HOME\settings.json` (or the normal
`~/.yunxi-next/settings.json` state root), and take effect on the next Host
start. The following environment variables have higher precedence than the
stored document, so they remain the operator, CI, and recovery override.

| Variable | Default | Effect |
| --- | --- | --- |
| `YUNXI_NEXT_CONTEXT_ENABLED` | `true` | Launch `context.compose@1` for `AGENTS.md` context |
| `YUNXI_NEXT_PERSONA_ENABLED` | `true` | Enable persona expression context |
| `YUNXI_NEXT_MEMORY_ENABLED` | `false` | Launch `memory.recall@1` and `memory.write@1` |
| `YUNXI_NEXT_STORAGE_ENABLED` | `true` | Launch persistent `storage.sessions@1` |
| `YUNXI_NEXT_COMPANION_ENABLED` | `false` | Launch deterministic `companion.decide@1` |
| `YUNXI_NEXT_MAILBOX_ENABLED` | companion value | Launch encrypted `companion.mailbox@1` |
| `YUNXI_NEXT_SCHEDULER_ENABLED` | companion value | Launch `scheduler.proactive@1` |
| `YUNXI_NEXT_SHELL_ENABLED` | `false` | Launch Host-approved `tool.shell@1` |
| `YUNXI_NEXT_PATCH_ENABLED` | `false` | Launch Host-approved `tool.patch@1` |
| `YUNXI_NEXT_FILES_ENABLED` | `false` | Launch read-only `tool.files@1` |
| `YUNXI_NEXT_MCP_ENABLED` | `false` | Launch the configured Host-approved `tool.mcp@1` bridge |
| `YUNXI_NEXT_SKILLS_ENABLED` | `false` | Launch read-only `tool.skills@1` discovery and context |
| `YUNXI_NEXT_COMPANION_TOOL_REQUESTS` | `false` | Permit scheduler plans that ask the user to approve a tool; never executes it |

The Next persona, memory, and companion variables take precedence over legacy
`YUNXI_PERSONA_ENABLED`, `YUNXI_MEMORY_ENABLED`, and
`YUNXI_COMPANION_ENABLED` variables.
When memory is enabled while persona expression is disabled, the persona
process runs only as the safety wrapper for memory context. A disabled
capability is not launched and has no catalog route.

The settings document accepts only the twelve known boolean fields, is capped
at 64 KiB, and uses revision-fenced same-directory replacement. It does not
store provider credentials, executable paths, MCP secrets, or arbitrary plugin
configuration. The Model capability is required and is not part of this
writable namespace.

Each optional built-in executable also has a development-only path override:
`YUNXI_NEXT_CONTEXT_PLUGIN`, `YUNXI_NEXT_PERSONA_PLUGIN`,
`YUNXI_NEXT_MEMORY_PLUGIN`, `YUNXI_NEXT_STORAGE_PLUGIN`,
`YUNXI_NEXT_COMPANION_PLUGIN`, `YUNXI_NEXT_MAILBOX_PLUGIN`,
`YUNXI_NEXT_SCHEDULER_PLUGIN`, `YUNXI_NEXT_SHELL_PLUGIN`,
`YUNXI_NEXT_PATCH_PLUGIN`, `YUNXI_NEXT_FILES_PLUGIN`,
`YUNXI_NEXT_MCP_PLUGIN`, and `YUNXI_NEXT_SKILLS_PLUGIN`.

When MCP is enabled, stdio remains the default. Configure one direct command;
the command is not parsed by a shell and its child environment is cleared
except for `PATH` and the explicitly listed JSON object:

| Variable | Example | Effect |
| --- | --- | --- |
| `YUNXI_NEXT_MCP_COMMAND` | `npx` | MCP executable or absolute path |
| `YUNXI_NEXT_MCP_ARGS_JSON` | `["-y","server-package"]` | Direct argument array |
| `YUNXI_NEXT_MCP_NAME` | `filesystem` | Lowercase server namespace; defaults to `default` |
| `YUNXI_NEXT_MCP_ENV_JSON` | `{ "TOKEN": "..." }` | Explicit child environment allowlist |

HTTP is opt-in and uses the same MCP plugin process. It requires an endpoint
and a Host-issued network scope. The scope matches the endpoint's scheme,
hostname, and port exactly; the URL path is not a separate authority boundary.

| Variable | Example | Effect |
| --- | --- | --- |
| `YUNXI_NEXT_MCP_TRANSPORT` | `http` | Select `http`, `https`, or `streamable-http`; omitted means `stdio` |
| `YUNXI_NEXT_MCP_URL` | `https://mcp.example.test/mcp` | MCP HTTP endpoint; query, fragment, and userinfo are rejected |
| `YUNXI_NEXT_MCP_HEADERS_JSON` | `{ "Authorization": "secret://mcp/token" }` | Header map; sensitive headers must use a Secret reference |
| `YUNXI_NEXT_MCP_SECRETS_JSON` | `{ "mcp/token": "value-from-secret-store" }` | Local bridge values keyed by reference; never sent in YunXi protocol frames |
| `YUNXI_NEXT_MCP_NETWORK_GRANT_JSON` | `["https://mcp.example.test/mcp"]` | Exact scheme/host/port scopes issued to the HTTP bridge |
| `YUNXI_NEXT_MCP_SECRET_GRANT_JSON` | `["mcp/token"]` | Secret references permitted for discovery and calls; values are not listed here |

For example:

```powershell
$env:YUNXI_NEXT_MCP_TRANSPORT = "http"
$env:YUNXI_NEXT_MCP_URL = "https://mcp.example.test/mcp"
$env:YUNXI_NEXT_MCP_HEADERS_JSON = '{"Authorization":"secret://mcp/token"}'
$env:YUNXI_NEXT_MCP_SECRETS_JSON = '{"mcp/token":"value-from-secret-store"}'
$env:YUNXI_NEXT_MCP_NETWORK_GRANT_JSON = '["https://mcp.example.test/mcp"]'
$env:YUNXI_NEXT_MCP_SECRET_GRANT_JSON = '["mcp/token"]'
$env:YUNXI_NEXT_MCP_ENABLED = "true"
yunxi-next
```

The HTTP bridge does not follow redirects and rejects attempts to override
`Host`, `Content-Length`, `Transfer-Encoding`, or other hop-by-hop headers.
`Authorization`, `Cookie`, `Proxy-Authorization`, `Set-Cookie`, and `X-API-Key`
cannot contain literal credential values. Secret values should come from the
operator's secret injection path; the example placeholder above is not a
repository credential.

When Skills is enabled, the Host accepts only a root inside the current
workspace. The Skills child starts with a cleared environment and receives the
root, disabled ids, and minimal process-launch variables; Provider credentials
are not forwarded.

| Variable | Example | Effect |
| --- | --- | --- |
| `YUNXI_NEXT_SKILLS_ROOT` | `.\skills` | Skill root; defaults to `<workspace>\skills` |
| `YUNXI_NEXT_SKILLS_DISABLED` | `review,release` | Comma-separated lowercase Skill ids to omit |

Each immediate child directory may contain `SKILL.md` and an optional
`tools.json`. Tool entries are validated declarations only. They are exposed to
the model as `skill.<id>.<tool>`, but execution is disabled and does not request
approval in this phase.

`YUNXI_NEXT_HOME` selects the global YunXi Next state root for settings and
memory. The
mailbox accepts `YUNXI_NEXT_MAILBOX_KEY_HEX` (64 hex characters) when an
externally managed encryption key is required; otherwise it creates a key
inside the granted workspace mailbox directory. The legacy
`YUNXI_MAILBOX_KEY_HEX` name is also accepted for transition use.
