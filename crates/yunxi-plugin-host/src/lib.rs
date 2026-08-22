#![doc = "Capability catalog for process-isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod catalog;

pub use catalog::{CapabilityCatalog, CatalogError, PluginRecord};
