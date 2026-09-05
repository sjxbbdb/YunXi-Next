use std::collections::VecDeque;

use serde_json::json;
use yunxi_agent_spine::{
    Agent, AgentConfig, AgentError, AgentTurnOutcome, ApprovalAwareToolBroker, CancellationToken,
    ConversationContextAssembler, ModelError, ModelProvider, ModelRequest, ToolApprovalPolicy,
    ToolBroker, ToolDecision, ToolError, ToolRequest, TurnState,
};
use yunxi_protocol::{
    ChatResult, ToolApprovalDecision, ToolApprovalState, ToolCall, ToolCallId, ToolCatalog,
    ToolDefinition, ToolName, ToolResultOutcome,
};

struct Model {
    responses: VecDeque<ChatResult>,
}

impl Model {
    fn new(responses: impl IntoIterator<Item = ChatResult>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
        }
    }
}

impl ModelProvider for Model {
    fn complete(
        &mut self,
        _request: &ModelRequest,
        _cancellation: &CancellationToken,
    ) -> Result<ChatResult, ModelError> {
        self.responses
            .pop_front()
            .ok_or_else(|| ModelError::new("model_exhausted", "model script exhausted", false))
    }
}

struct Broker {
    catalog: ToolCatalog,
    calls: Vec<String>,
}

impl Broker {
    fn echo() -> Self {
        let definition = ToolDefinition::new(
            ToolName::new("echo").expect("tool name"),
            "Return a fixture value",
            json!({"type": "object"}),
        )
        .expect("tool definition");
        Self {
            catalog: ToolCatalog::new(vec![definition]).expect("catalog"),
            calls: Vec::new(),
        }
    }
}

impl ToolBroker for Broker {
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        Ok(self.catalog.clone())
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        self.calls.push(request.call().name().to_string());
        ToolResultOutcome::completed(json!({"ok": true}))
            .map_err(|error| ToolError::new("fixture_result", error.to_string(), false))
    }
}

fn call() -> ToolCall {
    ToolCall::new("call-1", "echo", json!({"value": "hello"})).expect("tool call")
}

fn tool_response() -> ChatResult {
    ChatResult::new("", Some("tool_calls".to_string())).with_tool_calls(vec![call()])
}

fn approval(request: &yunxi_protocol::ToolApprovalRequest) -> ToolApprovalDecision {
    ToolApprovalDecision::new(
        request.round(),
        request.call_id().clone(),
        request.tool_name().clone(),
        ToolApprovalState::approved("test-ticket").expect("approval"),
    )
    .expect("approval decision")
}

#[test]
fn default_approval_path_waits_without_executing() {
    let mut agent = Agent::new(
        "root",
        Model::new([tool_response(), ChatResult::new("done", None)]),
        ConversationContextAssembler,
        Broker::echo(),
        AgentConfig::default(),
    )
    .expect("agent");

    let outcome = agent
        .run_text_turn_with_approval("run echo", &CancellationToken::new())
        .expect("approval request");
    let request = outcome.approval_request().expect("pending approval");
    assert_eq!(request.tool_name().as_ref(), "echo");
    assert_eq!(
        agent.state(),
        yunxi_agent_spine::AgentState::AwaitingApproval
    );
    assert_eq!(
        agent.last_turn().expect("snapshot").state(),
        TurnState::AwaitingApproval
    );
    assert_eq!(agent.pending_approval(), Some(request));
    assert!(agent.tools_mut().calls.is_empty());
}

#[test]
fn matching_approval_executes_and_resumes_the_same_turn() {
    let mut agent = Agent::new(
        "root",
        Model::new([tool_response(), ChatResult::new("done", None)]),
        ConversationContextAssembler,
        Broker::echo(),
        AgentConfig::default(),
    )
    .expect("agent");
    let token = CancellationToken::new();
    let pending = agent
        .run_text_turn_with_approval("run echo", &token)
        .expect("approval request");
    let request = pending.approval_request().expect("request").clone();

    let outcome = agent
        .approve_pending_tool(approval(&request), &token)
        .expect("completed turn");
    let result = outcome.completed().expect("completed result");
    assert_eq!(result.content(), "done");
    assert_eq!(result.snapshot().state(), TurnState::Completed);
    assert_eq!(agent.tools_mut().calls, vec!["echo"]);
    assert!(agent.pending_approval().is_none());
    assert_eq!(agent.state(), yunxi_agent_spine::AgentState::Ready);
    assert!(agent.session().records().iter().any(|record| {
        record.event().kind() == yunxi_agent_spine::SessionEventKind::ToolApprovalRequested
    }));
    assert!(agent.session().records().iter().any(|record| {
        record.event().kind() == yunxi_agent_spine::SessionEventKind::ToolApprovalResolved
    }));
}

#[test]
fn denial_is_model_visible_and_does_not_execute_the_tool() {
    let mut agent = Agent::new(
        "root",
        Model::new([tool_response(), ChatResult::new("continued", None)]),
        ConversationContextAssembler,
        Broker::echo(),
        AgentConfig::default(),
    )
    .expect("agent");
    let token = CancellationToken::new();
    let pending = agent
        .run_text_turn_with_approval("run echo", &token)
        .expect("approval request");
    let request = pending.approval_request().expect("request").clone();
    let denied = ToolApprovalDecision::new(
        request.round(),
        request.call_id().clone(),
        request.tool_name().clone(),
        ToolApprovalState::denied("user chose not to run it").expect("denial"),
    )
    .expect("decision");

    let outcome = agent
        .approve_pending_tool(denied, &token)
        .expect("turn continues");
    assert_eq!(
        outcome.completed().expect("completed").content(),
        "continued"
    );
    assert!(agent.tools_mut().calls.is_empty());
    let conversation = agent.session().conversation();
    assert!(
        conversation
            .iter()
            .any(|message| message.content().contains("user_denied"))
    );
}

#[test]
fn mismatched_approval_keeps_the_pending_call_blocked() {
    let mut agent = Agent::new(
        "root",
        Model::new([tool_response()]),
        ConversationContextAssembler,
        Broker::echo(),
        AgentConfig::default(),
    )
    .expect("agent");
    let token = CancellationToken::new();
    let pending = agent
        .run_text_turn_with_approval("run echo", &token)
        .expect("approval request");
    let request = pending.approval_request().expect("request");
    let wrong = ToolApprovalDecision::new(
        request.round(),
        ToolCallId::new("other-call").expect("call id"),
        request.tool_name().clone(),
        ToolApprovalState::approved("test-ticket").expect("approval"),
    )
    .expect("approval decision");

    let error = agent
        .approve_pending_tool(wrong, &token)
        .expect_err("mismatched approval");
    assert!(matches!(error, AgentError::InvalidInput(_)));
    assert!(agent.pending_approval().is_some());
    assert_eq!(
        agent.state(),
        yunxi_agent_spine::AgentState::AwaitingApproval
    );
    assert!(agent.tools_mut().calls.is_empty());
}

#[test]
fn approval_aware_wrapper_is_fail_closed_by_default_and_pluggable() {
    let broker = ApprovalAwareToolBroker::new(Broker::echo());
    let error = broker.broker().catalog().expect("wrapped catalog");
    assert_eq!(error.tools().len(), 1);

    let mut default_agent = Agent::new(
        "root",
        Model::new([tool_response()]),
        ConversationContextAssembler,
        ApprovalAwareToolBroker::new(Broker::echo()),
        AgentConfig::default(),
    )
    .expect("agent");
    let pending = default_agent
        .run_text_turn_with_approval("run echo", &CancellationToken::new())
        .expect("approval request");
    assert!(pending.approval_request().is_some());
    assert!(default_agent.tools_mut().broker_mut().calls.is_empty());

    let mut agent = Agent::new(
        "root",
        Model::new([tool_response(), ChatResult::new("done", None)]),
        ConversationContextAssembler,
        ApprovalAwareToolBroker::with_policy(Broker::echo(), AllowPolicy),
        AgentConfig::default(),
    )
    .expect("agent");
    let outcome = agent
        .run_text_turn_with_approval("run echo", &CancellationToken::new())
        .expect("explicit policy may execute");
    assert!(matches!(outcome, AgentTurnOutcome::Completed(_)));
    assert_eq!(agent.tools_mut().broker_mut().calls, vec!["echo"]);
}

struct AllowPolicy;

impl ToolApprovalPolicy for AllowPolicy {
    fn decide(&mut self, _request: &ToolRequest<'_>) -> Result<ToolDecision, ToolError> {
        Ok(ToolDecision::Execute)
    }
}
