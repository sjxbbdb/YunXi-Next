# Multi-agent Process Tests

`process.rs` launches the real coordinator executable through
`ProcessPluginHost`, verifies its manifest grants and typed operation path, then
restarts it to prove persisted child state survives an isolated process exit.
