//! Typed proactive scheduling policy calls.

use serde::{Deserialize, Serialize};

pub const SCHEDULER_PROACTIVE_EVALUATE_OPERATION: &str = "evaluate";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct QuietHours {
    start_minute: u16,
    end_minute: u16,
}

impl QuietHours {
    pub fn new(start_minute: u16, end_minute: u16) -> Option<Self> {
        (start_minute < 1440 && end_minute < 1440).then_some(Self {
            start_minute,
            end_minute,
        })
    }

    pub fn contains(self, minute: u16) -> bool {
        if self.start_minute <= self.end_minute {
            (self.start_minute..=self.end_minute).contains(&minute)
        } else {
            minute >= self.start_minute || minute <= self.end_minute
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProactiveTrigger {
    ReminderDue,
    UnfinishedTask,
    LongIdleCheckIn,
    TopicContinuation,
    PeriodicSummary,
    RelationshipMilestone,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProactiveAction {
    MessageOnly,
    SuggestNextStep,
    SummarizeStage,
    AskPermissionForTool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProactiveSchedulerRequest {
    now_minute_of_day: u16,
    proactive_in_session: u32,
    proactive_today: u32,
    idle_minutes: u64,
    reminder_due: bool,
    unfinished_task: Option<String>,
    topic_continuation: Option<String>,
    periodic_summary_due: bool,
    relationship_milestone: Option<String>,
    tool_request: Option<String>,
    max_per_session: u32,
    max_per_day: u32,
    quiet_hours: Option<QuietHours>,
    allow_tool_requests: bool,
}

impl ProactiveSchedulerRequest {
    pub fn new(now_minute_of_day: u16) -> Self {
        Self {
            now_minute_of_day: now_minute_of_day.min(1439),
            proactive_in_session: 0,
            proactive_today: 0,
            idle_minutes: 0,
            reminder_due: false,
            unfinished_task: None,
            topic_continuation: None,
            periodic_summary_due: false,
            relationship_milestone: None,
            tool_request: None,
            max_per_session: 3,
            max_per_day: 8,
            quiet_hours: None,
            allow_tool_requests: false,
        }
    }

    pub fn with_counts(mut self, session: u32, day: u32) -> Self {
        self.proactive_in_session = session;
        self.proactive_today = day;
        self
    }

    pub fn with_limits(mut self, session: u32, day: u32) -> Self {
        self.max_per_session = session;
        self.max_per_day = day;
        self
    }

    pub fn with_quiet_hours(mut self, quiet_hours: QuietHours) -> Self {
        self.quiet_hours = Some(quiet_hours);
        self
    }

    pub fn with_idle_minutes(mut self, minutes: u64) -> Self {
        self.idle_minutes = minutes;
        self
    }

    pub fn with_reminder_due(mut self, due: bool) -> Self {
        self.reminder_due = due;
        self
    }

    pub fn with_unfinished_task(mut self, task: impl Into<String>) -> Self {
        self.unfinished_task = Some(task.into());
        self
    }

    pub fn with_topic_continuation(mut self, topic: impl Into<String>) -> Self {
        self.topic_continuation = Some(topic.into());
        self
    }

    pub fn with_periodic_summary_due(mut self, due: bool) -> Self {
        self.periodic_summary_due = due;
        self
    }

    pub fn with_relationship_milestone(mut self, milestone: impl Into<String>) -> Self {
        self.relationship_milestone = Some(milestone.into());
        self
    }

    pub fn with_tool_request(mut self, tool: impl Into<String>) -> Self {
        self.tool_request = Some(tool.into());
        self
    }

    pub fn allow_tool_requests(mut self, allow: bool) -> Self {
        self.allow_tool_requests = allow;
        self
    }

    pub fn now_minute_of_day(&self) -> u16 {
        self.now_minute_of_day
    }

    pub fn proactive_in_session(&self) -> u32 {
        self.proactive_in_session
    }

    pub fn proactive_today(&self) -> u32 {
        self.proactive_today
    }

    pub fn idle_minutes(&self) -> u64 {
        self.idle_minutes
    }

    pub fn reminder_due(&self) -> bool {
        self.reminder_due
    }

    pub fn unfinished_task(&self) -> Option<&str> {
        self.unfinished_task.as_deref()
    }

    pub fn topic_continuation(&self) -> Option<&str> {
        self.topic_continuation.as_deref()
    }

    pub fn periodic_summary_due(&self) -> bool {
        self.periodic_summary_due
    }

    pub fn relationship_milestone(&self) -> Option<&str> {
        self.relationship_milestone.as_deref()
    }

    pub fn tool_request(&self) -> Option<&str> {
        self.tool_request.as_deref()
    }

    pub fn max_per_session(&self) -> u32 {
        self.max_per_session
    }

    pub fn max_per_day(&self) -> u32 {
        self.max_per_day
    }

    pub fn quiet_hours(&self) -> Option<QuietHours> {
        self.quiet_hours
    }

    pub fn tool_requests_allowed(&self) -> bool {
        self.allow_tool_requests
    }

    pub fn has_signal(&self) -> bool {
        self.reminder_due
            || self.unfinished_task.is_some()
            || self.topic_continuation.is_some()
            || self.periodic_summary_due
            || self.relationship_milestone.is_some()
            || self.tool_request.is_some()
            || self.idle_minutes >= 120
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProactivePlan {
    trigger: ProactiveTrigger,
    action: ProactiveAction,
    reason: String,
    message: String,
    requires_user_confirmation: bool,
}

impl ProactivePlan {
    pub fn new(
        trigger: ProactiveTrigger,
        action: ProactiveAction,
        reason: impl Into<String>,
        message: impl Into<String>,
        requires_user_confirmation: bool,
    ) -> Self {
        Self {
            trigger,
            action,
            reason: reason.into(),
            message: message.into(),
            requires_user_confirmation,
        }
    }

    pub fn trigger(&self) -> ProactiveTrigger {
        self.trigger
    }

    pub fn action(&self) -> ProactiveAction {
        self.action
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn requires_user_confirmation(&self) -> bool {
        self.requires_user_confirmation
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProactiveSchedulerResult {
    plans: Vec<ProactivePlan>,
    suppressed_reason: Option<String>,
}

impl ProactiveSchedulerResult {
    pub fn new(plans: Vec<ProactivePlan>, suppressed_reason: Option<String>) -> Self {
        Self {
            plans,
            suppressed_reason,
        }
    }

    pub fn plans(&self) -> &[ProactivePlan] {
        &self.plans
    }

    pub fn suppressed_reason(&self) -> Option<&str> {
        self.suppressed_reason.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_support_ranges_across_midnight() {
        let hours = QuietHours::new(22 * 60, 7 * 60).expect("valid quiet hours");
        assert!(hours.contains(23 * 60));
        assert!(hours.contains(6 * 60));
        assert!(!hours.contains(12 * 60));
    }
}
