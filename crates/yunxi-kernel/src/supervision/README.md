# Process Supervision

This private directory is the only layer that owns `std::process::Child`.
It translates runtime commands into operating-system actions and reports state
changes back as generation-tagged events.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Private supervisor facade exposed to runtime |
| `process.rs` | Child spawn, polling, termination, and supervisor thread |

Protocol handshakes, sandboxing, and resource limits are future layers. They
must not be hidden inside this process-lifecycle implementation.
