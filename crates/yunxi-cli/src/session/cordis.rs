//! The in-process Cordis bootstrap owned by one chat session.
//!
//! The process plugin host remains the boundary for optional capabilities.
//! This module only mounts the two trusted, statically linked foundations that
//! every session needs: the generic Cordis core and the replaceable Agent
//! spine. Keeping this bridge small makes it possible to replace the current
//! compatibility chat adapter without moving capability code into the kernel.

use yunxi_agent_spine::AgentConfig;
use yunxi_cordis_core::{Context, CordisError, Effect, Plugin, ServiceKey};
use yunxi_cordis_runtime::PluginRuntimeState;
use yunxi_cordis_runtime::{
    CordisRuntime, DefaultEnablement, PluginDefinition, PluginFactory, PluginManifest,
    PluginRegistry, PluginRisk, PluginRole, RuntimeError, RuntimeEventPage, RuntimeSnapshot,
};

pub(crate) const CORE_PLUGIN_ID: &str = "yunxi.core";
pub(crate) const AGENT_SPINE_PLUGIN_ID: &str = "yunxi.agent.spine";

const CORE_SERVICES_NAME: &str = "yunxi.core.services";
const AGENT_SPINE_SERVICES_NAME: &str = "yunxi.agent.spine.services";

/// Immutable bootstrap metadata made available to all Fiber scopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CoreServices {
    pub protocol_revision: u16,
}

/// The Agent spine's stable configuration seam. Concrete model and tool
/// implementations are deliberately absent; they remain external plugins.
#[derive(Clone, Debug)]
pub(crate) struct AgentSpineServices {
    pub config: AgentConfig,
}

pub(crate) const CORE_SERVICES: ServiceKey<CoreServices> = ServiceKey::new(CORE_SERVICES_NAME);
pub(crate) const AGENT_SPINE_SERVICES: ServiceKey<AgentSpineServices> =
    ServiceKey::new(AGENT_SPINE_SERVICES_NAME);

static DEFINITIONS: [PluginDefinition; 2] = [
    PluginDefinition::new(
        PluginManifest::new(
            CORE_PLUGIN_ID,
            "YunXi Cordis core",
            PluginRole::Core,
            PluginRisk::Safe,
            DefaultEnablement::Always,
        ),
        PluginFactory::new(core_factory),
    ),
    PluginDefinition::new(
        PluginManifest::new(
            AGENT_SPINE_PLUGIN_ID,
            "YunXi Agent spine",
            PluginRole::AgentSpine,
            PluginRisk::Safe,
            DefaultEnablement::Always,
        ),
        PluginFactory::new(agent_spine_factory),
    ),
];

pub(crate) struct CordisBridge {
    runtime: CordisRuntime,
}

impl CordisBridge {
    pub(crate) fn start() -> Result<Self, RuntimeError> {
        let registry = PluginRegistry::new(&DEFINITIONS)?;
        let mut runtime = CordisRuntime::new(registry);

        // Plugin-local services live in their Fiber scope. This immutable
        // bootstrap service is intentionally rooted so dependency resolution
        // is deterministic and independent of definition order.
        runtime.root_context().provide(
            CORE_SERVICES,
            CoreServices {
                protocol_revision: 1,
            },
        )?;
        runtime.start_default()?;
        Ok(Self { runtime })
    }

    pub(crate) fn snapshot(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        self.runtime.snapshot()
    }

    /// Return the bounded lifecycle journal owned by this session's trusted
    /// Cordis runtime. Callers receive metadata only and must use the returned
    /// cursor for incremental polling.
    pub(crate) fn events_since(&self, after_sequence: u64, limit: usize) -> RuntimeEventPage {
        self.runtime.events_since(after_sequence, limit)
    }

    pub(crate) fn ready(&self) -> bool {
        self.snapshot().is_ok_and(|snapshot| {
            snapshot
                .plugin(AGENT_SPINE_PLUGIN_ID)
                .is_some_and(|plugin| plugin.state() == PluginRuntimeState::Mounted)
        })
    }

    pub(crate) fn shutdown(&mut self) {
        let _ = self.runtime.shutdown();
    }
}

impl Drop for CordisBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn core_factory() -> Box<dyn Plugin> {
    Box::new(CorePlugin)
}

fn agent_spine_factory() -> Box<dyn Plugin> {
    Box::new(AgentSpinePlugin)
}

struct CorePlugin;

impl Plugin for CorePlugin {
    fn id(&self) -> &str {
        CORE_PLUGIN_ID
    }

    fn mount(&self, context: &Context) -> Result<(), CordisError> {
        let services = context.service(CORE_SERVICES)?;
        if services.protocol_revision != 1 {
            return Err(CordisError::PluginMountFailed {
                plugin: yunxi_cordis_core::PluginId::new(CORE_PLUGIN_ID)
                    .expect("static core plugin id is valid"),
                message: "unsupported Cordis bootstrap revision".to_owned(),
            });
        }
        context.install_effect(Effect::new(|| Ok(())))?;
        Ok(())
    }
}

struct AgentSpinePlugin;

impl Plugin for AgentSpinePlugin {
    fn id(&self) -> &str {
        AGENT_SPINE_PLUGIN_ID
    }

    fn dependencies(&self) -> Vec<yunxi_cordis_core::ServiceDependency> {
        vec![CORE_SERVICES.required()]
    }

    fn mount(&self, context: &Context) -> Result<(), CordisError> {
        let _core = context.service(CORE_SERVICES)?;
        context.provide(
            AGENT_SPINE_SERVICES,
            AgentSpineServices {
                config: AgentConfig::default(),
            },
        )?;
        let services = context.service(AGENT_SPINE_SERVICES)?;
        let _ = services.config.budget();
        context.install_effect(Effect::new(|| Ok(())))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yunxi_cordis_runtime::PluginRuntimeState;

    #[test]
    fn bridge_mounts_required_core_and_agent_spine() {
        let bridge = CordisBridge::start().expect("Cordis bridge");
        let snapshot = bridge.snapshot().expect("snapshot");
        assert!(bridge.ready());
        assert_eq!(snapshot.plugins().len(), 2);
        assert!(
            snapshot
                .plugins()
                .iter()
                .all(|plugin| plugin.state() == PluginRuntimeState::Mounted)
        );

        let events = bridge.events_since(0, 32);
        assert!(!events.events().is_empty());
        assert_eq!(events.events()[0].kind().to_string(), "startup_started");
        assert!(
            events
                .events()
                .iter()
                .any(|event| event.kind().to_string() == "plugin_mounted")
        );
    }

    #[test]
    fn bridge_shutdown_is_explicit_and_idempotent() {
        let mut bridge = CordisBridge::start().expect("Cordis bridge");
        bridge.shutdown();
        bridge.shutdown();
        assert!(bridge.snapshot().expect("snapshot").closed());
        let events = bridge.events_since(0, 32);
        assert!(
            events
                .events()
                .iter()
                .any(|event| event.kind().to_string() == "shutdown_completed")
        );
    }
}
