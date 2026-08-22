//! Errors returned while validating or applying composition data.

use std::error::Error;
use std::fmt;

use crate::entry::{EntryError, EntryId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompositionError {
    Entry(EntryError),
    InvalidProfileName {
        value: String,
    },
    InvalidLayerName {
        value: String,
    },
    TooManyOperations {
        count: usize,
        maximum: usize,
    },
    TooManyEntries {
        count: usize,
        maximum: usize,
    },
    DuplicateLayer {
        name: String,
    },
    DuplicateEntry {
        id: EntryId,
        layer: String,
    },
    MissingEntry {
        id: EntryId,
        layer: String,
        operation: &'static str,
    },
    ReplacementIdMismatch {
        target: EntryId,
        replacement: EntryId,
    },
}

impl fmt::Display for CompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entry(error) => error.fmt(formatter),
            Self::InvalidProfileName { value } => {
                write!(formatter, "invalid profile name `{value}`")
            }
            Self::InvalidLayerName { value } => {
                write!(formatter, "invalid layer name `{value}`")
            }
            Self::TooManyOperations { count, maximum } => write!(
                formatter,
                "config layer contains {count} operations; maximum is {maximum}"
            ),
            Self::TooManyEntries { count, maximum } => write!(
                formatter,
                "composition contains {count} entries; maximum is {maximum}"
            ),
            Self::DuplicateLayer { name } => {
                write!(
                    formatter,
                    "composition layer `{name}` is already registered"
                )
            }
            Self::DuplicateEntry { id, layer } => {
                write!(formatter, "layer `{layer}` inserts duplicate entry `{id}`")
            }
            Self::MissingEntry {
                id,
                layer,
                operation,
            } => write!(
                formatter,
                "layer `{layer}` cannot {operation} missing entry `{id}`"
            ),
            Self::ReplacementIdMismatch {
                target,
                replacement,
            } => write!(
                formatter,
                "replacement entry `{replacement}` does not match target `{target}`"
            ),
        }
    }
}

impl Error for CompositionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Entry(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EntryError> for CompositionError {
    fn from(error: EntryError) -> Self {
        Self::Entry(error)
    }
}

pub(crate) fn validate_label(
    value: &str,
    maximum: usize,
    profile: bool,
) -> Result<(), CompositionError> {
    let valid = !value.trim().is_empty()
        && value.len() <= maximum
        && value.is_ascii()
        && value
            .chars()
            .all(|character| !character.is_control() && !character.is_whitespace());
    if valid {
        return Ok(());
    }
    if profile {
        Err(CompositionError::InvalidProfileName {
            value: value.to_string(),
        })
    } else {
        Err(CompositionError::InvalidLayerName {
            value: value.to_string(),
        })
    }
}
