//! Black-box tests that launch real child processes to verify isolation.

use std::env;
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use yunxi_kernel::{
    KernelState, PluginCommand, PluginFailure, PluginId, PluginSpec, PluginState, YunxiKernel,
};

const TEST_PLUGIN_MODE: &str = "YUNXI_TEST_PLUGIN_MODE";

#[test]
fn plugin_subprocess_entrypoint() {
    let Ok(mode) = env::var(TEST_PLUGIN_MODE) else {
        return;
    };

    match mode.as_str() {
        "healthy" => loop {
            thread::sleep(Duration::from_millis(100));
        },
        "crash" => process::exit(42),
        _ => process::exit(64),
    }
}

#[test]
fn a_crashing_plugin_does_not_stop_the_kernel_or_its_sibling() {
    let mut kernel = YunxiKernel::new();
    let healthy_id = plugin_id("yunxi.test.healthy");
    let crashing_id = plugin_id("yunxi.test.crashing");
    kernel
        .register(fixture_spec(healthy_id.clone(), "healthy"))
        .expect("register healthy plugin");
    kernel
        .register(fixture_spec(crashing_id.clone(), "crash"))
        .expect("register crashing plugin");

    kernel.start(&healthy_id).expect("start healthy plugin");
    kernel.start(&crashing_id).expect("start crashing plugin");

    let healthy_state = wait_for_state(&mut kernel, &healthy_id, |state| {
        matches!(state, PluginState::Running { .. })
    });
    assert!(matches!(healthy_state, PluginState::Running { .. }));

    let crashing_state = wait_for_state(&mut kernel, &crashing_id, PluginState::is_failed);
    assert!(matches!(
        crashing_state,
        PluginState::Failed(PluginFailure::UnexpectedExit { code: Some(42) })
    ));

    kernel.refresh();
    assert_eq!(kernel.state(), KernelState::Running);
    assert!(kernel.is_healthy());
    assert!(matches!(
        kernel
            .plugin(&healthy_id)
            .expect("healthy plugin snapshot")
            .state(),
        PluginState::Running { .. }
    ));
    assert_eq!(kernel.snapshot().failed_plugin_count(), 1);

    kernel.shutdown();
    assert_eq!(kernel.state(), KernelState::Stopped);
    assert_eq!(
        kernel
            .plugin(&healthy_id)
            .expect("healthy plugin snapshot")
            .state(),
        &PluginState::Stopped
    );
}

#[test]
fn a_process_spawn_failure_is_contained() {
    let mut kernel = YunxiKernel::new();
    let id = plugin_id("yunxi.test.missing");
    let missing_program = env::temp_dir().join(format!(
        "yunxi-plugin-that-does-not-exist-{}",
        process::id()
    ));
    kernel
        .register(PluginSpec::new(
            id.clone(),
            PluginCommand::new(missing_program),
        ))
        .expect("register missing plugin");

    kernel.start(&id).expect("start supervisor");
    let state = wait_for_state(&mut kernel, &id, PluginState::is_failed);

    assert!(matches!(
        state,
        PluginState::Failed(PluginFailure::Spawn { .. })
    ));
    assert!(kernel.is_healthy());
    assert_eq!(kernel.state(), KernelState::Running);
}

#[test]
fn a_failed_plugin_restarts_only_when_explicitly_requested() {
    let mut kernel = YunxiKernel::new();
    let id = plugin_id("yunxi.test.restart");
    kernel
        .register(fixture_spec(id.clone(), "crash"))
        .expect("register crashing plugin");

    kernel.start(&id).expect("start first generation");
    wait_for_state(&mut kernel, &id, PluginState::is_failed);
    assert_eq!(
        kernel
            .plugin(&id)
            .expect("first failed snapshot")
            .generation(),
        1
    );

    thread::sleep(Duration::from_millis(100));
    kernel.refresh();
    let unchanged = kernel.plugin(&id).expect("unchanged failed snapshot");
    assert_eq!(unchanged.generation(), 1);
    assert!(unchanged.state().is_failed());

    kernel.start(&id).expect("start second generation");
    wait_for_state(&mut kernel, &id, PluginState::is_failed);
    assert_eq!(
        kernel
            .plugin(&id)
            .expect("second failed snapshot")
            .generation(),
        2
    );
    assert!(kernel.is_healthy());
}

fn fixture_spec(id: PluginId, mode: &str) -> PluginSpec {
    let executable = env::current_exe().expect("resolve integration test executable");
    let command = PluginCommand::new(executable)
        .args(["--exact", "plugin_subprocess_entrypoint", "--nocapture"])
        .env(TEST_PLUGIN_MODE, mode);
    PluginSpec::new(id, command)
}

fn plugin_id(value: &str) -> PluginId {
    PluginId::new(value).expect("valid test plugin id")
}

fn wait_for_state(
    kernel: &mut YunxiKernel,
    id: &PluginId,
    predicate: impl Fn(&PluginState) -> bool,
) -> PluginState {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        kernel.refresh();
        let state = kernel
            .plugin(id)
            .unwrap_or_else(|| panic!("missing plugin snapshot for {id}"))
            .state()
            .clone();
        if predicate(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for plugin {id}; last state: {state}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
