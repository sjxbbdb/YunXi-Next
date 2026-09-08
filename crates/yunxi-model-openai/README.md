# yunxi-model-openai

`yunxi-model-openai` is the first process-isolated YunXi capability plugin. It
calls an OpenAI-compatible Chat Completions endpoint and returns chat results
through the generic `model.chat@1:complete` invocation in `yunxi-protocol`.

The host never calls its HTTP client directly. The library entry point exists
so a distribution can package the plugin inside a child-process mode while
preserving process isolation.

The client supports bounded OpenAI-compatible SSE streaming for text and tool
call deltas. The current legacy plugin invocation remains completion-shaped so
older Hosts stay compatible; the stream API is used by the Agent event path,
including the CLI/TUI/Web adapters and `run --jsonl`. The Web carrier still
uses bounded finite SSE polls with cursors rather than a remotely persistent
connection. An API or observer error is reported as a failed request and does
not end the plugin loop.
Configuration is documented in
[`../../docs/provider-configuration.md`](../../docs/provider-configuration.md).

| Path | Responsibility |
| --- | --- |
| [`src/`](src/README.md) | Configuration, HTTP client, and plugin loop |
| [`src/bin/`](src/bin/README.md) | Standalone plugin process entry point |
| `Cargo.toml` | HTTP and protocol dependency boundary |
