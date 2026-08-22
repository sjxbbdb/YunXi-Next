# CLI integration tests

Tests in this directory launch the compiled `yunxi-next` executable, which then
launches isolated context, memory, persona, and model children according to
their switches. A loopback mock HTTP server verifies context composition,
legacy memory recall, disabled-plugin routing, and API error recovery without
using a real API key or network service.
