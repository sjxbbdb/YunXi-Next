#![doc = "Terminal chat host for YunXi Next."]
#![forbid(unsafe_code)]

mod args;
mod repl;
mod session;
mod ui;

use std::env;
use std::error::Error;
use std::fmt;
use std::io::{self, IsTerminal};

use args::{ArgumentError, CliAction};
use session::{ChatBackend, ChatFailure, ChatSession, SessionError};
use yunxi_protocol::ChatMessage;

pub const INTERNAL_MODEL_PLUGIN_ARGUMENT: &str = "__model-plugin";

pub fn run_from_env() -> Result<(), CliError> {
    match args::parse(env::args_os().skip(1))? {
        CliAction::Help => {
            print_help();
            Ok(())
        }
        CliAction::Version => {
            println!("yunxi next {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        CliAction::Run(options) => {
            let color = !options.no_color
                && env::var_os("NO_COLOR").is_none()
                && io::stdout().is_terminal();
            let mut session = ChatSession::launch(options.plugin_path.as_deref())?;
            if let Some(prompt) = options.once {
                let reply = session.complete(&[ChatMessage::user(prompt)])?;
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

fn print_help() {
    println!(
        "YunXi Next\n\nUSAGE:\n    yunxi next [OPTIONS]\n\nOPTIONS:\n    --once <PROMPT>   Send one prompt and exit\n    --plugin <PATH>   Use an external compatible model plugin\n    --no-color        Disable ANSI terminal colors\n    -h, --help        Print help\n    -V, --version     Print version"
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

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self {
            kind: CliErrorKind::Io(error),
        }
    }
}
