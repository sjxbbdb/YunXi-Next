//! Scoped service container, effect ownership, and diagnostics.

use std::any::{Any, TypeId, type_name};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::effect::{Effect, EffectId, EffectState};
use crate::error::CordisError;
use crate::event::{EventBus, EventBusSnapshot, Subscription};
use crate::plugin::{FiberId, FiberRecord, FiberSnapshot, FiberState};
use crate::service::{ServiceEntry, ServiceKey, validate_service_name};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScopeId(u64);

impl ScopeId {
    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ScopeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "scope-{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeState {
    Active,
    Disposing,
    Disposed,
}

impl fmt::Display for ScopeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Active => "active",
            Self::Disposing => "disposing",
            Self::Disposed => "disposed",
        })
    }
}

pub struct Context {
    pub(crate) inner: Arc<ContextInner>,
}

impl Clone for Context {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

pub(crate) struct ContextInner {
    pub(crate) id: ScopeId,
    /// A child keeps its parent alive so inherited services never silently
    /// disappear while the child is still in use.
    pub(crate) parent: Option<Arc<ContextInner>>,
    pub(crate) data: Mutex<ContextData>,
    pub(crate) events: EventBus,
}

pub(crate) struct ContextData {
    pub(crate) state: ScopeState,
    pub(crate) services: BTreeMap<String, ServiceEntry>,
    pub(crate) effects: BTreeMap<EffectId, EffectRecord>,
    /// The returned `Fiber` handle owns the record. A weak entry lets a
    /// mounted fiber retain its parent without creating a parent/fiber cycle.
    pub(crate) fibers: BTreeMap<FiberId, Weak<FiberRecord>>,
}

pub(crate) struct EffectRecord {
    pub(crate) effect: Option<Effect>,
    pub(crate) state: EffectState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSnapshot {
    name: String,
    type_name: &'static str,
}

impl ServiceSnapshot {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn type_name(&self) -> &'static str {
        self.type_name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectSnapshot {
    id: EffectId,
    state: EffectState,
}

impl EffectSnapshot {
    pub fn id(&self) -> EffectId {
        self.id
    }

    pub fn state(&self) -> EffectState {
        self.state
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSnapshot {
    scope_id: ScopeId,
    parent_scope_id: Option<ScopeId>,
    state: ScopeState,
    services: Vec<ServiceSnapshot>,
    effects: Vec<EffectSnapshot>,
    fibers: Vec<FiberSnapshot>,
    events: EventBusSnapshot,
}

impl ContextSnapshot {
    pub fn scope_id(&self) -> ScopeId {
        self.scope_id
    }

    pub fn parent_scope_id(&self) -> Option<ScopeId> {
        self.parent_scope_id
    }

    pub fn state(&self) -> ScopeState {
        self.state
    }

    pub fn services(&self) -> &[ServiceSnapshot] {
        &self.services
    }

    pub fn effects(&self) -> &[EffectSnapshot] {
        &self.effects
    }

    pub fn fibers(&self) -> &[FiberSnapshot] {
        &self.fibers
    }

    pub fn events(&self) -> &EventBusSnapshot {
        &self.events
    }
}

impl Context {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ContextInner {
                id: next_scope_id(),
                parent: None,
                data: Mutex::new(ContextData {
                    state: ScopeState::Active,
                    services: BTreeMap::new(),
                    effects: BTreeMap::new(),
                    fibers: BTreeMap::new(),
                }),
                events: EventBus::new(),
            }),
        }
    }

    pub fn id(&self) -> ScopeId {
        self.inner.id
    }

    pub fn parent_id(&self) -> Option<ScopeId> {
        self.inner.parent.as_ref().map(|parent| parent.id)
    }

    pub fn state(&self) -> Result<ScopeState, CordisError> {
        Ok(lock(&self.inner.data, "context")?.state)
    }

    pub fn child(&self) -> Result<Self, CordisError> {
        self.ensure_hierarchy_active()?;
        Ok(Self {
            inner: Arc::new(ContextInner {
                id: next_scope_id(),
                parent: Some(Arc::clone(&self.inner)),
                data: Mutex::new(ContextData {
                    state: ScopeState::Active,
                    services: BTreeMap::new(),
                    effects: BTreeMap::new(),
                    fibers: BTreeMap::new(),
                }),
                events: self.inner.events.clone(),
            }),
        })
    }

    pub fn events(&self) -> EventBus {
        self.inner.events.clone()
    }

    pub fn provide<T>(&self, key: ServiceKey<T>, value: T) -> Result<(), CordisError>
    where
        T: Send + Sync + 'static,
    {
        validate_service_name(key.name())?;
        self.ensure_hierarchy_active()?;
        let mut data = lock(&self.inner.data, "context")?;
        if data.services.contains_key(key.name()) {
            return Err(CordisError::DuplicateService {
                scope_id: self.id(),
                name: key.name().to_owned(),
            });
        }
        let value: Arc<dyn Any + Send + Sync> = Arc::new(value);
        data.services.insert(
            key.name().to_owned(),
            ServiceEntry {
                value,
                type_id: TypeId::of::<T>(),
                type_name: type_name::<T>(),
            },
        );
        Ok(())
    }

    pub fn service<T>(&self, key: ServiceKey<T>) -> Result<Arc<T>, CordisError>
    where
        T: Send + Sync + 'static,
    {
        let entry = self.find_service(key.name())?;
        if entry.type_id != TypeId::of::<T>() {
            return Err(CordisError::ServiceTypeMismatch {
                name: key.name().to_owned(),
                expected: type_name::<T>(),
                actual: entry.type_name,
            });
        }
        entry
            .value
            .downcast::<T>()
            .map_err(|_| CordisError::ServiceTypeMismatch {
                name: key.name().to_owned(),
                expected: type_name::<T>(),
                actual: entry.type_name,
            })
    }

    pub fn has_service<T>(&self, key: ServiceKey<T>) -> Result<bool, CordisError>
    where
        T: 'static,
    {
        self.has_service_name(key.name())
    }

    pub fn remove<T>(&self, key: ServiceKey<T>) -> Result<(), CordisError>
    where
        T: Send + Sync + 'static,
    {
        validate_service_name(key.name())?;
        self.ensure_hierarchy_active()?;
        let mut data = lock(&self.inner.data, "context")?;
        let Some(entry) = data.services.get(key.name()) else {
            return Err(CordisError::MissingService {
                name: key.name().to_owned(),
            });
        };
        if entry.type_id != TypeId::of::<T>() {
            return Err(CordisError::ServiceTypeMismatch {
                name: key.name().to_owned(),
                expected: type_name::<T>(),
                actual: entry.type_name,
            });
        }
        data.services.remove(key.name());
        Ok(())
    }

    pub fn install_effect(&self, effect: Effect) -> Result<EffectId, CordisError> {
        let id = effect.id();
        self.ensure_hierarchy_active()?;
        let mut data = lock(&self.inner.data, "context")?;
        data.effects.insert(
            id,
            EffectRecord {
                effect: Some(effect),
                state: EffectState::Active,
            },
        );
        Ok(id)
    }

    pub fn own_subscription(&self, subscription: Subscription) -> Result<EffectId, CordisError> {
        self.install_effect(Effect::new(move || {
            drop(subscription);
            Ok(())
        }))
    }

    pub fn dispose_effect(&self, id: EffectId) -> Result<(), CordisError> {
        let effect = {
            let mut data = lock(&self.inner.data, "context")?;
            let Some(record) = data.effects.get_mut(&id) else {
                return Err(CordisError::EffectNotFound { id });
            };
            if record.state != EffectState::Active {
                return Err(CordisError::EffectUnavailable {
                    id,
                    state: record.state,
                });
            }
            record.state = EffectState::Disposing;
            record.effect.take().expect("active effect has a disposer")
        };
        let result = effect.run();
        let mut data = lock(&self.inner.data, "context")?;
        if let Some(record) = data.effects.get_mut(&id) {
            record.state = if result.is_ok() {
                EffectState::Disposed
            } else {
                EffectState::Failed
            };
        }
        result.map_err(|message| CordisError::DisposerFailed { id, message })
    }

    pub fn dispose(&self) -> Result<(), CordisError> {
        let (fibers, effects) = {
            let mut data = lock(&self.inner.data, "context")?;
            match data.state {
                ScopeState::Disposed => return Ok(()),
                ScopeState::Disposing => {
                    return Err(CordisError::ContextClosed {
                        scope_id: self.id(),
                        state: data.state,
                    });
                }
                ScopeState::Active => data.state = ScopeState::Disposing,
            }

            let fibers = data
                .fibers
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            for fiber in &fibers {
                if let Ok(state) = fiber.state() {
                    if matches!(state, FiberState::Mounting | FiberState::Mounted) {
                        fiber.set_state(FiberState::Unmounting)?;
                    }
                }
            }

            let ids = data.effects.keys().copied().rev().collect::<Vec<_>>();
            let mut effects = Vec::new();
            for id in ids {
                if let Some(record) = data.effects.get_mut(&id) {
                    if record.state == EffectState::Active {
                        record.state = EffectState::Disposing;
                        if let Some(effect) = record.effect.take() {
                            effects.push((id, effect));
                        }
                    }
                }
            }
            (fibers, effects)
        };

        let mut first_error = None;
        for fiber in fibers {
            if let Err(error) = fiber.scope.dispose() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            let _ignored = fiber.set_state(FiberState::Unmounted);
        }
        for (id, effect) in effects {
            let result = effect.run();
            if let Ok(mut data) = self.inner.data.lock() {
                if let Some(record) = data.effects.get_mut(&id) {
                    record.state = if result.is_ok() {
                        EffectState::Disposed
                    } else {
                        EffectState::Failed
                    };
                }
            }
            if let Err(message) = result {
                if first_error.is_none() {
                    first_error = Some(CordisError::DisposerFailed { id, message });
                }
            }
        }

        let mut data = lock(&self.inner.data, "context")?;
        data.services.clear();
        data.fibers.clear();
        data.state = ScopeState::Disposed;
        first_error.map_or(Ok(()), Err)
    }

    pub fn snapshot(&self) -> Result<ContextSnapshot, CordisError> {
        let data = lock(&self.inner.data, "context")?;
        let services = data
            .services
            .iter()
            .map(|(name, entry)| ServiceSnapshot {
                name: name.clone(),
                type_name: entry.type_name,
            })
            .collect();
        let effects = data
            .effects
            .iter()
            .map(|(id, record)| EffectSnapshot {
                id: *id,
                state: record.state,
            })
            .collect();
        let fibers = data
            .fibers
            .values()
            .filter_map(Weak::upgrade)
            .map(|fiber| fiber.snapshot())
            .collect::<Result<Vec<_>, _>>()?;
        drop(data);
        Ok(ContextSnapshot {
            scope_id: self.id(),
            parent_scope_id: self.parent_id(),
            state: self.state()?,
            services,
            effects,
            fibers,
            events: self.events().snapshot()?,
        })
    }

    pub(crate) fn has_service_name(&self, name: &str) -> Result<bool, CordisError> {
        validate_service_name(name)?;
        let mut current = Some(self.clone());
        while let Some(context) = current {
            let data = lock(&context.inner.data, "context")?;
            ensure_active(context.id(), data.state)?;
            if data.services.contains_key(name) {
                return Ok(true);
            }
            current = context.inner.parent.as_ref().map(|inner| Context {
                inner: Arc::clone(inner),
            });
        }
        Ok(false)
    }

    pub(crate) fn register_fiber(&self, fiber: Arc<FiberRecord>) -> Result<(), CordisError> {
        self.ensure_hierarchy_active()?;
        let mut data = lock(&self.inner.data, "context")?;
        if data
            .fibers
            .values()
            .filter_map(Weak::upgrade)
            .any(|existing| {
                existing.plugin_id() == fiber.plugin_id()
                    && existing.state().is_ok_and(|state| {
                        matches!(state, FiberState::Mounting | FiberState::Mounted)
                    })
            })
        {
            return Err(CordisError::DuplicatePlugin {
                plugin: fiber.plugin_id().clone(),
            });
        }
        data.fibers.insert(fiber.id(), Arc::downgrade(&fiber));
        Ok(())
    }

    pub(crate) fn unregister_fiber(&self, id: FiberId) {
        if let Ok(mut data) = self.inner.data.lock() {
            data.fibers.remove(&id);
        }
    }

    fn ensure_hierarchy_active(&self) -> Result<(), CordisError> {
        let mut current = Some(self.clone());
        while let Some(context) = current {
            let data = lock(&context.inner.data, "context")?;
            ensure_active(context.id(), data.state)?;
            current = context.inner.parent.as_ref().map(|inner| Context {
                inner: Arc::clone(inner),
            });
        }
        Ok(())
    }

    fn find_service(&self, name: &str) -> Result<ServiceEntry, CordisError> {
        validate_service_name(name)?;
        let mut current = Some(self.clone());
        while let Some(context) = current {
            let data = lock(&context.inner.data, "context")?;
            ensure_active(context.id(), data.state)?;
            if let Some(entry) = data.services.get(name) {
                return Ok(entry.clone());
            }
            current = context.inner.parent.as_ref().map(|inner| Context {
                inner: Arc::clone(inner),
            });
        }
        Err(CordisError::MissingService {
            name: name.to_owned(),
        })
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

fn next_scope_id() -> ScopeId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    ScopeId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

fn ensure_active(scope_id: ScopeId, state: ScopeState) -> Result<(), CordisError> {
    if state == ScopeState::Active {
        Ok(())
    } else {
        Err(CordisError::ContextClosed { scope_id, state })
    }
}

pub(crate) fn lock<'a, T>(
    mutex: &'a Mutex<T>,
    component: &'static str,
) -> Result<MutexGuard<'a, T>, CordisError> {
    mutex
        .lock()
        .map_err(|_| CordisError::Poisoned { component })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::plugin::Plugin;

    const VALUE: ServiceKey<u32> = ServiceKey::new("test.value");
    const DEPENDENCY: ServiceKey<&'static str> = ServiceKey::new("test.dependency");

    #[test]
    fn child_scopes_inherit_and_shadow_services() {
        let root = Context::new();
        root.provide(VALUE, 1).unwrap();
        let child = root.child().unwrap();
        assert_eq!(*child.service(VALUE).unwrap(), 1);
        child.provide(VALUE, 2).unwrap();
        assert_eq!(*child.service(VALUE).unwrap(), 2);
        assert_eq!(*root.service(VALUE).unwrap(), 1);
    }

    #[test]
    fn child_keeps_its_parent_services_alive() {
        let child = {
            let root = Context::new();
            root.provide(VALUE, 9).unwrap();
            root.child().unwrap()
        };

        assert_eq!(*child.service(VALUE).unwrap(), 9);
        assert!(child.parent_id().is_some());
    }

    #[test]
    fn closed_parent_blocks_child_mutation_but_child_can_still_dispose() {
        let root = Context::new();
        let child = root.child().unwrap();
        root.dispose().unwrap();

        assert!(matches!(
            child.provide(VALUE, 1),
            Err(CordisError::ContextClosed { .. })
        ));
        child.dispose().unwrap();
        assert_eq!(child.state().unwrap(), ScopeState::Disposed);
    }

    #[test]
    fn effects_dispose_in_reverse_order_and_close_the_scope() {
        let context = Context::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let first = Arc::clone(&calls);
        context
            .install_effect(Effect::new(move || {
                first.lock().unwrap().push(1);
                Ok(())
            }))
            .unwrap();
        let second = Arc::clone(&calls);
        context
            .install_effect(Effect::new(move || {
                second.lock().unwrap().push(2);
                Ok(())
            }))
            .unwrap();
        context.dispose().unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![2, 1]);
        assert_eq!(context.state().unwrap(), ScopeState::Disposed);
        assert!(matches!(
            context.install_effect(Effect::new(|| Ok(()))),
            Err(CordisError::ContextClosed { .. })
        ));
    }

    struct FixturePlugin;

    impl Plugin for FixturePlugin {
        fn id(&self) -> &str {
            "test.fixture-plugin"
        }

        fn dependencies(&self) -> Vec<crate::ServiceDependency> {
            vec![DEPENDENCY.required()]
        }

        fn mount(&self, context: &Context) -> Result<(), CordisError> {
            context.provide(VALUE, 42)
        }
    }

    #[test]
    fn plugin_dependencies_are_checked_and_unmount_disposes_its_scope() {
        let root = Context::new();
        assert!(matches!(
            root.mount(FixturePlugin),
            Err(CordisError::MissingDependency { .. })
        ));
        root.provide(DEPENDENCY, "ready").unwrap();
        let fiber = root.mount(FixturePlugin).unwrap();
        assert_eq!(*fiber.context().service(VALUE).unwrap(), 42);
        fiber.unmount().unwrap();
        assert_eq!(root.snapshot().unwrap().fibers().len(), 0);
    }

    #[test]
    fn dropping_a_fiber_unmounts_it_without_retaining_a_parent_cycle() {
        let root = Context::new();
        root.provide(DEPENDENCY, "ready").unwrap();
        let fiber = root.mount(FixturePlugin).unwrap();
        assert_eq!(root.snapshot().unwrap().fibers().len(), 1);

        drop(fiber);

        assert!(root.snapshot().unwrap().fibers().is_empty());
    }

    struct TooManyDependencies;

    impl Plugin for TooManyDependencies {
        fn id(&self) -> &str {
            "test.too-many-dependencies"
        }

        fn dependencies(&self) -> Vec<crate::ServiceDependency> {
            (0..=crate::MAX_PLUGIN_DEPENDENCIES)
                .map(|_| DEPENDENCY.optional())
                .collect()
        }

        fn mount(&self, _context: &Context) -> Result<(), CordisError> {
            Ok(())
        }
    }

    #[test]
    fn plugin_dependency_declarations_are_bounded() {
        let root = Context::new();
        assert!(matches!(
            root.mount(TooManyDependencies),
            Err(CordisError::DependencyLimit { maximum, .. })
                if maximum == crate::MAX_PLUGIN_DEPENDENCIES
        ));
        assert!(root.snapshot().unwrap().fibers().is_empty());
    }

    #[test]
    fn snapshot_reports_the_local_runtime_shape() {
        let context = Context::new();
        context.provide(VALUE, 7).unwrap();
        context.install_effect(Effect::new(|| Ok(()))).unwrap();
        let snapshot = context.snapshot().unwrap();
        assert_eq!(snapshot.state(), ScopeState::Active);
        assert_eq!(snapshot.services()[0].name(), "test.value");
        assert_eq!(snapshot.effects().len(), 1);
        assert!(snapshot.events().events().is_empty());
    }
}
