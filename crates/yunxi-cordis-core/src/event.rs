//! Typed, synchronous event dispatch with explicit mode contracts.

use std::any::{Any, TypeId, type_name};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use crate::error::IdentifierKind;
use crate::service::validate_name;

/// Upper bound for event definitions and handlers kept by one bus.
pub const MAX_EVENT_DEFINITIONS: usize = 256;
pub const MAX_EVENT_HANDLERS: usize = 256;
/// Upper bound on concurrently spawned dispatch workers.
pub const MAX_PARALLEL_WORKERS: usize = 32;
const MAX_HANDLER_ERROR_BYTES: usize = 4096;

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct EventKey<E: 'static> {
    name: &'static str,
    marker: PhantomData<fn() -> E>,
}

impl<E: 'static> EventKey<E> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl<E: 'static> Copy for EventKey<E> {}

impl<E: 'static> Clone for EventKey<E> {
    fn clone(&self) -> Self {
        *self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventMode {
    Emit,
    Waterfall,
    Parallel,
    Serial,
    Bail,
}

impl fmt::Display for EventMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Emit => "emit",
            Self::Waterfall => "waterfall",
            Self::Parallel => "parallel",
            Self::Serial => "serial",
            Self::Bail => "bail",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HandlerId(u64);

impl HandlerId {
    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for HandlerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "handler-{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandlerError {
    message: String,
}

impl HandlerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: bounded_message(message.into()),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HandlerError {}

impl From<&str> for HandlerError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

impl From<String> for HandlerError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BailDecision {
    Continue,
    Handled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BailResult {
    Unhandled,
    Handled { handler: HandlerId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventSnapshot {
    name: String,
    type_name: &'static str,
    mode: EventMode,
    handler_count: usize,
}

impl EventSnapshot {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    pub fn mode(&self) -> EventMode {
        self.mode
    }

    pub fn handler_count(&self) -> usize {
        self.handler_count
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventBusSnapshot {
    events: Vec<EventSnapshot>,
}

impl EventBusSnapshot {
    pub fn events(&self) -> &[EventSnapshot] {
        &self.events
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventError {
    InvalidName {
        name: String,
    },
    TypeConflict {
        name: String,
        registered: &'static str,
        requested: &'static str,
    },
    ModeConflict {
        name: String,
        registered: EventMode,
        requested: EventMode,
    },
    HandlerFailed {
        event: String,
        handler: HandlerId,
        message: String,
    },
    HandlerPanicked {
        event: String,
        handler: HandlerId,
    },
    HandlerLimit {
        event: String,
        maximum: usize,
    },
    EventLimit {
        maximum: usize,
    },
    ThreadSpawn {
        event: String,
        message: String,
    },
    Poisoned,
}

impl fmt::Display for EventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name } => write!(formatter, "invalid event identifier `{name}`"),
            Self::TypeConflict {
                name,
                registered,
                requested,
            } => write!(
                formatter,
                "event `{name}` already uses `{registered}`, not `{requested}`"
            ),
            Self::ModeConflict {
                name,
                registered,
                requested,
            } => write!(
                formatter,
                "event `{name}` uses {registered} dispatch, not {requested}"
            ),
            Self::HandlerFailed {
                event,
                handler,
                message,
            } => write!(
                formatter,
                "handler {handler} for `{event}` failed: {message}"
            ),
            Self::HandlerPanicked { event, handler } => {
                write!(formatter, "handler {handler} for `{event}` panicked")
            }
            Self::HandlerLimit { event, maximum } => write!(
                formatter,
                "event `{event}` already has the maximum of {maximum} handlers"
            ),
            Self::EventLimit { maximum } => {
                write!(
                    formatter,
                    "event bus already has the maximum of {maximum} events"
                )
            }
            Self::ThreadSpawn { event, message } => {
                write!(
                    formatter,
                    "could not spawn a handler for `{event}`: {message}"
                )
            }
            Self::Poisoned => formatter.write_str("event bus lock is poisoned"),
        }
    }
}

impl Error for EventError {}

type EmitCallback = dyn Fn(&dyn Any) -> Result<(), HandlerError> + Send + Sync + 'static;
type SerialCallback = dyn Fn(&mut dyn Any) -> Result<(), HandlerError> + Send + Sync + 'static;
type WaterfallCallback = dyn Fn(Box<dyn Any + Send>) -> Result<Box<dyn Any + Send>, HandlerError>
    + Send
    + Sync
    + 'static;
type BailCallback = dyn Fn(&dyn Any) -> Result<BailDecision, HandlerError> + Send + Sync + 'static;

#[derive(Clone)]
enum RegisteredHandler {
    Emit {
        id: HandlerId,
        callback: Arc<EmitCallback>,
    },
    Waterfall {
        id: HandlerId,
        callback: Arc<WaterfallCallback>,
    },
    Parallel {
        id: HandlerId,
        callback: Arc<EmitCallback>,
    },
    Serial {
        id: HandlerId,
        callback: Arc<SerialCallback>,
    },
    Bail {
        id: HandlerId,
        callback: Arc<BailCallback>,
    },
}

impl RegisteredHandler {
    fn id(&self) -> HandlerId {
        match self {
            Self::Emit { id, .. }
            | Self::Waterfall { id, .. }
            | Self::Parallel { id, .. }
            | Self::Serial { id, .. }
            | Self::Bail { id, .. } => *id,
        }
    }
}

struct EventDefinition {
    type_id: TypeId,
    type_name: &'static str,
    mode: EventMode,
    handlers: Vec<RegisteredHandler>,
}

#[derive(Default)]
struct EventBusInner {
    next_handler: AtomicU64,
    definitions: Mutex<BTreeMap<String, EventDefinition>>,
}

#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<EventBusInner>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_emit<E, F>(&self, key: EventKey<E>, handler: F) -> Result<Subscription, EventError>
    where
        E: 'static,
        F: Fn(&E) -> Result<(), HandlerError> + Send + Sync + 'static,
    {
        let id = self.next_handler_id();
        let callback: Arc<EmitCallback> = Arc::new(move |payload| {
            let value = payload
                .downcast_ref::<E>()
                .ok_or_else(|| HandlerError::new("event payload type mismatch"))?;
            handler(value)
        });
        self.register(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Emit,
            RegisteredHandler::Emit { id, callback },
        )
    }

    pub fn on_waterfall<E, F>(
        &self,
        key: EventKey<E>,
        handler: F,
    ) -> Result<Subscription, EventError>
    where
        E: Send + 'static,
        F: Fn(E) -> Result<E, HandlerError> + Send + Sync + 'static,
    {
        let id = self.next_handler_id();
        let callback: Arc<WaterfallCallback> = Arc::new(move |payload| {
            let value = payload
                .downcast::<E>()
                .map_err(|_| HandlerError::new("event payload type mismatch"))?;
            handler(*value).map(|value| Box::new(value) as Box<dyn Any + Send>)
        });
        self.register(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Waterfall,
            RegisteredHandler::Waterfall { id, callback },
        )
    }

    pub fn on_parallel<E, F>(
        &self,
        key: EventKey<E>,
        handler: F,
    ) -> Result<Subscription, EventError>
    where
        E: 'static,
        F: Fn(&E) -> Result<(), HandlerError> + Send + Sync + 'static,
    {
        let id = self.next_handler_id();
        let callback: Arc<EmitCallback> = Arc::new(move |payload| {
            let value = payload
                .downcast_ref::<E>()
                .ok_or_else(|| HandlerError::new("event payload type mismatch"))?;
            handler(value)
        });
        self.register(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Parallel,
            RegisteredHandler::Parallel { id, callback },
        )
    }

    pub fn on_serial<E, F>(&self, key: EventKey<E>, handler: F) -> Result<Subscription, EventError>
    where
        E: 'static,
        F: Fn(&mut E) -> Result<(), HandlerError> + Send + Sync + 'static,
    {
        let id = self.next_handler_id();
        let callback: Arc<SerialCallback> = Arc::new(move |payload| {
            let value = payload
                .downcast_mut::<E>()
                .ok_or_else(|| HandlerError::new("event payload type mismatch"))?;
            handler(value)
        });
        self.register(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Serial,
            RegisteredHandler::Serial { id, callback },
        )
    }

    pub fn on_bail<E, F>(&self, key: EventKey<E>, handler: F) -> Result<Subscription, EventError>
    where
        E: 'static,
        F: Fn(&E) -> Result<BailDecision, HandlerError> + Send + Sync + 'static,
    {
        let id = self.next_handler_id();
        let callback: Arc<BailCallback> = Arc::new(move |payload| {
            let value = payload
                .downcast_ref::<E>()
                .ok_or_else(|| HandlerError::new("event payload type mismatch"))?;
            handler(value)
        });
        self.register(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Bail,
            RegisteredHandler::Bail { id, callback },
        )
    }

    pub fn emit<E>(&self, key: EventKey<E>, payload: &E) -> Result<(), EventError>
    where
        E: 'static,
    {
        let handlers = self.handlers(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Emit,
        )?;
        let mut first_error = None;
        for handler in handlers {
            let RegisteredHandler::Emit { id, callback } = handler else {
                continue;
            };
            if let Err(error) = invoke_immutable(&callback, key.name(), id, payload) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn waterfall<E>(&self, key: EventKey<E>, payload: E) -> Result<E, EventError>
    where
        E: Send + 'static,
    {
        let handlers = self.handlers(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Waterfall,
        )?;
        let mut current: Box<dyn Any + Send> = Box::new(payload);
        for handler in handlers {
            let RegisteredHandler::Waterfall { id, callback } = handler else {
                continue;
            };
            current = invoke_waterfall(&callback, key.name(), id, current)?;
        }
        current
            .downcast::<E>()
            .map(|value| *value)
            .map_err(|_| EventError::HandlerFailed {
                event: key.name().to_owned(),
                handler: HandlerId(0),
                message: "event payload type mismatch".to_owned(),
            })
    }

    pub fn parallel<E>(&self, key: EventKey<E>, payload: E) -> Result<(), EventError>
    where
        E: Send + Sync + 'static,
    {
        let handlers = self.handlers(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Parallel,
        )?;
        let payload: Arc<dyn Any + Send + Sync> = Arc::new(payload);
        let mut pending = handlers
            .into_iter()
            .filter_map(|handler| match handler {
                RegisteredHandler::Parallel { id, callback } => Some((id, callback)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut first_error = None;
        while !pending.is_empty() {
            let batch_len = pending.len().min(MAX_PARALLEL_WORKERS);
            let batch = pending.drain(..batch_len);
            let mut joins = Vec::with_capacity(batch_len);
            for (id, callback) in batch {
                let event = key.name().to_owned();
                let shared_payload = Arc::clone(&payload);
                let spawned = thread::Builder::new()
                    .name(format!("yunxi-event-{id}"))
                    .spawn(move || {
                        match catch_unwind(AssertUnwindSafe(|| callback(shared_payload.as_ref()))) {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(error)) => Err(EventError::HandlerFailed {
                                event,
                                handler: id,
                                message: error.to_string(),
                            }),
                            Err(_) => Err(EventError::HandlerPanicked { event, handler: id }),
                        }
                    });
                match spawned {
                    Ok(join) => joins.push((id, join)),
                    Err(error) => {
                        for (_, join) in joins {
                            let _ = join.join();
                        }
                        return Err(EventError::ThreadSpawn {
                            event: key.name().to_owned(),
                            message: error.to_string(),
                        });
                    }
                }
            }

            for (id, join) in joins {
                match join.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                    Err(_) => {
                        if first_error.is_none() {
                            first_error = Some(EventError::HandlerPanicked {
                                event: key.name().to_owned(),
                                handler: id,
                            });
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn serial<E>(&self, key: EventKey<E>, payload: &mut E) -> Result<(), EventError>
    where
        E: 'static,
    {
        let handlers = self.handlers(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Serial,
        )?;
        for handler in handlers {
            let RegisteredHandler::Serial { id, callback } = handler else {
                continue;
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                let erased: &mut dyn Any = payload;
                callback(erased)
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(EventError::HandlerFailed {
                        event: key.name().to_owned(),
                        handler: id,
                        message: error.to_string(),
                    });
                }
                Err(_) => {
                    return Err(EventError::HandlerPanicked {
                        event: key.name().to_owned(),
                        handler: id,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn bail<E>(&self, key: EventKey<E>, payload: &E) -> Result<BailResult, EventError>
    where
        E: 'static,
    {
        let handlers = self.handlers(
            key.name(),
            TypeId::of::<E>(),
            type_name::<E>(),
            EventMode::Bail,
        )?;
        for handler in handlers {
            let RegisteredHandler::Bail { id, callback } = handler else {
                continue;
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                let erased: &dyn Any = payload;
                callback(erased)
            }));
            match result {
                Ok(Ok(BailDecision::Continue)) => {}
                Ok(Ok(BailDecision::Handled)) => return Ok(BailResult::Handled { handler: id }),
                Ok(Err(error)) => {
                    return Err(EventError::HandlerFailed {
                        event: key.name().to_owned(),
                        handler: id,
                        message: error.to_string(),
                    });
                }
                Err(_) => {
                    return Err(EventError::HandlerPanicked {
                        event: key.name().to_owned(),
                        handler: id,
                    });
                }
            }
        }
        Ok(BailResult::Unhandled)
    }

    pub fn snapshot(&self) -> Result<EventBusSnapshot, EventError> {
        let definitions = self
            .inner
            .definitions
            .lock()
            .map_err(|_| EventError::Poisoned)?;
        let events = definitions
            .iter()
            .map(|(name, definition)| EventSnapshot {
                name: name.clone(),
                type_name: definition.type_name,
                mode: definition.mode,
                handler_count: definition.handlers.len(),
            })
            .collect();
        Ok(EventBusSnapshot { events })
    }

    fn next_handler_id(&self) -> HandlerId {
        HandlerId(self.inner.next_handler.fetch_add(1, Ordering::Relaxed))
    }

    fn register(
        &self,
        name: &'static str,
        type_id: TypeId,
        type_name: &'static str,
        mode: EventMode,
        handler: RegisteredHandler,
    ) -> Result<Subscription, EventError> {
        validate_name(name, IdentifierKind::Event).map_err(|_| EventError::InvalidName {
            name: name.to_owned(),
        })?;
        let id = handler.id();
        let mut definitions = self
            .inner
            .definitions
            .lock()
            .map_err(|_| EventError::Poisoned)?;
        if let Some(definition) = definitions.get_mut(name) {
            if definition.type_id != type_id {
                return Err(EventError::TypeConflict {
                    name: name.to_owned(),
                    registered: definition.type_name,
                    requested: type_name,
                });
            }
            if definition.mode != mode {
                return Err(EventError::ModeConflict {
                    name: name.to_owned(),
                    registered: definition.mode,
                    requested: mode,
                });
            }
            if definition.handlers.len() >= MAX_EVENT_HANDLERS {
                return Err(EventError::HandlerLimit {
                    event: name.to_owned(),
                    maximum: MAX_EVENT_HANDLERS,
                });
            }
            definition.handlers.push(handler);
        } else {
            if definitions.len() >= MAX_EVENT_DEFINITIONS {
                return Err(EventError::EventLimit {
                    maximum: MAX_EVENT_DEFINITIONS,
                });
            }
            definitions.insert(
                name.to_owned(),
                EventDefinition {
                    type_id,
                    type_name,
                    mode,
                    handlers: vec![handler],
                },
            );
        }
        Ok(Subscription {
            bus: Arc::downgrade(&self.inner),
            event_name: name.to_owned(),
            handler_id: id,
            active: true,
        })
    }

    fn handlers(
        &self,
        name: &'static str,
        type_id: TypeId,
        type_name: &'static str,
        mode: EventMode,
    ) -> Result<Vec<RegisteredHandler>, EventError> {
        validate_name(name, IdentifierKind::Event).map_err(|_| EventError::InvalidName {
            name: name.to_owned(),
        })?;
        let definitions = self
            .inner
            .definitions
            .lock()
            .map_err(|_| EventError::Poisoned)?;
        let Some(definition) = definitions.get(name) else {
            return Ok(Vec::new());
        };
        if definition.type_id != type_id {
            return Err(EventError::TypeConflict {
                name: name.to_owned(),
                registered: definition.type_name,
                requested: type_name,
            });
        }
        if definition.mode != mode {
            return Err(EventError::ModeConflict {
                name: name.to_owned(),
                registered: definition.mode,
                requested: mode,
            });
        }
        Ok(definition.handlers.clone())
    }
}

fn bounded_message(mut value: String) -> String {
    if value.len() > MAX_HANDLER_ERROR_BYTES {
        let mut end = MAX_HANDLER_ERROR_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

fn invoke_immutable<E>(
    callback: &Arc<EmitCallback>,
    event: &str,
    id: HandlerId,
    payload: &E,
) -> Result<(), EventError>
where
    E: 'static,
{
    match catch_unwind(AssertUnwindSafe(|| {
        let erased: &dyn Any = payload;
        callback(erased)
    })) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(EventError::HandlerFailed {
            event: event.to_owned(),
            handler: id,
            message: error.to_string(),
        }),
        Err(_) => Err(EventError::HandlerPanicked {
            event: event.to_owned(),
            handler: id,
        }),
    }
}

fn invoke_waterfall(
    callback: &Arc<WaterfallCallback>,
    event: &str,
    id: HandlerId,
    payload: Box<dyn Any + Send>,
) -> Result<Box<dyn Any + Send>, EventError> {
    match catch_unwind(AssertUnwindSafe(|| callback(payload))) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(EventError::HandlerFailed {
            event: event.to_owned(),
            handler: id,
            message: error.to_string(),
        }),
        Err(_) => Err(EventError::HandlerPanicked {
            event: event.to_owned(),
            handler: id,
        }),
    }
}

#[must_use = "keep the subscription or explicitly unsubscribe it"]
pub struct Subscription {
    bus: Weak<EventBusInner>,
    event_name: String,
    handler_id: HandlerId,
    active: bool,
}

impl Subscription {
    pub fn id(&self) -> HandlerId {
        self.handler_id
    }

    pub fn unsubscribe(&mut self) -> Result<(), EventError> {
        if !self.active {
            return Ok(());
        }
        let Some(bus) = self.bus.upgrade() else {
            self.active = false;
            return Ok(());
        };
        bus.remove(&self.event_name, self.handler_id)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(bus) = self.bus.upgrade() {
            let _ = bus.remove(&self.event_name, self.handler_id);
        }
        self.active = false;
    }
}

impl EventBusInner {
    fn remove(&self, event_name: &str, handler_id: HandlerId) -> Result<(), EventError> {
        let mut definitions = self.definitions.lock().map_err(|_| EventError::Poisoned)?;
        let Some(definition) = definitions.get_mut(event_name) else {
            return Ok(());
        };
        definition
            .handlers
            .retain(|handler| handler.id() != handler_id);
        if definition.handlers.is_empty() {
            definitions.remove(event_name);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;

    const EMIT: EventKey<u32> = EventKey::new("test.emit");
    const WATERFALL: EventKey<i32> = EventKey::new("test.waterfall");
    const PARALLEL: EventKey<u8> = EventKey::new("test.parallel");
    const SERIAL: EventKey<Vec<i32>> = EventKey::new("test.serial");
    const BAIL: EventKey<&'static str> = EventKey::new("test.bail");

    #[test]
    fn emit_runs_all_handlers_and_reports_the_first_error() {
        let bus = EventBus::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let first_calls = Arc::clone(&calls);
        let _first = bus
            .on_emit(EMIT, move |_| {
                first_calls.lock().unwrap().push(1);
                Err("first failure".into())
            })
            .unwrap();
        let first_id = _first.id();
        let second_calls = Arc::clone(&calls);
        let _second = bus
            .on_emit(EMIT, move |_| {
                second_calls.lock().unwrap().push(2);
                Ok(())
            })
            .unwrap();

        let error = bus.emit(EMIT, &7).unwrap_err();
        assert!(matches!(
            error,
            EventError::HandlerFailed { handler, .. } if handler == first_id
        ));
        assert_eq!(*calls.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn waterfall_passes_each_return_value_to_the_next_handler() {
        let bus = EventBus::new();
        let _first = bus.on_waterfall(WATERFALL, |value| Ok(value + 2)).unwrap();
        let _second = bus.on_waterfall(WATERFALL, |value| Ok(value * 3)).unwrap();
        assert_eq!(bus.waterfall(WATERFALL, 4).unwrap(), 18);
    }

    #[test]
    fn parallel_runs_every_handler() {
        let bus = EventBus::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut subscriptions = Vec::new();
        for _ in 0..4 {
            let calls = Arc::clone(&calls);
            subscriptions.push(
                bus.on_parallel(PARALLEL, move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap(),
            );
        }
        bus.parallel(PARALLEL, 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn serial_exposes_mutations_in_registration_order() {
        let bus = EventBus::new();
        let _first = bus
            .on_serial(SERIAL, |values| {
                values.push(1);
                Ok(())
            })
            .unwrap();
        let _second = bus
            .on_serial(SERIAL, |values| {
                values.push(values.len() as i32);
                Ok(())
            })
            .unwrap();
        let mut values = Vec::new();
        bus.serial(SERIAL, &mut values).unwrap();
        assert_eq!(values, vec![1, 1]);
    }

    #[test]
    fn bail_stops_at_the_first_handled_handler() {
        let bus = EventBus::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let first_calls = Arc::clone(&calls);
        let _first = bus
            .on_bail(BAIL, move |_| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(BailDecision::Continue)
            })
            .unwrap();
        let second_calls = Arc::clone(&calls);
        let _second = bus
            .on_bail(BAIL, move |_| {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(BailDecision::Handled)
            })
            .unwrap();
        let second_id = _second.id();
        let third_calls = Arc::clone(&calls);
        let _third = bus
            .on_bail(BAIL, move |_| {
                third_calls.fetch_add(1, Ordering::SeqCst);
                Ok(BailDecision::Handled)
            })
            .unwrap();

        assert!(matches!(
            bus.bail(BAIL, &"message").unwrap(),
            BailResult::Handled { handler } if handler == second_id
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parallel_dispatch_caps_concurrent_workers() {
        let bus = EventBus::new();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut subscriptions = Vec::new();

        for _ in 0..(MAX_PARALLEL_WORKERS * 2) {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            subscriptions.push(
                bus.on_parallel(PARALLEL, move |_| {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut observed = maximum.load(Ordering::SeqCst);
                    while current > observed {
                        match maximum.compare_exchange(
                            observed,
                            current,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(value) => observed = value,
                        }
                    }
                    thread::sleep(Duration::from_millis(1));
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap(),
            );
        }

        bus.parallel(PARALLEL, 1).unwrap();

        assert!(maximum.load(Ordering::SeqCst) <= MAX_PARALLEL_WORKERS);
    }

    #[test]
    fn mode_conflicts_and_drop_unsubscribe_are_observable() {
        let bus = EventBus::new();
        let subscription = bus.on_emit(EMIT, |_| Ok(())).unwrap();
        let error = bus
            .on_serial(EMIT, |_| Ok(()))
            .err()
            .expect("mode conflict");
        assert!(matches!(error, EventError::ModeConflict { .. }));
        drop(subscription);
        assert!(bus.snapshot().unwrap().events().is_empty());
    }
}
