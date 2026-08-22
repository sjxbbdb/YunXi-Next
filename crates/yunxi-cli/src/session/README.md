# Session Modules

`stateful.rs` owns post-response persistence and management calls for sessions,
memory writes, proactive scheduling, and the encrypted mailbox. Parent
`session.rs` owns plugin launch, read-only context assembly, and model routing.
