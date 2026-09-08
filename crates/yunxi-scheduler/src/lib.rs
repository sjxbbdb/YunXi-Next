#![doc = "Bounded proactive scheduling policy for YunXi Next."]
#![forbid(unsafe_code)]

mod plugin;
mod policy;
mod template;
mod worker;

pub use plugin::{SCHEDULER_PLUGIN_ID, SchedulerPluginError, run_scheduler_plugin};
pub use policy::evaluate;
pub use template::{GeneratedTemplate, TemplateContext, TemplateSource, render_love_letter};
pub use worker::{
    DEFAULT_WORKER_INTERVAL, EnqueueDisposition, MAX_ENQUEUES_PER_TICK, MAX_IDEMPOTENCY_KEYS,
    MIN_WORKER_INTERVAL, ProactiveSink, ScheduledMessage, SchedulerConfig, SchedulerFacade,
    SchedulerHandle, SchedulerSinkError, SchedulerStatus, SchedulerTick, TickOutcome, TickSource,
    WorkerLifecycle,
};
