# Chat Runtime

## Process Topology

```text
terminal user
    |
    v
yunxi-next CLI process
  - rolling conversation history
  - YunxiKernel lifecycle coordinator
  - capability provider catalog
  - host side of yunxi-protocol
    |
    | loopback TCP, newline-delimited JSON
    v
isolated model plugin process
  - provider configuration
  - OpenAI-compatible HTTP client
    |
    | HTTPS
    v
model API
```

The default distribution launches the same `yunxi-next` executable in a private
child mode. This avoids a second installation artifact without weakening
process isolation: provider HTTP code executes only after the new process starts.
An external compatible executable can instead be selected with `--plugin`.
Windows installs this binary as the independent `yunxi-next` command, leaving
the existing legacy `yunxi` command untouched.

## Readiness

1. The host binds an ephemeral IPv4 loopback port and launches the child.
2. The child sends its protocol version, stable plugin id, display metadata,
   versioned capabilities, and a launch-correlation token.
3. The host validates the id, token, version, declarations, and required
   `model.chat@1` capability.
4. `Welcome` and `Ready` complete the handshake before the CLI accepts input.

The correlation token prevents accidental attachment to the wrong launched
process. It is not a security or sandbox boundary. Frames are limited to 16
MiB, and model HTTP responses are limited to 32 MiB.

## Chat Requests

The CLI sends a protocol-v2 `Invoke` request for `model.chat@1:complete`. The
generic envelope carries a typed chat payload; it is also the call path future
memory, persona, tools, voice, and channel plugins use. The first implementation
uses complete responses rather than token streaming. On success, the user and
assistant messages enter a rolling 32-turn in-memory history. `/clear` discards
it, and process exit discards it permanently.

API credentials are read from inherited environment variables. The CLI
preflights configuration for useful startup errors, but credentials are never
serialized into local protocol frames or diagnostic output.

## Failure Semantics

- An HTTP/API failure becomes `InvocationFailed`; the model process stays alive
  and can serve the next request.
- A malformed or mismatched protocol response invalidates the session and the
  kernel stops that plugin process.
- A model process crash becomes observable plugin failure state. Kernel health
  and sibling plugin processes remain unchanged.
- Automatic restart is intentionally absent. A future policy must be bounded,
  observable, and owned above the kernel primitive.

Process isolation does not restrict filesystem, network, CPU, or memory access.
A separate sandbox design is required before running untrusted plugins.
