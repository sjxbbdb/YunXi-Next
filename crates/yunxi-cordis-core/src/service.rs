//! Stable typed service keys and dependency declarations.

use std::any::{Any, TypeId};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::error::{CordisError, IdentifierKind};

pub const MAX_SERVICE_NAME_BYTES: usize = 128;
/// A plugin may declare only a bounded number of service dependencies.
pub const MAX_PLUGIN_DEPENDENCIES: usize = 64;

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct ServiceKey<T: 'static> {
    name: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T: 'static> ServiceKey<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn required(&self) -> ServiceDependency {
        ServiceDependency::required(self.name)
    }

    pub fn optional(&self) -> ServiceDependency {
        ServiceDependency::optional(self.name)
    }
}

impl<T: 'static> Copy for ServiceKey<T> {}

impl<T: 'static> Clone for ServiceKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDependency {
    name: String,
    required: bool,
}

impl ServiceDependency {
    pub fn required(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: true,
        }
    }

    pub fn optional(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_required(&self) -> bool {
        self.required
    }
}

#[derive(Clone)]
pub(crate) struct ServiceEntry {
    pub(crate) value: Arc<dyn Any + Send + Sync>,
    pub(crate) type_id: TypeId,
    pub(crate) type_name: &'static str,
}

pub(crate) fn validate_service_name(name: &str) -> Result<(), CordisError> {
    validate_name(name, IdentifierKind::Service)
}

pub(crate) fn validate_name(name: &str, kind: IdentifierKind) -> Result<(), CordisError> {
    if name.is_empty()
        || name.len() > MAX_SERVICE_NAME_BYTES
        || !name.is_ascii()
        || name
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(CordisError::InvalidIdentifier {
            kind,
            value: name.to_owned(),
        });
    }
    Ok(())
}
