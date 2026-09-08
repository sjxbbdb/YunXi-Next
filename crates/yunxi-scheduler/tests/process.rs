//! Black-box verification of the isolated scheduler plugin process.

use std::time::Duration;

use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_plugin_host::{PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    CapabilityDescriptor, ProactiveSchedulerRequest, ProactiveSchedulerResult,
    SCHEDULER_PROACTIVE_EVALUATE_OPERATION, capabilities,
};
use yunxi_scheduler::SCHEDULER_PLUGIN_ID;

#[test]
fn scheduler_process_evaluates_a_typed_request() {
    let capability = CapabilityDescriptor::new(
        capabilities::SCHEDULER_PROACTIVE,
        capabilities::SCHEDULER_PROACTIVE_VERSION,
    )
    .expect("capability");
    let id = PluginId::new(SCHEDULER_PLUGIN_ID).expect("plugin id");
    let launch = PluginLaunch::new(
        id,
        PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-scheduler")),
    )
    .with_display_name("Scheduler process test")
    .with_handshake_timeout(Duration::from_secs(3))
    .with_io_timeouts(Some(Duration::from_secs(3)), Some(Duration::from_secs(3)));
    let mut host = ProcessPluginHost::new();
    host.launch(launch).expect("launch scheduler");
    let result = host
        .invoke::<_, ProactiveSchedulerResult>(
            &capability,
            SCHEDULER_PROACTIVE_EVALUATE_OPERATION,
            &ProactiveSchedulerRequest::new(12 * 60).with_reminder_due(true),
        )
        .expect("evaluate request");
    assert_eq!(result.plans().len(), 1);
    host.shutdown();
}
