use std::collections::VecDeque;

use serde_json::json;
use yunxi_agent_spine::{
    Agent, AgentConfig, AgentError, CancellationToken, ConversationContextAssembler, ModelError,
    ModelProvider, ModelRequest, SessionEventKind, ToolBroker, ToolError, ToolRequest, TurnBudget,
    TurnState,
};
use yunxi_protocol::{
    ChatRequest, ChatResult, ChatRole, ToolCall, ToolCatalog, ToolDefinition, ToolName,
    ToolResultOutcome,
};

struct ScriptedModel {
    responses: VecDeque<Result<ChatResult, ModelError>>,
    requests: Vec<ChatRequest>,
}

impl ScriptedModel {
    fn new(responses: impl IntoIterator<Item = Result<ChatResult, ModelError>>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
            requests: Vec::new(),
        }
    }
}

impl ModelProvider for ScriptedModel {
    fn complete(
        &mut self,
        request: &ModelRequest,
        _cancellation: &CancellationToken,
    ) -> Result<ChatResult, ModelError> {
        self.requests.push(request.chat().clone());
        self.responses.pop_front().unwrap_or_else(|| {
            Err(ModelError::new(
                "script_exhausted",
                "scripted model has no response",
                false,
            ))
        })
    }
}

struct FixtureBroker {
    catalog: ToolCatalog,
    fail: bool,
    cancel_on_execute: Option<CancellationToken>,
    calls: Vec<String>,
}

impl FixtureBroker {
    fn echo() -> Self {
        let definition = ToolDefinition::new(
            ToolName::new("echo").expect("tool name"),
            "Return a fixture value",
            json!({"type": "object"}),
        )
        .expect("tool definition");
        Self {
            catalog: ToolCatalog::new(vec![definition]).expect("catalog"),
            fail: false,
            cancel_on_execute: None,
            calls: Vec::new(),
        }
    }
}

impl ToolBroker for FixtureBroker {
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        Ok(self.catalog.clone())
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        self.calls.push(request.call().name().to_string());
        if let Some(token) = &self.cancel_on_execute {
            token.cancel("test cancellation");
        }
        if self.fail {
            Err(ToolError::new(
                "temporary_tool_failure",
                "fixture tool failed once",
                true,
            ))
        } else {
            ToolResultOutcome::completed(json!({"ok": true}))
                .map_err(|error| ToolError::new("fixture_result_error", error.to_string(), false))
        }
    }
}

fn call() -> ToolCall {
    ToolCall::new("call-1", "echo", json!({"value": "hello"})).expect("tool call")
}

fn tool_response() -> ChatResult {
    ChatResult::new("", Some("tool_calls".to_string())).with_tool_calls(vec![call()])
}

fn config(budget: TurnBudget) -> AgentConfig {
    AgentConfig::new(budget, yunxi_agent_spine::SessionLimits::default()).expect("config")
}

#[test]
fn drives_model_tool_result_and_next_model_step() {
    let model = ScriptedModel::new([
        Ok(tool_response()),
        Ok(ChatResult::new("done", Some("stop".to_string()))),
    ]);
    let broker = FixtureBroker::echo();
    let mut agent = Agent::new(
        "root",
        model,
        ConversationContextAssembler,
        broker,
        AgentConfig::default(),
    )
    .expect("agent");

    let result = agent
        .run_text_turn("hello", &CancellationToken::new())
        .expect("turn");
    assert_eq!(result.content(), "done");
    assert_eq!(result.snapshot().state(), TurnState::Completed);
    assert_eq!(result.snapshot().model_calls(), 2);
    assert_eq!(result.snapshot().tool_calls(), 1);
    assert_eq!(agent.model_mut().requests.len(), 2);
    let second_request = &agent.model_mut().requests[1];
    assert_eq!(second_request.messages().len(), 3);
    assert_eq!(second_request.messages()[0].role(), ChatRole::User);
    assert_eq!(second_request.messages()[1].role(), ChatRole::Assistant);
    assert_eq!(second_request.messages()[2].role(), ChatRole::Tool);
    assert_eq!(agent.session().records().len(), 10);
    assert_eq!(agent.state(), yunxi_agent_spine::AgentState::Ready);
}

#[test]
fn cancellation_stops_before_the_next_model_step() {
    let token = CancellationToken::new();
    let model = ScriptedModel::new([
        Ok(tool_response()),
        Ok(ChatResult::new("must not run", None)),
    ]);
    let mut broker = FixtureBroker::echo();
    broker.cancel_on_execute = Some(token.clone());
    let mut agent = Agent::new(
        "root",
        model,
        ConversationContextAssembler,
        broker,
        AgentConfig::default(),
    )
    .expect("agent");

    let error = agent
        .run_text_turn("stop after tool", &token)
        .expect_err("cancelled turn");
    assert!(matches!(error, AgentError::Cancelled(_)));
    assert_eq!(agent.model_mut().requests.len(), 1);
    assert_eq!(
        agent.last_turn().expect("snapshot").state(),
        TurnState::Cancelled
    );
    assert_eq!(
        agent
            .session()
            .records()
            .last()
            .expect("event")
            .event()
            .kind(),
        SessionEventKind::TurnCancelled
    );
    assert_eq!(agent.state(), yunxi_agent_spine::AgentState::Ready);
}

#[test]
fn round_budget_returns_a_structured_error_without_poisoning_agent() {
    let model = ScriptedModel::new([
        Ok(tool_response()),
        Ok(ChatResult::new("not reached", None)),
    ]);
    let mut agent = Agent::new(
        "root",
        model,
        ConversationContextAssembler,
        FixtureBroker::echo(),
        config(TurnBudget::new(1, 1, 1, 2)),
    )
    .expect("agent");

    let error = agent
        .run_text_turn("consume one round", &CancellationToken::new())
        .expect_err("round budget");
    assert!(matches!(
        error,
        AgentError::BudgetExceeded {
            kind: yunxi_agent_spine::BudgetKind::Rounds,
            ..
        }
    ));
    assert_eq!(
        agent.last_turn().expect("snapshot").state(),
        TurnState::Failed
    );
    assert_eq!(agent.state(), yunxi_agent_spine::AgentState::Ready);
}

#[test]
fn tool_failure_becomes_model_visible_and_a_later_turn_can_recover() {
    let model = ScriptedModel::new([
        Ok(tool_response()),
        Ok(ChatResult::new("tool failed, but I continued", None)),
        Ok(ChatResult::new("recovered", None)),
    ]);
    let mut broker = FixtureBroker::echo();
    broker.fail = true;
    let mut agent = Agent::new(
        "root",
        model,
        ConversationContextAssembler,
        broker,
        AgentConfig::default(),
    )
    .expect("agent");

    let first = agent
        .run_text_turn("try the tool", &CancellationToken::new())
        .expect("tool failure is recoverable");
    assert_eq!(first.content(), "tool failed, but I continued");
    assert_eq!(first.snapshot().state(), TurnState::Completed);

    let second = agent
        .run_text_turn("continue", &CancellationToken::new())
        .expect("agent remains reusable");
    assert_eq!(second.content(), "recovered");
    assert_eq!(second.snapshot().state(), TurnState::Completed);
    assert_eq!(agent.state(), yunxi_agent_spine::AgentState::Ready);
}

#[test]
fn model_failure_is_recorded_and_next_turn_is_allowed() {
    let model = ScriptedModel::new([
        Err(ModelError::new("temporary_model", "try again", true)),
        Ok(ChatResult::new("recovered", None)),
    ]);
    let mut agent = Agent::new(
        "root",
        model,
        ConversationContextAssembler,
        FixtureBroker::echo(),
        AgentConfig::default(),
    )
    .expect("agent");

    let first = agent
        .run_text_turn("first attempt", &CancellationToken::new())
        .expect_err("model failure");
    assert!(matches!(first, AgentError::Model(_)));
    assert_eq!(
        agent.last_turn().expect("snapshot").state(),
        TurnState::Failed
    );
    let second = agent
        .run_text_turn("retry", &CancellationToken::new())
        .expect("retry");
    assert_eq!(second.content(), "recovered");
}
