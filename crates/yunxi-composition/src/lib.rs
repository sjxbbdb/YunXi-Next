#![doc = "Bounded, pure Rust profile composition and plugin inventory primitives."]
#![forbid(unsafe_code)]

mod entry;
mod error;
mod inventory;
mod layer;
mod manifest;
mod profile;

pub use entry::{CompositionEntry, EntryError, EntryId, EntryIdError};
pub use error::CompositionError;
pub use inventory::{PluginFiberPhase, PluginInventoryEntry, PluginInventorySnapshot};
pub use layer::{ConfigLayer, LayerOperation};
pub use manifest::{DefaultEnablement, PluginManifest, PluginRisk, PluginRole};
pub use profile::{CompositionSnapshot, Profile};

/// Maximum number of entries in one effective profile.
pub const MAX_COMPOSED_ENTRIES: usize = 256;
/// Maximum operations in one config layer.
pub const MAX_LAYER_OPERATIONS: usize = 512;
/// Maximum UTF-8 bytes in a profile name.
pub const MAX_PROFILE_NAME_BYTES: usize = 64;
/// Maximum UTF-8 bytes in a layer name.
pub const MAX_LAYER_NAME_BYTES: usize = 128;
