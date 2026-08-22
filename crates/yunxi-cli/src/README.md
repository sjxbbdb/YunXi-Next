# CLI source

- `main.rs` selects normal CLI mode or the private model-plugin child mode.
- `lib.rs` coordinates argument handling, session startup, and terminal I/O.
- `args.rs` parses the deliberately small command-line surface.
- `session.rs` launches and supervises the isolated model plugin.
- `repl.rs` owns commands and rolling in-memory conversation history.
- `ui.rs` contains compact terminal presentation helpers.
