#![doc = "Bounded proactive scheduling policy for YunXi Next."]
#![forbid(unsafe_code)]

mod plugin;
mod policy;

pub use plugin::{SCHEDULER_PLUGIN_ID, SchedulerPluginError, run_scheduler_plugin};
pub use policy::evaluate;
