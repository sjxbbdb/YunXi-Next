# Source Map

| File | Responsibility |
| --- | --- |
| `dispatch.rs` | dsh unary method registry and `ClientRequest` dispatcher |
| `assets.rs` | exact embedded asset lookup, MIME metadata, and cache policy |
| `error.rs` | Gateway, contract, and bounded event-buffer errors |
| `events.rs` | `events.mux` and `events.host` in-memory queues |
| `http.rs` | bounded HTTP/1.1 parsing, `/api` routes, and TCP serving helpers |
| `projection.rs` | health, plugin inventory, and session-list projections |
| `sse.rs` | bounded SSE framing for mux and host events |
| `lib.rs` | public Gateway facade and exports |
