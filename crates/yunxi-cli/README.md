# `yunxi-cli`

This crate is YunXi Next's first user-facing surface. It owns the terminal
conversation loop, in-memory chat history, and host-side multi-plugin
orchestration. It composes optional context, memory, and persona results before
routing the final request to `model.chat@1`.

It intentionally does not contain provider HTTP code. The OpenAI-compatible
client and every inherited capability run in separately supervised child
processes.

## Use

```powershell
cargo run -p yunxi-cli --bin yunxi-next
cargo run -p yunxi-cli --bin yunxi-next -- --once "hello"
```

Interactive commands are `/help`, `/status`, `/clear`, and `/quit`. Use
`--plugin <PATH>` to launch a separate compatible model-plugin executable.
Provider setup is documented in
[`../../docs/provider-configuration.md`](../../docs/provider-configuration.md).
That document also lists the temporary environment-based capability switches.

## Files

- `src/` contains argument parsing, the chat session host, terminal rendering,
  and the executable entry point.
- `tests/` contains process-level tests of the complete CLI-to-plugin-to-API
  path.
