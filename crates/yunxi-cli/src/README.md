# CLI source

- `main.rs` selects normal CLI mode or one of the private built-in plugin modes.
- `lib.rs` coordinates argument handling, session startup, and terminal I/O.
- `args.rs` parses the deliberately small command-line surface.
- `management.rs` defines REPL management commands and history replacement results.
- `session.rs` launches plugins, composes optional identity context, and routes model calls.
- `session/` owns post-response state, model tools, and isolated child-agent turns.
- `web.rs` exposes session, approval, and revisioned capability-settings RPCs
  over the same session and plugin Host.
- `session/tool_loop.rs` owns the bounded model-tool catalog, MCP/Skill projections,
  and fail-closed argument decoding.
- `session/multi_agent.rs` owns coordinator calls and one isolated Model process
  per approved child turn.
- `repl.rs` owns commands and rolling in-memory conversation history.
- `ui.rs` contains compact terminal presentation helpers.
