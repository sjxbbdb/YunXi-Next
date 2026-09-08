//! Interactive commands and bounded in-memory conversation history.

use std::io::{self, BufRead, Write};

use yunxi_agent_spine::{CancellationToken, EventSinkError};
use yunxi_protocol::{AgentStreamEvent, ChatMessage};

use crate::management::{ManagementCommand, ManagementResult};
use crate::session::ChatBackend;
use crate::ui::Palette;

const MAX_HISTORY_MESSAGES: usize = 64;

pub(crate) fn run_interactive<B, R, W>(
    backend: &mut B,
    input: &mut R,
    output: &mut W,
    color: bool,
) -> io::Result<()>
where
    B: ChatBackend,
    R: BufRead,
    W: Write,
{
    let palette = Palette::new(color);
    writeln!(output, "{}", palette.title("YunXi Next"))?;
    writeln!(
        output,
        "{}",
        palette.muted(&format!(
            "provider: {} | model: {} | plugin: ready",
            backend.provider(),
            backend.model()
        ))
    )?;
    writeln!(output, "{}", palette.muted("Enter /help for commands."))?;
    write_notices(backend, output, &palette)?;

    let mut history = Vec::new();
    loop {
        write!(output, "\n{} ", palette.prompt("you>"))?;
        output.flush()?;

        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(output)?;
            return Ok(());
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }

        match text {
            "/help" => {
                writeln!(output, "/status  show kernel and plugin state")?;
                writeln!(output, "/plugins show the composed plugin inventory")?;
                writeln!(output, "/clear   clear conversation history")?;
                writeln!(output, "/sessions  list saved sessions")?;
                writeln!(output, "/resume <id>  resume a saved session")?;
                writeln!(output, "/new  start a new persistent session")?;
                writeln!(output, "/memory approve|reject <id>  review pending memory")?;
                writeln!(
                    output,
                    "/mailbox [read <id>]  list or read companion messages"
                )?;
                writeln!(
                    output,
                    "/shell <command>  queue a shell action for approval"
                )?;
                writeln!(output, "/patch <file>  queue a patch file for approval")?;
                writeln!(output, "/approve  approve the pending action")?;
                writeln!(output, "/deny  deny the pending action")?;
                writeln!(output, "/cancel  cancel the pending model action")?;
                writeln!(output, "/quit    exit YunXi")?;
            }
            "/status" => {
                let status = backend.status();
                writeln!(
                    output,
                    "kernel: {} | plugin: {} | protocol: {} | plugins: {} | capabilities: {} | failed: {}",
                    status.kernel,
                    status.plugin,
                    if status.protocol_ready {
                        "ready"
                    } else {
                        "unavailable"
                    },
                    status.plugins,
                    status.capabilities,
                    status.failed_plugins
                )?;
            }
            "/clear" => {
                history.clear();
                writeln!(output, "{}", palette.muted("Conversation cleared."))?;
            }
            "/quit" | "/exit" => return Ok(()),
            command if command.starts_with('/') => {
                match parse_management_command(command) {
                    Ok(Some(command)) => match backend.manage(command) {
                        Ok(result) => apply_management_result(result, &mut history, output)?,
                        Err(error) => {
                            writeln!(output, "{} {error}", palette.error("error:"))?;
                        }
                    },
                    Ok(None) => {
                        writeln!(
                            output,
                            "{} unknown command `{command}`",
                            palette.error("error:")
                        )?;
                    }
                    Err(error) => {
                        writeln!(output, "{} {error}", palette.error("error:"))?;
                    }
                }
                write_notices(backend, output, &palette)?;
            }
            prompt => {
                let mut request = history.clone();
                request.push(ChatMessage::user(prompt));
                let mut streamed_text = false;
                let result = {
                    let mut stream = |event: AgentStreamEvent| {
                        match event {
                            AgentStreamEvent::TextDelta { delta, .. } => {
                                if !streamed_text {
                                    write!(output, "\n{} ", palette.assistant("yunxi>"))
                                        .map_err(terminal_stream_error)?;
                                    streamed_text = true;
                                }
                                write!(output, "{delta}").map_err(terminal_stream_error)?;
                                output.flush().map_err(terminal_stream_error)?;
                            }
                            AgentStreamEvent::ToolStart { tool_name, .. } => {
                                if streamed_text {
                                    writeln!(output).map_err(terminal_stream_error)?;
                                    streamed_text = false;
                                }
                                writeln!(output, "{} {tool_name}", palette.muted("tool:"))
                                    .map_err(terminal_stream_error)?;
                            }
                            AgentStreamEvent::ToolProgress { progress, .. } => {
                                writeln!(output, "{} {progress}", palette.muted("progress:"))
                                    .map_err(terminal_stream_error)?;
                            }
                            AgentStreamEvent::ToolResult { .. }
                            | AgentStreamEvent::TurnState { .. }
                            | AgentStreamEvent::TurnError { .. }
                            | AgentStreamEvent::TurnDone { .. } => {}
                        }
                        Ok(())
                    };
                    backend.complete_streaming(&request, &CancellationToken::new(), &mut stream)
                };
                match result {
                    Ok(reply) => {
                        if streamed_text {
                            writeln!(output)?;
                        } else {
                            writeln!(output, "\n{} {reply}", palette.assistant("yunxi>"))?;
                        }
                        history.push(ChatMessage::user(prompt));
                        history.push(ChatMessage::assistant(reply));
                        trim_history(&mut history);
                    }
                    Err(error) => {
                        writeln!(output, "{} {error}", palette.error("error:"))?;
                    }
                }
                write_notices(backend, output, &palette)?;
            }
        }
    }
}

fn terminal_stream_error(_error: io::Error) -> EventSinkError {
    EventSinkError::Closed
}

fn parse_management_command(value: &str) -> Result<Option<ManagementCommand>, String> {
    if let Some(command) = value
        .strip_prefix("/shell ")
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        return Ok(Some(ManagementCommand::RequestShell(command.to_string())));
    }
    if let Some(path) = value
        .strip_prefix("/patch ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        return Ok(Some(ManagementCommand::RequestPatch(path.to_string())));
    }
    let parts = value.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["/plugins"] => Ok(Some(ManagementCommand::ListPlugins)),
        ["/sessions"] => Ok(Some(ManagementCommand::ListSessions)),
        ["/resume", id] => Ok(Some(ManagementCommand::ResumeSession((*id).to_string()))),
        ["/resume"] => Err("usage: /resume <session-id>".to_string()),
        ["/new"] => Ok(Some(ManagementCommand::NewSession)),
        ["/memory", "approve", id] => Ok(Some(ManagementCommand::ReviewMemory {
            id: (*id).to_string(),
            approve: true,
        })),
        ["/memory", "reject", id] => Ok(Some(ManagementCommand::ReviewMemory {
            id: (*id).to_string(),
            approve: false,
        })),
        ["/memory", ..] => Err("usage: /memory approve|reject <memory-id>".to_string()),
        ["/mailbox"] => Ok(Some(ManagementCommand::ListMailbox)),
        ["/mailbox", "read", id] => Ok(Some(ManagementCommand::ReadMailbox((*id).to_string()))),
        ["/mailbox", ..] => Err("usage: /mailbox [read <item-id>]".to_string()),
        ["/shell"] => Err("usage: /shell <command>".to_string()),
        ["/patch"] => Err("usage: /patch <patch-file>".to_string()),
        ["/approve"] => Ok(Some(ManagementCommand::ApproveAction)),
        ["/deny"] => Ok(Some(ManagementCommand::DenyAction)),
        ["/cancel"] => Ok(Some(ManagementCommand::CancelAction)),
        _ => Ok(None),
    }
}

fn apply_management_result<W: Write>(
    result: ManagementResult,
    history: &mut Vec<ChatMessage>,
    output: &mut W,
) -> io::Result<()> {
    if let Some(replacement) = result.replacement_history {
        *history = replacement;
        trim_history(history);
    }
    history.extend(result.append_history);
    trim_history(history);
    for line in result.lines {
        writeln!(output, "{line}")?;
    }
    if let Some(reply) = result.assistant_reply {
        writeln!(output, "\nyunxi> {reply}")?;
    }
    Ok(())
}

fn write_notices<B, W>(backend: &mut B, output: &mut W, palette: &Palette) -> io::Result<()>
where
    B: ChatBackend,
    W: Write,
{
    for notice in backend.drain_notices() {
        writeln!(output, "{} {notice}", palette.error("warning:"))?;
    }
    Ok(())
}

fn trim_history(history: &mut Vec<ChatMessage>) {
    if history.len() > MAX_HISTORY_MESSAGES {
        let excess = history.len() - MAX_HISTORY_MESSAGES;
        history.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::session::{BackendStatus, ChatFailure};

    struct FakeBackend {
        requests: Vec<Vec<ChatMessage>>,
    }

    impl ChatBackend for FakeBackend {
        fn complete(&mut self, messages: &[ChatMessage]) -> Result<String, ChatFailure> {
            self.requests.push(messages.to_vec());
            Ok(format!("reply {}", self.requests.len()))
        }

        fn provider(&self) -> &str {
            "fixture"
        }

        fn model(&self) -> &str {
            "fixture-model"
        }

        fn status(&mut self) -> BackendStatus {
            BackendStatus {
                kernel: "running".to_string(),
                plugin: "running".to_string(),
                protocol_ready: true,
                plugins: 1,
                capabilities: 1,
                failed_plugins: 0,
            }
        }

        fn drain_notices(&mut self) -> Vec<String> {
            Vec::new()
        }

        fn manage(&mut self, command: ManagementCommand) -> Result<ManagementResult, String> {
            match command {
                ManagementCommand::ListPlugins => {
                    Ok(ManagementResult::lines(vec!["plugins".to_string()]))
                }
                ManagementCommand::NewSession => Ok(ManagementResult::replace_history(
                    vec!["new".to_string()],
                    Vec::new(),
                )),
                _ => Ok(ManagementResult::lines(vec!["managed".to_string()])),
            }
        }
    }

    #[test]
    fn successful_turns_are_sent_back_as_conversation_history() {
        let mut backend = FakeBackend {
            requests: Vec::new(),
        };
        let mut input = Cursor::new("first\nsecond\n/quit\n");
        let mut output = Vec::new();

        run_interactive(&mut backend, &mut input, &mut output, false).expect("run repl");

        assert_eq!(backend.requests.len(), 2);
        assert_eq!(backend.requests[0], [ChatMessage::user("first")]);
        assert_eq!(backend.requests[1].len(), 3);
        assert_eq!(backend.requests[1][2], ChatMessage::user("second"));
        let output = String::from_utf8(output).expect("utf-8 output");
        assert!(output.contains("YunXi Next"));
        assert!(output.contains("reply 2"));
    }

    #[test]
    fn clear_command_removes_prior_turns() {
        let mut backend = FakeBackend {
            requests: Vec::new(),
        };
        let mut input = Cursor::new("first\n/clear\nsecond\n/quit\n");
        let mut output = Vec::new();

        run_interactive(&mut backend, &mut input, &mut output, false).expect("run repl");

        assert_eq!(backend.requests[1], [ChatMessage::user("second")]);
    }

    #[test]
    fn new_command_replaces_history_through_backend_management() {
        let mut backend = FakeBackend {
            requests: Vec::new(),
        };
        let mut input = Cursor::new("first\n/new\nsecond\n/quit\n");
        let mut output = Vec::new();

        run_interactive(&mut backend, &mut input, &mut output, false).expect("run repl");

        assert_eq!(backend.requests[1], [ChatMessage::user("second")]);
    }
}
