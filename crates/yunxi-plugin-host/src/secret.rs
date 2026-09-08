//! Host-owned secret access facade for isolated plugins.
//!
//! The process host is the only component allowed to issue a reference.  A
//! plugin receives no serializable secret value from this module; callers may
//! use the one-shot reference only inside an explicitly scoped callback.  A
//! separate integration layer can use that callback at the final provider or
//! sidecar boundary where a credential must be supplied.

use std::sync::Arc;

use yunxi_kernel::PluginId as KernelPluginId;
use yunxi_secret_broker::{
    FileSecretStore, MemorySecretStore, PluginId as SecretPluginId, SecretBroker, SecretError,
    SecretKey, SecretRef,
};

const HOST_PROVIDER_KEY: &str = "provider-api-key";

#[derive(Clone)]
enum BrokerInner {
    Memory(Arc<SecretBroker<MemorySecretStore>>),
    File(Arc<SecretBroker<FileSecretStore>>),
}

/// A cloneable handle to the Host's secret broker.
///
/// Values are never included in plugin manifests, protocol messages, status
/// snapshots, or `Debug` output. The default constructor is process-local;
/// deployments can opt into an encrypted file with [`Self::from_file`].
#[derive(Clone)]
pub struct HostSecretBroker {
    inner: BrokerInner,
}

impl HostSecretBroker {
    pub fn new() -> Self {
        Self {
            inner: BrokerInner::Memory(Arc::new(SecretBroker::new(MemorySecretStore::new()))),
        }
    }

    /// Opens a durable encrypted store with a caller-managed 32-byte key.
    ///
    /// Key generation and storage deliberately remain outside this facade so
    /// the Host never writes a plaintext master key beside encrypted data.
    pub fn from_file(
        path: impl Into<std::path::PathBuf>,
        master_key: impl AsRef<[u8]>,
    ) -> Result<Self, SecretError> {
        let store = FileSecretStore::new(path, master_key).map_err(SecretError::Store)?;
        Ok(Self {
            inner: BrokerInner::File(Arc::new(SecretBroker::new(store))),
        })
    }

    /// Stores a value under a host-side key before a plugin launch.
    pub fn put(&self, key: &str, value: impl AsRef<[u8]>) -> Result<(), SecretError> {
        let key = SecretKey::new(key)?;
        match &self.inner {
            BrokerInner::Memory(broker) => broker.put(&key, value),
            BrokerInner::File(broker) => broker.put(&key, value),
        }
    }

    /// Convenience method for the model/provider boundary. The key is fixed
    /// so callers cannot accidentally expose arbitrary environment names as a
    /// plugin-facing contract.
    pub fn put_provider_credential(&self, value: impl AsRef<[u8]>) -> Result<(), SecretError> {
        self.put(HOST_PROVIDER_KEY, value)
    }

    /// Removes a Host-owned value and invalidates future references to it.
    pub fn remove(&self, key: &str) -> Result<bool, SecretError> {
        let key = SecretKey::new(key)?;
        match &self.inner {
            BrokerInner::Memory(broker) => broker.remove(&key),
            BrokerInner::File(broker) => broker.remove(&key),
        }
    }

    pub fn remove_provider_credential(&self) -> Result<bool, SecretError> {
        self.remove(HOST_PROVIDER_KEY)
    }

    pub fn issue(&self, plugin_id: &KernelPluginId, key: &str) -> Result<SecretRef, SecretError> {
        let plugin = secret_plugin_id(plugin_id)?;
        let key = SecretKey::new(key)?;
        match &self.inner {
            BrokerInner::Memory(broker) => broker.issue(&plugin, &key),
            BrokerInner::File(broker) => broker.issue(&plugin, &key),
        }
    }

    pub fn issue_provider_credential(
        &self,
        plugin_id: &KernelPluginId,
    ) -> Result<SecretRef, SecretError> {
        self.issue(plugin_id, HOST_PROVIDER_KEY)
    }

    /// Resolves a reference for exactly one matching plugin and exposes bytes
    /// only for the duration of `callback`.
    pub fn with_secret<T>(
        &self,
        reference: SecretRef,
        plugin_id: &KernelPluginId,
        callback: impl FnOnce(&[u8]) -> T,
    ) -> Result<T, SecretError> {
        let plugin = secret_plugin_id(plugin_id)?;
        match &self.inner {
            BrokerInner::Memory(broker) => broker.with_secret(reference, &plugin, callback),
            BrokerInner::File(broker) => broker.with_secret(reference, &plugin, callback),
        }
    }

    pub fn set_plugin_enabled(
        &self,
        plugin_id: &KernelPluginId,
        enabled: bool,
    ) -> Result<(), SecretError> {
        let plugin = secret_plugin_id(plugin_id)?;
        match &self.inner {
            BrokerInner::Memory(broker) => broker.set_plugin_enabled(&plugin, enabled),
            BrokerInner::File(broker) => broker.set_plugin_enabled(&plugin, enabled),
        }
    }

    pub fn is_plugin_enabled(&self, plugin_id: &KernelPluginId) -> Result<bool, SecretError> {
        let plugin = secret_plugin_id(plugin_id)?;
        match &self.inner {
            BrokerInner::Memory(broker) => broker.is_plugin_enabled(&plugin),
            BrokerInner::File(broker) => broker.is_plugin_enabled(&plugin),
        }
    }

    pub fn audit_snapshot(&self) -> Result<Vec<yunxi_secret_broker::AuditEvent>, SecretError> {
        match &self.inner {
            BrokerInner::Memory(broker) => broker.audit_snapshot(),
            BrokerInner::File(broker) => broker.audit_snapshot(),
        }
    }
}

impl Default for HostSecretBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for HostSecretBroker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostSecretBroker(REDACTED)")
    }
}

fn secret_plugin_id(plugin_id: &KernelPluginId) -> Result<SecretPluginId, SecretError> {
    SecretPluginId::new(plugin_id.as_str())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use yunxi_kernel::PluginId;

    #[test]
    fn host_broker_scopes_and_revokes_provider_credentials() {
        let broker = HostSecretBroker::new();
        let plugin = PluginId::new("yunxi.test.model").expect("plugin id");
        broker
            .put_provider_credential(b"redacted-provider-token")
            .expect("store credential");
        let reference = broker
            .issue_provider_credential(&plugin)
            .expect("issue reference");
        let length = broker
            .with_secret(reference.clone(), &plugin, |bytes| bytes.len())
            .expect("resolve reference");
        assert_eq!(length, b"redacted-provider-token".len());
        assert!(
            broker.with_secret(reference, &plugin, |_| ()).is_err(),
            "references are one-shot"
        );

        let reference = broker
            .issue_provider_credential(&plugin)
            .expect("issue second reference");
        broker
            .set_plugin_enabled(&plugin, false)
            .expect("disable plugin");
        assert!(broker.with_secret(reference, &plugin, |_| ()).is_err());
        broker
            .set_plugin_enabled(&plugin, true)
            .expect("re-enable plugin");
        assert!(broker.is_plugin_enabled(&plugin).expect("status"));
    }

    #[test]
    fn broker_debug_and_audit_do_not_contain_credential_bytes() {
        let broker = HostSecretBroker::new();
        let plugin = PluginId::new("yunxi.test.model").expect("plugin id");
        broker
            .put_provider_credential(b"never-log-this-secret")
            .expect("store credential");
        let reference = broker
            .issue_provider_credential(&plugin)
            .expect("issue reference");
        let _ = broker.with_secret(reference, &plugin, |_| ());
        assert!(!format!("{broker:?}").contains("never-log-this-secret"));
        let audit = broker.audit_snapshot().expect("audit snapshot");
        assert!(
            !audit
                .iter()
                .any(|event| event.to_string().contains("never-log-this-secret"))
        );
    }

    #[test]
    fn encrypted_host_broker_persists_rotation_and_removal() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "yunxi-host-secret-broker-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create secret test directory");
        let path = root.join("host-secrets.bin");
        let key = [17_u8; yunxi_secret_broker::MASTER_KEY_BYTES];
        let plugin = PluginId::new("yunxi.test.model").expect("plugin id");

        {
            let broker = HostSecretBroker::from_file(&path, key).expect("open durable broker");
            broker
                .put_provider_credential(b"first-token")
                .expect("persist first token");
        }
        {
            let broker = HostSecretBroker::from_file(&path, key).expect("reopen durable broker");
            let reference = broker
                .issue_provider_credential(&plugin)
                .expect("issue persisted token");
            let token = broker
                .with_secret(reference, &plugin, |bytes| bytes.to_vec())
                .expect("resolve persisted token");
            assert_eq!(token, b"first-token");
            broker
                .put_provider_credential(b"rotated-token")
                .expect("rotate token");
        }
        {
            let broker = HostSecretBroker::from_file(&path, key).expect("reopen rotated broker");
            let reference = broker
                .issue_provider_credential(&plugin)
                .expect("issue rotated token");
            let token = broker
                .with_secret(reference, &plugin, |bytes| bytes.to_vec())
                .expect("resolve rotated token");
            assert_eq!(token, b"rotated-token");
            assert!(
                broker
                    .remove_provider_credential()
                    .expect("remove provider credential")
            );
        }
        {
            let broker = HostSecretBroker::from_file(&path, key).expect("reopen empty broker");
            let reference = broker
                .issue_provider_credential(&plugin)
                .expect("references do not expose store contents");
            assert!(matches!(
                broker.with_secret(reference, &plugin, |_| ()),
                Err(SecretError::Missing)
            ));
        }

        let _ = fs::remove_dir_all(root);
    }
}
