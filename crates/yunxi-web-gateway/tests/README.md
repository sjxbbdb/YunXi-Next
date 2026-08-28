# Integration Tests

`gateway.rs` exercises the public in-memory facade against the real Web
contract and composition inventory types. These tests intentionally avoid a
socket and plugin processes so dispatcher failures remain deterministic.

`http.rs` exercises the dependency-light HTTP/SSE carrier, including request
framing limits, dsh JSON envelopes, structured malformed-request responses,
isolated event channels, HTTP response bytes, and one loopback TCP connection.
