//! Small, dependency-light terminal UI over the Rust Agent session.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode,
};
use yunxi_agent_spine::{CancellationToken, EventSinkError};
use yunxi_protocol::{AgentStreamEvent, ChatMessage};

use crate::args::{
    CompanionCommand, ControlScope, ControlsCommand, EventMigrationCommand,
    ManagementCommand as PublicManagementCommand, MemoryCommand, MigrationCommand, PersonaCommand,
    SessionCommand, SessionOptions, VoiceCommand,
};
use crate::management::ManagementCommand as SessionManagementCommand;
use crate::session::{BackendStatus, ChatBackend, ChatSession};

const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_TRANSCRIPT_LINES: usize = 512;
const MAX_HISTORY_MESSAGES: usize = 64;

enum WorkerEvent {
    Text(String),
    Tool(String),
    Progress(String),
}

struct WorkerCompletion {
    host: Option<HostFacade>,
    result: Result<String, String>,
}

struct RunningTurn {
    prompt: String,
    cancellation: CancellationToken,
    events: Receiver<WorkerEvent>,
    completion: Receiver<WorkerCompletion>,
    worker: Option<JoinHandle<()>>,
}

struct App {
    host: Option<HostFacade>,
    session_options: SessionOptions,
    history: Vec<ChatMessage>,
    transcript: Vec<String>,
    input: String,
    live_reply: String,
    running: Option<RunningTurn>,
    notice: Option<String>,
    color: bool,
    should_quit: bool,
}

/// The TUI uses one host boundary for launch, management, status, and turns.
/// This keeps the terminal surface aligned with the Web host's ownership model.
struct HostFacade {
    session: ChatSession,
}

impl HostFacade {
    fn launch(options: &SessionOptions) -> Result<Self, String> {
        ChatSession::launch_with_options(options)
            .map(|session| Self { session })
            .map_err(|error| error.to_string())
    }

    fn provider(&self) -> &str {
        self.session.provider()
    }

    fn model(&self) -> &str {
        self.session.model()
    }

    fn status(&mut self) -> BackendStatus {
        self.session.status()
    }

    fn manage(
        &mut self,
        command: SessionManagementCommand,
    ) -> Result<crate::management::ManagementResult, String> {
        self.session.manage(command)
    }

    fn complete_streaming<S: yunxi_agent_spine::EventSink>(
        &mut self,
        messages: &[ChatMessage],
        cancellation: &CancellationToken,
        sink: &mut S,
    ) -> Result<String, crate::session::ChatFailure> {
        self.session
            .complete_streaming(messages, cancellation, sink)
    }
}

impl App {
    fn new(host: HostFacade, session_options: SessionOptions, color: bool) -> Self {
        Self {
            host: Some(host),
            session_options,
            history: Vec::new(),
            transcript: Vec::new(),
            input: String::new(),
            live_reply: String::new(),
            running: None,
            notice: None,
            color,
            should_quit: false,
        }
    }

    fn submit(&mut self) {
        let prompt = self.input.trim().to_string();
        self.input.clear();
        if prompt.is_empty() || self.running.is_some() {
            return;
        }
        if prompt == "/quit" || prompt == "/exit" {
            self.should_quit = true;
            return;
        }
        if prompt == "/clear" {
            self.history.clear();
            self.transcript.clear();
            self.notice = Some("conversation cleared".to_string());
            return;
        }
        if prompt == "/help" {
            self.notice = Some(
                "/status /cwd /session /plugins /tools /mcp /model [name] /provider [name] /cost /debug /details /sessions /memory /persona /companion /controls /voice /migrate /new /resume <id> /approve /deny /cancel /clear /quit".to_string(),
            );
            return;
        }
        if prompt.starts_with('/') {
            self.run_management(&prompt);
            return;
        }
        self.start_prompt(prompt);
    }

    fn start_prompt(&mut self, prompt: String) {
        let Some(mut host) = self.host.take() else {
            self.notice = Some("a turn is already running".to_string());
            return;
        };
        let mut request = self.history.clone();
        request.push(ChatMessage::user(prompt.clone()));
        self.transcript.push(format!("you> {prompt}"));
        self.trim_transcript();
        self.live_reply.clear();

        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (event_sender, events) = sync_channel(128);
        let (completion_sender, completion) = sync_channel(1);
        let worker = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut sink = |event: AgentStreamEvent| {
                    let event = match event {
                        AgentStreamEvent::TextDelta { delta, .. } => WorkerEvent::Text(delta),
                        AgentStreamEvent::ToolStart { tool_name, .. } => {
                            WorkerEvent::Tool(format!("tool: {tool_name}"))
                        }
                        AgentStreamEvent::ToolProgress { progress, .. } => {
                            WorkerEvent::Progress(progress)
                        }
                        AgentStreamEvent::ToolResult { .. }
                        | AgentStreamEvent::TurnState { .. }
                        | AgentStreamEvent::TurnError { .. }
                        | AgentStreamEvent::TurnDone { .. } => return Ok(()),
                    };
                    event_sender.send(event).map_err(|_| EventSinkError::Closed)
                };
                host.complete_streaming(&request, &worker_cancellation, &mut sink)
                    .map_err(|error| error.to_string())
            }));
            let completion = match result {
                Ok(result) => WorkerCompletion {
                    host: Some(host),
                    result,
                },
                Err(_) => WorkerCompletion {
                    host: None,
                    result: Err("the TUI turn worker panicked".to_string()),
                },
            };
            let _ignored = completion_sender.send(completion);
        });
        self.running = Some(RunningTurn {
            prompt,
            cancellation,
            events,
            completion,
            worker: Some(worker),
        });
    }

    fn run_management(&mut self, command: &str) {
        if self.run_legacy_view_command(command) {
            self.trim_transcript();
            return;
        }
        if self.run_public_management(command) {
            self.trim_transcript();
            return;
        }
        let Some(host) = self.host.as_mut() else {
            self.notice = Some("wait for the active turn to finish".to_string());
            return;
        };
        if command == "/status" {
            let status = host.status();
            self.transcript.push(format!(
                "kernel: {} | plugin: {} | protocol: {} | plugins: {} | capabilities: {} | failed: {}",
                status.kernel,
                status.plugin,
                if status.protocol_ready { "ready" } else { "unavailable" },
                status.plugins,
                status.capabilities,
                status.failed_plugins,
            ));
            self.trim_transcript();
            return;
        }
        let parsed = match management_command(command) {
            Ok(Some(command)) => command,
            Ok(None) => {
                self.notice = Some(format!("unknown command: {command}"));
                return;
            }
            Err(error) => {
                self.notice = Some(error);
                return;
            }
        };
        match host.manage(parsed) {
            Ok(result) => {
                self.transcript.extend(result.lines);
                if let Some(reply) = result.assistant_reply {
                    self.transcript.push(format!("yunxi> {reply}"));
                }
                if let Some(history) = result.replacement_history {
                    self.history = history;
                }
                self.history.extend(result.append_history);
                trim_history(&mut self.history);
                self.trim_transcript();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn cancel(&mut self) {
        if let Some(running) = self.running.as_ref() {
            running.cancellation.cancel("user requested cancellation");
            self.notice = Some("cancellation requested".to_string());
        } else if let Some(host) = self.host.as_mut() {
            if let Err(error) = host.manage(SessionManagementCommand::CancelAction) {
                self.notice = Some(error);
            } else {
                self.notice = Some("pending action cancelled".to_string());
            }
        }
    }

    fn poll_worker(&mut self) {
        let mut completed = None;
        let Some(running) = self.running.as_mut() else {
            return;
        };
        loop {
            match running.events.try_recv() {
                Ok(WorkerEvent::Text(delta)) => self.live_reply.push_str(&delta),
                Ok(WorkerEvent::Tool(tool)) => {
                    self.transcript.push(tool);
                }
                Ok(WorkerEvent::Progress(progress)) => {
                    self.transcript.push(format!("progress: {progress}"));
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        match running.completion.try_recv() {
            Ok(value) => completed = Some(value),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                completed = Some(WorkerCompletion {
                    host: None,
                    result: Err("the TUI turn worker stopped unexpectedly".to_string()),
                });
            }
        }
        if let Some(completion) = completed {
            let mut running = self.running.take().expect("running turn exists");
            if let Some(worker) = running.worker.take() {
                let _ignored = worker.join();
            }
            self.host = completion
                .host
                .or_else(|| HostFacade::launch(&self.session_options).ok());
            self.history.push(ChatMessage::user(running.prompt));
            match completion.result {
                Ok(reply) => {
                    if self.live_reply.is_empty() {
                        self.transcript.push(format!("yunxi> {reply}"));
                    } else {
                        self.transcript.push(format!("yunxi> {}", self.live_reply));
                    }
                    self.history.push(ChatMessage::assistant(reply));
                    trim_history(&mut self.history);
                }
                Err(error) => {
                    self.notice = Some(error);
                    if !self.live_reply.is_empty() {
                        self.transcript
                            .push(format!("partial> {}", self.live_reply));
                    }
                }
            }
            self.live_reply.clear();
            self.trim_transcript();
        }
    }

    fn trim_transcript(&mut self) {
        if self.transcript.len() > MAX_TRANSCRIPT_LINES {
            let excess = self.transcript.len() - MAX_TRANSCRIPT_LINES;
            self.transcript.drain(..excess);
        }
    }

    fn run_public_management(&mut self, command: &str) -> bool {
        let parsed = match parse_public_management_command(command) {
            None => return false,
            Some(Ok(None)) => return false,
            Some(Err(error)) => {
                self.notice = Some(error);
                return true;
            }
            Some(Ok(Some(command))) => command,
        };
        let cwd = self
            .session_options
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        match crate::commands::execute(&parsed, &cwd) {
            Ok(value) => {
                match serde_json::to_string_pretty(&value) {
                    Ok(rendered) => self.transcript.extend(rendered.lines().map(str::to_owned)),
                    Err(error) => self.notice = Some(format!("management output failed: {error}")),
                }
                self.trim_transcript();
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        true
    }

    fn run_legacy_view_command(&mut self, command: &str) -> bool {
        let parts = command.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["/cwd"] => {
                let cwd = self
                    .session_options
                    .cwd
                    .as_deref()
                    .map(|path| path.display().to_string())
                    .or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .map(|path| path.display().to_string())
                    })
                    .unwrap_or_else(|| ".".to_string());
                self.transcript.push(format!("cwd: {cwd}"));
                true
            }
            ["/session"] => {
                if let Some(host) = self.host.as_mut() {
                    let status = host.status();
                    self.transcript.push(format!(
                        "session: provider={} model={} kernel={} plugins={} capabilities={}",
                        host.provider(),
                        host.model(),
                        status.kernel,
                        status.plugins,
                        status.capabilities
                    ));
                }
                true
            }
            ["/tools"] | ["/mcp"] => {
                let Some(host) = self.host.as_mut() else {
                    self.notice = Some("wait for the active turn to finish".to_string());
                    return true;
                };
                match host.manage(SessionManagementCommand::ListPlugins) {
                    Ok(result) => self.transcript.extend(result.lines),
                    Err(error) => self.notice = Some(error),
                }
                true
            }
            ["/model"] | ["/provider"] => {
                if let Some(host) = self.host.as_ref() {
                    self.notice = Some(format!(
                        "{}: {}",
                        parts[0],
                        if parts[0] == "/model" {
                            host.model()
                        } else {
                            host.provider()
                        }
                    ));
                }
                true
            }
            [kind, value] if *kind == "/model" || *kind == "/provider" => {
                let mut options = self.session_options.clone();
                if *kind == "/model" {
                    options.model = Some((*value).to_string());
                } else {
                    options.provider = Some((*value).to_string());
                }
                match HostFacade::launch(&options) {
                    Ok(host) => {
                        self.host = Some(host);
                        self.session_options = options;
                        self.notice = Some(format!("{} switched to {value}", kind));
                    }
                    Err(error) => self.notice = Some(format!("backend switch failed: {error}")),
                }
                true
            }
            ["/cost"] => {
                self.notice = Some(
                    "token cost accounting is provider-owned; no estimate is available".to_string(),
                );
                true
            }
            ["/debug"] | ["/debug", ..] => {
                self.notice = Some("debug event details are available through `run --jsonl`; TUI lifecycle rendering is enabled".to_string());
                true
            }
            ["/details"] | ["/details", _] => {
                self.notice = Some("select a turn in the transcript; raw event details are available through `run --jsonl`".to_string());
                true
            }
            _ => false,
        }
    }

    fn draw(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let width = width.max(20);
        let height = height.max(6);
        queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
        self.print_colored(stdout, 0, "YunXi Next", Color::Cyan)?;
        let status = if self.running.is_some() {
            "running"
        } else {
            "ready"
        };
        let provider = self
            .host
            .as_ref()
            .map(|host| format!("{} / {}", host.provider(), host.model()))
            .unwrap_or_else(|| "worker".to_string());
        queue!(
            stdout,
            MoveTo(15, 0),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{provider} | {status}")),
            ResetColor
        )?;

        let body_height = usize::from(height.saturating_sub(4));
        let body = self.render_body(usize::from(width));
        let start = body.len().saturating_sub(body_height);
        for (row, line) in body.iter().skip(start).enumerate() {
            let row = u16::try_from(row + 1).unwrap_or(height.saturating_sub(3));
            queue!(
                stdout,
                MoveTo(0, row),
                Print(fit_line(line, usize::from(width)))
            )?;
        }
        if let Some(notice) = &self.notice {
            self.print_colored(
                stdout,
                height.saturating_sub(3),
                &format!("! {notice}"),
                Color::Yellow,
            )?;
        }
        let footer_row = height.saturating_sub(2);
        queue!(
            stdout,
            MoveTo(0, footer_row),
            SetForegroundColor(Color::DarkGrey),
            Print("Enter send  Ctrl-C cancel  Esc quit"),
            ResetColor,
            MoveTo(0, height.saturating_sub(1)),
            SetForegroundColor(Color::Green),
            Print("you> "),
            ResetColor,
            Print(fit_line(&self.input, usize::from(width).saturating_sub(5)))
        )?;
        let cursor_x =
            5_u16.saturating_add(u16::try_from(self.input.chars().count()).unwrap_or(u16::MAX));
        queue!(
            stdout,
            MoveTo(
                cursor_x.min(width.saturating_sub(1)),
                height.saturating_sub(1)
            )
        )?;
        stdout.flush()
    }

    fn render_body(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for line in &self.transcript {
            wrap_line(line, width, &mut lines);
        }
        if !self.live_reply.is_empty() {
            wrap_line(&format!("yunxi> {}", self.live_reply), width, &mut lines);
        }
        lines
    }

    fn print_colored(
        &self,
        stdout: &mut io::Stdout,
        row: u16,
        text: &str,
        color: Color,
    ) -> io::Result<()> {
        queue!(stdout, MoveTo(0, row))?;
        if self.color {
            queue!(stdout, SetForegroundColor(color), Print(text), ResetColor)?;
        } else {
            queue!(stdout, Print(text))?;
        }
        Ok(())
    }
}

pub(crate) fn run(
    session: ChatSession,
    session_options: SessionOptions,
    color: bool,
) -> io::Result<()> {
    let _terminal = TerminalGuard::enter()?;
    let mut stdout = io::stdout();
    let mut app = App::new(HostFacade { session }, session_options, color);
    loop {
        app.poll_worker();
        app.draw(&mut stdout)?;
        if app.should_quit {
            app.cancel();
            while app.running.is_some() {
                app.poll_worker();
                std::thread::sleep(Duration::from_millis(10));
            }
            break;
        }
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key);
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.cancel();
        return;
    }
    match key.code {
        KeyCode::Esc => app.should_quit = true,
        KeyCode::Enter => app.submit(),
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if app.input.len().saturating_add(character.len_utf8()) <= MAX_INPUT_BYTES {
                app.input.push(character);
            }
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {}
        _ => {}
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let _ignored = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ignored = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ignored = disable_raw_mode();
    }
}

fn parse_public_management_command(
    value: &str,
) -> Option<Result<Option<PublicManagementCommand>, String>> {
    let parts = value.split_whitespace().collect::<Vec<_>>();
    let command = match parts.as_slice() {
        ["/sessions"] | ["/sessions", "list"] => {
            Some(PublicManagementCommand::Sessions(SessionCommand::List {
                include_archived: false,
            }))
        }
        ["/sessions", "all"] | ["/sessions", "list", "--all"] => {
            Some(PublicManagementCommand::Sessions(SessionCommand::List {
                include_archived: true,
            }))
        }
        ["/sessions", "rollout", id] => Some(PublicManagementCommand::Sessions(
            SessionCommand::Rollout((*id).to_string()),
        )),
        ["/sessions", "history", id] => Some(PublicManagementCommand::Sessions(
            SessionCommand::History((*id).to_string()),
        )),
        ["/sessions", "graph"] => Some(PublicManagementCommand::Sessions(SessionCommand::Graph)),
        ["/sessions", "resume", id, prompt @ ..] => {
            Some(PublicManagementCommand::Sessions(SessionCommand::Resume {
                id: (*id).to_string(),
                prompt: prompt.iter().map(|value| (*value).to_string()).collect(),
            }))
        }
        ["/sessions", action, id]
            if matches!(
                *action,
                "show" | "archive" | "unarchive" | "pin" | "unpin" | "fork"
            ) =>
        {
            let command = match *action {
                "show" => SessionCommand::Show((*id).to_string()),
                "archive" => SessionCommand::Archive((*id).to_string()),
                "unarchive" => SessionCommand::Unarchive((*id).to_string()),
                "pin" => SessionCommand::Pin((*id).to_string()),
                "unpin" => SessionCommand::Unpin((*id).to_string()),
                "fork" => SessionCommand::Fork((*id).to_string()),
                _ => unreachable!(),
            };
            Some(PublicManagementCommand::Sessions(command))
        }
        ["/memory"] | ["/memory", "status"] => {
            Some(PublicManagementCommand::Memory(MemoryCommand::Status))
        }
        ["/memory", "list", rest @ ..] => {
            let (rest, global, workspace, _) = split_memory_flags(rest);
            if global && workspace {
                return Some(Err(
                    "memory list cannot use both --global and --workspace".to_string()
                ));
            }
            if !rest.is_empty() {
                return Some(Err("usage: /memory list [--global|--workspace]".to_string()));
            }
            Some(PublicManagementCommand::Memory(MemoryCommand::List {
                global,
                workspace,
            }))
        }
        ["/memory", "pending"] => Some(PublicManagementCommand::Memory(MemoryCommand::Pending)),
        ["/memory", "on"] => Some(PublicManagementCommand::Memory(MemoryCommand::On)),
        ["/memory", "off"] => Some(PublicManagementCommand::Memory(MemoryCommand::Off)),
        ["/memory", "clear", rest @ ..] => {
            let (rest, global, workspace, confirm) = split_memory_flags(rest);
            if !rest.is_empty() {
                return Some(Err(
                    "usage: /memory clear [--workspace] [--confirm]".to_string()
                ));
            }
            if global {
                return Some(Err("memory clear does not support --global".to_string()));
            }
            if !confirm {
                return Some(Err(
                    "memory clear requires --workspace --confirm".to_string()
                ));
            }
            Some(PublicManagementCommand::Memory(MemoryCommand::Clear {
                workspace: workspace || confirm,
                confirm,
            }))
        }
        ["/memory", "search", query @ ..] if !query.is_empty() => {
            let (query, global, workspace, _) = split_memory_flags(query);
            if global && workspace {
                return Some(Err(
                    "memory search cannot use both --global and --workspace".to_string(),
                ));
            }
            if query.is_empty() {
                return Some(Err(
                    "usage: /memory search <query> [--global|--workspace]".to_string()
                ));
            }
            Some(PublicManagementCommand::Memory(MemoryCommand::Search {
                query: query.join(" "),
                global,
                workspace,
            }))
        }
        ["/memory", action, id] if matches!(*action, "show" | "approve" | "reject" | "delete") => {
            let command = match *action {
                "show" => MemoryCommand::Show((*id).to_string()),
                "approve" => MemoryCommand::Approve((*id).to_string()),
                "reject" => MemoryCommand::Reject((*id).to_string()),
                "delete" => MemoryCommand::Delete((*id).to_string()),
                _ => unreachable!(),
            };
            Some(PublicManagementCommand::Memory(command))
        }
        ["/persona"] | ["/persona", "status"] => {
            Some(PublicManagementCommand::Persona(PersonaCommand::Status))
        }
        ["/persona", "list"] => Some(PublicManagementCommand::Persona(PersonaCommand::List)),
        ["/persona", "profile"] => Some(PublicManagementCommand::Persona(PersonaCommand::Profile(
            None,
        ))),
        ["/persona", "profile", id] => Some(PublicManagementCommand::Persona(
            PersonaCommand::Profile(Some((*id).to_string())),
        )),
        ["/persona", "set", id] => Some(PublicManagementCommand::Persona(PersonaCommand::Set(
            (*id).to_string(),
        ))),
        ["/persona", "import", path] => Some(PublicManagementCommand::Persona(
            PersonaCommand::Import((*path).to_string()),
        )),
        ["/persona", "on"] => Some(PublicManagementCommand::Persona(PersonaCommand::On)),
        ["/persona", "off"] => Some(PublicManagementCommand::Persona(PersonaCommand::Off)),
        ["/companion"] | ["/companion", "status"] => {
            Some(PublicManagementCommand::Companion(CompanionCommand::Status))
        }
        ["/companion", "history"] => Some(PublicManagementCommand::Companion(
            CompanionCommand::History,
        )),
        ["/companion", "clear", rest @ ..] => {
            let (_, _, _, confirm) = split_memory_flags(rest);
            if !confirm {
                return Some(Err("companion clear requires --confirm".to_string()));
            }
            Some(PublicManagementCommand::Companion(
                CompanionCommand::Clear { confirm: true },
            ))
        }
        ["/companion", "on"] => Some(PublicManagementCommand::Companion(CompanionCommand::On)),
        ["/companion", "off"] => Some(PublicManagementCommand::Companion(CompanionCommand::Off)),
        ["/companion", "check", prompt @ ..] if !prompt.is_empty() => Some(
            PublicManagementCommand::Companion(CompanionCommand::Check(prompt.join(" "))),
        ),
        ["/controls"] | ["/controls", "status"] => {
            Some(PublicManagementCommand::Controls(ControlsCommand::Status))
        }
        ["/controls", "show", scope] => {
            let scope = match parse_control_scope(scope) {
                Ok(scope) => scope,
                Err(error) => return Some(Err(error)),
            };
            Some(PublicManagementCommand::Controls(ControlsCommand::Show(
                scope,
            )))
        }
        ["/controls", "clear", scope, rest @ ..] => {
            let (_, _, _, confirm) = split_memory_flags(rest);
            if !confirm {
                return Some(Err("controls clear requires --confirm".to_string()));
            }
            let scope = match parse_control_scope(scope) {
                Ok(scope) => scope,
                Err(error) => return Some(Err(error)),
            };
            Some(PublicManagementCommand::Controls(ControlsCommand::Clear {
                scope,
                confirm: true,
            }))
        }
        ["/controls", "refresh"] => {
            Some(PublicManagementCommand::Controls(ControlsCommand::Refresh))
        }
        ["/controls", "audit"] => Some(PublicManagementCommand::Controls(ControlsCommand::Audit)),
        ["/controls", action, id] if matches!(*action, "enable" | "on" | "disable" | "off") => {
            let command = if matches!(*action, "enable" | "on") {
                ControlsCommand::Enable((*id).to_string())
            } else {
                ControlsCommand::Disable((*id).to_string())
            };
            Some(PublicManagementCommand::Controls(command))
        }
        ["/voice"] | ["/voice", "status"] => {
            Some(PublicManagementCommand::Voice(VoiceCommand::Status))
        }
        ["/voice", "doctor"] => Some(PublicManagementCommand::Voice(VoiceCommand::Doctor)),
        ["/voice", "devices"] => Some(PublicManagementCommand::Voice(VoiceCommand::Devices)),
        ["/voice", action, text @ ..]
            if !text.is_empty() && matches!(*action, "transcribe" | "speak" | "chat" | "talk") =>
        {
            let text = text.join(" ");
            let command = match *action {
                "transcribe" => VoiceCommand::Transcribe(text),
                "speak" => VoiceCommand::Speak(text),
                "chat" => VoiceCommand::Chat(text),
                "talk" => VoiceCommand::Talk(text),
                _ => unreachable!(),
            };
            Some(PublicManagementCommand::Voice(command))
        }
        ["/migrate"] | ["/migrate", "status"] => {
            Some(PublicManagementCommand::Migrate(MigrationCommand::Status))
        }
        ["/migrate", "plan"] => Some(PublicManagementCommand::Migrate(MigrationCommand::Plan)),
        ["/migrate", "apply"] => Some(PublicManagementCommand::Migrate(MigrationCommand::Apply)),
        ["/migrate", "rollback", id] => Some(PublicManagementCommand::Migrate(
            MigrationCommand::Rollback((*id).to_string()),
        )),
        ["/migrate", "events", "status", source] => Some(PublicManagementCommand::Migrate(
            MigrationCommand::Events(EventMigrationCommand::Status {
                source: (*source).into(),
            }),
        )),
        ["/migrate", "events", "replay", source] => Some(PublicManagementCommand::Migrate(
            MigrationCommand::Events(EventMigrationCommand::Replay {
                source: (*source).into(),
                after_cursor: 0,
                limit: 128,
            }),
        )),
        ["/migrate", "events", "plan", source] => Some(PublicManagementCommand::Migrate(
            MigrationCommand::Events(EventMigrationCommand::Plan {
                source: (*source).into(),
            }),
        )),
        ["/migrate", "events", "apply", source] => Some(PublicManagementCommand::Migrate(
            MigrationCommand::Events(EventMigrationCommand::Apply {
                source: (*source).into(),
            }),
        )),
        ["/migrate", "events", "rollback", source, id] => Some(PublicManagementCommand::Migrate(
            MigrationCommand::Events(EventMigrationCommand::Rollback {
                source: (*source).into(),
                migration_id: (*id).to_string(),
            }),
        )),
        [
            root @ ("/memory" | "/persona" | "/companion" | "/controls" | "/voice" | "/migrate"),
            ..,
        ] => {
            return Some(Err(format!("unknown {root} command")));
        }
        _ => None,
    };
    Some(Ok(command))
}

fn split_memory_flags<'a>(parts: &'a [&'a str]) -> (Vec<&'a str>, bool, bool, bool) {
    let mut remainder = Vec::new();
    let mut global = false;
    let mut workspace = false;
    let mut confirm = false;
    for part in parts {
        match *part {
            "--global" => global = true,
            "--workspace" => workspace = true,
            "--confirm" | "confirm" => confirm = true,
            _ => remainder.push(*part),
        }
    }
    (remainder, global, workspace, confirm)
}

fn parse_control_scope(value: &str) -> Result<ControlScope, String> {
    match value {
        "companion" => Ok(ControlScope::Companion),
        "memory" => Ok(ControlScope::Memory),
        "persona" => Ok(ControlScope::Persona),
        "relationship" => Ok(ControlScope::Relationship),
        _ => Err(format!("unknown control scope `{value}`")),
    }
}

fn management_command(value: &str) -> Result<Option<SessionManagementCommand>, String> {
    let parts = value.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["/plugins"] => Ok(Some(SessionManagementCommand::ListPlugins)),
        ["/new"] => Ok(Some(SessionManagementCommand::NewSession)),
        ["/resume", id] => Ok(Some(SessionManagementCommand::ResumeSession(
            (*id).to_string(),
        ))),
        ["/resume"] => Err("usage: /resume <session-id>".to_string()),
        ["/approve"] => Ok(Some(SessionManagementCommand::ApproveAction)),
        ["/deny"] => Ok(Some(SessionManagementCommand::DenyAction)),
        ["/cancel"] => Ok(Some(SessionManagementCommand::CancelAction)),
        _ => Ok(None),
    }
}

fn trim_history(history: &mut Vec<ChatMessage>) {
    if history.len() > MAX_HISTORY_MESSAGES {
        let excess = history.len() - MAX_HISTORY_MESSAGES;
        history.drain(..excess);
    }
}

fn wrap_line(line: &str, width: usize, output: &mut Vec<String>) {
    let width = width.max(1);
    if line.is_empty() {
        output.push(String::new());
        return;
    }
    let mut current = String::new();
    for character in line.chars() {
        if current.chars().count() >= width {
            output.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    if !current.is_empty() {
        output.push(current);
    }
}

fn fit_line(line: &str, width: usize) -> String {
    line.chars().take(width.max(1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_management_parser_covers_legacy_surfaces() {
        assert!(matches!(
            parse_public_management_command("/sessions list"),
            Some(Ok(Some(PublicManagementCommand::Sessions(
                SessionCommand::List { .. }
            ))))
        ));
        assert!(matches!(
            parse_public_management_command("/sessions rollout thread-1"),
            Some(Ok(Some(PublicManagementCommand::Sessions(
                SessionCommand::Rollout(id)
            )))) if id == "thread-1"
        ));
        assert!(matches!(
            parse_public_management_command("/sessions history thread-2"),
            Some(Ok(Some(PublicManagementCommand::Sessions(
                SessionCommand::History(id)
            )))) if id == "thread-2"
        ));
        assert!(matches!(
            parse_public_management_command("/sessions graph"),
            Some(Ok(Some(PublicManagementCommand::Sessions(
                SessionCommand::Graph
            ))))
        ));
        assert!(matches!(
            parse_public_management_command("/sessions resume thread-3 keep going"),
            Some(Ok(Some(PublicManagementCommand::Sessions(
                SessionCommand::Resume { id, prompt }
            )))) if id == "thread-3" && prompt == vec!["keep", "going"]
        ));
        assert!(matches!(
            parse_public_management_command("/memory list --workspace"),
            Some(Ok(Some(PublicManagementCommand::Memory(
                MemoryCommand::List { workspace, .. }
            )))) if workspace
        ));
        assert!(matches!(
            parse_public_management_command("/memory search two words --global"),
            Some(Ok(Some(PublicManagementCommand::Memory(
                MemoryCommand::Search { query, global, .. }
            )))) if query == "two words" && global
        ));
        assert!(matches!(
            parse_public_management_command("/companion history"),
            Some(Ok(Some(PublicManagementCommand::Companion(
                CompanionCommand::History
            ))))
        ));
        assert!(matches!(
            parse_public_management_command("/companion clear --confirm"),
            Some(Ok(Some(PublicManagementCommand::Companion(
                CompanionCommand::Clear { confirm }
            )))) if confirm
        ));
        assert!(matches!(
            parse_public_management_command("/controls show relationship"),
            Some(Ok(Some(PublicManagementCommand::Controls(
                ControlsCommand::Show(ControlScope::Relationship)
            ))))
        ));
        assert!(matches!(
            parse_public_management_command("/controls clear memory --confirm"),
            Some(Ok(Some(PublicManagementCommand::Controls(
                ControlsCommand::Clear { scope: ControlScope::Memory, confirm }
            )))) if confirm
        ));
        assert!(matches!(
            parse_public_management_command("/controls refresh"),
            Some(Ok(Some(PublicManagementCommand::Controls(
                ControlsCommand::Refresh
            ))))
        ));
        assert!(matches!(
            parse_public_management_command("/controls audit"),
            Some(Ok(Some(PublicManagementCommand::Controls(
                ControlsCommand::Audit
            ))))
        ));
        assert!(matches!(
            parse_public_management_command("/persona import profile.json"),
            Some(Ok(Some(PublicManagementCommand::Persona(
                PersonaCommand::Import(path)
            )))) if path == "profile.json"
        ));
        assert!(matches!(
            parse_public_management_command("/voice speak hello world"),
            Some(Ok(Some(PublicManagementCommand::Voice(VoiceCommand::Speak(text)))))
                if text == "hello world"
        ));
        assert!(matches!(
            parse_public_management_command("/migrate rollback migration-id"),
            Some(Ok(Some(PublicManagementCommand::Migrate(
                MigrationCommand::Rollback(id)
            )))) if id == "migration-id"
        ));
    }

    #[test]
    fn public_management_parser_rejects_unknown_subcommands() {
        assert!(matches!(
            parse_public_management_command("/controls maybe"),
            Some(Err(message)) if message.contains("unknown /controls command")
        ));
        assert!(matches!(
            parse_public_management_command("/unknown"),
            Some(Ok(None))
        ));
    }
}
