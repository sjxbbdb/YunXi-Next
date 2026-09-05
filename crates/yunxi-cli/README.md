# `yunxi-cli`

This crate is YunXi Next's first user-facing surface. It owns the terminal
conversation loop, bounded chat history, and host-side multi-plugin
orchestration. It composes optional context, memory, persona, and companion
results before routing the final request to `model.chat@1`, then coordinates
session, memory-write, scheduler, and mailbox capabilities.

It intentionally does not contain provider HTTP code. The OpenAI-compatible
client and every inherited capability run in separately supervised child
processes.

## Use

```powershell
cargo run -p yunxi-cli --bin yunxi-next
cargo run -p yunxi-cli --bin yunxi-next -- --once "hello"
cargo run -p yunxi-cli --bin yunxi-next -- web
```

`yunxi-next web` starts the bounded HTTP/SSE carrier on `127.0.0.1:8787`.
Use `web --bind 127.0.0.1:0` to request an available local port. Closing its
standard input shuts down the shared Host explicitly and lets all plugin
children exit through their normal shutdown path. The command serves the
embedded pinned dsh workbench with text chat, history, model projection, and
Host approvals. Settings > Plugins exposes the thirteen optional capability
switches. Writes are revision-fenced, persisted under `YUNXI_NEXT_HOME`, and
applied only when the Host restarts; the required Model plugin cannot be
disabled.

Interactive commands include `/sessions`, `/resume <id>`, `/new`,
`/memory approve|reject <id>`, `/mailbox [read <id>]`, `/status`, `/clear`, and
`/approve`, `/deny`, `/cancel`, and `/quit`. When Shell or Patch is enabled, a
model action call pauses at the same approval boundary as the manual commands;
the tool result is sent back to the model only after approval, denial, or
cancellation. When Files is enabled, read-only search and view calls run with a
workspace-read grant and do not request approval. Tool failures are returned to
the model and surfaced as a deduplicated CLI warning. When Skills is enabled,
bounded workspace-local instructions are injected and metadata-only tools are
projected without granting execution. When Multi-agent is enabled, approved
spawn/message calls launch a separate Model plugin process for each bounded
child turn; list and stored-state interruption remain coordinator operations.
Use `--plugin <PATH>` to
launch a separate compatible model-plugin executable.
Provider setup is documented in
[`../../docs/provider-configuration.md`](../../docs/provider-configuration.md).
That document also lists the persistent capability settings and the
higher-precedence environment overrides.

## Files

- `src/` contains argument parsing, the chat session host, terminal rendering,
  and the executable entry point.
- `tests/` contains process-level tests of the complete CLI-to-plugin-to-API
  path.
