# CLI integration tests

Tests in this directory launch the compiled `yunxi` executable, which then
launches its isolated model-plugin child. A loopback mock HTTP server verifies
the complete request path without using a real API key or network service.
