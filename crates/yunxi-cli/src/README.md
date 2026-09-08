# CLI source

- `main.rs` selects normal CLI mode or one of the private built-in plugin modes.
- `lib.rs` coordinates argument handling, session startup, and terminal I/O.
- `args.rs` parses the deliberately small command-line surface.
- `commands.rs` implements provider-free management, Voice/Weixin facades, and
  structured output.
- `control.rs` implements detached `status`, `diagnostics`, `enable`, `disable`,
  and validation-only `reload` commands for persisted plugin state.
- `migration.rs` exposes explicit read-only planning, Next-only apply, and
  manifest-guarded rollback.
- `management.rs` defines REPL management commands and history replacement results.
- `session.rs` launches plugins, composes optional identity context, and routes model calls.
- `session/` owns post-response state, model tools, and isolated child-agent turns.
- `session/auxiliary.rs` gives standalone Voice and Weixin commands a narrow
  supervised process-Host interface without starting a model provider.
- `web.rs` exposes streaming multi-session turns, approval, continuable
  subagents, and revisioned capability-settings RPCs over the same session and
  plugin Host.
- `session/tool_loop.rs` owns the bounded model-tool catalog, MCP/Skill projections,
  and fail-closed argument decoding.
- `session/multi_agent.rs` owns coordinator calls and one isolated Model process
  per approved child turn.
- `repl.rs` owns commands and rolling in-memory conversation history.
- `tui.rs` owns the bounded interactive terminal surface, the Host facade used
  by its turns, legacy management aliases, and active-turn cancellation.
- `ui.rs` contains compact terminal presentation helpers.
- `voice_runtime.rs` defines only the Voice process launch grants and deadline;
  provider selection and fallback execute inside the isolated plugin.
