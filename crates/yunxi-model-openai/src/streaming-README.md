# Streaming Module

`streaming.rs` owns the bounded SSE parser for OpenAI-compatible chat
providers. It emits text and tool-call deltas synchronously so the caller can
apply backpressure, and it accepts a cancellation predicate without coupling
the provider crate to the Agent spine. API credentials and raw response bytes
are never included in stream events or diagnostic errors.
