# yunxi-cordis-core

`yunxi-cordis-core` is a small, dependency-free Rust 2024 runtime for the
Cordis semantics used by YunXi Next. It provides scoped contexts, typed
services, plugin fibers, reversible effects, and a typed event bus. It does not
start processes, load dynamic libraries, perform I/O, or contain Agent
business capabilities.

The crate is intentionally small and dependency-free. It is part of the
YunXi Next workspace, but remains independent of the higher-level runtime and
does not pull in any application capability.

## Concepts

- `Context` is a scope. Child scopes inherit services from their parent and can
  shadow a service locally. Services disappear with their owning scope.
- `ServiceKey<T>` gives a service a stable string name and a compile-time
  value type. A duplicate name in one scope is rejected.
- `Plugin` declares service dependencies and mounts into a child `Context`.
  Dependencies are resolved against the mount context and its ancestors; a
  sibling plugin's private services are not visible. The returned `Fiber`
  exposes the plugin lifecycle and unmounts its scope.
- `Effect` is a one-shot disposer owned by a context. Effects run in reverse
  registration order when a scope is disposed.
- `EventBus` uses typed `EventKey<T>` values. Each event name has one dispatch
  mode contract: `emit`, `waterfall`, `parallel`, `serial`, or `bail`.
- `ContextSnapshot` and `EventBusSnapshot` contain deterministic, read-only
  diagnostics without exposing service values or callback internals.

## Small example

```rust
use yunxi_cordis_core::{Context, Effect, Plugin, ServiceKey};

const GREETING: ServiceKey<String> = ServiceKey::new("example.greeting");

struct GreetingPlugin;

impl Plugin for GreetingPlugin {
    fn id(&self) -> &str {
        "example.greeting-plugin"
    }

    fn mount(&self, context: &Context) -> Result<(), yunxi_cordis_core::CordisError> {
        context.provide(GREETING, "hello".to_owned())?;
        context.install_effect(Effect::new(|| {
            println!("greeting plugin stopped");
            Ok(())
        }))?;
        Ok(())
    }
}

let root = Context::new();
let fiber = root.mount(GreetingPlugin).expect("plugin mounts");
assert_eq!(fiber.context().service(GREETING).unwrap().as_str(), "hello");
fiber.unmount().expect("plugin unmounts");
```

An event subscription can be made scope-owned with
`Context::own_subscription`:

```rust
use yunxi_cordis_core::{Context, EventBus, EventKey};

const MESSAGE: EventKey<String> = EventKey::new("example.message");
let context = Context::new();
let bus = EventBus::new();
let subscription = bus.on_emit(MESSAGE, |message| {
    assert!(!message.is_empty());
    Ok(())
}).unwrap();
context.own_subscription(subscription).unwrap();
bus.emit(MESSAGE, &"ready".to_owned()).unwrap();
```

## Event semantics

The mode is part of the event contract. Registering a second handler with a
different mode or payload type is rejected.

- `emit`: invokes every handler in registration order with an immutable
  payload. A handler error is reported after remaining handlers have run.
- `waterfall`: passes an owned payload through handlers in registration order;
  each handler returns the value seen by the next handler.
- `parallel`: runs all handlers concurrently against the same immutable
  payload. Completion order is intentionally unspecified; errors are reported
  deterministically by registration order.
- `serial`: invokes handlers in order with a mutable payload and stops at the
  first error.
- `bail`: invokes handlers in order until one returns `Handled`.

Callbacks are isolated from the bus with panic capture. A panicking callback
becomes a structured event error instead of unwinding through the runtime.

## Deliberate limits

This first slice is synchronous except for `parallel` dispatch. It has no
async executor, dynamic library ABI, process supervisor, configuration loader,
hot reload protocol, persistence, or OS sandbox. Those belong above this
kernel and must not be smuggled into the trusted core.

## Verification

Run from this directory or use the manifest path from the repository root:

```powershell
cargo fmt --manifest-path crates/yunxi-cordis-core/Cargo.toml -- --check
cargo clippy --manifest-path crates/yunxi-cordis-core/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path crates/yunxi-cordis-core/Cargo.toml --all-targets
```
