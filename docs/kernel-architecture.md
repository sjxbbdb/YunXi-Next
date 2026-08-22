# Kernel Architecture

## Trusted Boundary

The kernel process contains only plugin descriptions, lifecycle state, and
supervisor coordination. Plugin implementation code runs in child processes.
An unexpected plugin exit therefore becomes data reported to the kernel rather
than an unwind or abort inside the trusted process.

```text
caller
  |
  v
YunxiKernel (runtime)
  | command                         event + generation
  v                                       ^
process supervisor -----------------------|
  |
  v
isolated plugin child process
```

## State Ownership

- `plugin` defines the possible states and immutable snapshots.
- `runtime` owns the authoritative current state for each registered plugin.
- `supervision` observes one child process and emits generation-tagged changes.
- Events from an older generation are ignored after an explicit restart.

## Current Guarantees

- Plugin process failure does not change kernel health.
- One plugin failure does not stop sibling plugin processes.
- Spawn failures are contained as plugin failures.
- Shutdown requests termination and joins every active supervisor thread.
- Failed plugins remain failed until an explicit restart.

## Explicit Non-Goals

- Filesystem, network, CPU, and memory sandboxing.
- Plugin protocol negotiation or readiness handshakes.
- Automatic restart and backoff policy.
- Models, tools, memory, voice, channels, or Web UI integration.

These capabilities require separate contracts and must not expand the trusted
kernel without an explicit design decision.
