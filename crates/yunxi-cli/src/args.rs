//! Parsing for the small, stable public command-line surface.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CliAction {
    Run(CliOptions),
    Help,
    Version,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CliOptions {
    pub once: Option<String>,
    pub plugin_path: Option<PathBuf>,
    pub no_color: bool,
}

pub(crate) fn parse<I, S>(arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut arguments = arguments.into_iter().map(Into::into);
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArgumentError {
    UnknownOption(String),
    MissingValue(&'static str),
    EmptyValue(&'static str),
    NonUnicodeValue(&'static str),
    DuplicateOption(&'static str),
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(option) => write!(formatter, "unknown option `{option}`"),
            Self::MissingValue(option) => write!(formatter, "{option} requires a value"),
            Self::EmptyValue(option) => write!(formatter, "{option} cannot be empty"),
            Self::NonUnicodeValue(option) => {
                write!(formatter, "{option} must be valid Unicode text")
            }
            Self::DuplicateOption(option) => write!(formatter, "{option} was provided twice"),
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
}
