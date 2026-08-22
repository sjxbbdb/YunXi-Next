# `yunxi-cli`

This crate is YunXi Next's first user-facing surface. It owns the terminal
conversation loop, in-memory chat history, and the host side of the model
plugin connection. It registers the model declaration with
`yunxi-plugin-host` before routing `model.chat` calls.

It intentionally does not contain provider HTTP code. The OpenAI-compatible
client runs in a separately supervised process supplied by
`yunxi-model-openai`.

## Use

```powershell
cargo run -p yunxi-cli --bin yunxi-next
cargo run -p yunxi-cli --bin yunxi-next -- --once "hello"
```

Interactive commands are `/help`, `/status`, `/clear`, and `/quit`. Use
`--plugin <PATH>` to launch a separate compatible model-plugin executable.
Provider setup is documented in
[`../../docs/provider-configuration.md`](../../docs/provider-configuration.md).

## Files

- `src/` contains argument parsing, the chat session host, terminal rendering,
  and the executable entry point.
- `tests/` contains process-level tests of the complete CLI-to-plugin-to-API
  path.
