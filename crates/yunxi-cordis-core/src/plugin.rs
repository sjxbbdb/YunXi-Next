//! Plugin declarations and scoped fiber lifecycle.

use std::error::Error;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::context::{Context, ContextInner, ScopeId};
use crate::error::CordisError;
use crate::service::{MAX_PLUGIN_DEPENDENCIES, ServiceDependency};

pub const MAX_PLUGIN_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, PluginIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PluginIdError::Empty);
        }
        if value.len() > MAX_PLUGIN_ID_BYTES {
            return Err(PluginIdError::TooLong {
                length: value.len(),
                maximum: MAX_PLUGIN_ID_BYTES,
            });
        }
        for (index, character) in value.char_indices() {
            if !character.is_ascii() || character.is_control() || character.is_whitespace() {
                return Err(PluginIdError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginIdError {
    Empty,
    TooLong { length: usize, maximum: usize },
    InvalidCharacter { index: usize, character: char },
}

impl fmt::Display for PluginIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("plugin id cannot be empty"),
            Self::TooLong { length, maximum } => {
                write!(
                    formatter,
                    "plugin id is {length} bytes; maximum is {maximum}"
                )
            }
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "plugin id contains unsupported character `{character}` at byte {index}"
            ),
        }
    }
}

impl Error for PluginIdError {}

pub trait Plugin: Send + Sync + 'static {
    fn id(&self) -> &str;

    fn dependencies(&self) -> Vec<ServiceDependency> {
        Vec::new()
    }

    fn mount(&self, context: &Context) -> Result<(), CordisError>;
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FiberId(u64);

impl FiberId {
    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for FiberId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "fiber-{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FiberState {
    Mounting,
    Mounted,
    Failed,
    Unmounting,
    Unmounted,
}

impl fmt::Display for FiberState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mounting => "mounting",
            Self::Mounted => "mounted",
            Self::Failed => "failed",
            Self::Unmounting => "unmounting",
            Self::Unmounted => "unmounted",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberSnapshot {
    id: FiberId,
    plugin_id: PluginId,
    scope_id: ScopeId,
    state: FiberState,
}

impl FiberSnapshot {
    pub fn id(&self) -> FiberId {
        self.id
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn scope_id(&self) -> ScopeId {
        self.scope_id
    }

    pub fn state(&self) -> FiberState {
        self.state
    }
}

pub(crate) struct FiberRecord {
    id: FiberId,
    plugin_id: PluginId,
    scope_id: ScopeId,
    pub(crate) scope: Context,
    state: Mutex<FiberState>,
}

impl FiberRecord {
    fn new(id: FiberId, plugin_id: PluginId, scope: Context) -> Self {
        Self {
            id,
            plugin_id,
            scope_id: scope.id(),
            scope,
            state: Mutex::new(FiberState::Mounting),
        }
    }

    pub(crate) fn id(&self) -> FiberId {
        self.id
    }

    pub(crate) fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub(crate) fn state(&self) -> Result<FiberState, CordisError> {
        self.state
            .lock()
            .map(|state| *state)
            .map_err(|_| CordisError::Poisoned { component: "fiber" })
    }

    pub(crate) fn set_state(&self, state: FiberState) -> Result<(), CordisError> {
        let mut current = self
            .state
            .lock()
            .map_err(|_| CordisError::Poisoned { component: "fiber" })?;
        *current = state;
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Result<FiberSnapshot, CordisError> {
        Ok(FiberSnapshot {
            id: self.id,
            plugin_id: self.plugin_id.clone(),
            scope_id: self.scope_id,
            state: self.state()?,
        })
    }
}

pub struct Fiber {
    record: Arc<FiberRecord>,
    parent: std::sync::Weak<ContextInner>,
    _plugin: Arc<dyn Plugin>,
}

impl Fiber {
    pub fn id(&self) -> FiberId {
        self.record.id()
    }

    pub fn plugin_id(&self) -> &PluginId {
        self.record.plugin_id()
    }

    pub fn scope_id(&self) -> ScopeId {
        self.record.scope_id
    }

    pub fn state(&self) -> Result<FiberState, CordisError> {
        self.record.state()
    }

    pub fn context(&self) -> Context {
        self.record.scope.clone()
    }

    pub fn snapshot(&self) -> Result<FiberSnapshot, CordisError> {
        self.record.snapshot()
    }

    pub fn unmount(&self) -> Result<(), CordisError> {
        match self.record.state()? {
            FiberState::Unmounted => return Ok(()),
            FiberState::Failed => {
                self.record.set_state(FiberState::Unmounted)?;
            }
            FiberState::Mounting | FiberState::Unmounting => {
                return Err(CordisError::InvalidFiberState {
                    fiber: self.id(),
                    state: self.record.state()?,
                    operation: "unmount",
                });
            }
            FiberState::Mounted => {
                self.record.set_state(FiberState::Unmounting)?;
                let result = self.record.scope.dispose();
                self.record.set_state(FiberState::Unmounted)?;
                if let Some(parent) = self.parent.upgrade() {
                    Context { inner: parent }.unregister_fiber(self.id());
                }
                return result;
            }
        }
        if let Some(parent) = self.parent.upgrade() {
            Context { inner: parent }.unregister_fiber(self.id());
        }
        Ok(())
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        let _ignored = self.unmount();
    }
}

impl Context {
    pub fn mount<P>(&self, plugin: P) -> Result<Fiber, CordisError>
    where
        P: Plugin,
    {
        let plugin: Arc<dyn Plugin> = Arc::new(plugin);
        let plugin_id = PluginId::new(plugin.id()).map_err(CordisError::InvalidPluginId)?;
        let dependencies =
            catch_unwind(AssertUnwindSafe(|| plugin.dependencies())).map_err(|_| {
                CordisError::PluginMountFailed {
                    plugin: plugin_id.clone(),
                    message: "plugin dependency declaration panicked".to_owned(),
                }
            })?;
        if dependencies.len() > MAX_PLUGIN_DEPENDENCIES {
            return Err(CordisError::DependencyLimit {
                plugin: plugin_id,
                maximum: MAX_PLUGIN_DEPENDENCIES,
            });
        }
        for dependency in dependencies {
            let available = self.has_service_name(dependency.name())?;
            if dependency.is_required() && !available {
                return Err(CordisError::MissingDependency {
                    plugin: plugin_id,
                    service: dependency.name().to_owned(),
                });
            }
        }

        let scope = self.child()?;
        let record = Arc::new(FiberRecord::new(next_fiber_id(), plugin_id.clone(), scope));
        if let Err(error) = self.register_fiber(Arc::clone(&record)) {
            let _ignored = record.scope.dispose();
            return Err(error);
        }
        let fiber = Fiber {
            record: Arc::clone(&record),
            parent: Arc::downgrade(&self.inner),
            _plugin: Arc::clone(&plugin),
        };

        let result = catch_unwind(AssertUnwindSafe(|| plugin.mount(&record.scope)));
        match result {
            Ok(Ok(())) => {
                record.set_state(FiberState::Mounted)?;
                Ok(fiber)
            }
            Ok(Err(error)) => fail_mount(self, record, plugin_id, error.to_string()),
            Err(_) => fail_mount(self, record, plugin_id, "plugin mount panicked".to_owned()),
        }
    }
}

fn fail_mount(
    parent: &Context,
    record: Arc<FiberRecord>,
    plugin: PluginId,
    message: String,
) -> Result<Fiber, CordisError> {
    let _ignored = record.set_state(FiberState::Failed);
    let cleanup = record.scope.dispose();
    parent.unregister_fiber(record.id());
    let message = match cleanup {
        Ok(()) => message,
        Err(error) => format!("{message}; cleanup failed: {error}"),
    };
    Err(CordisError::PluginMountFailed { plugin, message })
}

fn next_fiber_id() -> FiberId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    FiberId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static CLEANUPS: AtomicUsize = AtomicUsize::new(0);

    struct PanickingPlugin;

    impl Plugin for PanickingPlugin {
        fn id(&self) -> &str {
            "test.panicking-plugin"
        }

        fn mount(&self, context: &Context) -> Result<(), CordisError> {
            context.install_effect(crate::Effect::new(|| {
                CLEANUPS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))?;
            panic!("intentional mount panic")
        }
    }

    #[test]
    fn plugin_ids_reject_whitespace() {
        assert!(PluginId::new("test.plugin").is_ok());
        assert!(matches!(
            PluginId::new("test plugin"),
            Err(PluginIdError::InvalidCharacter { .. })
        ));
    }

    #[test]
    fn mount_panic_disposes_partial_effects_and_unregisters_the_fiber() {
        CLEANUPS.store(0, Ordering::SeqCst);
        let root = Context::new();

        assert!(matches!(
            root.mount(PanickingPlugin),
            Err(CordisError::PluginMountFailed { .. })
        ));
        assert_eq!(CLEANUPS.load(Ordering::SeqCst), 1);
        assert!(root.snapshot().unwrap().fibers().is_empty());
    }
}
