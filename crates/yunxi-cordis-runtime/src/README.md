# Runtime source map

- `manifest.rs`: role, risk, and default-enable policy.
- `registry.rs`: validated static plugin definitions and factories.
- `snapshot.rs`: read-only lifecycle, Fiber, startup, and failure projections.
- `error.rs`: structured registry and runtime errors.
- `runtime.rs`: root context, Fiber ownership, switch operations, startup, and
  bounded snapshot projections.
- `lib.rs`: public facade and re-exports.
