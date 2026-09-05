//! Compile-time plugin definitions and an immutable lookup table.

use std::collections::BTreeMap;

use yunxi_cordis_core::Plugin;

use crate::error::RuntimeError;
use crate::manifest::PluginManifest;

pub const MAX_STATIC_PLUGINS: usize = 256;

/// Function-pointer factory used by a static plugin definition.
pub type PluginFactoryFn = fn() -> Box<dyn Plugin>;

#[derive(Clone, Copy, Debug)]
pub struct PluginFactory {
    create: PluginFactoryFn,
}

impl PluginFactory {
    pub const fn new(create: PluginFactoryFn) -> Self {
        Self { create }
    }

    pub fn instantiate(self) -> Box<dyn Plugin> {
        (self.create)()
    }
}

/// One immutable entry in a static registry.
#[derive(Clone, Copy, Debug)]
pub struct PluginDefinition {
    manifest: PluginManifest,
    factory: PluginFactory,
}

impl PluginDefinition {
    pub const fn new(manifest: PluginManifest, factory: PluginFactory) -> Self {
        Self { manifest, factory }
    }

    pub const fn manifest(self) -> PluginManifest {
        self.manifest
    }

    pub const fn factory(self) -> PluginFactory {
        self.factory
    }

    pub const fn id(self) -> &'static str {
        self.manifest.id()
    }

    pub fn validate(self) -> Result<(), RuntimeError> {
        self.manifest
            .validate()
            .map_err(|source| RuntimeError::InvalidManifest {
                plugin_id: self.id().to_owned(),
                source,
            })
    }
}

/// An immutable index over a `&'static` definition slice.
///
/// There is deliberately no `register` method. Adding a plugin means adding
/// a definition to the static slice and rebuilding the binary.
pub struct PluginRegistry {
    definitions: &'static [PluginDefinition],
    indexes: BTreeMap<&'static str, usize>,
}

impl PluginRegistry {
    pub fn new(definitions: &'static [PluginDefinition]) -> Result<Self, RuntimeError> {
        if definitions.len() > MAX_STATIC_PLUGINS {
            return Err(RuntimeError::RegistryTooLarge {
                count: definitions.len(),
                maximum: MAX_STATIC_PLUGINS,
            });
        }

        let mut indexes = BTreeMap::new();
        for (index, definition) in definitions.iter().enumerate() {
            definition.validate()?;
            if indexes.insert(definition.id(), index).is_some() {
                return Err(RuntimeError::DuplicatePluginRegistration {
                    plugin_id: definition.id().to_owned(),
                });
            }
        }

        Ok(Self {
            definitions,
            indexes,
        })
    }

    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    pub fn definitions(&self) -> &'static [PluginDefinition] {
        self.definitions
    }

    pub fn get(&self, plugin_id: &str) -> Option<&'static PluginDefinition> {
        self.indexes
            .get(plugin_id)
            .map(|index| &self.definitions[*index])
    }

    pub fn require(&self, plugin_id: &str) -> Result<&'static PluginDefinition, RuntimeError> {
        self.get(plugin_id)
            .ok_or_else(|| RuntimeError::UnknownPlugin {
                plugin_id: bounded_plugin_id(plugin_id),
            })
    }

    pub(crate) fn index_of(&self, plugin_id: &str) -> Result<usize, RuntimeError> {
        self.indexes
            .get(plugin_id)
            .copied()
            .ok_or_else(|| RuntimeError::UnknownPlugin {
                plugin_id: bounded_plugin_id(plugin_id),
            })
    }

    pub(crate) fn definition_at(&self, index: usize) -> &'static PluginDefinition {
        &self.definitions[index]
    }
}

fn bounded_plugin_id(plugin_id: &str) -> String {
    if plugin_id.len() <= crate::manifest::MAX_PLUGIN_ID_BYTES {
        return plugin_id.to_owned();
    }
    let mut end = crate::manifest::MAX_PLUGIN_ID_BYTES.saturating_sub(3);
    while end > 0 && !plugin_id.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &plugin_id[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginManifest, PluginRisk};

    struct EmptyPlugin;

    impl Plugin for EmptyPlugin {
        fn id(&self) -> &str {
            "registry.empty"
        }

        fn mount(
            &self,
            _context: &yunxi_cordis_core::Context,
        ) -> Result<(), yunxi_cordis_core::CordisError> {
            Ok(())
        }
    }

    fn empty_factory() -> Box<dyn Plugin> {
        Box::new(EmptyPlugin)
    }

    static DUPLICATES: [PluginDefinition; 2] = [
        PluginDefinition::new(
            PluginManifest::safe_optional("registry.duplicate", "One"),
            PluginFactory::new(empty_factory),
        ),
        PluginDefinition::new(
            PluginManifest::new(
                "registry.duplicate",
                "Two",
                crate::PluginRole::Optional,
                PluginRisk::Safe,
                crate::DefaultEnablement::Never,
            ),
            PluginFactory::new(empty_factory),
        ),
    ];

    static VALID: [PluginDefinition; 1] = [PluginDefinition::new(
        PluginManifest::safe_optional("registry.valid", "Valid"),
        PluginFactory::new(empty_factory),
    )];

    #[test]
    fn duplicate_ids_are_rejected() {
        assert!(matches!(
            PluginRegistry::new(&DUPLICATES),
            Err(RuntimeError::DuplicatePluginRegistration { .. })
        ));
    }

    #[test]
    fn unknown_plugin_errors_do_not_echo_unbounded_input() {
        let registry = PluginRegistry::new(&VALID).unwrap();
        let unknown = "x".repeat(crate::manifest::MAX_PLUGIN_ID_BYTES * 8);
        let error = registry.require(&unknown).unwrap_err();
        assert!(
            matches!(error, RuntimeError::UnknownPlugin { ref plugin_id } if plugin_id.len() <= crate::manifest::MAX_PLUGIN_ID_BYTES)
        );
    }
}
