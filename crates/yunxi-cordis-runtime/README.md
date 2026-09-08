# yunxi-cordis-runtime

`yunxi-cordis-runtime` is the smallest usable Cordis meta-runtime above
`yunxi-cordis-core`. It is synchronous, bounded, pure Rust, and forbids
`unsafe` code.

## Responsibilities

- Build an immutable lookup index over a `&'static [PluginDefinition]`.
- Instantiate statically linked `PluginFactory` function pointers.
- Own one root `Context` and the Fibers mounted below it.
- Apply the manifest policy for `Core`, `AgentSpine`, and `Optional` roles.
- Apply the coarse user `enable`/`disable` switch.
- Keep optional mount failures local to the failed plugin.
- Expose deterministic snapshots for UI, CLI, and diagnostics.
- Retain a bounded, metadata-only lifecycle journal for cursor-based replay.

There is intentionally no `register` method. A plugin is added by placing a
definition in a static slice and rebuilding the binary:

```rust
use yunxi_cordis_runtime::{
    CordisRuntime, Plugin, PluginDefinition, PluginFactory, PluginManifest,
    PluginRegistry,
};

struct MemoryPlugin;

impl Plugin for MemoryPlugin {
    fn id(&self) -> &str {
        "example.memory"
    }

    fn mount(
        &self,
        _context: &yunxi_cordis_runtime::Context,
    ) -> Result<(), yunxi_cordis_runtime::CordisError> {
        Ok(())
    }
}

fn memory_factory() -> Box<dyn Plugin> {
    Box::new(MemoryPlugin)
}

static DEFINITIONS: &[PluginDefinition] = &[PluginDefinition::new(
    PluginManifest::safe_optional("example.memory", "Memory"),
    PluginFactory::new(memory_factory),
)];

let registry = PluginRegistry::new(DEFINITIONS)?;
let mut runtime = CordisRuntime::new(registry);
let report = runtime.start_default()?;
assert_eq!(report.activated(), &["example.memory"]);
```

For an eager construction path, `CordisRuntime::from_static(DEFINITIONS)`
combines registry validation, construction, and `start_default`.

## Default policy

- `Core` and `AgentSpine` use `Always` and cannot be disabled through the user
  switch.
- `Optional` with `risk=Safe` and `default=Safe` starts enabled.
- `Optional` with `risk=External` and `default=Never` starts disabled.
- A user override wins over the manifest default. `clear_override` returns an
  optional plugin to its manifest default.

The `Always` policy is reserved for required roles. `Safe` is reserved for a
safe optional default; an external optional plugin must declare `Never`. This
keeps the startup rule visible and prevents contradictory manifests.

The static registry is capped at `MAX_STATIC_PLUGINS` entries, and retained
failure messages are bounded. The runtime performs no background retry loop;
a later explicit `enable` or `start_default` call is the retry boundary.
Startup resolves dependencies against the root context and retries a
temporarily missing dependency only while another pending entry makes
progress. A dependency cycle or an absent service becomes a bounded startup
failure.

When an optional plugin fails during factory or mount, its Fiber is not kept,
its failure is retained in `PluginSnapshot`, and later plugins still start.
The runtime catches factory and core mount failures; the core crate also
catches plugin lifecycle panics.

Disabling a plugin unmounts its Fiber first and then removes it from the live
Fiber index. A snapshot may retain `fiber_state = Unmounted` as historical
diagnostic information while `fiber()` is `None`. Fiber queries return
read-only snapshots so callers cannot bypass the Core/AgentSpine switch.

## Lifecycle events

`CordisRuntime::events_since(after, limit)` returns a bounded
`RuntimeEventPage`. Events have monotonic sequence numbers and describe only
startup, mount, skip, failure, disable, unmount, and shutdown transitions.
Diagnostic messages are truncated at `MAX_RUNTIME_EVENT_MESSAGE_BYTES`, and
the journal retains at most `MAX_RUNTIME_EVENTS` entries. If a caller's cursor
falls behind that window, `page.gap()` is true; the caller should fetch a fresh
snapshot before continuing. The journal never stores service values, callback
addresses, credentials, or arbitrary plugin payloads.

## Explicit boundary

This crate does not load dynamic libraries, spawn plugin processes, provide
IPC, implement an OS sandbox, or perform hot reload. Those are later layers
above this trusted synchronous slice. Process isolation and versioned IPC
belong in a future Plugin Host; dynamic loading belongs behind a separately
audited ABI.

This crate is a member of the parent workspace and can also be checked by
passing its manifest path directly.

## Verification

From `D:\YunXi Next`:

```powershell
cargo fmt --manifest-path crates/yunxi-cordis-runtime/Cargo.toml -- --check
cargo check --manifest-path crates/yunxi-cordis-runtime/Cargo.toml --all-targets
cargo test --manifest-path crates/yunxi-cordis-runtime/Cargo.toml --all-targets
cargo clippy --manifest-path crates/yunxi-cordis-runtime/Cargo.toml --all-targets -- -D warnings
```
