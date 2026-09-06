# Model Plugin Source Layout

| File | Responsibility |
| --- | --- |
| `lib.rs` | Stable model-plugin facade |
| `config.rs` | Legacy-compatible provider environment resolution |
| `client.rs` | Bounded blocking Chat Completions HTTP client and tool-call adapter |
| `streaming.rs` | Bounded SSE parser, delta events, cancellation, and backpressure boundary |
| `plugin.rs` | Protocol request loop and API error containment |
| `bin/yunxi-model-openai.rs` | Standalone plugin executable entry point |

Configuration contains credentials but never serializes or logs them. HTTP
types remain private to the client module and do not leak into the wire
protocol. Tool definitions and calls cross the boundary only through the
versioned `yunxi-protocol` types; provider credentials remain child-process
configuration. Provider error messages are bounded and redact the configured
credential before they leave the client.
