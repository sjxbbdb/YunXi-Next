use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use crate::{
    MAX_AUDIT_EVENTS, MAX_SECRET_REFS,
    audit::{AuditEvent, AuditOperation, AuditOutcome},
    error::{SecretError, SecretStoreError},
    model::{PluginId, SecretKey, SecretRef, SecretValue},
    store::SecretStore,
};

static NEXT_BROKER_ID: AtomicU64 = AtomicU64::new(1);

struct Grant {
    plugin: PluginId,
    key: SecretKey,
}

struct BrokerState {
    next_reference_id: u64,
    grants: std::collections::BTreeMap<u64, Grant>,
    consumed: std::collections::BTreeSet<u64>,
    enabled_plugins: std::collections::BTreeMap<PluginId, bool>,
}

/// A host-owned broker that issues and consumes plugin-scoped secret references.
pub struct SecretBroker<S> {
    broker_id: u64,
    store: S,
    state: Mutex<BrokerState>,
    audit: Mutex<Vec<AuditEvent>>,
    next_audit_sequence: AtomicU64,
}

impl<S: SecretStore> SecretBroker<S> {
    pub fn new(store: S) -> Self {
        let broker_id = NEXT_BROKER_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            broker_id: if broker_id == 0 { 1 } else { broker_id },
            store,
            state: Mutex::new(BrokerState {
                next_reference_id: 1,
                grants: std::collections::BTreeMap::new(),
                consumed: std::collections::BTreeSet::new(),
                enabled_plugins: std::collections::BTreeMap::new(),
            }),
            audit: Mutex::new(Vec::new()),
            next_audit_sequence: AtomicU64::new(1),
        }
    }

    /// Enables or disables future and already-issued access for one plugin.
    pub fn set_plugin_enabled(&self, plugin: &PluginId, enabled: bool) -> Result<(), SecretError> {
        let mut state = self.lock_state()?;
        state.enabled_plugins.insert(plugin.clone(), enabled);
        if !enabled {
            // Revoke pending grants immediately. Keeping them around would
            // let a disabled plugin consume broker capacity after re-enable.
            state.grants.retain(|_, grant| grant.plugin != *plugin);
        }
        Ok(())
    }

    pub fn is_plugin_enabled(&self, plugin: &PluginId) -> Result<bool, SecretError> {
        let state = self.lock_state()?;
        Ok(state.enabled_plugins.get(plugin).copied().unwrap_or(true))
    }

    /// Issues an opaque, one-shot reference for exactly one plugin.
    pub fn issue(&self, plugin: &PluginId, key: &SecretKey) -> Result<SecretRef, SecretError> {
        let mut state = self.lock_state()?;
        if !state.enabled_plugins.get(plugin).copied().unwrap_or(true) {
            return Err(SecretError::Denied);
        }
        if state.grants.len() >= MAX_SECRET_REFS {
            return Err(SecretError::TooManyReferences {
                max: MAX_SECRET_REFS,
            });
        }
        let reference_id = state.next_reference_id;
        if reference_id == 0 {
            return Err(SecretError::ReferenceIdExhausted);
        }
        state.next_reference_id = reference_id.checked_add(1).unwrap_or(0);
        debug_assert!(!state.grants.contains_key(&reference_id));
        state.grants.insert(
            reference_id,
            Grant {
                plugin: plugin.clone(),
                key: key.clone(),
            },
        );
        Ok(SecretRef {
            broker_id: self.broker_id,
            reference_id,
        })
    }

    /// Resolves and consumes a reference, returning a value that remains
    /// non-formattable and non-serializable to the caller.
    pub fn resolve_once(
        &self,
        secret_ref: SecretRef,
        requester: &PluginId,
    ) -> Result<SecretValue, SecretError> {
        let operation = AuditOperation::Resolve;
        let reference_id = secret_ref.reference_id;
        let grant = match self.take_grant(&secret_ref, requester) {
            Ok(grant) => grant,
            Err(error) => {
                self.record_error(operation, requester, reference_id, error);
                return Err(error);
            }
        };
        match self.store.get(&grant.key) {
            Ok(value) => {
                self.record(
                    operation,
                    requester,
                    reference_id,
                    AuditOutcome::Success,
                    Some(value.len()),
                );
                Ok(value)
            }
            Err(error) => {
                let error = map_store_error(error);
                self.record_error(operation, requester, reference_id, error);
                Err(error)
            }
        }
    }

    /// Resolves and consumes a reference while exposing bytes only inside `callback`.
    pub fn with_secret<T>(
        &self,
        secret_ref: SecretRef,
        requester: &PluginId,
        callback: impl FnOnce(&[u8]) -> T,
    ) -> Result<T, SecretError> {
        let operation = AuditOperation::Inject;
        let reference_id = secret_ref.reference_id;
        let grant = match self.take_grant(&secret_ref, requester) {
            Ok(grant) => grant,
            Err(error) => {
                self.record_error(operation, requester, reference_id, error);
                return Err(error);
            }
        };
        let value = match self.store.get(&grant.key) {
            Ok(value) => value,
            Err(error) => {
                let error = map_store_error(error);
                self.record_error(operation, requester, reference_id, error);
                return Err(error);
            }
        };
        let result = catch_unwind(AssertUnwindSafe(|| callback(value.bytes())));
        match result {
            Ok(result) => {
                self.record(
                    operation,
                    requester,
                    reference_id,
                    AuditOutcome::Success,
                    Some(value.len()),
                );
                Ok(result)
            }
            Err(payload) => {
                // The value is dropped before resuming the panic, so the
                // broker never leaves a callback-owned secret buffer alive.
                self.record(
                    operation,
                    requester,
                    reference_id,
                    AuditOutcome::CallbackPanicked,
                    None,
                );
                drop(value);
                resume_unwind(payload)
            }
        }
    }

    pub fn audit_snapshot(&self) -> Result<Vec<AuditEvent>, SecretError> {
        Ok(self
            .audit
            .lock()
            .map_err(|_| SecretError::Store(SecretStoreError::Unavailable))?
            .clone())
    }

    fn take_grant(
        &self,
        secret_ref: &SecretRef,
        requester: &PluginId,
    ) -> Result<Grant, SecretError> {
        let mut state = self.lock_state()?;
        if secret_ref.broker_id != self.broker_id {
            return Err(SecretError::InvalidReference);
        }
        if state.consumed.contains(&secret_ref.reference_id) {
            return Err(SecretError::AlreadyResolved);
        }
        let Some(grant) = state.grants.get(&secret_ref.reference_id) else {
            return Err(SecretError::InvalidReference);
        };
        if grant.plugin != *requester {
            return Err(SecretError::Denied);
        }
        let enabled = state
            .enabled_plugins
            .get(&grant.plugin)
            .copied()
            .unwrap_or(true);
        let grant = state
            .grants
            .remove(&secret_ref.reference_id)
            .expect("grant was checked while holding the state lock");
        // A disabled plugin must not regain an already-issued reference if it
        // is enabled again later. A wrong requester, by contrast, cannot
        // consume a reference it does not own.
        state.consumed.insert(secret_ref.reference_id);
        if state.consumed.len() > MAX_SECRET_REFS {
            let oldest = state.consumed.first().copied();
            if let Some(oldest) = oldest {
                state.consumed.remove(&oldest);
            }
        }
        if !enabled {
            return Err(SecretError::Denied);
        }
        Ok(grant)
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, BrokerState>, SecretError> {
        self.state
            .lock()
            .map_err(|_| SecretError::Store(SecretStoreError::Unavailable))
    }

    fn record_error(
        &self,
        operation: AuditOperation,
        plugin: &PluginId,
        reference_id: u64,
        error: SecretError,
    ) {
        let outcome = match error {
            SecretError::Missing => AuditOutcome::Missing,
            SecretError::Denied => AuditOutcome::Denied,
            SecretError::AlreadyResolved => AuditOutcome::AlreadyResolved,
            SecretError::InvalidReference => AuditOutcome::InvalidReference,
            SecretError::Store(_) => AuditOutcome::StoreError,
            _ => AuditOutcome::StoreError,
        };
        self.record(operation, plugin, reference_id, outcome, None);
    }

    fn record(
        &self,
        operation: AuditOperation,
        plugin: &PluginId,
        reference_id: u64,
        outcome: AuditOutcome,
        value_len: Option<usize>,
    ) {
        let sequence = self.next_audit_sequence.fetch_add(1, Ordering::Relaxed);
        let event = AuditEvent {
            sequence,
            operation,
            plugin: plugin.clone(),
            reference_id,
            outcome,
            value_len,
        };
        if let Ok(mut audit) = self.audit.lock() {
            audit.push(event);
            if audit.len() > MAX_AUDIT_EVENTS {
                let overflow = audit.len() - MAX_AUDIT_EVENTS;
                audit.drain(..overflow);
            }
        }
    }
}

impl<S: SecretStore> SecretBroker<S> {
    /// Inserts or replaces a host-owned value in the configured backing store.
    pub fn put(&self, key: &SecretKey, value: impl AsRef<[u8]>) -> Result<(), SecretError> {
        let value = SecretValue::new(value)?;
        self.store.put(key, value).map_err(map_store_error)
    }

    pub fn remove(&self, key: &SecretKey) -> Result<bool, SecretError> {
        self.store.remove(key).map_err(map_store_error)
    }
}

fn map_store_error(error: SecretStoreError) -> SecretError {
    match error {
        SecretStoreError::Missing => SecretError::Missing,
        SecretStoreError::TooLarge { item, max } => SecretError::TooLarge { item, max },
        other => SecretError::Store(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputField, MemorySecretStore, SecretError};

    fn fixture() -> (SecretBroker<MemorySecretStore>, PluginId, SecretKey) {
        let store = MemorySecretStore::new();
        let key = SecretKey::new("provider-token").expect("valid key");
        store
            .insert(&key, b"TOP-SECRET-VALUE")
            .expect("insert succeeds");
        let plugin = PluginId::new("model-openai").expect("valid plugin");
        (SecretBroker::new(store), plugin, key)
    }

    #[test]
    fn authorization_is_scoped_and_references_are_one_shot() {
        let (broker, plugin, key) = fixture();
        let other = PluginId::new("untrusted-plugin").expect("valid plugin");
        let reference = broker.issue(&plugin, &key).expect("issue succeeds");
        assert!(matches!(
            broker.resolve_once(reference.clone(), &other),
            Err(SecretError::Denied)
        ));
        let value = broker
            .resolve_once(reference.clone(), &plugin)
            .expect("the wrong requester cannot burn the grant");
        assert!(value.with_bytes(|bytes| bytes == b"TOP-SECRET-VALUE"));
        assert!(matches!(
            broker.resolve_once(reference, &plugin),
            Err(SecretError::AlreadyResolved)
        ));
    }

    #[test]
    fn disabled_plugin_cannot_use_an_already_issued_reference() {
        let (broker, plugin, key) = fixture();
        let reference = broker.issue(&plugin, &key).expect("issue succeeds");
        broker
            .set_plugin_enabled(&plugin, false)
            .expect("toggle succeeds");
        assert!(matches!(
            broker.resolve_once(reference, &plugin),
            Err(SecretError::InvalidReference)
        ));
    }

    #[test]
    fn callback_gets_bytes_and_value_is_not_returned_by_injection() {
        let (broker, plugin, key) = fixture();
        let reference = broker.issue(&plugin, &key).expect("issue succeeds");
        let length = broker
            .with_secret(reference, &plugin, |bytes| bytes.len())
            .expect("injection succeeds");
        assert_eq!(length, 16);
    }

    #[test]
    fn missing_secret_is_explicit_and_audit_is_metadata_only() {
        let store = MemorySecretStore::new();
        let key = SecretKey::new("missing-key").expect("valid key");
        let plugin = PluginId::new("test-plugin").expect("valid plugin");
        let broker = SecretBroker::new(store);
        let reference = broker.issue(&plugin, &key).expect("issue succeeds");
        assert!(matches!(
            broker.resolve_once(reference, &plugin),
            Err(SecretError::Missing)
        ));
        let audit = broker.audit_snapshot().expect("audit succeeds");
        assert_eq!(audit[0].outcome, AuditOutcome::Missing);
        assert_eq!(audit[0].value_len, None);
    }

    #[test]
    fn bounds_are_enforced_without_echoing_inputs() {
        let oversized = "x".repeat(crate::MAX_SECRET_VALUE_BYTES + 1);
        let error = SecretValue::new(oversized).expect_err("value must be bounded");
        assert!(error.to_string().contains("secret value"));
        assert!(!error.to_string().contains("x".repeat(32).as_str()));

        let error = PluginId::new(String::new()).expect_err("plugin must be non-empty");
        assert_eq!(
            error,
            SecretError::EmptyInput {
                field: InputField::PluginId
            }
        );
    }

    #[test]
    fn outstanding_reference_count_is_bounded() {
        let (broker, plugin, key) = fixture();
        let mut references = Vec::with_capacity(crate::MAX_SECRET_REFS);
        for _ in 0..crate::MAX_SECRET_REFS {
            references.push(broker.issue(&plugin, &key).expect("reference fits bound"));
        }
        assert!(matches!(
            broker.issue(&plugin, &key),
            Err(SecretError::TooManyReferences { .. })
        ));
        drop(references);
    }

    #[test]
    fn disabling_a_plugin_releases_pending_grant_capacity() {
        let (broker, plugin, key) = fixture();
        let references = (0..MAX_SECRET_REFS)
            .map(|_| broker.issue(&plugin, &key).expect("reference fits bound"))
            .collect::<Vec<_>>();
        broker
            .set_plugin_enabled(&plugin, false)
            .expect("disable plugin");
        broker
            .set_plugin_enabled(&plugin, true)
            .expect("re-enable plugin");

        assert!(matches!(
            broker.resolve_once(references[0].clone(), &plugin),
            Err(SecretError::InvalidReference)
        ));
        assert!(broker.issue(&plugin, &key).is_ok());
    }

    #[test]
    fn callback_panic_is_audited_and_reference_is_consumed() {
        let (broker, plugin, key) = fixture();
        let reference = broker.issue(&plugin, &key).expect("issue succeeds");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            broker
                .with_secret(reference.clone(), &plugin, |_| {
                    panic!("test callback panic")
                })
                .expect("callback enters the secret boundary");
        }));
        assert!(panic.is_err());
        assert!(matches!(
            broker.with_secret(reference, &plugin, |_| ()),
            Err(SecretError::AlreadyResolved)
        ));
        let audit = broker.audit_snapshot().expect("audit succeeds");
        assert_eq!(audit[0].outcome, AuditOutcome::CallbackPanicked);
        assert_eq!(audit[0].value_len, None);
    }

    #[test]
    fn json_debug_display_and_errors_do_not_contain_secret_bytes() {
        let secret = "TOP-SECRET-VALUE";
        let (broker, plugin, key) = fixture();
        let reference = broker.issue(&plugin, &key).expect("issue succeeds");
        let value = broker
            .resolve_once(reference, &plugin)
            .expect("resolve succeeds");
        let events = broker.audit_snapshot().expect("audit succeeds");
        let json = events[0].to_json();
        assert!(!json.contains(secret));
        assert!(!format!("{:?}", events[0]).contains(secret));
        assert!(!events[0].to_string().contains(secret));
        assert!(!value.to_string().contains(secret));
        assert!(!format!("{value:?}").contains(secret));
        assert!(!SecretError::Denied.to_string().contains(secret));
        assert!(!SecretError::Missing.to_string().contains(secret));
    }
}
