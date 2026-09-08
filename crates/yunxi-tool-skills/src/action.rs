//! Opt-in, process-isolated execution for explicitly declared Skill actions.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use yunxi_protocol::{
    ActionGrant, ActionGrantError, MAX_SKILL_ACTION_FRAME_BYTES, SkillActionOutcome,
    SkillActionRequest, SkillActionResponse, SkillProtocolError,
};

use crate::discovery::discover;
use crate::{SkillsConfig, SkillsConfigError};

static AUDIT_COUNTER: AtomicU64 = AtomicU64::new(1);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Host-owned executor for the opt-in Skill action boundary.
#[derive(Clone, Debug)]
pub struct SkillActionExecutor {
    config: SkillsConfig,
}

impl SkillActionExecutor {
    pub fn new(config: SkillsConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &SkillsConfig {
        &self.config
    }

    /// Returns a bounded, non-executable projection of declared actions.
    ///
    /// Program paths and fixed arguments stay inside this crate. The Host only
    /// needs the tool identity and workspace-write requirement to decide
    /// whether the model tool is executable and which approval to request.
    pub fn declarations(&self) -> Result<Vec<SkillActionDeclaration>, SkillActionError> {
        let snapshot = discover(&self.config)?;
        Ok(snapshot
            .skills
            .iter()
            .flat_map(|skill| {
                skill.actions.iter().map(|action| {
                    SkillActionDeclaration::new(
                        skill.metadata.id(),
                        action.tool_name(),
                        action.requires_workspace_write(),
                    )
                })
            })
            .collect())
    }

    /// Executes one declared action after all host-side policy checks pass.
    ///
    /// The method is deliberately not part of the metadata plugin loop. A
    /// caller must opt in through [`SkillsConfig::with_actions_enabled`] and
    /// provide an approved [`ActionGrant`].
    pub fn execute(
        &self,
        request: &SkillActionRequest,
        grant: &ActionGrant,
    ) -> Result<SkillActionExecution, SkillActionError> {
        self.execute_with_cancellation(request, grant, || false)
    }

    /// Executes an action while polling a Host-owned cancellation source.
    ///
    /// The callback is intentionally Host-owned: the Skill child receives no
    /// cancellation token or grant. Once cancellation is observed, the child
    /// is killed and the result is reported as `Cancelled`.
    pub fn execute_with_cancellation<F>(
        &self,
        request: &SkillActionRequest,
        grant: &ActionGrant,
        is_cancelled: F,
    ) -> Result<SkillActionExecution, SkillActionError>
    where
        F: Fn() -> bool,
    {
        request.validate()?;
        if !self.config.actions_enabled() {
            return Err(SkillActionError::ActionsDisabled);
        }
        grant.validate().map_err(SkillActionError::Grant)?;
        if grant.allow_network() || !grant.secret_grant().is_empty() {
            return Err(SkillActionError::ForbiddenGrant);
        }

        let snapshot = discover(&self.config)?;
        let skill = snapshot
            .skills
            .iter()
            .find(|skill| skill.metadata.id() == request.skill_id())
            .ok_or_else(|| SkillActionError::SkillNotFound {
                skill_id: request.skill_id().to_string(),
            })?;
        let action = skill
            .actions
            .iter()
            .find(|action| action.tool_name() == request.tool_name())
            .ok_or_else(|| SkillActionError::ActionNotDeclared {
                skill_id: request.skill_id().to_string(),
                tool_name: request.tool_name().to_string(),
            })?;
        if action.requires_workspace_write() && !grant.allow_write() {
            return Err(SkillActionError::WorkspaceWriteRequired);
        }

        let skill_root = canonical_directory(self.config.root(), "Skills root")?;
        let workspace_root = canonical_directory(grant.workspace().root(), "workspace root")?;
        let working_directory =
            canonical_directory(grant.working_directory(), "Skill action working directory")?;
        ensure_within(&workspace_root, &skill_root, "Skills root")?;
        ensure_within(
            &workspace_root,
            &working_directory,
            "Skill action working directory",
        )?;
        ensure_within(&skill_root, &skill.directory, "Skill directory")?;
        let program = canonical_file(&skill.directory.join(action.program()))?;
        ensure_within(&skill.directory, &program, "Skill action program")?;

        let timeout = Duration::from_millis(action.timeout_millis().min(grant.timeout_millis()));
        let max_output_bytes = action.max_output_bytes().min(grant.max_output_bytes());
        run_child(
            request,
            action,
            &program,
            &working_directory,
            timeout,
            max_output_bytes,
            &is_cancelled,
        )
    }
}

/// Admission metadata exposed to the Host without executable path details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillActionDeclaration {
    skill_id: String,
    tool_name: String,
    requires_workspace_write: bool,
}

impl SkillActionDeclaration {
    fn new(skill_id: &str, tool_name: &str, requires_workspace_write: bool) -> Self {
        Self {
            skill_id: skill_id.to_string(),
            tool_name: tool_name.to_string(),
            requires_workspace_write,
        }
    }

    pub fn skill_id(&self) -> &str {
        &self.skill_id
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub const fn requires_workspace_write(&self) -> bool {
        self.requires_workspace_write
    }
}

/// Auditable result returned by the Host-side executor.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SkillActionExecution {
    audit_id: String,
    skill_id: String,
    tool_name: String,
    program: String,
    outcome: SkillActionOutcome,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    output_truncated: bool,
    duration_millis: u64,
}

impl SkillActionExecution {
    pub fn audit_id(&self) -> &str {
        &self.audit_id
    }

    pub fn skill_id(&self) -> &str {
        &self.skill_id
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub const fn outcome(&self) -> SkillActionOutcome {
        self.outcome
    }

    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    pub const fn output_truncated(&self) -> bool {
        self.output_truncated
    }

    pub const fn duration_millis(&self) -> u64 {
        self.duration_millis
    }
}

fn run_child(
    request: &SkillActionRequest,
    action: &yunxi_protocol::SkillActionSpec,
    program: &Path,
    working_directory: &Path,
    timeout: Duration,
    max_output_bytes: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<SkillActionExecution, SkillActionError> {
    let request_bytes = serde_json::to_vec(request).map_err(SkillActionError::Serialize)?;
    if request_bytes.len() > MAX_SKILL_ACTION_FRAME_BYTES {
        return Err(SkillActionError::FrameTooLarge {
            size: request_bytes.len(),
            maximum: MAX_SKILL_ACTION_FRAME_BYTES,
        });
    }

    let started = Instant::now();
    let mut child = Command::new(program)
        .args(action.arguments())
        .current_dir(working_directory)
        .env_clear()
        .env(
            "YUNXI_SKILL_ACTION_PROTOCOL_VERSION",
            yunxi_protocol::SKILL_ACTION_PROTOCOL_VERSION.to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| SkillActionError::Spawn {
            program: program.to_path_buf(),
            source,
        })?;

    let stdout = child
        .stdout
        .take()
        .ok_or(SkillActionError::MissingPipe("stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(SkillActionError::MissingPipe("stderr"))?;
    let stdout_thread = thread::spawn(|| read_bounded(stdout));
    let stderr_thread = thread::spawn(|| read_bounded(stderr));
    let stdin = child
        .stdin
        .take()
        .ok_or(SkillActionError::MissingPipe("stdin"))?;
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        stdin.write_all(&request_bytes)?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    });

    let deadline = started + timeout;
    let termination = loop {
        match child.try_wait() {
            Ok(Some(status)) => break ChildTermination::Exited(status),
            Ok(None) if is_cancelled() => {
                let _ = child.kill();
                let _ = child.wait();
                break ChildTermination::Cancelled;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break ChildTermination::TimedOut;
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SkillActionError::Wait { source });
            }
        }
    };

    let writer_result = writer
        .join()
        .map_err(|_| SkillActionError::WorkerPanicked("stdin"))?;
    let stdout = stdout_thread
        .join()
        .map_err(|_| SkillActionError::WorkerPanicked("stdout"))?
        .map_err(|source| SkillActionError::Io {
            operation: "read action stdout",
            source,
        })?;
    let process_stderr = stderr_thread
        .join()
        .map_err(|_| SkillActionError::WorkerPanicked("stderr"))?
        .map_err(|source| SkillActionError::Io {
            operation: "read action stderr",
            source,
        })?;
    let duration_millis = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let audit_id = new_audit_id();

    if process_stderr.oversized {
        return Err(SkillActionError::FrameTooLarge {
            size: MAX_SKILL_ACTION_FRAME_BYTES + 1,
            maximum: MAX_SKILL_ACTION_FRAME_BYTES,
        });
    }
    let process_stderr = String::from_utf8_lossy(&process_stderr.bytes);

    let ChildTermination::Exited(status) = termination else {
        let (outcome, message) = match termination {
            ChildTermination::TimedOut => (SkillActionOutcome::TimedOut, "Skill action timed out"),
            ChildTermination::Cancelled => {
                (SkillActionOutcome::Cancelled, "Skill action cancelled")
            }
            ChildTermination::Exited(_) => unreachable!(),
        };
        if process_stderr.len() > max_output_bytes {
            return Err(SkillActionError::OutputLimit {
                maximum: max_output_bytes,
            });
        }
        return Ok(execution(
            audit_id,
            request,
            action,
            ExecutionData {
                outcome,
                exit_code: None,
                stdout: String::new(),
                stderr: if process_stderr.is_empty() {
                    message.to_string()
                } else {
                    format!("{message}: {process_stderr}")
                },
                output_truncated: false,
                duration_millis,
            },
        ));
    };
    if let Err(source) = writer_result {
        return Err(SkillActionError::Io {
            operation: "write action request",
            source,
        });
    }
    if stdout.oversized {
        return Err(SkillActionError::FrameTooLarge {
            size: MAX_SKILL_ACTION_FRAME_BYTES + 1,
            maximum: MAX_SKILL_ACTION_FRAME_BYTES,
        });
    }
    let output = String::from_utf8(stdout.bytes).map_err(|_| SkillActionError::InvalidUtf8)?;
    let response = serde_json::from_str::<SkillActionResponse>(output.trim_end())
        .map_err(|source| SkillActionError::BadResponse(source.to_string()))?;
    response.validate()?;
    let stderr = merge_stderr(response.stderr(), &process_stderr);
    if response.stdout().len().saturating_add(stderr.len()) > max_output_bytes {
        return Err(SkillActionError::OutputLimit {
            maximum: max_output_bytes,
        });
    }
    let exit_code = status.code();
    if response.exit_code() != exit_code {
        return Err(SkillActionError::BadResponse(
            "response exit_code did not match process status".to_string(),
        ));
    }
    let outcome = if response.outcome() == SkillActionOutcome::Success && status.success() {
        SkillActionOutcome::Success
    } else if matches!(
        response.outcome(),
        SkillActionOutcome::TimedOut | SkillActionOutcome::Cancelled
    ) {
        response.outcome()
    } else {
        SkillActionOutcome::Failed
    };
    Ok(execution(
        audit_id,
        request,
        action,
        ExecutionData {
            outcome,
            exit_code,
            stdout: response.stdout().to_string(),
            stderr,
            output_truncated: response.output_truncated(),
            duration_millis,
        },
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildTermination {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

fn merge_stderr(response_stderr: &str, process_stderr: &str) -> String {
    match (response_stderr.is_empty(), process_stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => response_stderr.to_string(),
        (true, false) => process_stderr.to_string(),
        (false, false) => format!("{response_stderr}\n{process_stderr}"),
    }
}

struct ExecutionData {
    outcome: SkillActionOutcome,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    output_truncated: bool,
    duration_millis: u64,
}

fn execution(
    audit_id: String,
    request: &SkillActionRequest,
    action: &yunxi_protocol::SkillActionSpec,
    data: ExecutionData,
) -> SkillActionExecution {
    SkillActionExecution {
        audit_id,
        skill_id: request.skill_id().to_string(),
        tool_name: request.tool_name().to_string(),
        program: action.program().to_string(),
        outcome: data.outcome,
        exit_code: data.exit_code,
        stdout: data.stdout,
        stderr: data.stderr,
        output_truncated: data.output_truncated,
        duration_millis: data.duration_millis,
    }
}

struct BoundedBytes {
    bytes: Vec<u8>,
    oversized: bool,
}

fn read_bounded<R: Read>(mut reader: R) -> io::Result<BoundedBytes> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut oversized = false;
    let mut total = 0usize;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let previous = total;
        total = total.saturating_add(count);
        if previous < MAX_SKILL_ACTION_FRAME_BYTES {
            let remaining = MAX_SKILL_ACTION_FRAME_BYTES - previous;
            bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        }
        if total > MAX_SKILL_ACTION_FRAME_BYTES {
            oversized = true;
        }
    }
    Ok(BoundedBytes { bytes, oversized })
}

fn canonical_directory(path: &Path, label: &'static str) -> Result<PathBuf, SkillActionError> {
    let canonical = fs::canonicalize(path).map_err(|source| SkillActionError::Path {
        label,
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(SkillActionError::NotDirectory {
            label,
            path: canonical,
        });
    }
    Ok(canonical)
}

fn canonical_file(path: &Path) -> Result<PathBuf, SkillActionError> {
    let canonical = fs::canonicalize(path).map_err(|source| SkillActionError::Path {
        label: "Skill action program",
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_file() {
        return Err(SkillActionError::NotFile { path: canonical });
    }
    Ok(canonical)
}

fn ensure_within(root: &Path, path: &Path, label: &'static str) -> Result<(), SkillActionError> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(SkillActionError::OutOfScope {
            label,
            path: path.to_path_buf(),
        })
    }
}

fn new_audit_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = AUDIT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("skill-action-{timestamp:x}-{counter:x}")
}

#[derive(Debug)]
pub enum SkillActionError {
    ActionsDisabled,
    Grant(ActionGrantError),
    ForbiddenGrant,
    WorkspaceWriteRequired,
    Request(SkillProtocolError),
    Discovery(crate::discovery::DiscoveryError),
    Config(SkillsConfigError),
    SkillNotFound {
        skill_id: String,
    },
    ActionNotDeclared {
        skill_id: String,
        tool_name: String,
    },
    Path {
        label: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    NotDirectory {
        label: &'static str,
        path: PathBuf,
    },
    NotFile {
        path: PathBuf,
    },
    OutOfScope {
        label: &'static str,
        path: PathBuf,
    },
    Serialize(serde_json::Error),
    Spawn {
        program: PathBuf,
        source: io::Error,
    },
    Wait {
        source: io::Error,
    },
    Io {
        operation: &'static str,
        source: io::Error,
    },
    MissingPipe(&'static str),
    WorkerPanicked(&'static str),
    InvalidUtf8,
    FrameTooLarge {
        size: usize,
        maximum: usize,
    },
    BadResponse(String),
    OutputLimit {
        maximum: usize,
    },
}

impl fmt::Display for SkillActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActionsDisabled => formatter.write_str("executable Skill actions are disabled"),
            Self::Grant(error) => error.fmt(formatter),
            Self::ForbiddenGrant => {
                formatter.write_str("Skill actions cannot receive network or secret authority")
            }
            Self::WorkspaceWriteRequired => {
                formatter.write_str("Skill action requires an approved workspace write grant")
            }
            Self::Request(error) => error.fmt(formatter),
            Self::Discovery(error) => error.fmt(formatter),
            Self::Config(error) => error.fmt(formatter),
            Self::SkillNotFound { skill_id } => {
                write!(formatter, "Skill `{skill_id}` was not found")
            }
            Self::ActionNotDeclared {
                skill_id,
                tool_name,
            } => write!(
                formatter,
                "Skill `{skill_id}` has no executable declaration for `{tool_name}`"
            ),
            Self::Path {
                label,
                path,
                source,
            } => write!(
                formatter,
                "cannot resolve {label} {}: {source}",
                path.display()
            ),
            Self::NotDirectory { label, path } => {
                write!(formatter, "{label} is not a directory: {}", path.display())
            }
            Self::NotFile { path } => write!(
                formatter,
                "Skill action program is not a file: {}",
                path.display()
            ),
            Self::OutOfScope { label, path } => write!(
                formatter,
                "{label} is outside the approved scope: {}",
                path.display()
            ),
            Self::Serialize(error) => {
                write!(formatter, "cannot encode Skill action request: {error}")
            }
            Self::Spawn { program, source } => write!(
                formatter,
                "cannot start Skill action {}: {source}",
                program.display()
            ),
            Self::Wait { source } => write!(formatter, "cannot wait for Skill action: {source}"),
            Self::Io { operation, source } => write!(formatter, "cannot {operation}: {source}"),
            Self::MissingPipe(pipe) => {
                write!(formatter, "Skill action did not provide {pipe} pipe")
            }
            Self::WorkerPanicked(pipe) => write!(formatter, "Skill action {pipe} reader failed"),
            Self::InvalidUtf8 => formatter.write_str("Skill action response is not UTF-8"),
            Self::FrameTooLarge { size, maximum } => write!(
                formatter,
                "Skill action frame is {size} bytes; maximum is {maximum}"
            ),
            Self::BadResponse(message) => {
                write!(formatter, "invalid Skill action response: {message}")
            }
            Self::OutputLimit { maximum } => {
                write!(formatter, "Skill action output exceeded {maximum} bytes")
            }
        }
    }
}

impl Error for SkillActionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Grant(error) => Some(error),
            Self::Request(error) => Some(error),
            Self::Discovery(error) => Some(error),
            Self::Config(error) => Some(error),
            Self::Path { source, .. }
            | Self::Spawn { source, .. }
            | Self::Wait { source }
            | Self::Io { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SkillProtocolError> for SkillActionError {
    fn from(error: SkillProtocolError) -> Self {
        Self::Request(error)
    }
}

impl From<crate::discovery::DiscoveryError> for SkillActionError {
    fn from(error: crate::discovery::DiscoveryError) -> Self {
        Self::Discovery(error)
    }
}

impl From<SkillsConfigError> for SkillActionError {
    fn from(error: SkillsConfigError) -> Self {
        Self::Config(error)
    }
}
