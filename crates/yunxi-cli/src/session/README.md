# Session Modules

`stateful.rs` owns post-response persistence and management calls for sessions,
memory writes, proactive scheduling, and the encrypted mailbox. Parent
`session.rs` owns plugin launch, read-only context assembly, model routing, and
approval continuation for automatic tool calls, plus the browser-safe Web
projection. `tool_loop.rs` owns the built-in
Shell/Patch catalog, MCP/Skill dynamic projections, bounded argument decoding,
and tool-result conversion.
