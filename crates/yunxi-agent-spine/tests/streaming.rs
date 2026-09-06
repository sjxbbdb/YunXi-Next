use std::collections::VecDeque;

use serde_json::json;
use yunxi_agent_spine::{
    Agent, AgentConfig, BackpressureStrategy, CancellationToken, ConversationContextAssembler,
    EventChannel, ModelError, ModelEventSink, ModelProvider, ModelRequest, ToolBroker, ToolError,
    ToolProgressSink, ToolRequest, TurnState,
};
use yunxi_protocol::{
    ChatResult, StreamEvent, ToolCall, ToolCatalog, ToolDefinition, ToolName, ToolResultOutcome,
};

struct StreamingModel {
    responses: VecDeque<ChatResult>,
}

impl ModelProvider for StreamingModel {
    fn complete(
        &mut self,
        _request: &ModelRequest,
        _cancellation: &CancellationToken,
    ) -> Result<ChatResult, ModelError> {
        self.responses
            .pop_front()
            .ok_or_else(|| ModelError::new("model_exhausted", "fixture exhausted", false))
    }

    fn complete_streaming(
        &mut self,
        _request: &ModelRequest,
        _cancellation: &CancellationToken,
        events: &mut dyn ModelEventSink,
    ) -> Result<ChatResult, ModelError> {
        let response = self.complete(_request, _cancellation)?;
        if response.tool_calls().is_empty() {
            events.text_delta("he")?;
            events.text_delta("llo")?;
        }
        Ok(response)
    }
}

struct ProgressTool;

impl ToolBroker for ProgressTool {
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        let definition = ToolDefinition::new(
            ToolName::new("echo").expect("tool name"),
            "fixture tool",
            json!({"type": "object"}),
        )
        .map_err(|error| ToolError::new("catalog", error.to_string(), false))?;
        ToolCatalog::new(vec![definition])
            .map_err(|error| ToolError::new("catalog", error.to_string(), false))
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        ToolResultOutcome::completed(json!({"tool": request.call().name().as_str()}))
            .map_err(|error| ToolError::new("result", error.to_string(), false))
    }

    fn execute_with_progress(
        &mut self,
        request: ToolRequest<'_>,
        cancellation: &CancellationToken,
        progress: &mut dyn ToolProgressSink,
    ) -> Result<ToolResultOutcome, ToolError> {
        progress.progress("started")?;
        cancellation
            .check()
            .map_err(|error| ToolError::new(error.code(), "cancelled", false))?;
        self.execute(request, cancellation)
    }
}

fn tool_call() -> ToolCall {
    ToolCall::new("call-1", "echo", json!({"value": "x"})).expect("call")
}

#[test]
fn external_model_and_tool_adapters_can_publish_bounded_stream_events() {
    let model = StreamingModel {
        responses: VecDeque::from([
            ChatResult::new("", Some("tool_calls".to_string())).with_tool_calls(vec![tool_call()]),
            ChatResult::new("hello", Some("stop".to_string())),
        ]),
    };
    let mut agent = Agent::new(
        "stream-test",
        model,
        ConversationContextAssembler,
        ProgressTool,
        AgentConfig::default(),
    )
    .expect("agent");
    let (mut sender, receiver) =
        EventChannel::new(64, BackpressureStrategy::Block).expect("channel");

    let result = agent
        .run_text_turn_streaming("hello", &CancellationToken::new(), &mut sender)
        .expect("streaming turn");
    assert_eq!(result.content(), "hello");
    assert_eq!(result.snapshot().state(), TurnState::Completed);
    drop(sender);

    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::TextDelta { delta, .. } if delta == "he"
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::ToolStart { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolProgress { progress, .. } if progress == "started"
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::ToolResult { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::TurnDone {
            response: Some(_),
            ..
        }
    )));
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence() < pair[1].sequence())
    );
}
