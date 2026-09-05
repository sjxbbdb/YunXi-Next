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

The kernel has no serialization, HTTP, model, or terminal dependencies. The
CLI embeds the kernel as its lifecycle coordinator, while capability code runs
in child processes. `yunxi-plugin-host` owns the capability catalog and routing
checks above this kernel boundary.

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
- Failed plugins are retried by `yunxi-plugin-host` with bounded backoff, up to
  three automatic restarts per enable cycle; after exhaustion they remain
  disabled until an explicit enable or manual restart.
- `yunxi-plugin-host` can bind a launch to an exact expected capability set;
  an unexpected declaration is rejected before the route becomes available.
- Host-detected transport or malformed-frame failures remain `Failed` even
  after the supervisor finishes terminating that process.
- A protocol or API implementation cannot panic inside the kernel process.

## Explicit Non-Goals

- Filesystem, network, CPU, and memory sandboxing.
- Plugin wire formats and readiness handshakes; these belong to
  `yunxi-protocol` outside the kernel.
- The Cordis Context/Service/Event/Effect runtime; those generic primitives are
  implemented in `yunxi-cordis-core` and composed above the process kernel.
- Models, tools, memory, voice, channels, terminal rendering, or Web UI logic.

These capabilities require separate contracts and must not expand the trusted
kernel without an explicit design decision. Restart recovery is deliberately in
the Plugin Host, not in the minimal kernel state machine. Recovery is
synchronous and `refresh()`-driven in the current implementation.
