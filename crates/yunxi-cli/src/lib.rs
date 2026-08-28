#![doc = "Terminal chat host for YunXi Next."]
#![forbid(unsafe_code)]

mod args;
mod management;
mod repl;
mod session;
mod ui;
mod web;

use std::env;
use std::error::Error;
use std::fmt;
use std::io::{self, IsTerminal, Read, Write};
use std::net::TcpListener;

use args::{ArgumentError, CliAction, WebOptions};
use session::{ChatBackend, ChatFailure, ChatSession, SessionError};
use yunxi_protocol::ChatMessage;
use yunxi_web_gateway::{HttpCarrier, ShutdownToken};

pub use web::{WebHost, WebHostError};

pub const INTERNAL_MODEL_PLUGIN_ARGUMENT: &str = "__model-plugin";
pub const INTERNAL_COMPANION_PLUGIN_ARGUMENT: &str = "__companion-plugin";
pub const INTERNAL_MAILBOX_PLUGIN_ARGUMENT: &str = "__mailbox-plugin";
pub const INTERNAL_CONTEXT_PLUGIN_ARGUMENT: &str = "__context-plugin";
pub const INTERNAL_MEMORY_PLUGIN_ARGUMENT: &str = "__memory-plugin";
pub const INTERNAL_PERSONA_PLUGIN_ARGUMENT: &str = "__persona-plugin";
pub const INTERNAL_SCHEDULER_PLUGIN_ARGUMENT: &str = "__scheduler-plugin";
pub const INTERNAL_STORAGE_PLUGIN_ARGUMENT: &str = "__storage-plugin";
pub const INTERNAL_SHELL_PLUGIN_ARGUMENT: &str = "__shell-plugin";
pub const INTERNAL_PATCH_PLUGIN_ARGUMENT: &str = "__patch-plugin";
pub const INTERNAL_FILES_PLUGIN_ARGUMENT: &str = "__files-plugin";
pub const INTERNAL_MCP_PLUGIN_ARGUMENT: &str = "__mcp-plugin";
pub const INTERNAL_SKILLS_PLUGIN_ARGUMENT: &str = "__skills-plugin";
pub const INTERNAL_MCP_FIXTURE_ARGUMENT: &str = "__mcp-fixture";

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
        CliAction::Web(options) => run_web(options),
        CliAction::Run(options) => {
            let color = !options.no_color
                && env::var_os("NO_COLOR").is_none()
                && io::stdout().is_terminal();
            let mut session = ChatSession::launch(options.plugin_path.as_deref())?;
            if let Some(prompt) = options.once {
                let reply = session.complete(&[ChatMessage::user(prompt)])?;
                for notice in session.drain_notices() {
                    eprintln!("warning: {notice}");
                }
                println!("{reply}");
                return Ok(());
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

fn run_web(options: WebOptions) -> Result<(), CliError> {
    let listener = TcpListener::bind(options.bind)?;
    let address = listener.local_addr()?;
    let host = WebHost::launch(options.plugin_path.as_deref())?;
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
    HttpCarrier::new(host).serve_until(listener, &shutdown)?;
    Ok(())
}

fn print_help() {
    println!(
        "YunXi Next\n\nUSAGE:\n    yunxi-next [OPTIONS]\n    yunxi-next web [OPTIONS]\n\nCLI OPTIONS:\n    --once <PROMPT>   Send one prompt and exit\n    --plugin <PATH>   Use an external compatible model plugin\n    --no-color        Disable ANSI terminal colors\n\nWEB OPTIONS:\n    --bind <ADDR>     Bind the HTTP server (default: 127.0.0.1:8787)\n    --plugin <PATH>   Use an external compatible model plugin\n\nGLOBAL OPTIONS:\n    -h, --help        Print help\n    -V, --version     Print version"
    );
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
