//! Bounded JSONL process transport for replaceable voice sidecars.
//!
//! The transport owns only the process boundary. It does not know provider
//! credentials, audio devices, codecs, or SDKs. The child receives a
//! versioned JSONL request and must return one versioned JSONL response.
//! Requests are serialized one at a time; a failed, cancelled, or timed-out
//! exchange terminates the child so a broken sidecar cannot remain attached to
//! later work.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use crate::provider::ProviderDescriptor;
use crate::sidecar::{
    ExternalSidecarProvider, MAX_SIDECAR_PAYLOAD_BYTES, ProviderFeatures, SIDECAR_PROTOCOL_VERSION,
    SidecarRequestFrame, SidecarResponseFrame, SidecarTransport,
};
use crate::stream::{OperationContext, VoiceProviderError};

pub const DEFAULT_SIDECAR_MAX_FRAME_BYTES: usize = 512 * 1024;
pub const MAX_SIDECAR_FRAME_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_SIDECAR_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_SIDECAR_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MIN_SIDECAR_FRAME_BYTES: usize = 256;
const MAX_SIDECAR_PATH_BYTES: usize = 4096;
const MAX_SIDECAR_ARGUMENTS: usize = 64;
const MAX_SIDECAR_ARGUMENT_BYTES: usize = 4096;
const WAIT_SLICE: Duration = Duration::from_millis(20);

/// Configuration for an external sidecar executable.
///
/// The child environment is cleared by default. This prevents API keys and
/// other host process environment values from being inherited accidentally.
/// A sidecar should use its own secure credential mechanism; credentials are
/// never accepted in the JSONL contract.
#[derive(Clone)]
pub struct ProcessSidecarConfig {
    program: PathBuf,
    args: Vec<OsString>,
    working_directory: Option<PathBuf>,
    max_frame_bytes: usize,
    timeout: Duration,
}

impl fmt::Debug for ProcessSidecarConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessSidecarConfig")
            .field("program", &self.program)
            .field("args_count", &self.args.len())
            .field("working_directory", &self.working_directory)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl ProcessSidecarConfig {
    pub fn new(program: impl Into<PathBuf>) -> Result<Self, VoiceProviderError> {
        let config = Self {
            program: program.into(),
            args: Vec::new(),
            working_directory: None,
            max_frame_bytes: DEFAULT_SIDECAR_MAX_FRAME_BYTES,
            timeout: DEFAULT_SIDECAR_TIMEOUT,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_args<I, S>(mut self, args: I) -> Result<Self, VoiceProviderError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self.validate()?;
        Ok(self)
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Result<Self, VoiceProviderError> {
        self.args.push(arg.into());
        self.validate()?;
        Ok(self)
    }

    pub fn working_directory(
        mut self,
        path: impl Into<PathBuf>,
    ) -> Result<Self, VoiceProviderError> {
        self.working_directory = Some(path.into());
        self.validate()?;
        Ok(self)
    }

    pub fn max_frame_bytes(mut self, maximum: usize) -> Result<Self, VoiceProviderError> {
        self.max_frame_bytes = maximum;
        self.validate()?;
        Ok(self)
    }

    pub fn timeout(mut self, timeout: Duration) -> Result<Self, VoiceProviderError> {
        self.timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    pub fn program(&self) -> &std::path::Path {
        &self.program
    }

    pub fn max_frame_bytes_value(&self) -> usize {
        self.max_frame_bytes
    }

    pub fn timeout_value(&self) -> Duration {
        self.timeout
    }

    pub fn into_provider(
        self,
        descriptor: ProviderDescriptor,
        features: ProviderFeatures,
    ) -> Result<ExternalSidecarProvider<ProcessSidecarTransport>, VoiceProviderError> {
        ExternalSidecarProvider::new(descriptor, features, ProcessSidecarTransport::spawn(self)?)
    }

    fn validate(&self) -> Result<(), VoiceProviderError> {
        if self.program.as_os_str().is_empty()
            || self.program.to_string_lossy().len() > MAX_SIDECAR_PATH_BYTES
            || self.working_directory.as_ref().is_some_and(|path| {
                path.as_os_str().is_empty() || path.to_string_lossy().len() > MAX_SIDECAR_PATH_BYTES
            })
        {
            return Err(VoiceProviderError::invalid_provider("invalid_config"));
        }
        if self.args.len() > MAX_SIDECAR_ARGUMENTS
            || self
                .args
                .iter()
                .any(|arg| arg.to_string_lossy().len() > MAX_SIDECAR_ARGUMENT_BYTES)
        {
            return Err(VoiceProviderError::invalid_provider("invalid_config"));
        }
        if !(MIN_SIDECAR_FRAME_BYTES..=MAX_SIDECAR_FRAME_BYTES).contains(&self.max_frame_bytes)
            || self.max_frame_bytes < MAX_SIDECAR_PAYLOAD_BYTES / 2
        {
            return Err(VoiceProviderError::invalid_provider("invalid_config"));
        }
        if self.timeout.is_zero() || self.timeout > MAX_SIDECAR_TIMEOUT {
            return Err(VoiceProviderError::invalid_provider("invalid_config"));
        }
        Ok(())
    }

    /// Builds configuration from `YUNXI_VOICE_SIDECAR_*` values.
    ///
    /// The program variable is the only required value. Arguments use simple
    /// whitespace splitting and therefore cannot invoke a shell. The child
    /// still receives a cleared environment.
    pub fn from_environment() -> Result<Option<Self>, VoiceProviderError> {
        let Some(program) = std::env::var_os("YUNXI_VOICE_SIDECAR_PROGRAM") else {
            return Ok(None);
        };
        let mut config = Self::new(PathBuf::from(program))?;
        if let Some(args) = std::env::var_os("YUNXI_VOICE_SIDECAR_ARGS") {
            let args = args
                .to_string_lossy()
                .split_whitespace()
                .map(OsString::from)
                .collect::<Vec<_>>();
            config = config.with_args(args)?;
        }
        if let Some(maximum) = parse_env_u64("YUNXI_VOICE_SIDECAR_MAX_FRAME_BYTES")? {
            config = config.max_frame_bytes(
                usize::try_from(maximum)
                    .map_err(|_| VoiceProviderError::invalid_provider("invalid_config"))?,
            )?;
        }
        if let Some(timeout_ms) = parse_env_u64("YUNXI_VOICE_SIDECAR_TIMEOUT_MS")? {
            config = config.timeout(Duration::from_millis(timeout_ms))?;
        }
        Ok(Some(config))
    }
}

fn parse_env_u64(name: &str) -> Result<Option<u64>, VoiceProviderError> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    value
        .to_string_lossy()
        .parse::<u64>()
        .map(Some)
        .map_err(|_| VoiceProviderError::invalid_provider("invalid_config"))
}

enum ReaderEvent {
    Frame(Vec<u8>),
    FrameTooLarge,
    Eof,
    Error,
}

enum WriterCommand {
    Frame {
        bytes: Vec<u8>,
        completed: SyncSender<Result<(), ()>>,
    },
}

struct RunningSidecar {
    child: Child,
    writer: SyncSender<WriterCommand>,
    reader: Receiver<ReaderEvent>,
}

/// A single-process, bounded JSONL implementation of [`SidecarTransport`].
pub struct ProcessSidecarTransport {
    config: ProcessSidecarConfig,
    process: Option<RunningSidecar>,
    generation: u64,
}

impl fmt::Debug for ProcessSidecarTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessSidecarTransport")
            .field("program", &self.config.program)
            .field("max_frame_bytes", &self.config.max_frame_bytes)
            .field("timeout", &self.config.timeout)
            .field("running", &self.process.is_some())
            .field("generation", &self.generation)
            .finish()
    }
}

impl ProcessSidecarTransport {
    pub fn spawn(config: ProcessSidecarConfig) -> Result<Self, VoiceProviderError> {
        config.validate()?;
        let mut transport = Self {
            config,
            process: None,
            generation: 0,
        };
        transport.start_process()?;
        Ok(transport)
    }

    pub fn from_environment() -> Result<Option<Self>, VoiceProviderError> {
        let Some(config) = ProcessSidecarConfig::from_environment()? else {
            return Ok(None);
        };
        Self::spawn(config).map(Some)
    }

    pub fn from_env() -> Result<Option<Self>, VoiceProviderError> {
        Self::from_environment()
    }

    pub fn config(&self) -> &ProcessSidecarConfig {
        &self.config
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_running(&mut self) -> bool {
        self.process
            .as_mut()
            .and_then(|running| running.child.try_wait().ok())
            .is_some_and(|status| status.is_none())
    }

    /// Stops the current sidecar. The next exchange starts a fresh process.
    pub fn shutdown(&mut self) {
        self.stop_process();
    }

    /// Stops the current sidecar and increments the lifecycle generation.
    pub fn restart(&mut self) {
        self.stop_process();
        self.generation = self.generation.saturating_add(1);
    }

    fn start_process(&mut self) -> Result<(), VoiceProviderError> {
        if self.process.is_some() {
            return Ok(());
        }

        // Deliberately do not inherit the host environment. In particular,
        // no API key or bearer token can cross this boundary implicitly.
        let mut command = Command::new(&self.config.program);
        command
            .args(&self.config.args)
            .env_clear()
            .env(
                "YUNXI_VOICE_SIDECAR_PROTOCOL",
                SIDECAR_PROTOCOL_VERSION.to_string(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = &self.config.working_directory {
            command.current_dir(directory);
        }
        let mut child = command
            .spawn()
            .map_err(|_| VoiceProviderError::provider_failure("sidecar_spawn_failed", true))?;
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => return terminate_spawn_failure(child),
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => return terminate_spawn_failure(child),
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => return terminate_spawn_failure(child),
        };

        let (writer, writer_commands) = mpsc::sync_channel(1);
        if thread::Builder::new()
            .name("yunxi-voice-sidecar-writer".to_owned())
            .spawn(move || writer_loop(stdin, writer_commands))
            .is_err()
        {
            return terminate_spawn_failure(child);
        }
        let (reader_sender, reader) = mpsc::sync_channel(1);
        let maximum = self.config.max_frame_bytes;
        if thread::Builder::new()
            .name("yunxi-voice-sidecar-reader".to_owned())
            .spawn(move || reader_loop(stdout, maximum, reader_sender))
            .is_err()
        {
            return terminate_spawn_failure(child);
        }
        if thread::Builder::new()
            .name("yunxi-voice-sidecar-stderr".to_owned())
            .spawn(move || drain_stderr(stderr))
            .is_err()
        {
            return terminate_spawn_failure(child);
        }
        self.process = Some(RunningSidecar {
            child,
            writer,
            reader,
        });
        Ok(())
    }

    fn stop_process(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }

    fn exchange_inner(
        &mut self,
        request: SidecarRequestFrame,
        context: &OperationContext,
    ) -> Result<SidecarResponseFrame, VoiceProviderError> {
        context.check()?;
        request.validate()?;
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|_| VoiceProviderError::provider_failure("sidecar_encode_failed", false))?;
        if bytes.len().saturating_add(1) > self.config.max_frame_bytes {
            return Err(VoiceProviderError::provider_failure(
                "sidecar_request_too_large",
                false,
            ));
        }
        bytes.push(b'\n');
        let deadline = effective_deadline(context, self.config.timeout);
        let process = self
            .process
            .as_mut()
            .ok_or_else(|| VoiceProviderError::provider_failure("sidecar_unavailable", true))?;
        let (completed_sender, completed_receiver) = mpsc::sync_channel(1);
        process
            .writer
            .try_send(WriterCommand::Frame {
                bytes,
                completed: completed_sender,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => VoiceProviderError::provider_failure("sidecar_busy", true),
                TrySendError::Disconnected(_) => {
                    VoiceProviderError::provider_failure("sidecar_exited", true)
                }
            })?;
        wait_for_writer(&completed_receiver, context, deadline)?;

        loop {
            context.check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(VoiceProviderError::TimedOut);
            }
            match process.reader.recv_timeout(remaining.min(WAIT_SLICE)) {
                Ok(ReaderEvent::Frame(frame)) => {
                    if frame.len() > self.config.max_frame_bytes {
                        return Err(VoiceProviderError::provider_failure(
                            "sidecar_frame_too_large",
                            false,
                        ));
                    }
                    let response =
                        serde_json::from_slice::<SidecarResponseFrame>(&frame).map_err(|_| {
                            VoiceProviderError::provider_failure("sidecar_bad_frame", false)
                        })?;
                    response.validate().map_err(|_| {
                        VoiceProviderError::provider_failure("sidecar_bad_frame", false)
                    })?;
                    return Ok(response);
                }
                Ok(ReaderEvent::FrameTooLarge) => {
                    return Err(VoiceProviderError::provider_failure(
                        "sidecar_frame_too_large",
                        false,
                    ));
                }
                Ok(ReaderEvent::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(VoiceProviderError::provider_failure("sidecar_exited", true));
                }
                Ok(ReaderEvent::Error) => {
                    return Err(VoiceProviderError::provider_failure(
                        "sidecar_read_failed",
                        true,
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if process.child.try_wait().ok().flatten().is_some() {
                        return Err(VoiceProviderError::provider_failure("sidecar_exited", true));
                    }
                }
            }
        }
    }
}

fn terminate_spawn_failure(mut child: Child) -> Result<(), VoiceProviderError> {
    let _ = child.kill();
    let _ = child.wait();
    Err(VoiceProviderError::provider_failure(
        "sidecar_spawn_failed",
        true,
    ))
}

impl SidecarTransport for ProcessSidecarTransport {
    fn exchange(
        &mut self,
        request: SidecarRequestFrame,
        context: &OperationContext,
    ) -> Result<SidecarResponseFrame, VoiceProviderError> {
        context.check()?;
        // A sidecar can exit between exchanges without producing a readable
        // frame. Remove that stale process before queueing a new request so a
        // later caller gets a fresh isolated child instead of a pipe error.
        if self
            .process
            .as_mut()
            .is_some_and(|process| process.child.try_wait().ok().flatten().is_some())
        {
            self.stop_process();
        }
        if self.process.is_none() {
            self.start_process()?;
        }
        match self.exchange_inner(request, context) {
            Ok(response) => Ok(response),
            Err(error) => {
                if should_reset_process(&error) {
                    self.restart();
                }
                Err(error)
            }
        }
    }

    fn reset(&mut self) {
        self.restart();
    }
}

fn should_reset_process(error: &VoiceProviderError) -> bool {
    matches!(
        error,
        VoiceProviderError::Cancelled | VoiceProviderError::TimedOut
    ) || matches!(
        error,
        VoiceProviderError::ProviderFailure { code, .. }
            if matches!(
                code.as_str(),
                "sidecar_bad_frame"
                    | "sidecar_frame_too_large"
                    | "sidecar_exited"
                    | "sidecar_read_failed"
                    | "sidecar_write_failed"
            )
    )
}

impl Drop for ProcessSidecarTransport {
    fn drop(&mut self) {
        self.stop_process();
    }
}

fn effective_deadline(context: &OperationContext, timeout: Duration) -> Instant {
    let configured = Instant::now() + timeout;
    context
        .deadline()
        .map_or(configured, |deadline| deadline.min(configured))
}

fn wait_for_writer(
    receiver: &Receiver<Result<(), ()>>,
    context: &OperationContext,
    deadline: Instant,
) -> Result<(), VoiceProviderError> {
    loop {
        context.check()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(VoiceProviderError::TimedOut);
        }
        match receiver.recv_timeout(remaining.min(WAIT_SLICE)) {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(())) | Err(RecvTimeoutError::Disconnected) => {
                return Err(VoiceProviderError::provider_failure(
                    "sidecar_write_failed",
                    true,
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn writer_loop(mut stdin: std::process::ChildStdin, receiver: Receiver<WriterCommand>) {
    while let Ok(WriterCommand::Frame { bytes, completed }) = receiver.recv() {
        let result = stdin.write_all(&bytes).and_then(|()| stdin.flush());
        let success = result.is_ok();
        let _ = completed.send(if success { Ok(()) } else { Err(()) });
        if !success {
            break;
        }
    }
}

fn reader_loop(stdout: ChildStdout, maximum: usize, sender: SyncSender<ReaderEvent>) {
    let mut reader = io::BufReader::new(stdout);
    loop {
        let event = match read_bounded_line(&mut reader, maximum) {
            Ok(Some(frame)) => ReaderEvent::Frame(frame),
            Ok(None) => ReaderEvent::Eof,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => ReaderEvent::FrameTooLarge,
            Err(_) => ReaderEvent::Error,
        };
        let terminal = !matches!(&event, ReaderEvent::Frame(_));
        if sender.send(event).is_err() || terminal {
            return;
        }
    }
}

fn read_bounded_line(
    reader: &mut BufReader<ChildStdout>,
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete frame",
                ))
            };
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if line.len().saturating_add(newline) > maximum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "frame too large",
                ));
            }
            line.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            return Ok(Some(line));
        }
        if line.len().saturating_add(available.len()) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame too large",
            ));
        }
        line.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
}

fn drain_stderr(mut stderr: ChildStderr) {
    let mut buffer = [0_u8; 512];
    while stderr.read(&mut buffer).is_ok_and(|count| count != 0) {}
}
