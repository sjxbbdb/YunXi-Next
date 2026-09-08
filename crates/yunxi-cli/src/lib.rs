#![doc = "Terminal chat host for YunXi Next."]
#![forbid(unsafe_code)]

mod args;
mod commands;
mod control;
mod management;
mod migration;
mod repl;
mod session;
mod tui;
mod ui;
mod voice_runtime;
mod web;

use std::env;
use std::error::Error;
use std::fmt;
use std::io::{self, IsTerminal, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use args::{ArgumentError, CliAction, OutputMode, TuiOptions, WebOptions};
use session::{ChatBackend, ChatFailure, ChatSession, SessionError};
use yunxi_agent_spine::{CancellationToken, EventSinkError};
use yunxi_protocol::{AgentStreamEvent, ChatMessage};
use yunxi_settings::next_state_root;
use yunxi_web_gateway::{HttpCarrier, ShutdownToken};

pub use web::{WebHost, WebHostError};

pub const INTERNAL_MODEL_PLUGIN_ARGUMENT: &str = "__model-plugin";
pub const INTERNAL_COMPANION_PLUGIN_ARGUMENT: &str = "__companion-plugin";
pub const INTERNAL_MAILBOX_PLUGIN_ARGUMENT: &str = "__mailbox-plugin";
pub const INTERNAL_CONTEXT_PLUGIN_ARGUMENT: &str = "__context-plugin";
pub const INTERNAL_MEMORY_PLUGIN_ARGUMENT: &str = "__memory-plugin";
pub const INTERNAL_MULTI_AGENT_PLUGIN_ARGUMENT: &str = "__multi-agent-plugin";
pub const INTERNAL_PERSONA_PLUGIN_ARGUMENT: &str = "__persona-plugin";
pub const INTERNAL_SCHEDULER_PLUGIN_ARGUMENT: &str = "__scheduler-plugin";
pub const INTERNAL_STORAGE_PLUGIN_ARGUMENT: &str = "__storage-plugin";
pub const INTERNAL_SHELL_PLUGIN_ARGUMENT: &str = "__shell-plugin";
pub const INTERNAL_PATCH_PLUGIN_ARGUMENT: &str = "__patch-plugin";
pub const INTERNAL_FILES_PLUGIN_ARGUMENT: &str = "__files-plugin";
pub const INTERNAL_MCP_PLUGIN_ARGUMENT: &str = "__mcp-plugin";
pub const INTERNAL_SKILLS_PLUGIN_ARGUMENT: &str = "__skills-plugin";
pub const INTERNAL_VOICE_PLUGIN_ARGUMENT: &str = "__voice-plugin";
pub const INTERNAL_WEIXIN_PLUGIN_ARGUMENT: &str = "__weixin-plugin";
pub const INTERNAL_MCP_FIXTURE_ARGUMENT: &str = "__mcp-fixture";
const WEB_EVENT_JOURNAL_ENV: &str = "YUNXI_NEXT_WEB_EVENT_JOURNAL";

pub fn run_from_env() -> Result<(), CliError> {
    match args::parse(env::args_os().skip(1))? {
        CliAction::Help => {
            print_help();
            Ok(())
        }
        CliAction::Version => {
            println!("yunxi-next {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        CliAction::Tui(options) => run_tui(options),
        CliAction::Web(options) => run_web(options),
        CliAction::Control(options) => control::run(options).map_err(|error| CliError {
            kind: CliErrorKind::Control(error),
        }),
        CliAction::Management(options) => commands::run(options).map_err(|error| CliError {
            kind: CliErrorKind::Management(error),
        }),
        CliAction::Run(options) => {
            let color = !options.no_color
                && env::var_os("NO_COLOR").is_none()
                && io::stdout().is_terminal();
            let mut session = ChatSession::launch_with_options(&options.session)?;
            if let Some(prompt) = options.once {
                if options.output == OutputMode::Json {
                    let reply = session.complete(&[ChatMessage::user(prompt)])?;
                    let warnings = session.drain_notices();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                        "schemaVersion": 1,
                        "ok": true,
                        "provider": session.provider(),
                        "model": session.model(),
                        "reply": reply,
                        "warnings": warnings,
                        }))
                        .map_err(|error| CliError {
                            kind: CliErrorKind::Output(error.to_string()),
                        })?
                    );
                    return Ok(());
                }
                if options.output == OutputMode::Jsonl {
                    let provider = session.provider().to_string();
                    let model = session.model().to_string();
                    let stdout = io::stdout();
                    let mut output = stdout.lock();
                    let mut sink = |event: AgentStreamEvent| {
                        let value = serde_json::json!({
                            "type": "event",
                            "event": event,
                        });
                        serde_json::to_writer(&mut output, &value)
                            .map_err(|_| EventSinkError::Closed)?;
                        output
                            .write_all(b"\n")
                            .map_err(|_| EventSinkError::Closed)?;
                        output.flush().map_err(|_| EventSinkError::Closed)
                    };
                    let reply = session.complete_streaming(
                        &[ChatMessage::user(prompt)],
                        &CancellationToken::new(),
                        &mut sink,
                    )?;
                    let warnings = session.drain_notices();
                    for warning in warnings {
                        write_json_line(
                            &mut output,
                            &serde_json::json!({
                                "type": "warning",
                                "message": warning,
                            }),
                        )?;
                    }
                    write_json_line(
                        &mut output,
                        &serde_json::json!({
                            "type": "result",
                            "schemaVersion": 1,
                            "ok": true,
                            "provider": provider,
                            "model": model,
                            "reply": reply,
                        }),
                    )?;
                    return Ok(());
                }
                let mut streamed = false;
                let reply = {
                    let mut sink = |event: AgentStreamEvent| {
                        if let AgentStreamEvent::TextDelta { delta, .. } = event {
                            print!("{delta}");
                            io::stdout().flush().map_err(|_| EventSinkError::Closed)?;
                            streamed = true;
                        }
                        Ok(())
                    };
                    session.complete_streaming(
                        &[ChatMessage::user(prompt)],
                        &CancellationToken::new(),
                        &mut sink,
                    )?
                };
                for notice in session.drain_notices() {
                    eprintln!("warning: {notice}");
                }
                if streamed {
                    println!();
                } else {
                    println!("{reply}");
                }
                return Ok(());
            }
            if options.output != OutputMode::Human {
                return Err(CliError {
                    kind: CliErrorKind::Arguments(ArgumentError::InvalidValue(
                        "--json/--jsonl",
                        "a prompt is required for machine-readable output".to_string(),
                    )),
                });
            }

            let stdin = io::stdin();
            let stdout = io::stdout();
            let mut input = stdin.lock();
            let mut output = stdout.lock();
            repl::run_interactive(&mut session, &mut input, &mut output, color)?;
            Ok(())
        }
    }
}

fn run_tui(options: TuiOptions) -> Result<(), CliError> {
    let color =
        !options.no_color && env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal();
    let session = ChatSession::launch_with_options(&options.session)?;
    tui::run(session, options.session, color).map_err(|error| CliError {
        kind: CliErrorKind::Io(error),
    })
}

fn run_web(options: WebOptions) -> Result<(), CliError> {
    let listener = TcpListener::bind(options.bind)?;
    let address = listener.local_addr()?;
    let host = WebHost::launch_with_options(options.session)?;
    let journal_path = web_event_journal_path()?;
    let carrier = HttpCarrier::new(host)
        .with_event_journal_path(&journal_path)
        .map_err(|error| CliError {
            kind: CliErrorKind::Output(format!(
                "failed to open Web event journal {}: {error}",
                journal_path.display()
            )),
        })?;
    println!("YunXi Next Web listening on http://{address}");
    io::stdout().flush()?;

    let shutdown = ShutdownToken::new();
    let stdin_shutdown = shutdown.clone();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut buffer = [0_u8; 1];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => {
                    stdin_shutdown.request_shutdown();
                    break;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(10035) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => {
                    stdin_shutdown.request_shutdown();
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    carrier.serve_until_concurrent(listener, &shutdown)?;
    Ok(())
}

fn web_event_journal_path() -> Result<PathBuf, CliError> {
    let path = env::var_os(WEB_EVENT_JOURNAL_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| next_state_root().join("web").join("events.jsonl"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

fn print_help() {
    println!(
        r#"YunXi Next

USAGE:
    yunxi-next [OPTIONS] [PROMPT ...]
    yunxi-next run [OPTIONS] [PROMPT ...]
    yunxi-next tui [OPTIONS]
    yunxi-next web [OPTIONS]
    yunxi-next <doctor|status|diagnostics> [--cwd <PATH>] [--json]
    yunxi-next sessions [list|show|archive|unarchive|pin|unpin|fork] [--cwd <PATH>] [--json]
    yunxi-next memory [status|search|approve|reject] [--cwd <PATH>] [--json]
    yunxi-next voice [status|doctor|devices|transcribe|speak|chat|talk] [--json]
    yunxi-next weixin [status|doctor|login|poll-login|serve|pair|session|logout] [--json]
    yunxi-next migrate [status|plan|apply|rollback <ID>] [--cwd <PATH>] [--json]
    yunxi-next migrate events <status|replay|plan|apply> <JSONL> [--cwd <PATH>] [--json]
    yunxi-next migrate events rollback <JSONL> <ID> [--cwd <PATH>] [--json]
    yunxi-next <enable|disable|reload> [PLUGIN_ID] [--json]

RUN OPTIONS:
    --once <PROMPT>       Send one prompt and exit
    --cwd <PATH>         Use this workspace for the session
    --provider <NAME>    Override provider profile for this process
    --model <NAME>       Override model for this process
    --approval <MODE>    never, on-request, on-failure, or untrusted
    --sandbox <MODE>     read-only, workspace-write, or danger-full-access
    --plugin <PATH>      Use an external compatible model plugin
    --json               Emit one structured result (requires a prompt)
    --jsonl              Emit JSON Lines events (requires a prompt)
    --no-color           Disable ANSI terminal colors

WEB OPTIONS:
    --bind <ADDR>        Listen on an explicit address (default: 127.0.0.1:8787)
    --cwd <PATH>         Use this workspace for the Web session
    --provider <NAME>    Override the Web provider profile
    --model <NAME>       Override the Web model
    --approval <MODE>    Set Web tool approval mode
    --sandbox <MODE>     Set Web tool sandbox mode
    --plugin <PATH>      Use an external compatible model plugin

CONTROL COMMANDS:
    status               Show the effective plugin inventory
    doctor               Show local runtime and integration checks
    diagnostics          Show settings warnings and inventory diagnostics
    enable <ID>          Persistently enable an optional plugin
    disable <ID>         Persistently disable an optional plugin
    reload [ID]          Validate the next plugin generation
    --json               Emit machine-readable output; --jsonl is reserved for run events

MANAGEMENT COMMANDS:
    sessions             List or mutate saved sessions without starting a model
    memory               Inspect recallable memory or approve/reject a record
    voice                Inspect devices or run bounded transcribe/speak/chat/talk operations
    weixin               Login, serve, pair, bind sessions, or inspect the account transport

GLOBAL OPTIONS:
    -h, --help           Print help
    -V, --version        Print version"#
    );
    println!(
        r#"
DETAILS:
    tui turns run in a background worker; Ctrl-C or /cancel requests cancellation
    sessions --all includes archived sessions
    voice and weixin use deterministic loopback when no external provider is configured
    metadata commands accept --json; --jsonl is reserved for run event streams
    use -- before a prompt to keep reserved words such as sessions as prompt text"#
    );
}
fn write_json_line<W: Write>(output: &mut W, value: &serde_json::Value) -> Result<(), CliError> {
    serde_json::to_writer(&mut *output, value).map_err(|error| CliError {
        kind: CliErrorKind::Output(error.to_string()),
    })?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[derive(Debug)]
pub struct CliError {
    kind: CliErrorKind,
}

#[derive(Debug)]
enum CliErrorKind {
    Arguments(ArgumentError),
    Session(SessionError),
    Chat(ChatFailure),
    WebHost(WebHostError),
    Control(control::ControlError),
    Management(commands::ManagementError),
    Output(String),
    Io(io::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            CliErrorKind::Arguments(error) => write!(formatter, "command line error: {error}"),
            CliErrorKind::Session(error) => {
                write!(formatter, "failed to start chat session: {error}")
            }
            CliErrorKind::Chat(error) => error.fmt(formatter),
            CliErrorKind::WebHost(error) => error.fmt(formatter),
            CliErrorKind::Control(error) => write!(formatter, "control command failed: {error}"),
            CliErrorKind::Management(error) => {
                write!(formatter, "management command failed: {error}")
            }
            CliErrorKind::Output(error) => write!(formatter, "output failed: {error}"),
            CliErrorKind::Io(error) => write!(formatter, "terminal I/O failed: {error}"),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            CliErrorKind::Arguments(error) => Some(error),
            CliErrorKind::Session(error) => Some(error),
            CliErrorKind::Chat(error) => Some(error),
            CliErrorKind::WebHost(error) => Some(error),
            CliErrorKind::Control(error) => Some(error),
            CliErrorKind::Management(error) => Some(error),
            CliErrorKind::Output(_) => None,
            CliErrorKind::Io(error) => Some(error),
        }
    }
}

impl From<ArgumentError> for CliError {
    fn from(error: ArgumentError) -> Self {
        Self {
            kind: CliErrorKind::Arguments(error),
        }
    }
}

impl From<SessionError> for CliError {
    fn from(error: SessionError) -> Self {
        Self {
            kind: CliErrorKind::Session(error),
        }
    }
}

impl From<ChatFailure> for CliError {
    fn from(error: ChatFailure) -> Self {
        Self {
            kind: CliErrorKind::Chat(error),
        }
    }
}

impl From<WebHostError> for CliError {
    fn from(error: WebHostError) -> Self {
        Self {
            kind: CliErrorKind::WebHost(error),
        }
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self {
            kind: CliErrorKind::Io(error),
        }
    }
}
