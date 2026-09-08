# Multi-agent Process Tests

`process.rs` launches the real coordinator executable through
`ProcessPluginHost`, verifies its manifest grants and typed operation path, then
restarts it to prove persisted child state survives an isolated process exit.

The runtime unit tests use injected deterministic executors rather than model
or tool fixtures. They cover parallel worker rendezvous and elapsed time,
branch failure isolation, immediate targeted cancellation, child-grant denial,
model/grant routing, typed grant-to-tool catalog projection, resumable
transcripts, and bounded runtime events.

The CLI process suite also starts a real isolated child model and the real
read-only file plugin. It verifies that `workspace_read` exposes `file.read`,
returns the file result to the child model, and keeps `shell.execute` out of the
child catalog.
