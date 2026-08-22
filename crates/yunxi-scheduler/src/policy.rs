//! Pure proactive scheduling policy with no side effects.

use yunxi_protocol::{
    ProactiveAction, ProactivePlan, ProactiveSchedulerRequest, ProactiveSchedulerResult,
    ProactiveTrigger,
};

pub fn evaluate(request: &ProactiveSchedulerRequest) -> ProactiveSchedulerResult {
    if !request.has_signal() {
        return ProactiveSchedulerResult::new(Vec::new(), Some("no_signal".to_string()));
    }
    if request.proactive_in_session() >= request.max_per_session() {
        return ProactiveSchedulerResult::new(
            Vec::new(),
            Some("session_limit_reached".to_string()),
        );
    }
    if request.proactive_today() >= request.max_per_day() {
        return ProactiveSchedulerResult::new(Vec::new(), Some("daily_limit_reached".to_string()));
    }
    if request
        .quiet_hours()
        .is_some_and(|hours| hours.contains(request.now_minute_of_day()))
    {
        return ProactiveSchedulerResult::new(Vec::new(), Some("quiet_hours".to_string()));
    }

    let plan = if let Some(tool) = request.tool_request() {
        if !request.tool_requests_allowed() {
            return ProactiveSchedulerResult::new(
                Vec::new(),
                Some("tool_requests_disabled".to_string()),
            );
        }
        ProactivePlan::new(
            ProactiveTrigger::UnfinishedTask,
            ProactiveAction::AskPermissionForTool,
            "a proactive tool action was suggested",
            format!("如果你确认，我可以请求执行：{}。", compact(tool)),
            true,
        )
    } else if request.reminder_due() {
        ProactivePlan::new(
            ProactiveTrigger::ReminderDue,
            ProactiveAction::MessageOnly,
            "a reminder is due",
            "提醒：有一项到期事项需要你留意。",
            false,
        )
    } else if let Some(task) = request.unfinished_task() {
        ProactivePlan::new(
            ProactiveTrigger::UnfinishedTask,
            ProactiveAction::SuggestNextStep,
            "an unfinished task was observed",
            format!("轻提示：还可以继续处理“{}”。", compact(task)),
            false,
        )
    } else if request.idle_minutes() >= 120 {
        ProactivePlan::new(
            ProactiveTrigger::LongIdleCheckIn,
            ProactiveAction::MessageOnly,
            "the session has been idle for an extended period",
            "好久没有继续了，回来时可以从上次停下的地方接着做。",
            false,
        )
    } else if let Some(topic) = request.topic_continuation() {
        ProactivePlan::new(
            ProactiveTrigger::TopicContinuation,
            ProactiveAction::SuggestNextStep,
            "recent context suggests a topic can be continued",
            format!("可以继续关注“{}”。", compact(topic)),
            false,
        )
    } else if request.periodic_summary_due() {
        ProactivePlan::new(
            ProactiveTrigger::PeriodicSummary,
            ProactiveAction::SummarizeStage,
            "a bounded stage summary is due",
            "阶段小结：可以整理一下当前进展、未完成事项和下一步。",
            false,
        )
    } else if let Some(change) = request.relationship_milestone() {
        ProactivePlan::new(
            ProactiveTrigger::RelationshipMilestone,
            ProactiveAction::MessageOnly,
            "a relationship or context milestone changed",
            format!("最近的上下文有变化：{}。", compact(change)),
            false,
        )
    } else {
        return ProactiveSchedulerResult::new(Vec::new(), Some("no_action".to_string()));
    };
    ProactiveSchedulerResult::new(vec![plan], None)
}

fn compact(value: &str) -> String {
    let value = value.trim();
    if value.chars().count() <= 80 {
        return value.to_string();
    }
    let mut output = value.chars().take(77).collect::<String>();
    output.push_str("...");
    output
}

#[cfg(test)]
mod tests {
    use yunxi_protocol::QuietHours;

    use super::*;

    #[test]
    fn quiet_hours_suppress_due_reminder() {
        let request = ProactiveSchedulerRequest::new(23 * 60)
            .with_reminder_due(true)
            .with_quiet_hours(QuietHours::new(22 * 60, 7 * 60).expect("quiet hours"));
        let result = evaluate(&request);
        assert!(result.plans().is_empty());
        assert_eq!(result.suppressed_reason(), Some("quiet_hours"));
    }

    #[test]
    fn tool_plan_never_executes_and_requires_confirmation() {
        let result = evaluate(
            &ProactiveSchedulerRequest::new(12 * 60)
                .with_tool_request("open notes")
                .allow_tool_requests(true),
        );
        assert_eq!(result.plans().len(), 1);
        assert!(result.plans()[0].requires_user_confirmation());
        assert_eq!(
            result.plans()[0].action(),
            ProactiveAction::AskPermissionForTool
        );
    }
}
