//! Bounded, generation-aware retry state for supervised plugin hosts.
//!
//! The controller owns only policy: generation fencing, the three-attempt
//! budget, and explicit enable-cycle transitions. `runtime` owns process
//! startup and handshake orchestration.

/// Hard upper bound for automatic restarts in one enable cycle.
pub const MAX_AUTOMATIC_RESTARTS: u8 = 3;

/// Default retry budget used by YunXi plugin hosts.
pub const DEFAULT_MAX_AUTOMATIC_RESTARTS: u8 = MAX_AUTOMATIC_RESTARTS;

/// Bounded retry configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    max_automatic_restarts: u8,
}

impl RetryPolicy {
    /// Creates a policy. Values above the product safety bound are capped.
    pub fn new(max_automatic_restarts: u8) -> Self {
        Self {
            max_automatic_restarts: max_automatic_restarts.min(MAX_AUTOMATIC_RESTARTS),
        }
    }

    pub const fn max_automatic_restarts(self) -> u8 {
        self.max_automatic_restarts
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_AUTOMATIC_RESTARTS)
    }
}

/// The action a supervisor should take after a current-generation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryAction {
    /// Start the next generation. `attempt` is one-based.
    Restart { attempt: u8 },
    /// Stop routing to the plugin and wait for explicit user re-enabling.
    Disable,
}

impl RetryAction {
    pub const fn is_restart(self) -> bool {
        matches!(self, Self::Restart { .. })
    }

    pub const fn attempt(self) -> Option<u8> {
        match self {
            Self::Restart { attempt } => Some(attempt),
            Self::Disable => None,
        }
    }
}

/// Read-only state exposed to a host status projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetrySnapshot {
    enabled: bool,
    generation: Option<u64>,
    automatic_restarts: u8,
    max_automatic_restarts: u8,
}

impl RetrySnapshot {
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    pub const fn generation(self) -> Option<u64> {
        self.generation
    }

    pub const fn automatic_restarts(self) -> u8 {
        self.automatic_restarts
    }

    pub const fn max_automatic_restarts(self) -> u8 {
        self.max_automatic_restarts
    }

    pub const fn exhausted(self) -> bool {
        !self.enabled && self.automatic_restarts >= self.max_automatic_restarts
    }
}

/// Generation fence and retry counter for one plugin enable cycle.
///
/// A failure is consumed at most once for a generation. Once a newer
/// generation is observed, events tagged with an older generation are
/// rejected and cannot spend another retry or disable the current plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryController {
    policy: RetryPolicy,
    enabled: bool,
    generation: Option<u64>,
    automatic_restarts: u8,
    handled_failure_generation: Option<u64>,
}

impl RetryController {
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            enabled: true,
            generation: None,
            automatic_restarts: 0,
            handled_failure_generation: None,
        }
    }

    pub const fn policy(&self) -> RetryPolicy {
        self.policy
    }

    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Records a generation if it is newer than the current one.
    ///
    /// Returns `false` for stale events. Re-observing the current generation
    /// is harmless and returns `true`.
    pub fn observe_generation(&mut self, generation: u64) -> bool {
        if !self.enabled {
            return false;
        }
        if let Some(current) = self.generation {
            if generation < current {
                return false;
            }
            if generation == current {
                return true;
            }
        }
        self.generation = Some(generation);
        self.handled_failure_generation = None;
        true
    }

    pub fn accepts_generation(&self, generation: u64) -> bool {
        self.enabled && self.generation == Some(generation)
    }

    /// Consumes a current-generation failure and returns the bounded action.
    /// Duplicate failure notifications for the same generation are ignored.
    pub fn on_failure(&mut self, generation: u64) -> Option<RetryAction> {
        if !self.accepts_generation(generation)
            || self.handled_failure_generation == Some(generation)
        {
            return None;
        }
        self.handled_failure_generation = Some(generation);

        if self.automatic_restarts < self.policy.max_automatic_restarts {
            self.automatic_restarts += 1;
            Some(RetryAction::Restart {
                attempt: self.automatic_restarts,
            })
        } else {
            self.enabled = false;
            Some(RetryAction::Disable)
        }
    }

    /// Resets the failure budget for an explicit user restart.
    ///
    /// The current generation is cleared so a delayed event from the old
    /// process cannot affect the newly started generation.
    pub fn manual_restart(&mut self) {
        self.enabled = true;
        self.generation = None;
        self.automatic_restarts = 0;
        self.handled_failure_generation = None;
    }

    /// Disables the cycle until an explicit `manual_restart` call.
    pub fn disable(&mut self) {
        self.enabled = false;
        self.generation = None;
        self.handled_failure_generation = None;
    }

    pub const fn snapshot(&self) -> RetrySnapshot {
        RetrySnapshot {
            enabled: self.enabled,
            generation: self.generation,
            automatic_restarts: self.automatic_restarts,
            max_automatic_restarts: self.policy.max_automatic_restarts,
        }
    }
}

impl Default for RetryController {
    fn default() -> Self {
        Self::new(RetryPolicy::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_caps_the_automatic_restart_budget() {
        assert_eq!(RetryPolicy::new(99).max_automatic_restarts(), 3);
        assert_eq!(RetryPolicy::new(0).max_automatic_restarts(), 0);
    }

    #[test]
    fn three_restarts_then_disable() {
        let mut controller = RetryController::default();

        for generation in 1..=3 {
            assert!(controller.observe_generation(generation));
            assert_eq!(
                controller.on_failure(generation),
                Some(RetryAction::Restart {
                    attempt: generation as u8
                })
            );
        }

        assert!(controller.observe_generation(4));
        assert_eq!(controller.on_failure(4), Some(RetryAction::Disable));
        assert!(!controller.is_enabled());
        assert_eq!(controller.snapshot().automatic_restarts(), 3);
        assert!(controller.snapshot().exhausted());
    }

    #[test]
    fn manual_restart_clears_failure_budget_and_generation() {
        let mut controller = RetryController::default();
        assert!(controller.observe_generation(1));
        assert_eq!(
            controller.on_failure(1),
            Some(RetryAction::Restart { attempt: 1 })
        );

        controller.manual_restart();
        assert!(controller.is_enabled());
        assert_eq!(controller.snapshot().automatic_restarts(), 0);
        assert_eq!(controller.snapshot().generation(), None);
        assert!(!controller.accepts_generation(1));

        assert!(controller.observe_generation(2));
        assert_eq!(
            controller.on_failure(2),
            Some(RetryAction::Restart { attempt: 1 })
        );
    }

    #[test]
    fn stale_generation_failures_are_discarded() {
        let mut controller = RetryController::default();
        assert!(controller.observe_generation(10));
        assert_eq!(
            controller.on_failure(10),
            Some(RetryAction::Restart { attempt: 1 })
        );
        assert!(controller.observe_generation(11));

        assert!(!controller.accepts_generation(10));
        assert_eq!(controller.on_failure(10), None);
        assert_eq!(controller.snapshot().automatic_restarts(), 1);
        assert_eq!(
            controller.on_failure(11),
            Some(RetryAction::Restart { attempt: 2 })
        );
    }

    #[test]
    fn duplicate_failure_for_one_generation_spends_only_one_attempt() {
        let mut controller = RetryController::default();
        assert!(controller.observe_generation(7));
        assert_eq!(
            controller.on_failure(7),
            Some(RetryAction::Restart { attempt: 1 })
        );
        assert_eq!(controller.on_failure(7), None);
        assert_eq!(controller.snapshot().automatic_restarts(), 1);
        assert!(controller.is_enabled());
    }

    #[test]
    fn sibling_controller_has_an_independent_budget() {
        let mut failed = RetryController::default();
        let healthy = RetryController::default();
        assert!(failed.observe_generation(1));
        assert_eq!(
            failed.on_failure(1),
            Some(RetryAction::Restart { attempt: 1 })
        );

        assert!(healthy.is_enabled());
        assert_eq!(healthy.snapshot().automatic_restarts(), 0);
        assert_eq!(healthy.snapshot().generation(), None);
    }

    #[test]
    fn disabled_controller_ignores_late_events() {
        let mut controller = RetryController::default();
        assert!(controller.observe_generation(1));
        controller.disable();
        assert!(!controller.observe_generation(2));
        assert_eq!(controller.on_failure(1), None);
        assert_eq!(controller.on_failure(2), None);
    }
}
