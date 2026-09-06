# YunXi Next Cordis Rust Architecture

## Status

This document is the approved architecture baseline for the Cordis-oriented
reconstruction of YunXi Next. It applies only to `D:\YunXi Next`. The current
implementation is an integrated first slice: the Cordis crates provide the
trusted bootstrap, and the default CLI/Web turn path runs through the Rust
Agent spine. `ChatSession` remains the application facade for lifecycle,
storage, post-response hooks, and Web projection.

`D:\YunXi Agent` remains a read-only behavioral reference and a working
fallback. No migration step may write to that repository, alter its executable,
change the global PATH, edit the registry, or install a system service.

## Design Rule

Use the current dsh/Cordis semantics as the default design wherever they solve
the problem already. YunXi-specific changes are limited to:

1. A pure Rust implementation of the runtime and Agent backend.
2. Process-level failure isolation for external capabilities.
3. A simple user-facing plugin on/off model.
4. Explicit protection of the legacy YunXi installation.

This is semantic compatibility, not a TypeScript API clone.

## Runtime Layers

```text
yunxi-cordis-core
  Context / Service / inject / Fiber / Event / Effect

yunxi-agent-spine
  Session / prompt context / Tool Broker / Agent / Agent Loop

yunxi-plugin-host + yunxi-kernel
  Host groups / versioned IPC / lifecycle / restart / resource bounds

capability plugins
  Model / Memory / Persona / Shell / Files / Voice / Weixin / Web / ...

CLI and Web adapters
  Presentation and transport only; no second Agent runtime
```

### `yunxi-cordis-core`

The trusted meta-kernel owns only generic composition primitives:

- scoped `Context` and named `Service` registration;
- dependency declarations and readiness resolution;
- plugin mounting, Fiber state, disposal, and child ownership;
- reversible effects for registrations and resources;
- typed events with explicit dispatch semantics;
- bounded diagnostics and lifecycle inspection.

Profile, bundle, patch, and overlay composition is owned by
`yunxi-composition`, above these generic primitives.

It does not contain HTTP, provider credentials, filesystem execution, shell
execution, voice SDKs, Web code, or legacy YunXi business rules.

### `yunxi-agent-spine`

The default Agent spine is a first-party Rust bundle and remains replaceable.
It provides the minimum useful Agent without embedding concrete capabilities:

- an Agent and turn state machine;
- append-only session events and bounded history;
- context assembly interfaces;
- model and tool provider seams;
- a loop that advances model response, tool calls, results, and the next step;
- cancellation, round limits, and resource budgets.

Concrete model adapters, context sources, memory providers, and tool executors
remain plugins.

The spine also has a fail-closed approval path: a tool request can suspend the
turn in `AwaitingApproval`, and only a matching host decision for round, call
id, and tool name can continue it. The CLI supplies the spine traits through
the process Host. The spine owns model/tool turn progression and approval
continuation; `session.rs` still owns durable persistence, post-response
processing, and Web projection. Keeping those application hooks outside the
spine preserves its replaceable boundary.

## External Plugin Runtime

An external capability is a logical Cordis plugin represented inside the main
context by a proxy service. Its implementation runs in a Rust Host process.

- Host processes are grouped by trust and failure domain; the current CLI
  starts enabled built-ins during Host composition.
- The explicit package directory is discovered and reconciled by
  `PluginDiscoveryManager`; dependency ordering, enablement, replacement, and
  manager-owned unload are bounded and deterministic at refresh boundaries.
- Lazy start, host reuse, arbitrary package formats, and finer-grained
  failure-domain grouping remain planned runtime work.
- High-risk or unstable capabilities receive a dedicated Host.
- IPC uses a bounded, versioned request/response/event protocol.
- A replacement Host generation is started and handshaken before new traffic is
  routed to it; the failed generation is then disposed.
- Host failure is data in the kernel, never an unwind in the kernel process.

The first implementation uses standalone Rust executables and does not load
arbitrary Rust dynamic libraries. WASM or another in-process format can be
added only after the protocol and security boundary are stable. Package
discovery and lifecycle reconciliation do not change that boundary.

## Plugin Switches

The user-facing control is one switch per plugin:

- disabled: no process, no permission, no service/tool/event registration;
- enabled: let the Host validate the plugin manifest, grant its declared
  capabilities, and start its Host; the current Web surface shows state and
  the one plugin switch rather than a separate permission tree;
- disabling revokes the plugin's grants, removes registrations, and stops its
  Host;
- no fine-grained permission tree is required for the v1 UI.

Plugins with no external side effects are enabled by default in the current
policy. Plugins that access files, networks, processes, credentials, or devices
default to off. The current CLI preserves Context, Persona, and Storage as
enabled baseline services; Model and the Agent spine are mandatory.
The manifest is the machine-readable declaration; the current UI keeps the
first version intentionally small and does not expose a second permission tree.

The legacy project write boundary is a system invariant and cannot be opened by
a normal plugin switch. The settings crate contains 15 built-in optional keys,
and the current CLI/Web launch path wires all 15. Voice and Weixin are live
Host/inventory routes when enabled, backed by deterministic fixtures rather than
production device or login adapters.

## Failure Policy

For an unexpected Host exit:

1. record the failure and preserve the last valid state;
2. retry with bounded backoff;
3. restart at most three times for the current enable request;
4. mark the plugin stopped after the third failure;
5. expose the failure in CLI and Web and wait for an explicit user re-enable.

A failed optional plugin must not stop the Agent spine, the kernel, or sibling
plugin Hosts. The Plugin Host retries a failed generation at most three times
per enable cycle; retry advancement is synchronous and refresh-driven. Old-
generation events are ignored after a generation switch.

## dsh Composition Mapping

The Rust composer follows the dsh model:

- a profile selects ordered bundles;
- bundles contribute plugin entries and defaults;
- profile and user patches override or insert entries;
- the effective tree is inspectable before boot;
- runtime registrations unwind with the owning Fiber.

The Web client may reuse dsh interaction patterns or bundles, but it is an
adapter over the Rust Gateway and never owns provider, approval, storage, or
plugin lifecycle logic.

## Non-Goals for the First Stable Kernel

- porting the dsh TypeScript API byte-for-byte;
- loading arbitrary Rust `cdylib` plugins;
- unrestricted dynamic self-modification by the model;
- a second runtime hidden inside Web, Weixin, or voice adapters;
- changes to the legacy YunXi repository or its installation;
- claiming an OS sandbox before platform enforcement is implemented and tested.

## Acceptance Baseline

The current acceptance baseline is the stable integrated slice, not a claim of
full YunXi feature parity. Tests demonstrate:

- a minimal model chat works with only the core spine and a model plugin;
- disabled plugins do not launch or register routes;
- safe default plugins launch without enabling optional capabilities;
- a crashed, timed-out, malformed, or repeatedly failing Host is isolated;
- user disable removes its services and stops future calls;
- old-generation messages cannot mutate current state;
- CLI and Web use the same Host facade;
- the legacy repository remains unchanged and runnable.

Remaining planned work includes direct service discovery from the Cordis
runtime, arbitrary dynamic-library/WASM package formats, true live hot-unmount
semantics, and production Voice/Weixin adapters. Package-based external
discovery and lifecycle reconciliation are already part of the Plugin Host
baseline. The current Voice and Weixin crates provide process fixtures and typed
contracts plus launch/inventory wiring. The present Web switch path rebuilds the
Host before applying a change rather than hot-unloading a running CLI Host.
