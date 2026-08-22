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
  - ordered capability orchestration
    |
    | separate loopback JSONL connection per process
    |
    +--> yunxi-context process --> bounded AGENTS.md reads
    +--> yunxi-memory process  --> read-only legacy JSONL memory
    +--> yunxi-persona process --> profile/soul + safe context compiler
    `--> model plugin process  --> HTTPS --> model API
```

The default distribution launches the same `yunxi-next` executable in a private
child mode for each built-in capability. This avoids extra installation
artifacts without weakening process isolation: capability code executes only
after its child process starts. Every capability crate also builds a standalone
plugin binary for later packaging. An external compatible model executable can
be selected with `--plugin`; optional plugin paths have development environment
overrides. Windows installs `yunxi-next` independently and leaves the existing
legacy `yunxi` command untouched.

## Readiness

1. The host binds an ephemeral IPv4 loopback port and launches the child.
2. The child sends its protocol version, stable plugin id, display metadata,
   versioned capabilities, and a launch-correlation token.
3. The host validates the id, token, version, declarations, and the exact
   capability expected from that process.
4. `Welcome` and `Ready` complete the handshake before the CLI accepts input.

The model plugin is required. Context and persona are optional and enabled by
default; read-only memory is optional and disabled by default. A disabled
capability is never launched or registered.

The correlation token prevents accidental attachment to the wrong launched
process. It is not a security or sandbox boundary. Frames are limited to 16
MiB, and model HTTP responses are limited to 32 MiB.

## Chat Requests

For each turn the CLI uses protocol-v2 `Invoke` frames in this order:

1. `context.compose@1:compose` loads ordered project instructions.
2. `memory.recall@1:recall`, when enabled, returns bounded boot and dynamic
   records from legacy storage.
3. `persona.context@1:compile` safely combines persona and routed memory into a
   system context.
4. `model.chat@1:complete` receives those system messages followed by rolling
   conversation history and the current user message.

The first implementation uses complete responses rather than token streaming.
On success, user and assistant messages enter a rolling 32-turn in-memory
history. `/clear` discards it, and process exit discards it permanently.

API credentials are read from inherited environment variables. The CLI
preflights configuration for useful startup errors, but credentials are never
serialized into local protocol frames or diagnostic output.

## Failure Semantics

- An HTTP/API failure becomes `InvocationFailed`; the model process stays alive
  and can serve the next request.
- A malformed frame, mismatched response, timeout, or process exit removes only
  that plugin's routes and records a failed lifecycle state.
- Optional context, memory, and persona failures produce one user-visible
  warning per distinct failure and the turn continues with remaining context.
- A model process crash makes model chat unavailable, while kernel health and
  sibling plugin processes remain unchanged.
- Automatic restart is intentionally absent. A future policy must be bounded,
  observable, and owned above the kernel primitive.

Process isolation does not restrict filesystem, network, CPU, or memory access.
A separate sandbox design is required before running untrusted plugins.
