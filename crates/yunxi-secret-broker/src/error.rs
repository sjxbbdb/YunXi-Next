use std::fmt;

/// The bounded input category that failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputField {
    PluginId,
    SecretKey,
    EnvironmentName,
}

impl fmt::Display for InputField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::PluginId => "plugin id",
            Self::SecretKey => "secret key",
            Self::EnvironmentName => "environment name",
        };
        formatter.write_str(name)
    }
}

/// The bounded item whose configured maximum was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundedItem {
    PluginId,
    SecretKey,
    EnvironmentName,
    SecretValue,
    References,
    StoreBytes,
    StoreEntries,
    File,
}

impl fmt::Display for BoundedItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::PluginId => "plugin id",
            Self::SecretKey => "secret key",
            Self::EnvironmentName => "environment name",
            Self::SecretValue => "secret value",
            Self::References => "secret references",
            Self::StoreBytes => "secret store bytes",
            Self::StoreEntries => "secret store entries",
            Self::File => "secret store file",
        };
        formatter.write_str(name)
    }
}

/// Errors reported by a backing secret store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretStoreError {
    Missing,
    Unavailable,
    InvalidEncoding,
    TooLarge { item: BoundedItem, max: usize },
    InvalidMasterKey { expected: usize, actual: usize },
    InvalidPath,
    Corrupt,
    Crypto,
    Io { operation: &'static str },
    RevisionExhausted,
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("secret is missing"),
            Self::Unavailable => formatter.write_str("secret store is unavailable"),
            Self::InvalidEncoding => formatter.write_str("secret has invalid encoding"),
            Self::TooLarge { item, max } => write!(formatter, "{item} exceeds {max} bytes"),
            Self::InvalidMasterKey { expected, actual } => {
                write!(
                    formatter,
                    "master key must contain {expected} bytes (received {actual})"
                )
            }
            Self::InvalidPath => formatter.write_str("secret store path is invalid"),
            Self::Corrupt => {
                formatter.write_str("secret store is corrupt or failed authentication")
            }
            Self::Crypto => formatter.write_str("secret store encryption failed"),
            Self::Io { operation } => {
                write!(formatter, "secret store {operation} operation failed")
            }
            Self::RevisionExhausted => formatter.write_str("secret store revision is exhausted"),
        }
    }
}

impl std::error::Error for SecretStoreError {}

/// Errors from reference validation, authorization, storage, or bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretError {
    InvalidReference,
    AlreadyResolved,
    Denied,
    Missing,
    Store(SecretStoreError),
    InvalidInput { field: InputField },
    EmptyInput { field: InputField },
    TooLarge { item: BoundedItem, max: usize },
    TooManyReferences { max: usize },
    ReferenceIdExhausted,
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference => formatter.write_str("invalid secret reference"),
            Self::AlreadyResolved => formatter.write_str("secret reference was already resolved"),
            Self::Denied => formatter.write_str("secret access denied"),
            Self::Missing => formatter.write_str("secret is missing"),
            Self::Store(error) => error.fmt(formatter),
            Self::InvalidInput { field } => write!(formatter, "invalid {field}"),
            Self::EmptyInput { field } => write!(formatter, "{field} must not be empty"),
            Self::TooLarge { item, max } => write!(formatter, "{item} exceeds {max} bytes"),
            Self::TooManyReferences { max } => {
                write!(formatter, "secret reference limit of {max} was reached")
            }
            Self::ReferenceIdExhausted => {
                formatter.write_str("secret reference id space was exhausted")
            }
        }
    }
}

impl std::error::Error for SecretError {}
