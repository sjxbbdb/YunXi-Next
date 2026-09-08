# YunXi Multi-agent

This crate owns the isolated `tool.multi-agent@1` coordination process. It
persists bounded parent-child graphs, per-agent transcripts, budgets, lifecycle
events, and recursive interruption beneath a Host-granted Next workspace.

The coordinator never calls a model, reads Provider credentials, or executes a
child tool. The typed `AsyncMultiAgentRuntime` accepts a `ChildExecutor` adapter
and runs each active turn on its own worker thread. The adapter receives the
selected model, exact child tool grants, bounded transcript, and cooperative
cancellation token. Cancellation is persisted as an interrupted branch, while
a worker error or panic is persisted as a failed branch without cancelling
siblings.

The runtime is the scheduling and state boundary; it does not pretend to be a
model provider. A host or model-plugin integration supplies the `ChildExecutor`
implementation that routes a turn to a real model. Each turn also carries a
typed `ChildToolCatalog`. The Host-side child adapter projects only the
executable tools authorized by the persisted child grant:
`file.search` and `file.read` for `workspace_read`, plus `patch.apply` when
`workspace_write` and a writable sandbox are both present. No shell, MCP,
Skills, credentials, or recursive agent tools enter a child catalog. The WebHost
uses this boundary for bounded continuable child prompts, concurrent child
jobs, streamed child events, and active-child interruption. Unit tests use
small in-memory executors only to deterministically verify concurrency,
cancellation, routing, persistence, and failure isolation; the CLI process
tests cross the real model and file-plugin boundaries for executable-tool use.

The `list` operation returns bounded graph snapshots. The read-only `inspect`
operation returns one child snapshot and its bounded transcript for the Web
projection; it does not mutate the graph or expose credentials. A read-only
caller can inspect a post-restart graph even when it cannot persist recovery.

The process plugin continues to expose the typed persistence operations. Runtime
workers are available through the library API, including parallel spawn,
targeted or recursive interruption, bounded event projection, and resumable
child sessions with per-turn model selection. Child grant requests are checked
against the root authority and the stored parent branch before a worker starts,
and the worker policy checks the parent authority again; failed or cancelled
branches are persisted independently of their siblings.
