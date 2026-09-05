//! dsh Web plugin-inventory projection.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    CompositionEntry, CompositionSnapshot, EntryId, PluginManifest, PluginRisk, PluginRole,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginFiberPhase {
    Disabled,
    Pending,
    Loading,
    Active,
    Failed,
    Unloading,
}

impl fmt::Display for PluginFiberPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Disabled => "disabled",
            Self::Pending => "pending",
            Self::Loading => "loading",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Unloading => "unloading",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PluginInventoryEntry {
    #[serde(rename = "entryId")]
    entry_id: EntryId,
    #[serde(rename = "moduleName")]
    module_name: String,
    enabled: bool,
    #[serde(rename = "fiberPhase")]
    fiber_phase: Option<PluginFiberPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<PluginManifest>,
}

impl PluginInventoryEntry {
    pub fn entry_id(&self) -> &EntryId {
        &self.entry_id
    }

    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn fiber_phase(&self) -> Option<PluginFiberPhase> {
        self.fiber_phase
    }

    pub fn runtime_status(&self) -> Option<PluginFiberPhase> {
        self.fiber_phase
    }

    pub fn manifest(&self) -> Option<PluginManifest> {
        self.manifest
    }

    pub fn role(&self) -> Option<PluginRole> {
        self.manifest.map(PluginManifest::role)
    }

    pub fn risk(&self) -> Option<PluginRisk> {
        self.manifest.map(PluginManifest::risk)
    }

    pub fn default_enabled(&self) -> Option<bool> {
        self.manifest.map(PluginManifest::default_enabled)
    }

    pub(crate) fn from_entry(
        entry: &CompositionEntry,
        fiber_phase: Option<PluginFiberPhase>,
    ) -> Self {
        let manifest = entry.manifest();
        let fiber_phase = fiber_phase.or_else(|| {
            manifest.map(|_| {
                if entry.enabled() {
                    PluginFiberPhase::Pending
                } else {
                    PluginFiberPhase::Disabled
                }
            })
        });
        Self {
            entry_id: entry.id().clone(),
            module_name: entry.module_name().to_string(),
            enabled: entry.enabled(),
            fiber_phase,
            manifest,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PluginInventorySnapshot {
    entries: Vec<PluginInventoryEntry>,
}

impl PluginInventorySnapshot {
    pub fn entries(&self) -> &[PluginInventoryEntry] {
        &self.entries
    }

    pub fn from_composition(snapshot: &CompositionSnapshot) -> Self {
        Self::with_phases(snapshot, &BTreeMap::new())
    }

    pub fn with_phases(
        snapshot: &CompositionSnapshot,
        phases: &BTreeMap<EntryId, PluginFiberPhase>,
    ) -> Self {
        Self {
            entries: snapshot
                .entries()
                .iter()
                .map(|entry| {
                    PluginInventoryEntry::from_entry(entry, phases.get(entry.id()).copied())
                })
                .collect(),
        }
    }
}

impl CompositionSnapshot {
    pub fn inventory(&self) -> PluginInventorySnapshot {
        PluginInventorySnapshot::from_composition(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfigLayer, EntryId, LayerOperation, Profile};

    #[test]
    fn inventory_matches_the_browser_wire_shape() {
        let entry = CompositionEntry::new("tool-shell", "yunxi.tool.shell")
            .expect("entry")
            .with_enabled(false);
        let mut layer = ConfigLayer::new("base").expect("layer");
        layer
            .push(LayerOperation::insert(vec![entry]))
            .expect("insert");
        let mut profile = Profile::new("web").expect("profile");
        profile.add_bundle(layer).expect("bundle");
        let snapshot = profile.compose().expect("compose");
        let inventory = snapshot.inventory();
        let json = serde_json::to_value(&inventory).expect("serialize inventory");
        assert_eq!(
            json,
            serde_json::json!({
                "entries": [{
                    "entryId": "tool-shell",
                    "moduleName": "yunxi.tool.shell",
                    "enabled": false,
                    "fiberPhase": null
                }]
            })
        );
    }

    #[test]
    fn lifecycle_phases_are_optional_and_keyed_by_stable_entry_id() {
        let mut layer = ConfigLayer::new("base").expect("layer");
        layer
            .insert(vec![
                CompositionEntry::new("model", "yunxi.model").expect("entry"),
            ])
            .expect("insert");
        let mut profile = Profile::new("web").expect("profile");
        profile.add_bundle(layer).expect("bundle");
        let snapshot = profile.compose().expect("compose");
        let mut phases = BTreeMap::new();
        phases.insert(EntryId::new("model").expect("id"), PluginFiberPhase::Active);
        let inventory = PluginInventorySnapshot::with_phases(&snapshot, &phases);
        assert_eq!(
            inventory.entries()[0].fiber_phase(),
            Some(PluginFiberPhase::Active)
        );
    }

    #[test]
    fn manifest_entries_expose_role_risk_default_and_disabled_status() {
        let entry = CompositionEntry::new_with_manifest(
            "tool-shell",
            "yunxi.tool.shell",
            PluginManifest::optional(PluginRisk::External),
        )
        .expect("entry");
        let mut layer = ConfigLayer::new("base").expect("layer");
        layer.insert(vec![entry]).expect("insert");
        let mut profile = Profile::new("headless").expect("profile");
        profile.add_bundle(layer).expect("bundle");

        let inventory = profile.compose().expect("compose").inventory();
        let item = &inventory.entries()[0];
        assert_eq!(item.role(), Some(PluginRole::Optional));
        assert_eq!(item.risk(), Some(PluginRisk::External));
        assert_eq!(item.default_enabled(), Some(false));
        assert_eq!(item.runtime_status(), Some(PluginFiberPhase::Disabled));
    }
}
