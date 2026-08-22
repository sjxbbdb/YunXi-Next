# yunxi-web-contract

Bounded, dependency-light Rust types for the DeepSeek Harness (dsh) browser
wire contract. The crate covers the four RPC message quadrants, dsh's
`RpcResult` shape, and the `events.mux` / `events.host` stream envelopes.

It is deliberately a contract crate, not an HTTP or WebSocket server. A future
Gateway can choose `axum`, `hyper`, or another transport without changing the
JSON model or moving network code into the kernel. The crate never stores API
credentials and validates message and payload limits before a decoded value is
handed to a service.

## Source Ownership

- `bounds.rs` owns wire-size and text validation limits.
- `error.rs` owns validation and encoding errors.
- `rpc.rs` owns the four dsh RPC message forms and `RpcResult`.
- `events.rs` owns the two named event-stream channels.
- `lib.rs` exports the stable contract facade.
