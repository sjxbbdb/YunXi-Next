use crate::cancellation::CancellationToken;
use crate::error::ToolError;
use yunxi_protocol::{
    GrantKind, ToolApprovalDecision, ToolApprovalRequest, ToolApprovalState, ToolCall, ToolCatalog,
    ToolResultOutcome,
};

pub struct ToolRequest<'a> {
    turn_id: &'a str,
    round: u16,
    call: &'a ToolCall,
}

impl<'a> ToolRequest<'a> {
    pub(crate) fn new(turn_id: &'a str, round: u16, call: &'a ToolCall) -> Self {
        Self {
            turn_id,
            round,
            call,
        }
    }

    pub fn turn_id(&self) -> &str {
        self.turn_id
    }

    pub const fn round(&self) -> u16 {
        self.round
    }

    pub fn call(&self) -> &ToolCall {
        self.call
    }
}

/// The broker's decision before a tool call is allowed to run.
///
/// `RequestApproval` is the default. A broker or policy must explicitly
/// return `Execute` to allow an unapproved call, which keeps the approval
/// path fail-closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolDecision {
    Execute,
    RequestApproval(ToolApprovalRequest),
    Reject { code: String, message: String },
}

/// The externally visible result of an approval-aware tool execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolExecutionOutcome {
    Executed(ToolResultOutcome),
    AwaitingApproval(ToolApprovalRequest),
}

/// Receives bounded progress callbacks from a tool adapter.
pub trait ToolProgressSink {
    fn progress(&mut self, progress: &str) -> Result<(), ToolError>;
}

/// Supplies the approval decision for a tool call.
pub trait ToolApprovalPolicy {
    fn decide(&mut self, request: &ToolRequest<'_>) -> Result<ToolDecision, ToolError>;
}

/// A fail-closed policy for tools with side effects or unknown risk.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequireApproval;

impl ToolApprovalPolicy for RequireApproval {
    fn decide(&mut self, request: &ToolRequest<'_>) -> Result<ToolDecision, ToolError> {
        ToolApprovalRequest::new(
            request.round(),
            request.call().id().clone(),
            request.call().name().clone(),
            format!("Execute tool {}", request.call().name()),
            vec![GrantKind::Approval],
        )
        .map(ToolDecision::RequestApproval)
        .map_err(|error| ToolError::new("invalid_approval_request", error.to_string(), false))
    }
}

pub trait ToolBroker {
    fn catalog(&self) -> Result<ToolCatalog, ToolError>;

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError>;

    /// Streaming extension point with a compatibility default.  Existing
    /// synchronous brokers continue to use `execute` unchanged.
    fn execute_with_progress(
        &mut self,
        request: ToolRequest<'_>,
        cancellation: &CancellationToken,
        _progress: &mut dyn ToolProgressSink,
    ) -> Result<ToolResultOutcome, ToolError> {
        self.execute(request, cancellation)
    }

    /// Approval-aware equivalent of [`Self::execute_with_progress`].
    fn execute_with_approval_and_progress(
        &mut self,
        request: ToolRequest<'_>,
        approval: Option<&ToolApprovalDecision>,
        cancellation: &CancellationToken,
        _progress: &mut dyn ToolProgressSink,
    ) -> Result<ToolExecutionOutcome, ToolError> {
        self.execute_with_approval(request, approval, cancellation)
    }

    /// Executes a call after a matching approval has been supplied.
    ///
    /// The default keeps existing brokers source-compatible. Approval-aware
    /// brokers can override this hook to validate or consume the ticket.
    fn execute_approved(
        &mut self,
        request: ToolRequest<'_>,
        _approval: &ToolApprovalDecision,
        cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        self.execute(request, cancellation)
    }

    /// Decides whether the approval-aware path may execute a request.
    ///
    /// The default is deliberately fail-closed. Existing brokers retain
    /// their direct `execute` compatibility API, while callers that need
    /// approval semantics use `execute_with_approval` or the Agent's
    /// approval-aware turn API.
    fn decide(&mut self, request: &ToolRequest<'_>) -> Result<ToolDecision, ToolError> {
        RequireApproval.decide(request)
    }

    /// Executes a request only after an explicit approval decision, or
    /// returns the request that must be shown to a user first.
    fn execute_with_approval(
        &mut self,
        request: ToolRequest<'_>,
        approval: Option<&ToolApprovalDecision>,
        cancellation: &CancellationToken,
    ) -> Result<ToolExecutionOutcome, ToolError> {
        match approval {
            Some(approval) => {
                validate_approval(&request, approval)?;
                match approval.state() {
                    ToolApprovalState::Approved { .. } => self
                        .execute_approved(request, approval, cancellation)
                        .map(ToolExecutionOutcome::Executed),
                    ToolApprovalState::Denied { reason } => {
                        ToolResultOutcome::rejected("user_denied", reason.clone())
                            .map(ToolExecutionOutcome::Executed)
                            .map_err(|error| {
                                ToolError::new("invalid_tool_rejection", error.to_string(), false)
                            })
                    }
                }
            }
            None => {
                let decision = self.decide(&request)?;
                match decision {
                    ToolDecision::Execute => self
                        .execute(request, cancellation)
                        .map(ToolExecutionOutcome::Executed),
                    ToolDecision::RequestApproval(approval_request) => {
                        validate_approval_request(&request, &approval_request)?;
                        Ok(ToolExecutionOutcome::AwaitingApproval(approval_request))
                    }
                    ToolDecision::Reject { code, message } => {
                        ToolResultOutcome::rejected(code, message)
                            .map(ToolExecutionOutcome::Executed)
                            .map_err(|error| {
                                ToolError::new("invalid_tool_rejection", error.to_string(), false)
                            })
                    }
                }
            }
        }
    }
}

/// Wraps a legacy broker with an explicit approval policy.
///
/// The wrapper's legacy `ToolBroker::execute` method is fail-closed. Use
/// `ToolBroker::execute_with_approval` or `Agent::run_turn_with_approval` to
/// obtain and resolve approval requests.
pub struct ApprovalAwareToolBroker<B, P = RequireApproval> {
    broker: B,
    policy: P,
}

impl<B> ApprovalAwareToolBroker<B> {
    pub fn new(broker: B) -> Self {
        Self {
            broker,
            policy: RequireApproval,
        }
    }
}

impl<B, P> ApprovalAwareToolBroker<B, P> {
    pub fn with_policy(broker: B, policy: P) -> Self {
        Self { broker, policy }
    }

    pub fn broker(&self) -> &B {
        &self.broker
    }

    pub fn broker_mut(&mut self) -> &mut B {
        &mut self.broker
    }

    pub fn policy(&self) -> &P {
        &self.policy
    }

    pub fn policy_mut(&mut self) -> &mut P {
        &mut self.policy
    }
}

impl<B, P> ToolBroker for ApprovalAwareToolBroker<B, P>
where
    B: ToolBroker,
    P: ToolApprovalPolicy,
{
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        self.broker.catalog()
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        Err(ToolError::new(
            "approval_required",
            format!(
                "approval is required before executing {}",
                request.call().name()
            ),
            false,
        ))
    }

    fn decide(&mut self, request: &ToolRequest<'_>) -> Result<ToolDecision, ToolError> {
        self.policy.decide(request)
    }

    fn execute_with_approval(
        &mut self,
        request: ToolRequest<'_>,
        approval: Option<&ToolApprovalDecision>,
        cancellation: &CancellationToken,
    ) -> Result<ToolExecutionOutcome, ToolError> {
        match approval {
            Some(approval) => {
                validate_approval(&request, approval)?;
                match approval.state() {
                    ToolApprovalState::Approved { .. } => self
                        .broker
                        .execute_approved(request, approval, cancellation)
                        .map(ToolExecutionOutcome::Executed),
                    ToolApprovalState::Denied { reason } => {
                        ToolResultOutcome::rejected("user_denied", reason.clone())
                            .map(ToolExecutionOutcome::Executed)
                            .map_err(|error| {
                                ToolError::new("invalid_tool_rejection", error.to_string(), false)
                            })
                    }
                }
            }
            None => match self.policy.decide(&request)? {
                ToolDecision::Execute => self
                    .broker
                    .execute(request, cancellation)
                    .map(ToolExecutionOutcome::Executed),
                ToolDecision::RequestApproval(approval_request) => {
                    validate_approval_request(&request, &approval_request)?;
                    Ok(ToolExecutionOutcome::AwaitingApproval(approval_request))
                }
                ToolDecision::Reject { code, message } => {
                    ToolResultOutcome::rejected(code, message)
                        .map(ToolExecutionOutcome::Executed)
                        .map_err(|error| {
                            ToolError::new("invalid_tool_rejection", error.to_string(), false)
                        })
                }
            },
        }
    }
}

fn validate_approval(
    request: &ToolRequest<'_>,
    approval: &ToolApprovalDecision,
) -> Result<(), ToolError> {
    if approval.round() != request.round()
        || approval.call_id() != request.call().id()
        || approval.tool_name() != request.call().name()
    {
        return Err(ToolError::new(
            "approval_mismatch",
            "approval does not match the pending tool call",
            false,
        ));
    }
    approval
        .validate()
        .map_err(|error| ToolError::new("invalid_approval", error.to_string(), false))
}

fn validate_approval_request(
    request: &ToolRequest<'_>,
    approval_request: &ToolApprovalRequest,
) -> Result<(), ToolError> {
    if request.round() != approval_request.round()
        || request.call().id() != approval_request.call_id()
        || request.call().name() != approval_request.tool_name()
    {
        return Err(ToolError::new(
            "approval_request_mismatch",
            "approval request does not match the pending tool call",
            false,
        ));
    }
    approval_request
        .validate()
        .map_err(|error| ToolError::new("invalid_approval_request", error.to_string(), false))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyToolBroker;

impl ToolBroker for EmptyToolBroker {
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        ToolCatalog::new(Vec::new())
            .map_err(|error| ToolError::new("invalid_empty_catalog", error.to_string(), false))
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        Err(ToolError::new(
            "tool_unavailable",
            format!("no broker is registered for {}", request.call().name()),
            false,
        ))
    }
}
