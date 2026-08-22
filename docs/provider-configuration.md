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
yunxi next
```

Custom compatible endpoint:

```powershell
$env:YUNXI_PROVIDER_PROFILE = "local"
$env:YUNXI_PROVIDER_BASE_URL = "http://127.0.0.1:8000/v1"
$env:YUNXI_PROVIDER_API_KEY = "local-key"
$env:YUNXI_AGENT_MODEL = "local-model"
yunxi next
```

Use the shell or a secret manager to inject real credentials. Do not commit
them to this repository.
