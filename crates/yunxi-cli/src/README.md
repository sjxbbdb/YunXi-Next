# CLI source

- `main.rs` selects normal CLI mode or one of the private built-in plugin modes.
- `lib.rs` coordinates argument handling, session startup, and terminal I/O.
- `args.rs` parses the deliberately small command-line surface.
- `management.rs` defines REPL management commands and history replacement results.
- `session.rs` launches plugins, composes optional identity context, and routes model calls.
- `session/` owns post-response state and management capability calls.
- `repl.rs` owns commands and rolling in-memory conversation history.
- `ui.rs` contains compact terminal presentation helpers.
