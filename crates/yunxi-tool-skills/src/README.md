# Skills Sources

`config.rs` parses the explicit Skills root and disabled-id settings.
`discovery.rs` reads and validates bounded `SKILL.md` files and optional
metadata-only `tools.json` files plus separately validated `actions.json`
declarations. `action.rs` owns the opt-in direct-child JSONL executor and
grant/path/limit checks. `plugin.rs` owns the isolated metadata protocol loop.
`lib.rs` is the small public facade used by the CLI and binary entry point.
