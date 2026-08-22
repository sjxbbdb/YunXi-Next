//! Deterministic capability indexing outside the trusted process kernel.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use yunxi_kernel::{PluginId, PluginIdError};
use yunxi_protocol::{CapabilityDescriptor, CapabilityId, PluginConnectionInfo};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginRecord {
    id: PluginId,
    display_name: String,
    version: String,
    capabilities: Vec<CapabilityDescriptor>,
}

impl PluginRecord {
    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn capabilities(&self) -> &[CapabilityDescriptor] {
        &self.capabilities
    }
}

#[derive(Debug, Default)]
pub struct CapabilityCatalog {
    plugins: BTreeMap<PluginId, PluginRecord>,
    providers: BTreeMap<CapabilityId, BTreeSet<PluginId>>,
}

impl CapabilityCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_connection(
        &mut self,
        connection: &PluginConnectionInfo,
    ) -> Result<PluginId, CatalogError> {
        let id = PluginId::new(connection.plugin_id().to_string())
            .map_err(CatalogError::InvalidPluginId)?;
        self.register(
            id.clone(),
            connection.display_name(),
            connection.plugin_version(),
            connection.capabilities().to_vec(),
        )?;
        Ok(id)
    }

    pub fn register(
        &mut self,
        id: PluginId,
        display_name: impl Into<String>,
        version: impl Into<String>,
        capabilities: Vec<CapabilityDescriptor>,
    ) -> Result<(), CatalogError> {
        if self.plugins.contains_key(&id) {
            return Err(CatalogError::DuplicatePlugin { id });
        }

        let mut unique_capabilities = BTreeSet::new();
        for capability in &capabilities {
            if !unique_capabilities.insert(capability.id().clone()) {
                return Err(CatalogError::DuplicateCapability {
                    plugin_id: id,
                    capability: capability.id().clone(),
                });
            }
        }

        let record = PluginRecord {
            id: id.clone(),
            display_name: display_name.into(),
            version: version.into(),
            capabilities,
        };
        for capability in &record.capabilities {
            self.providers
                .entry(capability.id().clone())
                .or_default()
                .insert(id.clone());
        }
        self.plugins.insert(id, record);
        Ok(())
    }

    pub fn unregister(&mut self, id: &PluginId) -> Option<PluginRecord> {
        let record = self.plugins.remove(id)?;
        for capability in &record.capabilities {
            if let Some(providers) = self.providers.get_mut(capability.id()) {
                providers.remove(id);
                if providers.is_empty() {
                    self.providers.remove(capability.id());
                }
            }
        }
        Some(record)
    }

    pub fn plugin(&self, id: &PluginId) -> Option<&PluginRecord> {
        self.plugins.get(id)
    }

    pub fn providers(&self, capability: &str, version: u32) -> Vec<&PluginRecord> {
        self.providers
            .get(capability)
            .into_iter()
            .flat_map(BTreeSet::iter)
            .filter_map(|id| self.plugins.get(id))
            .filter(|plugin| {
                plugin.capabilities.iter().any(|descriptor| {
                    descriptor.id().as_str() == capability && descriptor.version() == version
                })
            })
            .collect()
    }

    pub fn resolve_unique(
        &self,
        capability: &str,
        version: u32,
    ) -> Result<&PluginRecord, CatalogError> {
        let providers = self.providers(capability, version);
        match providers.as_slice() {
            [] => Err(CatalogError::MissingCapability {
                capability: capability.to_string(),
                version,
            }),
            [provider] => Ok(provider),
            _ => Err(CatalogError::AmbiguousCapability {
                capability: capability.to_string(),
                version,
                providers: providers
                    .iter()
                    .map(|provider| provider.id().clone())
                    .collect(),
            }),
        }
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    pub fn capability_count(&self) -> usize {
        self.providers.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    InvalidPluginId(PluginIdError),
    DuplicatePlugin {
        id: PluginId,
    },
    DuplicateCapability {
        plugin_id: PluginId,
        capability: CapabilityId,
    },
    MissingCapability {
        capability: String,
        version: u32,
    },
    AmbiguousCapability {
        capability: String,
        version: u32,
        providers: Vec<PluginId>,
    },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPluginId(error) => write!(formatter, "invalid plugin id: {error}"),
            Self::DuplicatePlugin { id } => {
                write!(formatter, "plugin `{id}` is already registered")
            }
            Self::DuplicateCapability {
                plugin_id,
                capability,
            } => write!(
                formatter,
                "plugin `{plugin_id}` declares capability `{capability}` more than once"
            ),
            Self::MissingCapability {
                capability,
                version,
            } => {
                write!(
                    formatter,
                    "no plugin provides capability `{capability}@{version}`"
                )
            }
            Self::AmbiguousCapability {
                capability,
                version,
                providers,
            } => write!(
                formatter,
                "capability `{capability}@{version}` has multiple providers: {}",
                providers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl Error for CatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPluginId(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_id(value: &str) -> PluginId {
        PluginId::new(value).expect("valid plugin id")
    }

    fn capability(value: &str) -> CapabilityDescriptor {
        CapabilityDescriptor::new(value, 1).expect("valid capability")
    }

    #[test]
    fn unique_capabilities_resolve_to_their_plugin() {
        let mut catalog = CapabilityCatalog::new();
        catalog
            .register(
                plugin_id("yunxi.model.fixture"),
                "Fixture model",
                "1.0.0",
                vec![capability("model.chat")],
            )
            .expect("register plugin");

        let provider = catalog
            .resolve_unique("model.chat", 1)
            .expect("resolve capability");
        assert_eq!(provider.id().as_str(), "yunxi.model.fixture");
        assert_eq!(catalog.plugin_count(), 1);
        assert_eq!(catalog.capability_count(), 1);
    }

    #[test]
    fn multiple_providers_require_an_explicit_selection_policy() {
        let mut catalog = CapabilityCatalog::new();
        for id in ["yunxi.model.one", "yunxi.model.two"] {
            catalog
                .register(plugin_id(id), id, "1.0.0", vec![capability("model.chat")])
                .expect("register plugin");
        }

        let error = catalog
            .resolve_unique("model.chat", 1)
            .expect_err("ambiguous routing must fail");
        assert!(matches!(
            error,
            CatalogError::AmbiguousCapability { providers, .. } if providers.len() == 2
        ));
    }

    #[test]
    fn unregister_removes_only_that_plugins_routes() {
        let mut catalog = CapabilityCatalog::new();
        let first = plugin_id("yunxi.context.fixture");
        catalog
            .register(
                first.clone(),
                "Context",
                "1.0.0",
                vec![capability("context.compose")],
            )
            .expect("register context plugin");
        catalog
            .register(
                plugin_id("yunxi.memory.fixture"),
                "Memory",
                "1.0.0",
                vec![capability("memory.recall")],
            )
            .expect("register memory plugin");

        catalog.unregister(&first).expect("unregister plugin");
        assert!(catalog.providers("context.compose", 1).is_empty());
        assert_eq!(catalog.providers("memory.recall", 1).len(), 1);
    }

    #[test]
    fn incompatible_capability_versions_do_not_route() {
        let mut catalog = CapabilityCatalog::new();
        catalog
            .register(
                plugin_id("yunxi.model.v2"),
                "Future model",
                "2.0.0",
                vec![CapabilityDescriptor::new("model.chat", 2).expect("valid capability")],
            )
            .expect("register plugin");

        assert!(matches!(
            catalog.resolve_unique("model.chat", 1),
            Err(CatalogError::MissingCapability { version: 1, .. })
        ));
        assert_eq!(catalog.providers("model.chat", 2).len(), 1);
    }

    #[test]
    fn a_plugin_cannot_register_the_same_capability_twice() {
        let mut catalog = CapabilityCatalog::new();
        let duplicate = capability("model.chat");

        let error = catalog
            .register(
                plugin_id("yunxi.model.fixture"),
                "Fixture model",
                "1.0.0",
                vec![duplicate.clone(), duplicate],
            )
            .expect_err("duplicate capability must fail");

        assert!(matches!(error, CatalogError::DuplicateCapability { .. }));
        assert_eq!(catalog.plugin_count(), 0);
        assert_eq!(catalog.capability_count(), 0);
    }
}
