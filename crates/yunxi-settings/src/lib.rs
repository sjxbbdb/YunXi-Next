#![doc = "Bounded YunXi Next capability settings and persistence."]
#![forbid(unsafe_code)]

mod capabilities;
mod store;

pub use capabilities::{
    CAPABILITY_SETTINGS_NAMESPACE, CapabilityOverrides, CapabilitySetting, CapabilitySwitches,
};
pub use store::{
    CapabilityEdit, CapabilitySettingsError, CapabilitySettingsStore, MAX_SETTINGS_FILE_BYTES,
    SETTINGS_FILE_NAME, next_state_root,
};
