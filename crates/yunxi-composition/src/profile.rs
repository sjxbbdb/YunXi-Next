//! Ordered profile, bundle, and overlay composition.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::error::validate_label;
use crate::{
    CompositionEntry, CompositionError, ConfigLayer, EntryId, LayerOperation, MAX_COMPOSED_ENTRIES,
    MAX_PROFILE_NAME_BYTES,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    name: String,
    bundles: Vec<ConfigLayer>,
    profile_layer: Option<ConfigLayer>,
    overlays: Vec<ConfigLayer>,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Result<Self, CompositionError> {
        let name = name.into();
        validate_label(&name, MAX_PROFILE_NAME_BYTES, true)?;
        Ok(Self {
            name,
            bundles: Vec::new(),
            profile_layer: None,
            overlays: Vec::new(),
        })
    }

    pub fn add_bundle(&mut self, layer: ConfigLayer) -> Result<(), CompositionError> {
        self.register_layer_name(layer.name())?;
        self.bundles.push(layer);
        Ok(())
    }

    pub fn set_profile_layer(&mut self, layer: ConfigLayer) -> Result<(), CompositionError> {
        if self
            .bundles
            .iter()
            .chain(self.overlays.iter())
            .chain(self.profile_layer.iter())
            .any(|existing| existing.name() == layer.name())
        {
            return Err(CompositionError::DuplicateLayer {
                name: layer.name().to_string(),
            });
        }
        self.profile_layer = Some(layer);
        Ok(())
    }

    pub fn add_overlay(&mut self, layer: ConfigLayer) -> Result<(), CompositionError> {
        self.register_layer_name(layer.name())?;
        self.overlays.push(layer);
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn bundles(&self) -> &[ConfigLayer] {
        &self.bundles
    }

    pub fn profile_layer(&self) -> Option<&ConfigLayer> {
        self.profile_layer.as_ref()
    }

    pub fn overlays(&self) -> &[ConfigLayer] {
        &self.overlays
    }

    pub fn compose(&self) -> Result<CompositionSnapshot, CompositionError> {
        let mut entries = Vec::new();
        let mut applied_layers = Vec::new();
        for layer in &self.bundles {
            apply_layer(layer, &mut entries)?;
            applied_layers.push(layer.name().to_string());
        }
        if let Some(layer) = &self.profile_layer {
            apply_layer(layer, &mut entries)?;
            applied_layers.push(layer.name().to_string());
        }
        for layer in &self.overlays {
            apply_layer(layer, &mut entries)?;
            applied_layers.push(layer.name().to_string());
        }
        Ok(CompositionSnapshot {
            profile_name: self.name.clone(),
            entries,
            applied_layers,
        })
    }

    fn register_layer_name(&self, name: &str) -> Result<(), CompositionError> {
        let duplicate = self
            .bundles
            .iter()
            .chain(self.profile_layer.iter())
            .chain(self.overlays.iter())
            .any(|layer| layer.name() == name);
        if duplicate {
            return Err(CompositionError::DuplicateLayer {
                name: name.to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompositionSnapshot {
    profile_name: String,
    entries: Vec<CompositionEntry>,
    applied_layers: Vec<String>,
}

impl CompositionSnapshot {
    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    pub fn entries(&self) -> &[CompositionEntry] {
        &self.entries
    }

    pub fn applied_layers(&self) -> &[String] {
        &self.applied_layers
    }

    pub fn entry(&self, id: &EntryId) -> Option<&CompositionEntry> {
        self.entries.iter().find(|entry| entry.id() == id)
    }
}

fn apply_layer(
    layer: &ConfigLayer,
    entries: &mut Vec<CompositionEntry>,
) -> Result<(), CompositionError> {
    for operation in layer.operations() {
        match operation {
            LayerOperation::Insert { entries: additions } => {
                let projected = entries.len().saturating_add(additions.len());
                if projected > MAX_COMPOSED_ENTRIES {
                    return Err(CompositionError::TooManyEntries {
                        count: projected,
                        maximum: MAX_COMPOSED_ENTRIES,
                    });
                }
                let mut inserted = BTreeSet::new();
                for entry in additions {
                    if !inserted.insert(entry.id().clone())
                        || entries.iter().any(|current| current.id() == entry.id())
                    {
                        return Err(CompositionError::DuplicateEntry {
                            id: entry.id().clone(),
                            layer: layer.name().to_string(),
                        });
                    }
                }
                entries.extend(additions.iter().cloned());
            }
            LayerOperation::Replace { id, entry } => {
                if id != entry.id() {
                    return Err(CompositionError::ReplacementIdMismatch {
                        target: id.clone(),
                        replacement: entry.id().clone(),
                    });
                }
                let index = find_entry(entries, id, layer, operation)?;
                entries[index] = entry.clone();
            }
            LayerOperation::Enable { id } => {
                let index = find_entry(entries, id, layer, operation)?;
                entries[index].set_enabled(true);
            }
            LayerOperation::Disable { id } => {
                let index = find_entry(entries, id, layer, operation)?;
                entries[index].set_enabled(false);
            }
            LayerOperation::Remove { id } => {
                let index = find_entry(entries, id, layer, operation)?;
                entries.remove(index);
            }
        }
    }
    Ok(())
}

fn find_entry(
    entries: &[CompositionEntry],
    id: &EntryId,
    layer: &ConfigLayer,
    operation: &LayerOperation,
) -> Result<usize, CompositionError> {
    entries
        .iter()
        .position(|entry| entry.id() == id)
        .ok_or_else(|| CompositionError::MissingEntry {
            id: id.clone(),
            layer: layer.name().to_string(),
            operation: operation.name(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(value: &str) -> EntryId {
        EntryId::new(value).expect("valid id")
    }

    fn entry(id: &str, module: &str) -> CompositionEntry {
        CompositionEntry::new(id, module).expect("valid entry")
    }

    #[test]
    fn bundle_profile_and_overlay_layers_apply_in_order() {
        let mut profile = Profile::new("web").expect("profile");
        let mut base = ConfigLayer::new("base").expect("base");
        base.insert(vec![entry("reader", "fixture.reader")])
            .expect("insert");
        profile.add_bundle(base).expect("bundle");

        let mut profile_layer = ConfigLayer::new("profile").expect("profile layer");
        profile_layer
            .replace(
                id("reader"),
                entry("reader", "fixture.reader.v2")
                    .with_config(json!({ "source": "profile" }))
                    .expect("config"),
            )
            .expect("replace");
        profile
            .set_profile_layer(profile_layer)
            .expect("profile layer");

        let mut user = ConfigLayer::new("user").expect("user");
        user.disable(id("reader")).expect("disable");
        profile.add_overlay(user).expect("overlay");

        let snapshot = profile.compose().expect("compose");
        assert_eq!(snapshot.entries().len(), 1);
        assert_eq!(snapshot.entries()[0].module_name(), "fixture.reader.v2");
        assert!(!snapshot.entries()[0].enabled());
        assert_eq!(snapshot.applied_layers(), ["base", "profile", "user"]);
    }

    #[test]
    fn missing_targets_fail_loudly_instead_of_mutating_partial_state() {
        let mut profile = Profile::new("headless").expect("profile");
        let mut layer = ConfigLayer::new("user").expect("layer");
        layer.remove(id("missing")).expect("operation");
        profile.add_overlay(layer).expect("overlay");
        let error = profile.compose().expect_err("missing target must fail");
        assert!(matches!(
            error,
            CompositionError::MissingEntry {
                operation: "remove",
                ..
            }
        ));
    }

    #[test]
    fn duplicate_layers_are_rejected_before_boot() {
        let mut profile = Profile::new("web").expect("profile");
        profile
            .add_bundle(ConfigLayer::new("base").expect("layer"))
            .expect("first layer");
        let error = profile
            .add_overlay(ConfigLayer::new("base").expect("layer"))
            .expect_err("duplicate layer");
        assert!(matches!(error, CompositionError::DuplicateLayer { .. }));

        let mut profile = Profile::new("second").expect("profile");
        profile
            .set_profile_layer(ConfigLayer::new("profile").expect("layer"))
            .expect("first profile layer");
        let error = profile
            .set_profile_layer(ConfigLayer::new("profile").expect("layer"))
            .expect_err("duplicate profile layer");
        assert!(matches!(error, CompositionError::DuplicateLayer { .. }));
    }
}
