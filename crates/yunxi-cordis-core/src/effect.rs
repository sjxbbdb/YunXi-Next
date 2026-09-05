//! One-shot effect disposers owned by a context scope.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EffectId(u64);

impl EffectId {
    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EffectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "effect-{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectState {
    Active,
    Disposing,
    Disposed,
    Failed,
}

impl fmt::Display for EffectState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Active => "active",
            Self::Disposing => "disposing",
            Self::Disposed => "disposed",
            Self::Failed => "failed",
        })
    }
}

pub struct Effect {
    id: EffectId,
    disposer: Option<Box<dyn FnOnce() -> Result<(), String> + Send + 'static>>,
}

impl Effect {
    pub fn new<F>(disposer: F) -> Self
    where
        F: FnOnce() -> Result<(), String> + Send + 'static,
    {
        Self {
            id: next_id(),
            disposer: Some(Box::new(disposer)),
        }
    }

    pub fn id(&self) -> EffectId {
        self.id
    }

    pub(crate) fn run(mut self) -> Result<(), String> {
        let Some(disposer) = self.disposer.take() else {
            return Err("effect disposer was already consumed".to_owned());
        };
        match catch_unwind(AssertUnwindSafe(disposer)) {
            Ok(result) => result,
            Err(_) => Err("effect disposer panicked".to_owned()),
        }
    }
}

fn next_id() -> EffectId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    EffectId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}
