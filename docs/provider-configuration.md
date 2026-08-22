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

These process-launch switches are the temporary management surface until the
persisted plugin manifest and Web settings page are implemented.

| Variable | Default | Effect |
| --- | --- | --- |
| `YUNXI_NEXT_CONTEXT_ENABLED` | `true` | Launch `context.compose@1` for `AGENTS.md` context |
| `YUNXI_NEXT_PERSONA_ENABLED` | `true` | Enable persona expression context |
| `YUNXI_NEXT_MEMORY_ENABLED` | `false` | Launch `memory.recall@1` and `memory.write@1` |
| `YUNXI_NEXT_STORAGE_ENABLED` | `true` | Launch persistent `storage.sessions@1` |
| `YUNXI_NEXT_COMPANION_ENABLED` | `false` | Launch deterministic `companion.decide@1` |
| `YUNXI_NEXT_MAILBOX_ENABLED` | companion value | Launch encrypted `companion.mailbox@1` |
| `YUNXI_NEXT_SCHEDULER_ENABLED` | companion value | Launch `scheduler.proactive@1` |
| `YUNXI_NEXT_COMPANION_TOOL_REQUESTS` | `false` | Permit scheduler plans that ask the user to approve a tool; never executes it |

The Next persona, memory, and companion variables take precedence over legacy
`YUNXI_PERSONA_ENABLED`, `YUNXI_MEMORY_ENABLED`, and
`YUNXI_COMPANION_ENABLED` variables.
When memory is enabled while persona expression is disabled, the persona
process runs only as the safety wrapper for memory context. A disabled
capability is not launched and has no catalog route.

Each optional built-in executable also has a development-only path override:
`YUNXI_NEXT_CONTEXT_PLUGIN`, `YUNXI_NEXT_PERSONA_PLUGIN`,
`YUNXI_NEXT_MEMORY_PLUGIN`, `YUNXI_NEXT_STORAGE_PLUGIN`,
`YUNXI_NEXT_COMPANION_PLUGIN`, `YUNXI_NEXT_MAILBOX_PLUGIN`, and
`YUNXI_NEXT_SCHEDULER_PLUGIN`.

`YUNXI_NEXT_HOME` selects the global YunXi Next state root for memory. The
mailbox accepts `YUNXI_NEXT_MAILBOX_KEY_HEX` (64 hex characters) when an
externally managed encryption key is required; otherwise it creates a key
inside the granted workspace mailbox directory. The legacy
`YUNXI_MAILBOX_KEY_HEX` name is also accepted for transition use.
