//! Parsing for the small, stable public command-line surface.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

const DEFAULT_WEB_BIND: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8787);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CliAction {
    Run(CliOptions),
    Web(WebOptions),
    Control(ControlOptions),
    Help,
    Version,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CliOptions {
    pub once: Option<String>,
    pub plugin_path: Option<PathBuf>,
    pub no_color: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WebOptions {
    pub bind: SocketAddr,
    pub plugin_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ControlOptions {
    pub command: ControlCommand,
    pub json: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ControlCommand {
    Status,
    Enable(String),
    Disable(String),
    Reload(Option<String>),
    Diagnostics,
}

impl Default for WebOptions {
    fn default() -> Self {
        Self {
            bind: DEFAULT_WEB_BIND,
            plugin_path: None,
        }
    }
}

pub(crate) fn parse<I, S>(arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut arguments = arguments.into_iter().map(Into::into);
    let first = arguments.next();
    if first.as_deref() == Some(OsStr::new("web")) {
        return parse_web(arguments);
    }

    if let Some("status" | "diagnostics" | "enable" | "disable" | "reload") =
        first.as_deref().and_then(OsStr::to_str)
    {
        // Keep command names in the first position so existing run-mode
        // options retain their exact parsing behavior.
        return parse_control(first.as_deref().and_then(OsStr::to_str).unwrap(), arguments);
    }

    parse_run(first.into_iter().chain(arguments))
}

fn parse_control<I>(command: &str, arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = arguments.into_iter();
    let mut json = false;
    let mut positional = None;

    for argument in arguments {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--json") => {
                if json {
                    return Err(ArgumentError::DuplicateOption("--json"));
                }
                json = true;
            }
            Some(value) if value.starts_with('-') => {
                return Err(ArgumentError::UnknownOption(value.to_string()));
            }
            _ => {
                let value = argument
                    .into_string()
                    .map_err(|_| ArgumentError::NonUnicodeValue("plugin-id"))?;
                if value.trim().is_empty() {
                    return Err(ArgumentError::EmptyValue("plugin-id"));
                }
                if positional.is_some() {
                    return Err(ArgumentError::UnexpectedArgument(value));
                }
                positional = Some(value);
            }
        }
    }

    let command = match command {
        "status" => {
            reject_positional(command, positional)?;
            ControlCommand::Status
        }
        "diagnostics" => {
            reject_positional(command, positional)?;
            ControlCommand::Diagnostics
        }
        "enable" => {
            ControlCommand::Enable(positional.ok_or(ArgumentError::MissingValue("plugin-id"))?)
        }
        "disable" => {
            ControlCommand::Disable(positional.ok_or(ArgumentError::MissingValue("plugin-id"))?)
        }
        "reload" => ControlCommand::Reload(positional),
        _ => unreachable!("control command was checked by parse"),
    };

    Ok(CliAction::Control(ControlOptions { command, json }))
}

fn reject_positional(command: &str, positional: Option<String>) -> Result<(), ArgumentError> {
    if let Some(value) = positional {
        return Err(ArgumentError::UnexpectedArgument(format!(
            "{command} {value}"
        )));
    }
    Ok(())
}

fn parse_run<I>(arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut options = CliOptions::default();

    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--no-color") => options.no_color = true,
            Some("--once") => {
                if options.once.is_some() {
                    return Err(ArgumentError::DuplicateOption("--once"));
                }
                let value = arguments
                    .next()
                    .ok_or(ArgumentError::MissingValue("--once"))?
                    .into_string()
                    .map_err(|_| ArgumentError::NonUnicodeValue("--once"))?;
                if value.trim().is_empty() {
                    return Err(ArgumentError::EmptyValue("--once"));
                }
                options.once = Some(value);
            }
            Some("--plugin") => {
                if options.plugin_path.is_some() {
                    return Err(ArgumentError::DuplicateOption("--plugin"));
                }
                let value = arguments
                    .next()
                    .ok_or(ArgumentError::MissingValue("--plugin"))?;
                if value.is_empty() {
                    return Err(ArgumentError::EmptyValue("--plugin"));
                }
                options.plugin_path = Some(PathBuf::from(value));
            }
            _ => {
                return Err(ArgumentError::UnknownOption(
                    argument.to_string_lossy().into_owned(),
                ));
            }
        }
    }

    Ok(CliAction::Run(options))
}

fn parse_web<I>(arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut options = WebOptions::default();
    let mut bind_seen = false;

    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--bind") => {
                let value = arguments
                    .next()
                    .ok_or(ArgumentError::MissingValue("--bind"))?
                    .into_string()
                    .map_err(|_| ArgumentError::NonUnicodeValue("--bind"))?;
                if value.trim().is_empty() {
                    return Err(ArgumentError::EmptyValue("--bind"));
                }
                if bind_seen {
                    return Err(ArgumentError::DuplicateOption("--bind"));
                }
                bind_seen = true;
                options.bind = value
                    .parse()
                    .map_err(|_| ArgumentError::InvalidValue("--bind", value.clone()))?;
            }
            Some("--plugin") => {
                if options.plugin_path.is_some() {
                    return Err(ArgumentError::DuplicateOption("--plugin"));
                }
                let value = arguments
                    .next()
                    .ok_or(ArgumentError::MissingValue("--plugin"))?;
                if value.is_empty() {
                    return Err(ArgumentError::EmptyValue("--plugin"));
                }
                options.plugin_path = Some(PathBuf::from(value));
            }
            _ => {
                return Err(ArgumentError::UnknownOption(
                    argument.to_string_lossy().into_owned(),
                ));
            }
        }
    }

    Ok(CliAction::Web(options))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArgumentError {
    UnknownOption(String),
    UnexpectedArgument(String),
    MissingValue(&'static str),
    EmptyValue(&'static str),
    NonUnicodeValue(&'static str),
    DuplicateOption(&'static str),
    InvalidValue(&'static str, String),
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(option) => write!(formatter, "unknown option `{option}`"),
            Self::UnexpectedArgument(argument) => {
                write!(formatter, "unexpected argument `{argument}`")
            }
            Self::MissingValue(option) => write!(formatter, "{option} requires a value"),
            Self::EmptyValue(option) => write!(formatter, "{option} cannot be empty"),
            Self::NonUnicodeValue(option) => {
                write!(formatter, "{option} must be valid Unicode text")
            }
            Self::DuplicateOption(option) => write!(formatter, "{option} was provided twice"),
            Self::InvalidValue(option, value) => {
                write!(formatter, "{option} has invalid value `{value}`")
            }
        }
    }
}

impl Error for ArgumentError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_prompt_and_external_plugin() {
        let action = parse([
            "--once",
            "hello",
            "--plugin",
            "C:/plugins/model.exe",
            "--no-color",
        ])
        .expect("parse options");

        assert_eq!(
            action,
            CliAction::Run(CliOptions {
                once: Some("hello".to_string()),
                plugin_path: Some(PathBuf::from("C:/plugins/model.exe")),
                no_color: true,
            })
        );
    }

    #[test]
    fn rejects_missing_option_values() {
        assert_eq!(
            parse(["--once"]),
            Err(ArgumentError::MissingValue("--once"))
        );
    }

    #[test]
    fn uses_a_standalone_command_surface() {
        assert_eq!(
            parse(std::iter::empty::<&str>()),
            Ok(CliAction::Run(CliOptions::default()))
        );
        assert_eq!(
            parse(["next"]),
            Err(ArgumentError::UnknownOption("next".to_string()))
        );
    }

    #[test]
    fn parses_web_server_options_without_changing_the_cli_defaults() {
        assert_eq!(
            parse(["web", "--bind", "127.0.0.1:0", "--plugin", "model.exe"]),
            Ok(CliAction::Web(WebOptions {
                bind: "127.0.0.1:0".parse().expect("socket address"),
                plugin_path: Some(PathBuf::from("model.exe")),
            }))
        );
        assert_eq!(
            parse(std::iter::empty::<&str>()),
            Ok(CliAction::Run(CliOptions::default()))
        );
        assert_eq!(
            parse(["web", "--bind", "not-an-address"]),
            Err(ArgumentError::InvalidValue(
                "--bind",
                "not-an-address".to_string()
            ))
        );
    }

    #[test]
    fn parses_control_commands_without_reinterpreting_run_or_web_options() {
        assert_eq!(
            parse(["status", "--json"]),
            Ok(CliAction::Control(ControlOptions {
                command: ControlCommand::Status,
                json: true,
            }))
        );
        assert_eq!(
            parse(["enable", "yunxi.tool.shell"]),
            Ok(CliAction::Control(ControlOptions {
                command: ControlCommand::Enable("yunxi.tool.shell".to_string()),
                json: false,
            }))
        );
        assert_eq!(
            parse(["disable", "yunxi.tool.shell", "--json"]),
            Ok(CliAction::Control(ControlOptions {
                command: ControlCommand::Disable("yunxi.tool.shell".to_string()),
                json: true,
            }))
        );
        assert_eq!(
            parse(["reload", "yunxi.tool.shell"]),
            Ok(CliAction::Control(ControlOptions {
                command: ControlCommand::Reload(Some("yunxi.tool.shell".to_string())),
                json: false,
            }))
        );
        assert_eq!(
            parse(["diagnostics"]),
            Ok(CliAction::Control(ControlOptions {
                command: ControlCommand::Diagnostics,
                json: false,
            }))
        );

        assert_eq!(
            parse(["enable"]),
            Err(ArgumentError::MissingValue("plugin-id"))
        );
        assert_eq!(
            parse(["status", "unexpected"]),
            Err(ArgumentError::UnexpectedArgument(
                "status unexpected".to_string()
            ))
        );
        assert_eq!(
            parse(["status", "--json", "--json"]),
            Err(ArgumentError::DuplicateOption("--json"))
        );
    }
}
