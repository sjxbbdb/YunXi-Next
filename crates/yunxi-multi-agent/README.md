# YunXi Multi-agent

This crate owns the isolated `tool.multi-agent@1` coordination process. It
persists bounded parent-child graphs, per-agent transcripts, budgets, lifecycle
events, and recursive interruption beneath a Host-granted Next workspace.

The coordinator never calls a model, reads Provider credentials, or executes a
child tool. The trusted Host starts a separate model plugin process for each
child turn and reports completion or failure back through the typed protocol.
This keeps branch failures separate from the main model route.

The current Host integration executes child turns synchronously. Recursive
interrupts persist between turns, but live cancellation of an in-flight child
model request and parallel background workers remain outside this baseline.
