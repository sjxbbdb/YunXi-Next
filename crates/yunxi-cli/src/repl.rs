//! Interactive commands and bounded in-memory conversation history.

use std::io::{self, BufRead, Write};

use yunxi_protocol::ChatMessage;

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
                writeln!(output, "/clear   clear conversation history")?;
                writeln!(output, "/quit    exit YunXi")?;
            }
            "/status" => {
                let status = backend.status();
                writeln!(
                    output,
                    "kernel: {} | plugin: {} | protocol: {} | plugins: {} | capabilities: {}",
                    status.kernel,
                    status.plugin,
                    if status.protocol_ready {
                        "ready"
                    } else {
                        "unavailable"
                    },
                    status.plugins,
                    status.capabilities
                )?;
            }
            "/clear" => {
                history.clear();
                writeln!(output, "{}", palette.muted("Conversation cleared."))?;
            }
            "/quit" | "/exit" => return Ok(()),
            command if command.starts_with('/') => {
                writeln!(
                    output,
                    "{} unknown command `{command}`",
                    palette.error("error:")
                )?;
            }
            prompt => {
                let mut request = history.clone();
                request.push(ChatMessage::user(prompt));
                match backend.complete(&request) {
                    Ok(reply) => {
                        writeln!(output, "\n{} {reply}", palette.assistant("yunxi>"))?;
                        history.push(ChatMessage::user(prompt));
                        history.push(ChatMessage::assistant(reply));
                        trim_history(&mut history);
                    }
                    Err(error) => {
                        writeln!(output, "{} {error}", palette.error("error:"))?;
                    }
                }
            }
        }
    }
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
}
