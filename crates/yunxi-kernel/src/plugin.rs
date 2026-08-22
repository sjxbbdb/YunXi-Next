use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const MAX_PLUGIN_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, PluginIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PluginIdError::Empty);
        }
        if value.len() > MAX_PLUGIN_ID_BYTES {
            return Err(PluginIdError::TooLong {
                length: value.len(),
                maximum: MAX_PLUGIN_ID_BYTES,
            });
        }
        for (index, character) in value.char_indices() {
            if !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.') {
                return Err(PluginIdError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for PluginId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginIdError {
    Empty,
    TooLong { length: usize, maximum: usize },
    InvalidCharacter { index: usize, character: char },
}

impl fmt::Display for PluginIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("plugin id cannot be empty"),
            Self::TooLong { length, maximum } => {
                write!(
                    formatter,
                    "plugin id is {length} bytes; maximum is {maximum}"
                )
            }
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "plugin id contains unsupported character `{character}` at byte {index}"
            ),
        }
    }
}

impl Error for PluginIdError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCommand {
    program: PathBuf,
    arguments: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
    current_dir: Option<PathBuf>,
}

impl PluginCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            current_dir: None,
        }
    }

    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub fn args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.environment
    }

    pub fn configured_current_dir(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }

    pub(crate) fn spawn(&self) -> std::io::Result<Child> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(current_dir) = &self.current_dir {
            command.current_dir(current_dir);
        }
        command.envs(&self.environment);
        command.spawn()
    }
}

impl From<&OsStr> for PluginCommand {
    fn from(program: &OsStr) -> Self {
        Self::new(program)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSpec {
    id: PluginId,
    display_name: String,
    command: PluginCommand,
}

impl PluginSpec {
    pub fn new(id: PluginId, command: PluginCommand) -> Self {
        let display_name = id.to_string();
        Self {
            id,
            display_name,
            command,
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        let display_name = display_name.into();
        if !display_name.trim().is_empty() {
            self.display_name = display_name;
        }
        self
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn command(&self) -> &PluginCommand {
        &self.command
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginFailure {
    Spawn { message: String },
    UnexpectedExit { code: Option<i32> },
    Monitor { message: String },
    Supervisor { message: String },
}

impl fmt::Display for PluginFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { message } => write!(formatter, "process failed to start: {message}"),
            Self::UnexpectedExit { code: Some(code) } => {
                write!(formatter, "process exited unexpectedly with code {code}")
            }
            Self::UnexpectedExit { code: None } => {
                formatter.write_str("process exited unexpectedly without an exit code")
            }
            Self::Monitor { message } => write!(formatter, "process monitor failed: {message}"),
            Self::Supervisor { message } => write!(formatter, "supervisor failed: {message}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginState {
    Registered,
    Starting,
    Running { pid: u32 },
    Stopping,
    Stopped,
    Failed(PluginFailure),
}

impl PluginState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running { .. } | Self::Stopping)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed(_))
    }
}

impl fmt::Display for PluginState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registered => formatter.write_str("registered"),
            Self::Starting => formatter.write_str("starting"),
            Self::Running { pid } => write!(formatter, "running as process {pid}"),
            Self::Stopping => formatter.write_str("stopping"),
            Self::Stopped => formatter.write_str("stopped"),
            Self::Failed(failure) => write!(formatter, "failed: {failure}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSnapshot {
    id: PluginId,
    display_name: String,
    state: PluginState,
    generation: u64,
}

impl PluginSnapshot {
    pub(crate) fn new(
        id: PluginId,
        display_name: String,
        state: PluginState,
        generation: u64,
    ) -> Self {
        Self {
            id,
            display_name,
            state,
            generation,
        }
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn state(&self) -> &PluginState {
        &self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_ids_accept_stable_ascii_identifiers() {
        let id = PluginId::new("yunxi.voice_v1").expect("valid plugin id");
        assert_eq!(id.as_str(), "yunxi.voice_v1");
    }

    #[test]
    fn plugin_ids_reject_path_like_values() {
        let error = PluginId::new("../voice").expect_err("path-like id must fail");
        assert!(matches!(
            error,
            PluginIdError::InvalidCharacter { character: '/', .. }
        ));
    }
}
