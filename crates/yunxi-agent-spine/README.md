# yunxi-agent-spine

`yunxi-agent-spine` is the default, replaceable Rust Agent layer for YunXi
Next. It is deliberately a small orchestration crate. It does not perform
HTTP, shell execution, file access, voice I/O, Web serving, or plugin process
management.

## Responsibilities

- Track Agent and turn state.
- Append bounded, append-only session events.
- Ask a `ContextAssembler` for a protocol `ChatRequest`.
- Ask a `ModelProvider` for a protocol `ChatResult`.
- Expose a `ToolBroker` catalog and dispatch tool calls.
- Expose a fail-closed, approval-aware tool execution path.
- Drive model -> tool calls -> tool results -> next model step.
- Enforce round, model-call, and tool-call budgets.
- Check a cloneable cancellation token at every orchestration boundary.
- Return structured errors without poisoning the reusable Agent.

The implementation reuses `yunxi-protocol` types instead of defining a second
message or tool wire format: `ChatMessage`, `ChatRequest`, `ChatResult`,
`ToolCatalog`, `ToolCallBatch`, `ToolResult`, and `ToolResultOutcome` are the
boundary types.

## Extension points

External plugin adapters implement three small traits:

```text
ModelProvider       model plugin / IPC adapter
ContextAssembler    persona, memory, session, or policy composition
ToolBroker          tool catalog and tool-host dispatch
```

The traits are synchronous because the current local plugin protocol is
blocking. An adapter may own its IPC or blocking network implementation; this
crate remains unaware of the transport and credentials.

Tool execution errors are converted into a structured failed `ToolResult` and
the loop continues, so a transient tool failure can be shown to the model.
Model failures, cancellation, malformed model output, and exhausted budgets
finish only the current turn. The Agent returns to `Ready` and can be reused.

## Approval-aware tool execution

`ToolBroker::execute` remains the original synchronous compatibility method.
It is the low-level, already-authorized execution hook and is intentionally
unchanged so existing brokers continue to compile. New callers that need
approval semantics use `run_turn_with_approval`:

```rust,no_run
use yunxi_agent_spine::{
    AgentTurnOutcome, CancellationToken, ToolApprovalPolicy, ToolBroker, ToolDecision,
    ToolRequest,
};
use yunxi_protocol::{ToolApprovalDecision, ToolApprovalState};

let cancellation = CancellationToken::new();
let outcome = agent.run_text_turn_with_approval("run the tool", &cancellation)?;
let AgentTurnOutcome::AwaitingApproval(request) = outcome else {
    unreachable!("the scripted model requested a tool");
};

let decision = ToolApprovalDecision::new(
    request.round(),
    request.call_id().clone(),
    request.tool_name().clone(),
    ToolApprovalState::approved("host-ticket")?,
)?;
let completed = agent.approve_pending_tool(decision, &cancellation)?;
```

The default `ToolBroker::decide` returns `ToolDecision::RequestApproval` with
the protocol's `Approval` grant. No tool is called until the approval matches
the pending round, call id, and tool name. A denied decision becomes a
model-visible `Rejected` tool result and does not invoke the broker.

An approval-aware broker may override `execute_approved` to validate or
consume the opaque approval ticket. Its default implementation delegates to
the old `execute` hook for source compatibility.

For a legacy broker, `ApprovalAwareToolBroker::new` adds this fail-closed
boundary. A custom `ToolApprovalPolicy` may return `Reject` or explicitly
return `Execute` for a narrow, low-risk tool. The wrapper's legacy `execute`
method itself still fails closed.

Approval requests and resolutions are append-only `SessionEvent` records, and
an approval-aware Agent remains in `AgentState::AwaitingApproval` until the
pending call is resolved. The continuation is bounded by the existing turn,
model-call, and tool-call budgets. This crate only models the decision and
continuation; it does not contact a user, device, or network.

## Session bounds

`SessionLog` accepts events only through `append`. It validates each event,
assigns a monotonic sequence, and enforces event-count, total-byte, and
per-event-byte limits before mutating the log. There is no public mutable view
or delete operation.

The default context assembler rebuilds the conversation from the log and
passes the current tool catalog to the model. A custom assembler can replace it
without changing the loop.

## Cordis integration boundary

This first crate intentionally depends only on the protocol. The Cordis bridge
is supplied by the CLI/runtime layer rather than leaking Cordis internals into
the replaceable Agent spine. An adapter needs only the smallest stable surface:

- scoped service lookup and registration;
- lifecycle-owned cleanup for registrations and subscriptions;
- typed event publication/subscription;
- loader activation and deactivation hooks.

The Agent loop and protocol adapters remain above that surface. The concrete
Cordis implementation lives in `yunxi-cordis-core` and
`yunxi-cordis-runtime`; this crate does not duplicate it.
