//! A bounded, host-controlled boundary for resolving plugin secrets.
//!
//! The crate deliberately has no serialization dependency. `SecretRef` and
//! `SecretValue` are process-local types and do not implement `Serialize` or
//! `Deserialize`. Secret bytes can only be borrowed through a scoped callback.

#![forbid(unsafe_code)]

mod audit;
mod broker;
mod error;
mod model;
mod store;

pub use audit::{AuditEvent, AuditOperation, AuditOutcome};
pub use broker::SecretBroker;
pub use error::{BoundedItem, InputField, SecretError, SecretStoreError};
pub use model::{PluginId, SecretKey, SecretRef, SecretValue};
pub use store::{EnvironmentSecretStore, FileSecretStore, MemorySecretStore, SecretStore};

/// Maximum UTF-8 bytes in a plugin identifier.
pub const MAX_PLUGIN_ID_BYTES: usize = 128;
/// Maximum UTF-8 bytes in a host-side secret key.
pub const MAX_SECRET_KEY_BYTES: usize = 256;
/// Maximum UTF-8 bytes in an environment variable name.
pub const MAX_ENVIRONMENT_NAME_BYTES: usize = 256;
/// Maximum number of bytes accepted for one secret value.
pub const MAX_SECRET_VALUE_BYTES: usize = 64 * 1024;
/// Maximum number of outstanding references held by one broker.
pub const MAX_SECRET_REFS: usize = 1024;
/// Maximum number of audit records retained by one broker.
pub const MAX_AUDIT_EVENTS: usize = 1024;
/// Maximum number of entries retained by the bounded in-memory store.
pub const MAX_SECRET_STORE_ENTRIES: usize = 1024;
/// Maximum aggregate bytes retained by the bounded in-memory store.
pub const MAX_SECRET_STORE_BYTES: usize = 16 * 1024 * 1024;
/// Size of a caller-provided master key accepted by `FileSecretStore`.
pub const MASTER_KEY_BYTES: usize = 32;
/// Maximum encoded size of one persisted, authenticated snapshot.
pub const MAX_SECRET_FILE_BYTES: usize = 20 * 1024 * 1024;
