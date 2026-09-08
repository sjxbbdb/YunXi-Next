# Source Map

- `lib.rs` exposes scheduling policy and the plugin entry point.
- `policy.rs` evaluates bounded proactive signals.
- worker.rs owns the cancellable recurring facade, sink boundary, status, and
  idempotency window.
- template.rs owns the deterministic, explicitly labelled love-letter
  fallback.
- `plugin.rs` dispatches the versioned capability call.
- `bin/` contains the standalone plugin executable.
