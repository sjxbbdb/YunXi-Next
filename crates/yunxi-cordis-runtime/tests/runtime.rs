use std::sync::atomic::{AtomicUsize, Ordering};

use yunxi_cordis_core::{Context, CordisError, Effect, Plugin, ServiceDependency, ServiceKey};
use yunxi_cordis_runtime::{
    CordisRuntime, DefaultEnablement, FiberState, PluginDefinition, PluginFactory, PluginManifest,
    PluginRegistry, PluginRisk, PluginRole, PluginRuntimeState, RuntimeError,
};

struct FixturePlugin {
    id: &'static str,
    failure: bool,
}

impl Plugin for FixturePlugin {
    fn id(&self) -> &str {
        self.id
    }

    fn mount(&self, _context: &Context) -> Result<(), CordisError> {
        if self.failure {
            Err(CordisError::MissingService {
                name: "fixture.missing".to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

fn core_factory() -> Box<dyn Plugin> {
    Box::new(FixturePlugin {
        id: "integration.core",
        failure: false,
    })
}

fn agent_factory() -> Box<dyn Plugin> {
    Box::new(FixturePlugin {
        id: "integration.agent",
        failure: false,
    })
}

fn safe_factory() -> Box<dyn Plugin> {
    Box::new(FixturePlugin {
        id: "integration.safe",
        failure: false,
    })
}

fn external_factory() -> Box<dyn Plugin> {
    Box::new(FixturePlugin {
        id: "integration.external",
        failure: false,
    })
}

fn failing_factory() -> Box<dyn Plugin> {
    Box::new(FixturePlugin {
        id: "integration.failing",
        failure: true,
    })
}

fn factory_safe_factory() -> Box<dyn Plugin> {
    Box::new(FixturePlugin {
        id: "integration.factory-safe",
        failure: false,
    })
}

fn panicking_factory() -> Box<dyn Plugin> {
    panic!("intentional factory failure")
}

static DEFINITIONS: [PluginDefinition; 5] = [
    PluginDefinition::new(
        PluginManifest::core("integration.core", "Core"),
        PluginFactory::new(core_factory),
    ),
    PluginDefinition::new(
        PluginManifest::agent_spine("integration.agent", "Agent spine"),
        PluginFactory::new(agent_factory),
    ),
    PluginDefinition::new(
        PluginManifest::safe_optional("integration.safe", "Safe"),
        PluginFactory::new(safe_factory),
    ),
    PluginDefinition::new(
        PluginManifest::external_optional("integration.external", "External"),
        PluginFactory::new(external_factory),
    ),
    PluginDefinition::new(
        PluginManifest::safe_optional("integration.failing", "Failing"),
        PluginFactory::new(failing_factory),
    ),
];

static LIFECYCLE_DISPOSED: AtomicUsize = AtomicUsize::new(0);
static PRESTART_DISPOSED: AtomicUsize = AtomicUsize::new(0);

struct LifecyclePlugin;

impl Plugin for LifecyclePlugin {
    fn id(&self) -> &str {
        "integration.lifecycle"
    }

    fn mount(&self, context: &Context) -> Result<(), CordisError> {
        context.install_effect(Effect::new(|| {
            LIFECYCLE_DISPOSED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }))?;
        Ok(())
    }
}

fn lifecycle_factory() -> Box<dyn Plugin> {
    Box::new(LifecyclePlugin)
}

static LIFECYCLE_DEFINITIONS: [PluginDefinition; 1] = [PluginDefinition::new(
    PluginManifest::safe_optional("integration.lifecycle", "Lifecycle"),
    PluginFactory::new(lifecycle_factory),
)];

struct PreStartPlugin;

impl Plugin for PreStartPlugin {
    fn id(&self) -> &str {
        "integration.pre-start"
    }

    fn mount(&self, context: &Context) -> Result<(), CordisError> {
        context.install_effect(Effect::new(|| {
            PRESTART_DISPOSED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }))?;
        Ok(())
    }
}

fn prestart_factory() -> Box<dyn Plugin> {
    Box::new(PreStartPlugin)
}

static PRESTART_DEFINITIONS: [PluginDefinition; 1] = [PluginDefinition::new(
    PluginManifest::safe_optional("integration.pre-start", "Pre-start"),
    PluginFactory::new(prestart_factory),
)];

static FACTORY_FAILURE_DEFINITIONS: [PluginDefinition; 2] = [
    PluginDefinition::new(
        PluginManifest::safe_optional("integration.factory-failing", "Factory failing"),
        PluginFactory::new(panicking_factory),
    ),
    PluginDefinition::new(
        PluginManifest::safe_optional("integration.factory-safe", "Factory safe"),
        PluginFactory::new(factory_safe_factory),
    ),
];

const LATE_SERVICE: ServiceKey<u32> = ServiceKey::new("integration.late-service");

struct LateConsumer;

impl Plugin for LateConsumer {
    fn id(&self) -> &str {
        "integration.late-consumer"
    }

    fn dependencies(&self) -> Vec<ServiceDependency> {
        vec![LATE_SERVICE.required()]
    }

    fn mount(&self, context: &Context) -> Result<(), CordisError> {
        let _service = context.service(LATE_SERVICE)?;
        Ok(())
    }
}

fn late_consumer_factory() -> Box<dyn Plugin> {
    Box::new(LateConsumer)
}

static DEPENDENCY_DEFINITIONS: [PluginDefinition; 1] = [PluginDefinition::new(
    PluginManifest::safe_optional("integration.late-consumer", "Late consumer"),
    PluginFactory::new(late_consumer_factory),
)];

#[test]
fn default_policy_and_user_override_are_visible_in_snapshot() {
    let registry = PluginRegistry::new(&DEFINITIONS).expect("static definitions are valid");
    let mut runtime = CordisRuntime::new(registry);

    let before = runtime.snapshot().expect("snapshot works");
    let core = before.plugin("integration.core").expect("core exists");
    assert!(core.enabled());
    assert!(core.default_enabled());
    assert_eq!(core.role(), PluginRole::Core);
    assert_eq!(core.state(), PluginRuntimeState::Pending);

    let external = before
        .plugin("integration.external")
        .expect("external exists");
    assert!(!external.enabled());
    assert!(!external.default_enabled());
    assert_eq!(external.override_value(), None);
    assert_eq!(external.fiber_state(), None);

    let report = runtime
        .start_default()
        .expect("required plugins mount successfully");
    assert_eq!(
        report.activated(),
        &["integration.core", "integration.agent", "integration.safe"]
    );
    assert_eq!(report.skipped(), &["integration.external"]);
    assert_eq!(report.failures().len(), 1);

    runtime
        .enable("integration.external")
        .expect("user override enables external plugin");
    let enabled = runtime.plugin("integration.external").expect("query works");
    assert!(enabled.enabled());
    assert_eq!(enabled.override_value(), Some(true));
    assert_eq!(enabled.state(), PluginRuntimeState::Mounted);
    assert_eq!(enabled.fiber_state(), Some(FiberState::Mounted));

    runtime
        .clear_override("integration.external")
        .expect("clear override restores the manifest default");
    let restored = runtime.plugin("integration.external").expect("query works");
    assert!(!restored.enabled());
    assert_eq!(restored.override_value(), None);
    assert_eq!(restored.state(), PluginRuntimeState::Disabled);
}

#[test]
fn disable_unmounts_and_removes_the_fiber_before_enable_mounts_again() {
    LIFECYCLE_DISPOSED.store(0, Ordering::SeqCst);
    let registry = PluginRegistry::new(&LIFECYCLE_DEFINITIONS).expect("definitions are valid");
    let mut runtime = CordisRuntime::new(registry);
    runtime.start_default().expect("startup works");

    let first_id = runtime
        .plugin("integration.lifecycle")
        .expect("plugin exists")
        .fiber_id()
        .expect("fiber is mounted");
    assert!(runtime.fiber_by_id(first_id).unwrap().is_some());

    runtime
        .disable("integration.lifecycle")
        .expect("disable works");
    let disabled = runtime
        .plugin("integration.lifecycle")
        .expect("query works");
    assert!(!disabled.enabled());
    assert_eq!(disabled.state(), PluginRuntimeState::Disabled);
    assert_eq!(disabled.fiber_state(), Some(FiberState::Unmounted));
    assert!(disabled.fiber().is_none());
    assert!(runtime.fiber_by_id(first_id).unwrap().is_none());
    assert_eq!(LIFECYCLE_DISPOSED.load(Ordering::SeqCst), 1);

    runtime
        .enable("integration.lifecycle")
        .expect("enable works");
    let enabled = runtime
        .plugin("integration.lifecycle")
        .expect("query works");
    assert!(enabled.enabled());
    assert_eq!(enabled.fiber_state(), Some(FiberState::Mounted));
    assert_ne!(enabled.fiber_id(), Some(first_id));
}

#[test]
fn optional_failure_is_isolated_from_healthy_plugins() {
    let registry = PluginRegistry::new(&DEFINITIONS).expect("static definitions are valid");
    let mut runtime = CordisRuntime::new(registry);
    let report = runtime
        .start_default()
        .expect("optional failure is nonfatal");

    let failure = report
        .failures()
        .iter()
        .find(|failure| failure.plugin_id() == "integration.failing")
        .expect("failed optional plugin is reported");
    assert!(!failure.required());
    assert_eq!(
        failure.failure().phase(),
        yunxi_cordis_runtime::FailurePhase::Mount
    );

    let failed = runtime
        .plugin("integration.failing")
        .expect("failed plugin remains queryable");
    assert!(failed.enabled());
    assert_eq!(failed.state(), PluginRuntimeState::Failed);
    assert_eq!(failed.fiber_state(), Some(FiberState::Failed));
    assert!(failed.failure().is_some());

    let healthy = runtime
        .plugin("integration.safe")
        .expect("healthy sibling exists");
    assert_eq!(healthy.state(), PluginRuntimeState::Mounted);
    assert!(healthy.fiber().is_some());
}

#[test]
fn required_plugins_cannot_be_disabled_and_unknown_ids_are_structured() {
    let registry = PluginRegistry::new(&DEFINITIONS).expect("static definitions are valid");
    let mut runtime = CordisRuntime::new(registry);
    assert!(matches!(
        runtime.disable("integration.core"),
        Err(RuntimeError::CorePluginCannotDisable {
            role: PluginRole::Core,
            ..
        })
    ));
    assert!(matches!(
        runtime.disable("integration.agent"),
        Err(RuntimeError::CorePluginCannotDisable {
            role: PluginRole::AgentSpine,
            ..
        })
    ));
    assert!(matches!(
        runtime.enable("integration.unknown"),
        Err(RuntimeError::UnknownPlugin { .. })
    ));

    runtime.mount("integration.external").expect("mount works");
    assert!(matches!(
        runtime.mount("integration.external"),
        Err(RuntimeError::InvalidPluginState {
            operation: "mount",
            ..
        })
    ));
}

#[test]
fn static_registry_rejects_duplicate_registration() {
    static DUPLICATES: [PluginDefinition; 2] = [
        PluginDefinition::new(
            PluginManifest::safe_optional("integration.duplicate", "One"),
            PluginFactory::new(safe_factory),
        ),
        PluginDefinition::new(
            PluginManifest::new(
                "integration.duplicate",
                "Two",
                PluginRole::Optional,
                PluginRisk::Safe,
                DefaultEnablement::Never,
            ),
            PluginFactory::new(safe_factory),
        ),
    ];
    assert!(matches!(
        PluginRegistry::new(&DUPLICATES),
        Err(RuntimeError::DuplicatePluginRegistration { .. })
    ));
}

#[test]
fn disabling_before_start_also_unmounts_a_manually_mounted_plugin() {
    PRESTART_DISPOSED.store(0, Ordering::SeqCst);
    let registry = PluginRegistry::new(&PRESTART_DEFINITIONS).expect("definitions are valid");
    let mut runtime = CordisRuntime::new(registry);
    runtime
        .mount("integration.pre-start")
        .expect("manual mount works");
    runtime
        .disable("integration.pre-start")
        .expect("pre-start disable works");
    assert!(runtime.fiber("integration.pre-start").unwrap().is_none());
    assert_eq!(PRESTART_DISPOSED.load(Ordering::SeqCst), 1);
}

#[test]
fn factory_failure_is_retained_without_blocking_a_sibling() {
    let registry =
        PluginRegistry::new(&FACTORY_FAILURE_DEFINITIONS).expect("definitions are valid");
    let mut runtime = CordisRuntime::new(registry);
    let report = runtime
        .start_default()
        .expect("optional factory failure is nonfatal");
    assert_eq!(report.failures().len(), 1);
    assert_eq!(
        report.failures()[0].failure().phase(),
        yunxi_cordis_runtime::FailurePhase::Factory
    );
    assert_eq!(
        runtime
            .plugin("integration.factory-failing")
            .unwrap()
            .fiber_state(),
        None
    );
    assert_eq!(
        runtime.plugin("integration.factory-safe").unwrap().state(),
        PluginRuntimeState::Mounted
    );
}

#[test]
fn dependency_resolution_uses_ancestor_services() {
    let registry = PluginRegistry::new(&DEPENDENCY_DEFINITIONS).expect("definitions are valid");
    let mut runtime = CordisRuntime::new(registry);
    runtime
        .root_context()
        .provide(LATE_SERVICE, 42)
        .expect("root service is available to plugin scopes");

    let report = runtime
        .start_default()
        .expect("an ancestor service resolves the dependency");

    assert!(report.failures().is_empty());
    assert_eq!(report.activated(), &["integration.late-consumer"]);
    assert_eq!(
        runtime.plugin("integration.late-consumer").unwrap().state(),
        PluginRuntimeState::Mounted
    );
}

#[test]
fn unresolved_dependencies_fail_once_without_retrying_forever() {
    let registry = PluginRegistry::new(&DEPENDENCY_DEFINITIONS).expect("definitions are valid");
    let mut runtime = CordisRuntime::new(registry);

    let report = runtime
        .start_default()
        .expect("optional failure is retained");

    assert_eq!(report.failures().len(), 1);
    assert_eq!(
        report.failures()[0].failure().phase(),
        yunxi_cordis_runtime::FailurePhase::Mount
    );
    assert_eq!(
        runtime.plugin("integration.late-consumer").unwrap().state(),
        PluginRuntimeState::Failed
    );
}

#[test]
fn contradictory_optional_policy_is_rejected_by_the_static_registry() {
    static INVALID: [PluginDefinition; 1] = [PluginDefinition::new(
        PluginManifest::new(
            "integration.invalid-policy",
            "Invalid policy",
            PluginRole::Optional,
            PluginRisk::External,
            DefaultEnablement::Safe,
        ),
        PluginFactory::new(external_factory),
    )];
    assert!(matches!(
        PluginRegistry::new(&INVALID),
        Err(RuntimeError::InvalidManifest { .. })
    ));
}
