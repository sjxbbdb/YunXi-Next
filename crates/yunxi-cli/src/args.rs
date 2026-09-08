//! Parsing for the stable public command-line surface.

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
    Tui(TuiOptions),
    Web(WebOptions),
    Control(ControlOptions),
    Management(ManagementOptions),
    Help,
    Version,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OutputMode {
    #[default]
    Human,
    Json,
    Jsonl,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ApprovalMode {
    Never,
    #[default]
    OnRequest,
    OnFailure,
    Untrusted,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SessionOptions {
    pub cwd: Option<PathBuf>,
    pub plugin_path: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub approval: ApprovalMode,
    pub sandbox: SandboxMode,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CliOptions {
    pub once: Option<String>,
    pub session: SessionOptions,
    pub no_color: bool,
    pub output: OutputMode,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TuiOptions {
    pub session: SessionOptions,
    pub no_color: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WebOptions {
    pub bind: SocketAddr,
    pub session: SessionOptions,
}

impl Default for WebOptions {
    fn default() -> Self {
        Self {
            bind: DEFAULT_WEB_BIND,
            session: SessionOptions::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ControlOptions {
    pub command: ControlCommand,
    pub cwd: Option<PathBuf>,
    pub json: bool,
    pub jsonl: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ControlCommand {
    Status,
    Doctor,
    Enable(String),
    Disable(String),
    Reload(Option<String>),
    Diagnostics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagementOptions {
    pub command: ManagementCommand,
    pub cwd: Option<PathBuf>,
    pub json: bool,
    pub jsonl: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ManagementCommand {
    Sessions(SessionCommand),
    Memory(MemoryCommand),
    Persona(PersonaCommand),
    Companion(CompanionCommand),
    Controls(ControlsCommand),
    Voice(VoiceCommand),
    Weixin(WeixinCommand),
    Migrate(MigrationCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MigrationCommand {
    Status,
    Plan,
    Apply,
    Rollback(String),
    Events(EventMigrationCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EventMigrationCommand {
    Status {
        source: PathBuf,
    },
    Replay {
        source: PathBuf,
        after_cursor: u64,
        limit: usize,
    },
    Plan {
        source: PathBuf,
    },
    Apply {
        source: PathBuf,
    },
    Rollback {
        source: PathBuf,
        migration_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionCommand {
    List { include_archived: bool },
    Show(String),
    Rollout(String),
    History(String),
    Graph,
    Resume { id: String, prompt: Vec<String> },
    Archive(String),
    Unarchive(String),
    Pin(String),
    Unpin(String),
    Fork(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MemoryCommand {
    Status,
    List {
        global: bool,
        workspace: bool,
    },
    Show(String),
    Pending,
    Search {
        query: String,
        global: bool,
        workspace: bool,
    },
    Approve(String),
    Reject(String),
    Delete(String),
    Clear {
        workspace: bool,
        confirm: bool,
    },
    On,
    Off,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PersonaCommand {
    Status,
    Profile(Option<String>),
    List,
    Import(String),
    Set(String),
    On,
    Off,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompanionCommand {
    Status,
    Check(String),
    History,
    Clear { confirm: bool },
    On,
    Off,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ControlsCommand {
    Status,
    Show(ControlScope),
    Clear { scope: ControlScope, confirm: bool },
    Refresh,
    Audit,
    Enable(String),
    Disable(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlScope {
    Companion,
    Memory,
    Persona,
    Relationship,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VoiceCommand {
    Status,
    Doctor,
    Devices,
    Transcribe(String),
    Speak(String),
    Chat(String),
    Talk(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WeixinCommand {
    Status,
    Doctor,
    Login,
    PollLogin(Option<String>),
    Serve,
    Pair(WeixinPairCommand),
    Session(WeixinSessionCommand),
    Logout,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WeixinPairCommand {
    Request(String),
    Approve(String),
    Deny { request_id: String, reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WeixinSessionCommand {
    Bind { session_id: String, peer_id: String },
    Unbind(String),
    List,
}

pub(crate) fn parse<I, S>(arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
    if let Some(command_index) = leading_control_command_index(&arguments)? {
        let command = arguments[command_index]
            .to_str()
            .ok_or(ArgumentError::NonUnicodeValue("command"))?;
        let mut command_arguments = arguments[..command_index].to_vec();
        command_arguments.extend(arguments[command_index + 1..].iter().cloned());
        return match command {
            "status" | "doctor" | "diagnostics" | "enable" | "disable" | "reload" => {
                parse_control(command, command_arguments)
            }
            "sessions" | "memory" | "persona" | "companion" | "controls" | "voice" | "weixin"
            | "migrate" => parse_management(command, command_arguments),
            _ => unreachable!(),
        };
    }
    let mut arguments = arguments.into_iter();
    let first = arguments.next();
    match first.as_deref().and_then(OsStr::to_str) {
        Some("tui") => parse_tui(arguments),
        Some("web") => parse_web(arguments),
        Some("status" | "doctor" | "diagnostics" | "enable" | "disable" | "reload") => {
            parse_control(first.as_deref().and_then(OsStr::to_str).unwrap(), arguments)
        }
        Some(
            "sessions" | "memory" | "persona" | "companion" | "controls" | "voice" | "weixin"
            | "migrate",
        ) => parse_management(first.as_deref().and_then(OsStr::to_str).unwrap(), arguments),
        Some("run") => parse_run(arguments),
        _ => parse_run(first.into_iter().chain(arguments)),
    }
}

fn leading_control_command_index(arguments: &[OsString]) -> Result<Option<usize>, ArgumentError> {
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].to_str() {
            Some("--cwd") => {
                if index + 1 >= arguments.len() {
                    return Err(ArgumentError::MissingValue("--cwd"));
                }
                index += 2;
            }
            Some("--json" | "--jsonl") => index += 1,
            Some(
                "status" | "doctor" | "diagnostics" | "enable" | "disable" | "reload" | "sessions"
                | "memory" | "persona" | "companion" | "controls" | "voice" | "weixin" | "migrate",
            ) => return Ok(Some(index)),
            Some(value) if value.starts_with('-') => return Ok(None),
            Some(_) | None => return Ok(None),
        }
    }
    Ok(None)
}

fn parse_run<I>(arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut options = CliOptions::default();
    let mut positional = Vec::new();
    let mut end_of_options = false;
    let mut approval_seen = false;
    let mut sandbox_seen = false;
    while let Some(argument) = arguments.next() {
        if end_of_options {
            positional.push(text_argument(argument, "prompt")?);
            continue;
        }
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--") => end_of_options = true,
            Some("--no-color") => options.no_color = true,
            Some("--json") => options.output = set_output(options.output, OutputMode::Json)?,
            Some("--jsonl") => options.output = set_output(options.output, OutputMode::Jsonl)?,
            Some("--once") => {
                if options.once.is_some() {
                    return Err(ArgumentError::DuplicateOption("--once"));
                }
                options.once = Some(text_value(&mut arguments, "--once")?);
            }
            Some("--plugin") => {
                if options.session.plugin_path.is_some() {
                    return Err(ArgumentError::DuplicateOption("--plugin"));
                }
                options.session.plugin_path = Some(path_value(&mut arguments, "--plugin")?);
            }
            Some("--cwd") => {
                if options.session.cwd.is_some() {
                    return Err(ArgumentError::DuplicateOption("--cwd"));
                }
                options.session.cwd = Some(path_value(&mut arguments, "--cwd")?);
            }
            Some("--provider") => {
                if options.session.provider.is_some() {
                    return Err(ArgumentError::DuplicateOption("--provider"));
                }
                options.session.provider = Some(text_value(&mut arguments, "--provider")?);
            }
            Some("--model") => {
                if options.session.model.is_some() {
                    return Err(ArgumentError::DuplicateOption("--model"));
                }
                options.session.model = Some(text_value(&mut arguments, "--model")?);
            }
            Some("--approval") => {
                if approval_seen {
                    return Err(ArgumentError::DuplicateOption("--approval"));
                }
                approval_seen = true;
                options.session.approval =
                    parse_approval(&text_value(&mut arguments, "--approval")?)?;
            }
            Some("--sandbox") => {
                if sandbox_seen {
                    return Err(ArgumentError::DuplicateOption("--sandbox"));
                }
                sandbox_seen = true;
                options.session.sandbox = parse_sandbox(&text_value(&mut arguments, "--sandbox")?)?;
            }
            Some(value) if value.starts_with('-') => {
                return Err(ArgumentError::UnknownOption(value.to_string()));
            }
            _ => positional.push(
                argument
                    .into_string()
                    .map_err(|_| ArgumentError::NonUnicodeValue("prompt"))?,
            ),
        }
    }
    if options.once.is_none() && !positional.is_empty() {
        options.once = Some(positional.join(" "));
    } else if options.once.is_some() && !positional.is_empty() {
        return Err(ArgumentError::UnexpectedArgument(positional.join(" ")));
    }
    Ok(CliAction::Run(options))
}

fn parse_tui<I>(arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut options = TuiOptions::default();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--no-color") => options.no_color = true,
            Some("--plugin") => {
                options.session.plugin_path = Some(path_value(&mut arguments, "--plugin")?)
            }
            Some("--cwd") => options.session.cwd = Some(path_value(&mut arguments, "--cwd")?),
            Some("--provider") => {
                options.session.provider = Some(text_value(&mut arguments, "--provider")?)
            }
            Some("--model") => options.session.model = Some(text_value(&mut arguments, "--model")?),
            Some("--approval") => {
                options.session.approval =
                    parse_approval(&text_value(&mut arguments, "--approval")?)?
            }
            Some("--sandbox") => {
                options.session.sandbox = parse_sandbox(&text_value(&mut arguments, "--sandbox")?)?
            }
            Some(value) => return Err(ArgumentError::UnknownOption(value.to_string())),
            None => return Err(ArgumentError::NonUnicodeValue("option")),
        }
    }
    Ok(CliAction::Tui(options))
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
                if bind_seen {
                    return Err(ArgumentError::DuplicateOption("--bind"));
                }
                let value = text_value(&mut arguments, "--bind")?;
                options.bind = value
                    .parse()
                    .map_err(|_| ArgumentError::InvalidValue("--bind", value))?;
                bind_seen = true;
            }
            Some("--plugin") => {
                options.session.plugin_path = Some(path_value(&mut arguments, "--plugin")?)
            }
            Some("--cwd") => options.session.cwd = Some(path_value(&mut arguments, "--cwd")?),
            Some("--provider") => {
                options.session.provider = Some(text_value(&mut arguments, "--provider")?)
            }
            Some("--model") => options.session.model = Some(text_value(&mut arguments, "--model")?),
            Some("--approval") => {
                options.session.approval =
                    parse_approval(&text_value(&mut arguments, "--approval")?)?
            }
            Some("--sandbox") => {
                options.session.sandbox = parse_sandbox(&text_value(&mut arguments, "--sandbox")?)?
            }
            Some(value) => return Err(ArgumentError::UnknownOption(value.to_string())),
            None => return Err(ArgumentError::NonUnicodeValue("option")),
        }
    }
    Ok(CliAction::Web(options))
}

fn parse_control<I>(command: &str, arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut positional = Vec::new();
    let mut cwd = None;
    let mut json = false;
    let jsonl = false;
    let mut cwd_seen = false;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--json") => {
                if json || jsonl {
                    return Err(ArgumentError::DuplicateOption("--json"));
                }
                json = true;
            }
            Some("--jsonl") => {
                return Err(ArgumentError::InvalidValue(
                    "--jsonl",
                    "metadata commands require --json".to_string(),
                ));
            }
            Some("--cwd") => {
                if cwd_seen {
                    return Err(ArgumentError::DuplicateOption("--cwd"));
                }
                cwd_seen = true;
                cwd = Some(path_value(&mut arguments, "--cwd")?);
            }
            Some(value) if value.starts_with('-') => {
                return Err(ArgumentError::UnknownOption(value.to_string()));
            }
            _ => positional.push(text_argument(argument, "argument")?),
        }
    }
    let value = one_or_none(positional, command)?;
    let command = match command {
        "status" => reject_value(command, value).map(|_| ControlCommand::Status)?,
        "doctor" => reject_value(command, value).map(|_| ControlCommand::Doctor)?,
        "diagnostics" => reject_value(command, value).map(|_| ControlCommand::Diagnostics)?,
        "enable" => ControlCommand::Enable(value.ok_or(ArgumentError::MissingValue("plugin-id"))?),
        "disable" => {
            ControlCommand::Disable(value.ok_or(ArgumentError::MissingValue("plugin-id"))?)
        }
        "reload" => ControlCommand::Reload(value),
        _ => unreachable!(),
    };
    Ok(CliAction::Control(ControlOptions {
        command,
        cwd,
        json,
        jsonl,
    }))
}

fn parse_management<I>(command: &str, arguments: I) -> Result<CliAction, ArgumentError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut positional = Vec::new();
    let mut cwd = None;
    let mut json = false;
    let jsonl = false;
    let mut cwd_seen = false;
    let mut memory_workspace = false;
    let mut memory_global = false;
    let mut memory_confirm = false;
    let mut companion_confirm = false;
    let mut controls_confirm = false;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--json") => {
                if json || jsonl {
                    return Err(ArgumentError::DuplicateOption("--json"));
                }
                json = true;
            }
            Some("--jsonl") => {
                return Err(ArgumentError::InvalidValue(
                    "--jsonl",
                    "metadata commands require --json".to_string(),
                ));
            }
            Some("--cwd") => {
                if cwd_seen {
                    return Err(ArgumentError::DuplicateOption("--cwd"));
                }
                cwd_seen = true;
                cwd = Some(path_value(&mut arguments, "--cwd")?);
            }
            Some("--all") if command == "sessions" => positional.push("--all".to_string()),
            Some("--workspace") if command == "memory" => {
                if memory_workspace {
                    return Err(ArgumentError::DuplicateOption("--workspace"));
                }
                memory_workspace = true;
            }
            Some("--global") if command == "memory" => {
                if memory_global {
                    return Err(ArgumentError::DuplicateOption("--global"));
                }
                memory_global = true;
            }
            Some("--confirm") if matches!(command, "memory" | "companion" | "controls") => {
                match command {
                    "memory" => {
                        if memory_confirm {
                            return Err(ArgumentError::DuplicateOption("--confirm"));
                        }
                        memory_confirm = true;
                    }
                    "companion" => {
                        if companion_confirm {
                            return Err(ArgumentError::DuplicateOption("--confirm"));
                        }
                        companion_confirm = true;
                    }
                    "controls" => {
                        if controls_confirm {
                            return Err(ArgumentError::DuplicateOption("--confirm"));
                        }
                        controls_confirm = true;
                    }
                    _ => unreachable!(),
                }
            }
            Some("--confirm") => {
                return Err(ArgumentError::UnknownOption("--confirm".to_string()));
            }
            Some("--workspace") | Some("--global") if command != "memory" => {
                return Err(ArgumentError::UnknownOption(
                    argument.to_string_lossy().into_owned(),
                ));
            }
            Some(value) if value.starts_with('-') => {
                return Err(ArgumentError::UnknownOption(value.to_string()));
            }
            _ => positional.push(text_argument(argument, "argument")?),
        }
    }
    let command = match command {
        "sessions" => ManagementCommand::Sessions(parse_sessions(positional)?),
        "memory" => ManagementCommand::Memory(parse_memory(
            positional,
            memory_global,
            memory_workspace,
            memory_confirm,
        )?),
        "persona" => ManagementCommand::Persona(parse_persona(positional)?),
        "companion" => {
            ManagementCommand::Companion(parse_companion(positional, companion_confirm)?)
        }
        "controls" => ManagementCommand::Controls(parse_controls(positional, controls_confirm)?),
        "voice" => ManagementCommand::Voice(parse_voice(positional)?),
        "weixin" => ManagementCommand::Weixin(parse_weixin(positional)?),
        "migrate" => ManagementCommand::Migrate(parse_migration(positional)?),
        _ => unreachable!(),
    };
    Ok(CliAction::Management(ManagementOptions {
        command,
        cwd,
        json,
        jsonl,
    }))
}

fn parse_sessions(values: Vec<String>) -> Result<SessionCommand, ArgumentError> {
    match values.as_slice() {
        [] => Ok(SessionCommand::List {
            include_archived: false,
        }),
        [value] if value == "list" => Ok(SessionCommand::List {
            include_archived: false,
        }),
        [value] if value == "--all" => Ok(SessionCommand::List {
            include_archived: true,
        }),
        [action, id] if action == "show" => Ok(SessionCommand::Show(id.clone())),
        [action, id] if action == "rollout" => Ok(SessionCommand::Rollout(id.clone())),
        [action, id] if action == "history" => Ok(SessionCommand::History(id.clone())),
        [action] if action == "graph" => Ok(SessionCommand::Graph),
        [action, id] if action == "resume" => Ok(SessionCommand::Resume {
            id: id.clone(),
            prompt: Vec::new(),
        }),
        [action, id, prompt @ ..] if action == "resume" => Ok(SessionCommand::Resume {
            id: id.clone(),
            prompt: prompt.to_vec(),
        }),
        [action, id] if action == "archive" => Ok(SessionCommand::Archive(id.clone())),
        [action, id] if action == "unarchive" => Ok(SessionCommand::Unarchive(id.clone())),
        [action, id] if action == "pin" => Ok(SessionCommand::Pin(id.clone())),
        [action, id] if action == "unpin" => Ok(SessionCommand::Unpin(id.clone())),
        [action, id] if action == "fork" => Ok(SessionCommand::Fork(id.clone())),
        [value] => Err(ArgumentError::UnexpectedArgument(format!(
            "sessions {value}"
        ))),
        values => Err(ArgumentError::UnexpectedArgument(values.join(" "))),
    }
}

fn parse_memory(
    values: Vec<String>,
    global: bool,
    workspace: bool,
    confirm: bool,
) -> Result<MemoryCommand, ArgumentError> {
    if global && workspace {
        return Err(ArgumentError::ConflictingOptions("--global/--workspace"));
    }
    match values.as_slice() {
        [] => Ok(MemoryCommand::Status),
        [value] if value == "status" => Ok(MemoryCommand::Status),
        [value] if value == "list" => Ok(MemoryCommand::List { global, workspace }),
        [value] if value == "pending" => Ok(MemoryCommand::Pending),
        [value] if value == "clear" => {
            if !workspace || !confirm {
                return Err(ArgumentError::UnexpectedArgument(
                    "memory clear requires --workspace --confirm".to_string(),
                ));
            }
            Ok(MemoryCommand::Clear { workspace, confirm })
        }
        [value] if value == "on" => Ok(MemoryCommand::On),
        [value] if value == "off" => Ok(MemoryCommand::Off),
        [action, id] if action == "show" => Ok(MemoryCommand::Show(id.clone())),
        [action, query @ ..] if action == "search" && !query.is_empty() => {
            Ok(MemoryCommand::Search {
                query: query.join(" "),
                global,
                workspace,
            })
        }
        [action, id] if action == "approve" => Ok(MemoryCommand::Approve(id.clone())),
        [action, id] if action == "reject" => Ok(MemoryCommand::Reject(id.clone())),
        [action, id] if action == "delete" => Ok(MemoryCommand::Delete(id.clone())),
        [value] => Err(ArgumentError::UnexpectedArgument(format!("memory {value}"))),
        values => Err(ArgumentError::UnexpectedArgument(values.join(" "))),
    }
}

fn parse_persona(values: Vec<String>) -> Result<PersonaCommand, ArgumentError> {
    match values.as_slice() {
        [] => Ok(PersonaCommand::Status),
        [value] if value == "status" => Ok(PersonaCommand::Status),
        [value] if value == "list" => Ok(PersonaCommand::List),
        [value] if value == "profile" => Ok(PersonaCommand::Profile(None)),
        [action, id] if action == "profile" => Ok(PersonaCommand::Profile(Some(id.clone()))),
        [action, path] if action == "import" => Ok(PersonaCommand::Import(path.clone())),
        [action, id] if action == "set" => Ok(PersonaCommand::Set(id.clone())),
        [value] if value == "on" => Ok(PersonaCommand::On),
        [value] if value == "off" => Ok(PersonaCommand::Off),
        values => Err(ArgumentError::UnexpectedArgument(format!(
            "persona {}",
            values.join(" ")
        ))),
    }
}

fn parse_companion(values: Vec<String>, confirm: bool) -> Result<CompanionCommand, ArgumentError> {
    match values.as_slice() {
        [] => Ok(CompanionCommand::Status),
        [value] if value == "status" => Ok(CompanionCommand::Status),
        [action, prompt @ ..] if action == "check" && !prompt.is_empty() => {
            Ok(CompanionCommand::Check(prompt.join(" ")))
        }
        [value] if value == "history" => Ok(CompanionCommand::History),
        [value] if value == "clear" => Ok(CompanionCommand::Clear { confirm }),
        [value] if value == "on" => Ok(CompanionCommand::On),
        [value] if value == "off" => Ok(CompanionCommand::Off),
        values => Err(ArgumentError::UnexpectedArgument(format!(
            "companion {}",
            values.join(" ")
        ))),
    }
}

fn parse_controls(values: Vec<String>, confirm: bool) -> Result<ControlsCommand, ArgumentError> {
    match values.as_slice() {
        [] => Ok(ControlsCommand::Status),
        [value] if value == "status" || value == "list" => Ok(ControlsCommand::Status),
        [value] if value == "refresh" => Ok(ControlsCommand::Refresh),
        [value] if value == "audit" => Ok(ControlsCommand::Audit),
        [action, scope] if action == "show" => {
            Ok(ControlsCommand::Show(parse_control_scope(scope)?))
        }
        [action, scope] if action == "clear" => Ok(ControlsCommand::Clear {
            scope: parse_control_scope(scope)?,
            confirm,
        }),
        [action, id] if action == "enable" || action == "on" => {
            Ok(ControlsCommand::Enable(id.clone()))
        }
        [action, id] if action == "disable" || action == "off" => {
            Ok(ControlsCommand::Disable(id.clone()))
        }
        values => Err(ArgumentError::UnexpectedArgument(format!(
            "controls {}",
            values.join(" ")
        ))),
    }
}

fn parse_control_scope(value: &str) -> Result<ControlScope, ArgumentError> {
    match value {
        "companion" => Ok(ControlScope::Companion),
        "memory" => Ok(ControlScope::Memory),
        "persona" => Ok(ControlScope::Persona),
        "relationship" => Ok(ControlScope::Relationship),
        _ => Err(ArgumentError::InvalidValue(
            "control scope",
            value.to_string(),
        )),
    }
}

fn parse_voice(values: Vec<String>) -> Result<VoiceCommand, ArgumentError> {
    match values.as_slice() {
        [] => Ok(VoiceCommand::Status),
        [value] if value == "status" => Ok(VoiceCommand::Status),
        [value] if value == "doctor" => Ok(VoiceCommand::Doctor),
        [value] if value == "devices" => Ok(VoiceCommand::Devices),
        [action, text] if action == "transcribe" => Ok(VoiceCommand::Transcribe(text.clone())),
        [action, text] if action == "speak" => Ok(VoiceCommand::Speak(text.clone())),
        [action, text] if action == "chat" => Ok(VoiceCommand::Chat(text.clone())),
        [action, text] if action == "talk" => Ok(VoiceCommand::Talk(text.clone())),
        values => Err(ArgumentError::UnexpectedArgument(format!(
            "voice {}",
            values.join(" ")
        ))),
    }
}

fn parse_weixin(values: Vec<String>) -> Result<WeixinCommand, ArgumentError> {
    match values.as_slice() {
        [] => Ok(WeixinCommand::Status),
        [value] if value == "status" => Ok(WeixinCommand::Status),
        [value] if value == "doctor" => Ok(WeixinCommand::Doctor),
        [value] if value == "login" => Ok(WeixinCommand::Login),
        [value] if value == "poll-login" || value == "poll_login" => {
            Ok(WeixinCommand::PollLogin(None))
        }
        [action, verify_code] if action == "poll-login" || action == "poll_login" => {
            Ok(WeixinCommand::PollLogin(Some(verify_code.clone())))
        }
        [value] if value == "serve" => Ok(WeixinCommand::Serve),
        [action, kind, peer_id] if action == "pair" && kind == "request" => Ok(
            WeixinCommand::Pair(WeixinPairCommand::Request(peer_id.clone())),
        ),
        [action, kind, request_id] if action == "pair" && kind == "approve" => Ok(
            WeixinCommand::Pair(WeixinPairCommand::Approve(request_id.clone())),
        ),
        [action, kind, request_id, reason] if action == "pair" && kind == "deny" => {
            Ok(WeixinCommand::Pair(WeixinPairCommand::Deny {
                request_id: request_id.clone(),
                reason: reason.clone(),
            }))
        }
        [action, kind] if action == "session" && kind == "list" => {
            Ok(WeixinCommand::Session(WeixinSessionCommand::List))
        }
        [action, kind, session_id, peer_id] if action == "session" && kind == "bind" => {
            Ok(WeixinCommand::Session(WeixinSessionCommand::Bind {
                session_id: session_id.clone(),
                peer_id: peer_id.clone(),
            }))
        }
        [action, kind, session_id] if action == "session" && kind == "unbind" => Ok(
            WeixinCommand::Session(WeixinSessionCommand::Unbind(session_id.clone())),
        ),
        [value] if value == "logout" => Ok(WeixinCommand::Logout),
        values => Err(ArgumentError::UnexpectedArgument(format!(
            "weixin {}",
            values.join(" ")
        ))),
    }
}

fn parse_migration(values: Vec<String>) -> Result<MigrationCommand, ArgumentError> {
    let values = values.as_slice();
    if values.first().is_some_and(|value| value == "events") {
        return parse_event_migration(&values[1..]).map(MigrationCommand::Events);
    }
    let values = if values.first().is_some_and(|value| value == "sessions") {
        &values[1..]
    } else {
        values
    };
    match values {
        [] => Ok(MigrationCommand::Status),
        [action] if action == "status" => Ok(MigrationCommand::Status),
        [action] if action == "plan" => Ok(MigrationCommand::Plan),
        [action] if action == "apply" => Ok(MigrationCommand::Apply),
        [action, id] if action == "rollback" => Ok(MigrationCommand::Rollback(id.clone())),
        values => Err(ArgumentError::UnexpectedArgument(format!(
            "migrate {}",
            values.join(" ")
        ))),
    }
}

fn parse_event_migration(values: &[String]) -> Result<EventMigrationCommand, ArgumentError> {
    let path = |value: &String| PathBuf::from(value);
    match values {
        [action, source] if action == "status" => Ok(EventMigrationCommand::Status {
            source: path(source),
        }),
        [action, source] if action == "replay" => Ok(EventMigrationCommand::Replay {
            source: path(source),
            after_cursor: 0,
            limit: 128,
        }),
        [action, source, after_cursor] if action == "replay" => Ok(EventMigrationCommand::Replay {
            source: path(source),
            after_cursor: parse_u64_argument("event cursor", after_cursor)?,
            limit: 128,
        }),
        [action, source, after_cursor, limit] if action == "replay" => {
            Ok(EventMigrationCommand::Replay {
                source: path(source),
                after_cursor: parse_u64_argument("event cursor", after_cursor)?,
                limit: parse_usize_argument("event page limit", limit)?,
            })
        }
        [action, source] if action == "plan" => Ok(EventMigrationCommand::Plan {
            source: path(source),
        }),
        [action, source] if action == "apply" => Ok(EventMigrationCommand::Apply {
            source: path(source),
        }),
        [action, source, migration_id] if action == "rollback" => {
            Ok(EventMigrationCommand::Rollback {
                source: path(source),
                migration_id: migration_id.clone(),
            })
        }
        _ => Err(ArgumentError::UnexpectedArgument(format!(
            "migrate events {}",
            values.join(" ")
        ))),
    }
}

fn parse_u64_argument(name: &'static str, value: &str) -> Result<u64, ArgumentError> {
    value
        .parse()
        .map_err(|_| ArgumentError::InvalidValue(name, value.to_string()))
}

fn parse_usize_argument(name: &'static str, value: &str) -> Result<usize, ArgumentError> {
    value
        .parse()
        .map_err(|_| ArgumentError::InvalidValue(name, value.to_string()))
}

fn text_value<I>(arguments: &mut I, option: &'static str) -> Result<String, ArgumentError>
where
    I: Iterator<Item = OsString>,
{
    let value = arguments
        .next()
        .ok_or(ArgumentError::MissingValue(option))?;
    text_argument(value, option)
}

fn path_value<I>(arguments: &mut I, option: &'static str) -> Result<PathBuf, ArgumentError>
where
    I: Iterator<Item = OsString>,
{
    Ok(PathBuf::from(text_value(arguments, option)?))
}

fn text_argument(value: OsString, option: &'static str) -> Result<String, ArgumentError> {
    let value = value
        .into_string()
        .map_err(|_| ArgumentError::NonUnicodeValue(option))?;
    if value.trim().is_empty() {
        return Err(ArgumentError::EmptyValue(option));
    }
    Ok(value)
}

fn one_or_none(values: Vec<String>, command: &str) -> Result<Option<String>, ArgumentError> {
    match values.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some(value.clone())),
        values => Err(ArgumentError::UnexpectedArgument(format!(
            "{command} {}",
            values.join(" ")
        ))),
    }
}

fn reject_value(command: &str, value: Option<String>) -> Result<(), ArgumentError> {
    if let Some(value) = value {
        return Err(ArgumentError::UnexpectedArgument(format!(
            "{command} {value}"
        )));
    }
    Ok(())
}

fn parse_approval(value: &str) -> Result<ApprovalMode, ArgumentError> {
    match value.to_ascii_lowercase().as_str() {
        "never" | "auto" => Ok(ApprovalMode::Never),
        "on-request" | "on_request" | "ask" => Ok(ApprovalMode::OnRequest),
        "on-failure" | "on_failure" => Ok(ApprovalMode::OnFailure),
        "untrusted" => Ok(ApprovalMode::Untrusted),
        _ => Err(ArgumentError::InvalidValue("--approval", value.to_string())),
    }
}

fn parse_sandbox(value: &str) -> Result<SandboxMode, ArgumentError> {
    match value.to_ascii_lowercase().as_str() {
        "read-only" | "read_only" | "readonly" => Ok(SandboxMode::ReadOnly),
        "workspace-write" | "workspace_write" | "workspace" => Ok(SandboxMode::WorkspaceWrite),
        "danger-full-access" | "danger_full_access" | "full" => Ok(SandboxMode::DangerFullAccess),
        _ => Err(ArgumentError::InvalidValue("--sandbox", value.to_string())),
    }
}

fn set_output(current: OutputMode, requested: OutputMode) -> Result<OutputMode, ArgumentError> {
    if current != OutputMode::Human {
        return Err(ArgumentError::ConflictingOptions("--json/--jsonl"));
    }
    Ok(requested)
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
    ConflictingOptions(&'static str),
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
            Self::ConflictingOptions(options) => {
                write!(formatter, "conflicting output options: {options}")
            }
        }
    }
}

impl Error for ArgumentError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_run_remains_empty_and_legacy_prompt_is_accepted() {
        assert_eq!(
            parse(std::iter::empty::<&str>()),
            Ok(CliAction::Run(CliOptions::default()))
        );
        let action = parse([
            "hello",
            "world",
            "--provider",
            "deepseek",
            "--model",
            "deepseek-chat",
        ])
        .expect("parse prompt");
        let CliAction::Run(options) = action else {
            panic!("run action")
        };
        assert_eq!(options.once.as_deref(), Some("hello world"));
        assert_eq!(options.session.provider.as_deref(), Some("deepseek"));
        assert_eq!(options.session.model.as_deref(), Some("deepseek-chat"));
    }

    #[test]
    fn parses_runtime_safety_and_machine_output_options() {
        let action = parse([
            "--once",
            "hello",
            "--cwd",
            "C:/workspace",
            "--approval",
            "never",
            "--sandbox",
            "read-only",
            "--jsonl",
        ])
        .expect("parse options");
        assert_eq!(
            action,
            CliAction::Run(CliOptions {
                once: Some("hello".to_string()),
                session: SessionOptions {
                    cwd: Some(PathBuf::from("C:/workspace")),
                    approval: ApprovalMode::Never,
                    sandbox: SandboxMode::ReadOnly,
                    ..SessionOptions::default()
                },
                output: OutputMode::Jsonl,
                ..CliOptions::default()
            })
        );
    }

    #[test]
    fn parses_control_and_management_entries_without_ambiguity() {
        assert_eq!(
            parse(["doctor", "--cwd", ".", "--json"]),
            Ok(CliAction::Control(ControlOptions {
                command: ControlCommand::Doctor,
                cwd: Some(PathBuf::from(".")),
                json: true,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["sessions", "show", "abc", "--json"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Sessions(SessionCommand::Show("abc".to_string())),
                cwd: None,
                json: true,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["sessions", "rollout", "abc"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Sessions(SessionCommand::Rollout("abc".to_string())),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["sessions", "history", "abc"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Sessions(SessionCommand::History("abc".to_string())),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["sessions", "graph"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Sessions(SessionCommand::Graph),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["sessions", "resume", "abc", "keep", "going"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Sessions(SessionCommand::Resume {
                    id: "abc".to_string(),
                    prompt: vec!["keep".to_string(), "going".to_string()],
                }),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["voice", "devices"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Voice(VoiceCommand::Devices),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["voice", "speak", "hello"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Voice(VoiceCommand::Speak("hello".to_string())),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["memory", "list", "--global"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Memory(MemoryCommand::List {
                    global: true,
                    workspace: false,
                }),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["memory", "search", "two", "words", "--workspace"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Memory(MemoryCommand::Search {
                    query: "two words".to_string(),
                    global: false,
                    workspace: true,
                }),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["companion", "check", "two", "words"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Companion(CompanionCommand::Check(
                    "two words".to_string(),
                )),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["companion", "history"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Companion(CompanionCommand::History),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["companion", "clear", "--confirm"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Companion(CompanionCommand::Clear { confirm: true }),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["controls", "show", "relationship"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Controls(ControlsCommand::Show(
                    ControlScope::Relationship,
                )),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["controls", "clear", "companion", "--confirm"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Controls(ControlsCommand::Clear {
                    scope: ControlScope::Companion,
                    confirm: true,
                }),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["controls", "refresh"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Controls(ControlsCommand::Refresh),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["controls", "audit"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Controls(ControlsCommand::Audit),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["weixin", "pair", "request", "peer-1"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Weixin(WeixinCommand::Pair(
                    WeixinPairCommand::Request("peer-1".to_string()),
                )),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["migrate", "sessions", "plan", "--json"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Migrate(MigrationCommand::Plan),
                cwd: None,
                json: true,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["migrate", "rollback", "migration-1"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Migrate(MigrationCommand::Rollback(
                    "migration-1".to_string(),
                )),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse([
                "migrate",
                "events",
                "replay",
                ".yunxi/sessions/legacy.jsonl",
                "12",
                "32",
                "--json",
            ]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Migrate(MigrationCommand::Events(
                    EventMigrationCommand::Replay {
                        source: PathBuf::from(".yunxi/sessions/legacy.jsonl"),
                        after_cursor: 12,
                        limit: 32,
                    },
                )),
                cwd: None,
                json: true,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["--cwd", ".", "--json", "sessions", "list"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Sessions(SessionCommand::List {
                    include_archived: false,
                }),
                cwd: Some(PathBuf::from(".")),
                json: true,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["sessions", "--all"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Sessions(SessionCommand::List {
                    include_archived: true,
                }),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["memory", "clear", "--workspace", "--confirm"]),
            Ok(CliAction::Management(ManagementOptions {
                command: ManagementCommand::Memory(MemoryCommand::Clear {
                    workspace: true,
                    confirm: true,
                }),
                cwd: None,
                json: false,
                jsonl: false,
            }))
        );
        assert_eq!(
            parse(["--cwd", ".", "doctor", "--json"]),
            Ok(CliAction::Control(ControlOptions {
                command: ControlCommand::Doctor,
                cwd: Some(PathBuf::from(".")),
                json: true,
                jsonl: false,
            }))
        );
    }

    #[test]
    fn rejects_conflicting_machine_formats_and_invalid_modes() {
        assert_eq!(
            parse(["--json", "--jsonl"]),
            Err(ArgumentError::ConflictingOptions("--json/--jsonl"))
        );
        assert_eq!(
            parse(["memory", "list", "--global", "--workspace"]),
            Err(ArgumentError::ConflictingOptions("--global/--workspace"))
        );
        assert_eq!(
            parse(["--approval", "unsafe"]),
            Err(ArgumentError::InvalidValue(
                "--approval",
                "unsafe".to_string()
            ))
        );
        assert_eq!(
            parse(["--sandbox", "unsafe"]),
            Err(ArgumentError::InvalidValue(
                "--sandbox",
                "unsafe".to_string()
            ))
        );
        assert_eq!(
            parse(["sessions", "list", "--jsonl"]),
            Err(ArgumentError::InvalidValue(
                "--jsonl",
                "metadata commands require --json".to_string(),
            ))
        );
        assert_eq!(
            parse(["memory", "clear"]),
            Err(ArgumentError::UnexpectedArgument(
                "memory clear requires --workspace --confirm".to_string(),
            ))
        );
        assert_eq!(
            parse(["--", "sessions"]),
            Ok(CliAction::Run(CliOptions {
                once: Some("sessions".to_string()),
                ..CliOptions::default()
            }))
        );
    }
}
