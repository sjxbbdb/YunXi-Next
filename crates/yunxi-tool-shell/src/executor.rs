//! Validation and bounded child-process execution for the shell capability.

use std::error::Error;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use yunxi_protocol::{ActionGrantError, ShellExecuteRequest, ShellExecuteResult};

const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_TIMEOUT_MILLIS: u64 = 600_000;

pub fn execute(request: &ShellExecuteRequest) -> Result<ShellExecuteResult, ShellError> {
    let grant = request.grant();
    grant.validate().map_err(ShellError::InvalidGrant)?;
    if !grant.workspace().allows_legacy_read() {
        return Err(ShellError::ReadNotGranted);
    }

    let command = request.command().trim();
    if command.is_empty() {
        return Err(ShellError::EmptyCommand);
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Err(ShellError::CommandTooLong {
            length: command.len(),
            maximum: MAX_COMMAND_BYTES,
        });
    }
    if command.contains('\0') {
        return Err(ShellError::NulInCommand);
    }
    if !grant.allow_network() && looks_like_network_command(command) {
        return Err(ShellError::NetworkNotGranted);
    }
    if !grant.allow_write() && looks_like_write_command(command) {
        return Err(ShellError::WriteNotGranted);
    }

    let (root, cwd) = resolve_scope(grant.workspace().root(), grant.working_directory())?;
    if !cwd.starts_with(&root) {
        return Err(ShellError::OutsideWorkspace { cwd, root });
    }

    let (program, arguments) = shell_argv(command);
    let mut child = command_builder(&program, &arguments, &cwd)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ShellError::Io("stdout pipe was not created".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ShellError::Io("stderr pipe was not created".to_string()))?;
    let output_limit = grant.max_output_bytes();
    let stdout_thread = thread::spawn(move || read_bounded(stdout, output_limit));
    let stderr_thread = thread::spawn(move || read_bounded(stderr, output_limit));

    let timeout = Duration::from_millis(grant.timeout_millis().min(MAX_TIMEOUT_MILLIS));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ignored = child.kill();
                break child
                    .wait()
                    .map_err(|error| ShellError::Wait(error.to_string()))?;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ignored = child.kill();
                let _ignored = child.wait();
                return Err(ShellError::Wait(error.to_string()));
            }
        }
    };

    let (stdout, stdout_truncated) = stdout_thread
        .join()
        .map_err(|_| ShellError::Io("stdout reader panicked".to_string()))?
        .map_err(|error| ShellError::Io(error.to_string()))?;
    let (stderr, stderr_truncated) = stderr_thread
        .join()
        .map_err(|_| ShellError::Io("stderr reader panicked".to_string()))?
        .map_err(|error| ShellError::Io(error.to_string()))?;

    Ok(ShellExecuteResult::new(
        status.code(),
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
        timed_out,
        stdout_truncated || stderr_truncated,
    ))
}

fn resolve_scope(root: &Path, cwd: &Path) -> Result<(PathBuf, PathBuf), ShellError> {
    let root = std::fs::canonicalize(root).map_err(|error| ShellError::Workspace {
        path: root.to_path_buf(),
        message: error.to_string(),
    })?;
    let cwd = std::fs::canonicalize(cwd).map_err(|error| ShellError::Workspace {
        path: cwd.to_path_buf(),
        message: error.to_string(),
    })?;
    if !cwd.is_dir() {
        return Err(ShellError::Workspace {
            path: cwd,
            message: "working directory is not a directory".to_string(),
        });
    }
    Ok((root, cwd))
}

fn shell_argv(command: &str) -> (PathBuf, Vec<String>) {
    if cfg!(windows) {
        let program = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("cmd.exe"));
        (
            program,
            vec!["/D".to_string(), "/C".to_string(), command.to_string()],
        )
    } else {
        let program = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        (program, vec!["-c".to_string(), command.to_string()])
    }
}

fn command_builder(
    program: &Path,
    arguments: &[String],
    cwd: &Path,
) -> Result<std::process::Child, ShellError> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    copy_safe_environment(&mut command);
    command
        .spawn()
        .map_err(|error| ShellError::Spawn(error.to_string()))
}

fn copy_safe_environment(command: &mut Command) {
    for name in [
        "PATH",
        "Path",
        "SystemRoot",
        "ComSpec",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "HOME",
        "USER",
        "TMPDIR",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn read_bounded<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool), io::Error>
where
    R: Read,
{
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        if remaining > 0 {
            output.extend_from_slice(&buffer[..count.min(remaining)]);
        }
        if count > remaining {
            truncated = true;
        }
    }
    Ok((output, truncated))
}

fn looks_like_network_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "curl ",
        "wget ",
        "invoke-webrequest",
        "invoke-restmethod",
        " irm ",
        "ssh ",
        "scp ",
        "sftp ",
        "ftp ",
        " nc ",
        "netcat ",
    ]
    .iter()
    .any(|marker| lower.starts_with(marker.trim()) || lower.contains(marker))
}

fn looks_like_write_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        ">",
        "rm ",
        "del ",
        "remove-item",
        "set-content",
        "out-file",
        "mkdir ",
        "new-item",
        "touch ",
        " mv ",
        "move-item",
        " cp ",
        "copy-item",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[derive(Debug)]
pub enum ShellError {
    InvalidGrant(ActionGrantError),
    ReadNotGranted,
    WriteNotGranted,
    NetworkNotGranted,
    EmptyCommand,
    CommandTooLong { length: usize, maximum: usize },
    NulInCommand,
    Workspace { path: PathBuf, message: String },
    OutsideWorkspace { cwd: PathBuf, root: PathBuf },
    Spawn(String),
    Wait(String),
    Io(String),
}

impl ShellError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidGrant(_) | Self::ReadNotGranted => "grant_denied",
            Self::WriteNotGranted => "write_not_granted",
            Self::NetworkNotGranted => "network_not_granted",
            Self::EmptyCommand | Self::CommandTooLong { .. } | Self::NulInCommand => {
                "invalid_command"
            }
            Self::Workspace { .. } | Self::OutsideWorkspace { .. } => "workspace_denied",
            Self::Spawn(_) => "spawn_failed",
            Self::Wait(_) => "execution_failed",
            Self::Io(_) => "output_failed",
        }
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGrant(error) => write!(formatter, "invalid action grant: {error}"),
            Self::ReadNotGranted => {
                formatter.write_str("workspace read permission was not granted")
            }
            Self::WriteNotGranted => {
                formatter.write_str("command appears to write but write permission was not granted")
            }
            Self::NetworkNotGranted => formatter.write_str(
                "command appears to use the network but network permission was not granted",
            ),
            Self::EmptyCommand => formatter.write_str("shell command cannot be empty"),
            Self::CommandTooLong { length, maximum } => write!(
                formatter,
                "shell command is {length} bytes; maximum is {maximum}"
            ),
            Self::NulInCommand => formatter.write_str("shell command contains a NUL byte"),
            Self::Workspace { path, message } => write!(
                formatter,
                "workspace path {} is unavailable: {message}",
                path.display()
            ),
            Self::OutsideWorkspace { cwd, root } => write!(
                formatter,
                "working directory {} is outside workspace {}",
                cwd.display(),
                root.display()
            ),
            Self::Spawn(message) => write!(formatter, "failed to start shell: {message}"),
            Self::Wait(message) => write!(formatter, "shell wait failed: {message}"),
            Self::Io(message) => write!(formatter, "shell output failed: {message}"),
        }
    }
}

impl Error for ShellError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidGrant(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yunxi_protocol::WorkspaceGrant;

    fn grant(root: &Path) -> yunxi_protocol::ActionGrant {
        yunxi_protocol::ActionGrant::approved(
            WorkspaceGrant::read_write(root).with_workspace_write(),
            root,
            "fixture-ticket",
        )
        .with_write(true)
    }

    #[test]
    fn approved_command_runs_with_bounded_result() {
        let root = std::env::temp_dir();
        let request = ShellExecuteRequest::new(
            grant(&root),
            if cfg!(windows) {
                "echo fixture"
            } else {
                "printf fixture"
            },
        );
        let result = execute(&request).expect("execute fixture command");
        assert_eq!(result.exit_code(), Some(0));
        assert!(result.stdout().contains("fixture"));
        assert!(!result.timed_out());
    }

    #[test]
    fn denied_grant_never_spawns_a_command() {
        let root = std::env::temp_dir();
        let request = ShellExecuteRequest::new(
            yunxi_protocol::ActionGrant::pending(WorkspaceGrant::read_only(&root), &root),
            "echo fixture",
        );
        let error = execute(&request).expect_err("denied grant");
        assert_eq!(error.code(), "grant_denied");
    }

    #[test]
    fn read_only_grant_rejects_obvious_write_command() {
        let root = std::env::temp_dir();
        let request = ShellExecuteRequest::new(
            yunxi_protocol::ActionGrant::approved(
                WorkspaceGrant::read_only(&root),
                &root,
                "fixture-ticket",
            ),
            if cfg!(windows) {
                "echo fixture > output.txt"
            } else {
                "printf fixture > output.txt"
            },
        );
        let error = execute(&request).expect_err("write command must be denied");
        assert_eq!(error.code(), "write_not_granted");
    }
}
