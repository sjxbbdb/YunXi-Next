#![doc = "Isolated multi-agent coordination capability for YunXi Next."]
#![forbid(unsafe_code)]

mod plugin;
mod runtime;
mod store;

pub use plugin::{MULTI_AGENT_PLUGIN_ID, MultiAgentPluginError, run_multi_agent_plugin};
pub use runtime::{
    AsyncMultiAgentRuntime, CancellationFuture, CancellationToken, ChildExecutor, ChildToolCatalog,
    ChildToolKind, ChildTurn, ChildWorkerError, ChildWorkerFuture, ChildWorkerSpec,
    MAX_RUNTIME_EVENTS, RuntimeError, RuntimeProjection, WorkerEvent, WorkerEventKind,
    WorkerHandle, WorkerOutcome, WorkerPlan, WorkerProjection, WorkerRecovery,
    WorkerRecoveryOrigin, WorkerRecoveryPlan,
};
pub use store::{CoordinatorStore, MultiAgentStoreError};
