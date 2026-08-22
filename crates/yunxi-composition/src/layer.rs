//! Serializable operations that make up one composition layer.

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::validate_label;
use crate::{
    CompositionEntry, CompositionError, EntryId, MAX_LAYER_NAME_BYTES, MAX_LAYER_OPERATIONS,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum LayerOperation {
    Insert {
        entries: Vec<CompositionEntry>,
    },
    Replace {
        id: EntryId,
        entry: CompositionEntry,
    },
    Enable {
        id: EntryId,
    },
    Disable {
        id: EntryId,
    },
    Remove {
        id: EntryId,
    },
}

impl LayerOperation {
    pub fn insert(entries: Vec<CompositionEntry>) -> Self {
        Self::Insert { entries }
    }

    pub fn replace(id: EntryId, entry: CompositionEntry) -> Self {
        Self::Replace { id, entry }
    }

    pub fn enable(id: EntryId) -> Self {
        Self::Enable { id }
    }

    pub fn disable(id: EntryId) -> Self {
        Self::Disable { id }
    }

    pub fn remove(id: EntryId) -> Self {
        Self::Remove { id }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Insert { .. } => "insert",
            Self::Replace { .. } => "replace",
            Self::Enable { .. } => "enable",
            Self::Disable { .. } => "disable",
            Self::Remove { .. } => "remove",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigLayer {
    name: String,
    operations: Vec<LayerOperation>,
}

impl ConfigLayer {
    pub fn new(name: impl Into<String>) -> Result<Self, CompositionError> {
        let name = name.into();
        validate_label(&name, MAX_LAYER_NAME_BYTES, false)?;
        Ok(Self {
            name,
            operations: Vec::new(),
        })
    }

    pub fn from_operations(
        name: impl Into<String>,
        operations: Vec<LayerOperation>,
    ) -> Result<Self, CompositionError> {
        let mut layer = Self::new(name)?;
        for operation in operations {
            layer.push(operation)?;
        }
        Ok(layer)
    }

    pub fn push(&mut self, operation: LayerOperation) -> Result<(), CompositionError> {
        let count = self.operations.len().saturating_add(1);
        if count > MAX_LAYER_OPERATIONS {
            return Err(CompositionError::TooManyOperations {
                count,
                maximum: MAX_LAYER_OPERATIONS,
            });
        }
        self.operations.push(operation);
        Ok(())
    }

    pub fn insert(&mut self, entries: Vec<CompositionEntry>) -> Result<(), CompositionError> {
        self.push(LayerOperation::insert(entries))
    }

    pub fn replace(
        &mut self,
        id: EntryId,
        entry: CompositionEntry,
    ) -> Result<(), CompositionError> {
        self.push(LayerOperation::replace(id, entry))
    }

    pub fn enable(&mut self, id: EntryId) -> Result<(), CompositionError> {
        self.push(LayerOperation::enable(id))
    }

    pub fn disable(&mut self, id: EntryId) -> Result<(), CompositionError> {
        self.push(LayerOperation::disable(id))
    }

    pub fn remove(&mut self, id: EntryId) -> Result<(), CompositionError> {
        self.push(LayerOperation::remove(id))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn operations(&self) -> &[LayerOperation] {
        &self.operations
    }
}

impl<'de> Deserialize<'de> for ConfigLayer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireLayer {
            name: String,
            operations: Vec<LayerOperation>,
        }

        let wire = WireLayer::deserialize(deserializer)?;
        Self::from_operations(wire.name, wire.operations).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> EntryId {
        EntryId::new(value).expect("valid id")
    }

    #[test]
    fn layer_serialization_keeps_operation_tags_explicit() {
        let entry = CompositionEntry::new("reader", "fixture.reader").expect("entry");
        let layer = ConfigLayer::from_operations(
            "bundle",
            vec![
                LayerOperation::insert(vec![entry]),
                LayerOperation::disable(id("reader")),
            ],
        )
        .expect("layer");
        let json = serde_json::to_string(&layer).expect("serialize layer");
        assert!(json.contains("\"op\":\"insert\""));
        assert_eq!(
            serde_json::from_str::<ConfigLayer>(&json).expect("round trip"),
            layer
        );
    }

    #[test]
    fn operation_count_is_bounded() {
        let mut layer = ConfigLayer::new("bounded").expect("layer");
        for index in 0..MAX_LAYER_OPERATIONS {
            layer
                .push(LayerOperation::enable(id(&format!("entry-{index}"))))
                .expect("within limit");
        }
        let error = layer
            .push(LayerOperation::enable(id("overflow")))
            .expect_err("limit must hold");
        assert!(matches!(error, CompositionError::TooManyOperations { .. }));
    }
}
