#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

#[path = "../src/session/spine_adapter.rs"]
mod spine_adapter;

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use spine_adapter::{
    DenyUnapprovedToolCalls, ProcessPluginContextAssembler, ProcessPluginHostHandle,
    ProcessPluginModelProvider, ProcessPluginToolBroker,
};
use yunxi_agent_spine::{
    Agent, AgentConfig, CancellationToken, ModelProvider, ModelRequest, TurnBudget,
};
use yunxi_plugin_host::ProcessPluginHost;
use yunxi_protocol::{
    CapabilityDescriptor, ChatResult, ToolCall, ToolCatalog, ToolDefinition, ToolName,
};

fn host_handle() -> ProcessPluginHostHandle {
    ProcessPluginHostHandle::new(ProcessPluginHost::new())
}

fn capability(id: &str) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, 1).unwrap()
}

fn empty_catalog() -> ToolCatalog {
    ToolCatalog::new(Vec::new()).unwrap()
}

#[test]
fn model_adapter_uses_the_existing_host_route_and_reports_missing_route() {
    let mut provider = ProcessPluginModelProvider::new(host_handle(), capability("model.chat"));
    let request = ModelRequest::new(
        "turn-1",
        1,
        yunxi_protocol::ChatRequest::new(vec![yunxi_protocol::ChatMessage::user("hello")]),
    );

    let error = provider
        .complete(&request, &CancellationToken::new())
        .expect_err("empty host must not fabricate a model response");
    assert_eq!(error.code(), "model_plugin_call_failed");
    assert!(error.message().contains("model.chat@1"));
}

#[test]
fn context_adapter_is_best_effort_when_the_optional_context_route_is_down() {
    let mut agent = Agent::new(
        "context-test",
        StaticModel,
        ProcessPluginContextAssembler::new().with_context_plugin(
            host_handle(),
            capability("context.compose"),
            r"C:\workspace",
        ),
        ProcessPluginToolBroker::new(host_handle(), empty_catalog(), DenyUnapprovedToolCalls),
        AgentConfig::new(TurnBudget::new(1, 1, 1, 1), Default::default()).unwrap(),
    )
    .unwrap();

    let result = agent.run_text_turn("hello", &CancellationToken::new());
    assert_eq!(result.unwrap().content(), "ok");
}

#[test]
fn tool_adapter_fails_closed_before_it_can_call_the_host() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let model = ToolThenText { seen: seen.clone() };
    let catalog = ToolCatalog::new(vec![
        ToolDefinition::new(
            ToolName::new("shell.execute").unwrap(),
            "execute a command",
            json!({"type": "object"}),
        )
        .unwrap(),
    ])
    .unwrap();
    let mut agent = Agent::new(
        "tool-test",
        model,
        ProcessPluginContextAssembler::new(),
        ProcessPluginToolBroker::new(host_handle(), catalog, DenyUnapprovedToolCalls),
        AgentConfig::new(TurnBudget::new(2, 1, 1, 2), Default::default()).unwrap(),
    )
    .unwrap();

    let result = agent
        .run_text_turn("run it", &CancellationToken::new())
        .unwrap();
    assert_eq!(result.content(), "continued after denial");
    assert_eq!(seen.borrow().len(), 2);
    assert!(seen.borrow()[1].contains("approval is required"));
}

struct StaticModel;

impl ModelProvider for StaticModel {
    fn complete(
        &mut self,
        _request: &ModelRequest,
        _cancellation: &CancellationToken,
    ) -> Result<ChatResult, yunxi_agent_spine::ModelError> {
        Ok(ChatResult::new("ok", Some("stop".to_string())))
    }
}

struct ToolThenText {
    seen: Rc<RefCell<Vec<String>>>,
}

impl ModelProvider for ToolThenText {
    fn complete(
        &mut self,
        request: &ModelRequest,
        _cancellation: &CancellationToken,
    ) -> Result<ChatResult, yunxi_agent_spine::ModelError> {
        self.seen.borrow_mut().push(
            request
                .chat()
                .messages()
                .last()
                .unwrap()
                .content()
                .to_string(),
        );
        if self.seen.borrow().len() == 1 {
            let call =
                ToolCall::new("call-1", "shell.execute", json!({"command": "echo hi"})).unwrap();
            Ok(ChatResult::new("", Some("tool_calls".to_string())).with_tool_calls(vec![call]))
        } else {
            Ok(ChatResult::new(
                "continued after denial",
                Some("stop".to_string()),
            ))
        }
    }
}
