use std::fmt;

use zeroize::Zeroize;

use crate::{
    MAX_PLUGIN_ID_BYTES, MAX_SECRET_KEY_BYTES, MAX_SECRET_VALUE_BYTES,
    error::{BoundedItem, InputField, SecretError},
};

fn validate_text(
    value: String,
    field: InputField,
    item: BoundedItem,
    max: usize,
) -> Result<String, SecretError> {
    if value.is_empty() {
        return Err(SecretError::EmptyInput { field });
    }
    if value.len() > max {
        return Err(SecretError::TooLarge { item, max });
    }
    if value.chars().any(char::is_control) {
        return Err(SecretError::InvalidInput { field });
    }
    Ok(value)
}

/// A host-defined plugin identity used for authorization.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, SecretError> {
        Ok(Self(validate_text(
            value.into(),
            InputField::PluginId,
            BoundedItem::PluginId,
            MAX_PLUGIN_ID_BYTES,
        )?))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("PluginId").field(&self.0).finish()
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A host-side logical key. It identifies a secret but never contains its value.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SecretKey(String);

impl SecretKey {
    pub fn new(value: impl Into<String>) -> Result<Self, SecretError> {
        Ok(Self(validate_text(
            value.into(),
            InputField::SecretKey,
            BoundedItem::SecretKey,
            MAX_SECRET_KEY_BYTES,
        )?))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretKey(REDACTED)")
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<secret-key>")
    }
}

/// Secret bytes with no serialization or value-bearing formatting implementation.
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, SecretError> {
        let bytes = value.as_ref();
        if bytes.len() > MAX_SECRET_VALUE_BYTES {
            return Err(SecretError::TooLarge {
                item: BoundedItem::SecretValue,
                max: MAX_SECRET_VALUE_BYTES,
            });
        }
        Ok(Self(bytes.to_vec()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrows the value only for the duration of `callback`.
    pub fn with_bytes<T>(&self, callback: impl FnOnce(&[u8]) -> T) -> T {
        callback(&self.0)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl SecretValue {
    pub(crate) fn clone_value(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue(REDACTED)")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<secret-value>")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_value_debug_and_display_are_redacted() {
        let secret = "json-debug-display-secret";
        let value = SecretValue::new(secret).expect("value is within bounds");
        assert!(!format!("{value:?}").contains(secret));
        assert!(!value.to_string().contains(secret));
        assert_eq!(value.with_bytes(|bytes| bytes.len()), secret.len());
    }

    #[test]
    fn secret_ref_debug_and_display_are_redacted() {
        let reference = SecretRef {
            broker_id: 7,
            reference_id: 11,
        };
        assert!(!format!("{reference:?}").contains("7"));
        assert!(!format!("{reference:?}").contains("11"));
        assert_eq!(reference.to_string(), "<secret-ref>");
    }
}

/// An opaque, process-local, single-use reference issued by `SecretBroker`.
#[derive(Clone)]
pub struct SecretRef {
    pub(crate) broker_id: u64,
    pub(crate) reference_id: u64,
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretRef(REDACTED)")
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<secret-ref>")
    }
}
