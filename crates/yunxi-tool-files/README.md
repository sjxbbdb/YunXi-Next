# `yunxi-tool-files`

`yunxi-tool-files` is the read-only `tool.files@1` plugin. It provides bounded
workspace file-name search and UTF-8 file viewing through a Host-issued
`WorkspaceGrant`.

It never writes files, executes commands, follows paths outside the granted
workspace, or persists file contents. The process is optional and is not
launched when the Files capability switch is disabled.

| Path | Responsibility |
| --- | --- |
| `src/executor.rs` | Canonical path checks, bounded traversal, and UTF-8 reads |
| `src/plugin.rs` | Manifest handshake and typed operation dispatch |
| `src/lib.rs` | Public plugin facade |
| `src/bin/yunxi-tool-files.rs` | Standalone executable entry point |
