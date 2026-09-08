//! Bounded filesystem discovery for `SKILL.md` directories.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;
use yunxi_protocol::{
    MAX_SKILL_INSTRUCTION_BYTES, MAX_SKILL_METADATA, MAX_SKILL_PATH_BYTES,
    MAX_SKILL_TOOL_DECLARATIONS, MAX_SKILL_TOOL_SCHEMA_BYTES, SkillActionSpec, SkillContextBlock,
    SkillMetadata, SkillProtocolError, SkillToolDescriptor,
};

use crate::SkillsConfig;

const SKILL_FILE_NAME: &str = "SKILL.md";
const TOOLS_FILE_NAME: &str = "tools.json";
const ACTIONS_FILE_NAME: &str = "actions.json";
const MAX_SKILL_FILE_BYTES: usize = MAX_SKILL_INSTRUCTION_BYTES + 8 * 1024;
const MAX_TOOLS_FILE_BYTES: usize = MAX_SKILL_TOOL_SCHEMA_BYTES;
const MAX_ACTIONS_FILE_BYTES: usize = 128 * 1024;
const MAX_DISCOVERY_WARNINGS: usize = 32;
const MAX_DISCOVERY_WARNING_BYTES: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct DiscoveredSkill {
    pub metadata: SkillMetadata,
    pub instructions: String,
    pub directory: PathBuf,
    pub actions: Vec<SkillActionSpec>,
}

#[derive(Clone, Debug)]
pub(crate) struct DiscoverySnapshot {
    pub root: String,
    pub skills: Vec<DiscoveredSkill>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

pub(crate) fn discover(config: &SkillsConfig) -> Result<DiscoverySnapshot, DiscoveryError> {
    let root = config.root();
    let root_display = root.to_string_lossy().to_string();
    if root_display.len() > MAX_SKILL_PATH_BYTES {
        return Err(DiscoveryError::RootTooLong {
            length: root_display.len(),
            maximum: MAX_SKILL_PATH_BYTES,
        });
    }
    if !root.exists() {
        return Ok(DiscoverySnapshot {
            root: root_display,
            skills: Vec::new(),
            warnings: vec![format!("Skills root does not exist: {}", root.display())],
            truncated: false,
        });
    }
    if !root.is_dir() {
        return Err(DiscoveryError::RootNotDirectory {
            path: root.to_path_buf(),
        });
    }

    let canonical_root =
        fs::canonicalize(root).map_err(|source| DiscoveryError::CanonicalizeRoot {
            path: root.to_path_buf(),
            source,
        })?;
    let mut entries = fs::read_dir(root)
        .map_err(|source| DiscoveryError::ReadDirectory {
            path: root.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| DiscoveryError::ReadDirectoryEntry { source })?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    let mut truncated = false;
    let mut skill_ids = BTreeSet::new();
    let mut tool_count: usize = 0;
    for entry in entries {
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| DiscoveryError::ReadDirectoryEntry { source })?
            .is_dir()
        {
            continue;
        }
        let canonical_skill_dir = match fs::canonicalize(&path) {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!(
                    "ignored Skill directory {}: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if !canonical_skill_dir.starts_with(&canonical_root) {
            warnings.push(format!(
                "ignored Skill directory outside configured root: {}",
                path.display()
            ));
            continue;
        }
        let skill_file = path.join(SKILL_FILE_NAME);
        if !skill_file.is_file() {
            continue;
        }
        let canonical_skill_file = match fs::canonicalize(&skill_file) {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!(
                    "ignored Skill file {}: {error}",
                    skill_file.display()
                ));
                continue;
            }
        };
        if !canonical_skill_file.starts_with(&canonical_root) {
            warnings.push(format!(
                "ignored Skill file outside configured root: {}",
                skill_file.display()
            ));
            continue;
        }

        match load_skill(root, &canonical_root, &path, &skill_file) {
            Ok(skill) => {
                if skills.len() >= MAX_SKILL_METADATA {
                    truncated = true;
                    continue;
                }
                if config.is_disabled(skill.metadata.id()) {
                    continue;
                }
                if !skill_ids.insert(skill.metadata.id().to_string()) {
                    warnings.push(format!(
                        "ignored duplicate Skill id `{}`",
                        skill.metadata.id()
                    ));
                    continue;
                }
                if tool_count.saturating_add(skill.metadata.tools().len())
                    > MAX_SKILL_TOOL_DECLARATIONS
                {
                    truncated = true;
                    continue;
                }
                tool_count = tool_count.saturating_add(skill.metadata.tools().len());
                skills.push(skill);
            }
            Err(error) => warnings.push(format!("ignored Skill {}: {error}", path.display())),
        }
    }
    Ok(DiscoverySnapshot {
        root: root_display,
        skills,
        warnings: bound_warnings(warnings),
        truncated,
    })
}

fn load_skill(
    root: &Path,
    canonical_root: &Path,
    skill_dir: &Path,
    skill_file: &Path,
) -> Result<DiscoveredSkill, SkillLoadError> {
    let bytes = read_bounded(skill_file, MAX_SKILL_FILE_BYTES)?;
    let content = String::from_utf8(bytes).map_err(|_| SkillLoadError::NotUtf8)?;
    let (frontmatter, instructions) = split_document(&content);
    let directory_name = skill_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(SkillLoadError::InvalidDirectoryName)?;
    let id = directory_name.to_ascii_lowercase();
    let name = frontmatter
        .get("name")
        .cloned()
        .unwrap_or_else(|| directory_name.to_string());
    let description = frontmatter
        .get("description")
        .cloned()
        .unwrap_or_else(|| format!("Instructions for {name}"));
    let relative_path = skill_file
        .strip_prefix(root)
        .map_err(|_| SkillLoadError::PathEscape)?
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");
    SkillContextBlock::new(id.clone(), instructions.clone()).map_err(SkillLoadError::Protocol)?;
    let tools = load_tools(canonical_root, skill_dir)?;
    let actions = load_actions(canonical_root, skill_dir)?;
    if actions
        .iter()
        .any(|action| !tools.iter().any(|tool| tool.name() == action.tool_name()))
    {
        return Err(SkillLoadError::ActionToolNotDeclared);
    }
    let metadata = SkillMetadata::new(
        id,
        name,
        description,
        relative_path,
        instructions.len(),
        tools,
    )
    .map_err(SkillLoadError::Protocol)?;
    Ok(DiscoveredSkill {
        metadata,
        instructions,
        directory: fs::canonicalize(skill_dir).map_err(|source| SkillLoadError::Io {
            path: skill_dir.to_path_buf(),
            source,
        })?,
        actions,
    })
}

fn load_actions(
    canonical_root: &Path,
    skill_dir: &Path,
) -> Result<Vec<SkillActionSpec>, SkillLoadError> {
    let path = skill_dir.join(ACTIONS_FILE_NAME);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let canonical_path = fs::canonicalize(&path).map_err(|source| SkillLoadError::Io {
        path: path.clone(),
        source,
    })?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(SkillLoadError::PathEscape);
    }
    let bytes = read_bounded(&path, MAX_ACTIONS_FILE_BYTES)?;
    let actions = serde_json::from_slice::<Vec<SkillActionSpec>>(&bytes).map_err(|source| {
        SkillLoadError::Json {
            path: path.clone(),
            message: source.to_string(),
        }
    })?;
    if actions.len() > yunxi_protocol::MAX_SKILL_ACTIONS {
        return Err(SkillLoadError::TooManyActions {
            count: actions.len(),
            maximum: yunxi_protocol::MAX_SKILL_ACTIONS,
        });
    }
    let mut names = BTreeSet::new();
    for action in &actions {
        action.validate().map_err(SkillLoadError::Protocol)?;
        if !names.insert(action.tool_name()) {
            return Err(SkillLoadError::DuplicateAction {
                tool_name: action.tool_name().to_string(),
            });
        }
    }
    Ok(actions)
}

fn load_tools(
    canonical_root: &Path,
    skill_dir: &Path,
) -> Result<Vec<SkillToolDescriptor>, SkillLoadError> {
    let path = skill_dir.join(TOOLS_FILE_NAME);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let canonical_path = fs::canonicalize(&path).map_err(|source| SkillLoadError::Io {
        path: path.clone(),
        source,
    })?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(SkillLoadError::PathEscape);
    }
    let bytes = read_bounded(&path, MAX_TOOLS_FILE_BYTES)?;
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|source| SkillLoadError::Json {
        path: path.clone(),
        message: source.to_string(),
    })?;
    let tools = serde_json::from_value::<Vec<SkillToolDescriptor>>(value).map_err(|source| {
        SkillLoadError::Json {
            path,
            message: source.to_string(),
        }
    })?;
    Ok(tools)
}

fn split_document(content: &str) -> (std::collections::BTreeMap<String, String>, String) {
    let lines = content.lines().collect::<Vec<_>>();
    if lines.first().is_none_or(|line| line.trim() != "---") {
        return (std::collections::BTreeMap::new(), content.to_string());
    }
    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == "---").then_some(index))
    else {
        return (std::collections::BTreeMap::new(), content.to_string());
    };
    let mut values = std::collections::BTreeMap::new();
    for line in &lines[1..end] {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if !matches!(key.as_str(), "name" | "description") {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        values.insert(key, value);
    }
    let instructions = lines[end + 1..].join("\n");
    (values, instructions)
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, SkillLoadError> {
    let file = File::open(path).map_err(|source| SkillLoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take((maximum.saturating_add(1)) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| SkillLoadError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > maximum {
        return Err(SkillLoadError::TooLarge {
            path: path.to_path_buf(),
            size: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
}

fn bound_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.truncate(MAX_DISCOVERY_WARNINGS);
    warnings
        .into_iter()
        .map(|warning| {
            let mut warning = warning
                .chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .collect::<String>();
            if warning.len() > MAX_DISCOVERY_WARNING_BYTES {
                let mut end = MAX_DISCOVERY_WARNING_BYTES;
                while !warning.is_char_boundary(end) {
                    end -= 1;
                }
                warning.truncate(end);
            }
            warning
        })
        .collect()
}

#[derive(Debug)]
pub(crate) enum SkillLoadError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    TooLarge {
        path: PathBuf,
        size: usize,
        maximum: usize,
    },
    NotUtf8,
    InvalidDirectoryName,
    PathEscape,
    Json {
        path: PathBuf,
        message: String,
    },
    Protocol(SkillProtocolError),
    ActionToolNotDeclared,
    TooManyActions {
        count: usize,
        maximum: usize,
    },
    DuplicateAction {
        tool_name: String,
    },
}

impl fmt::Display for SkillLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::TooLarge {
                path,
                size,
                maximum,
            } => write!(
                formatter,
                "{} is {size} bytes; maximum is {maximum}",
                path.display()
            ),
            Self::NotUtf8 => formatter.write_str("SKILL.md is not valid UTF-8"),
            Self::InvalidDirectoryName => {
                formatter.write_str("Skill directory name is not valid UTF-8")
            }
            Self::PathEscape => formatter.write_str("Skill path escaped the configured root"),
            Self::Json { path, message } => {
                write!(formatter, "invalid {}: {message}", path.display())
            }
            Self::Protocol(error) => error.fmt(formatter),
            Self::ActionToolNotDeclared => {
                formatter.write_str("Skill action must reference a declared metadata tool")
            }
            Self::TooManyActions { count, maximum } => {
                write!(
                    formatter,
                    "Skill declares {count} actions; maximum is {maximum}"
                )
            }
            Self::DuplicateAction { tool_name } => {
                write!(formatter, "Skill action tool `{tool_name}` is duplicated")
            }
        }
    }
}

impl Error for SkillLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Protocol(error) => Some(error),
            Self::TooLarge { .. }
            | Self::NotUtf8
            | Self::InvalidDirectoryName
            | Self::PathEscape
            | Self::Json { .. }
            | Self::ActionToolNotDeclared
            | Self::TooManyActions { .. }
            | Self::DuplicateAction { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum DiscoveryError {
    RootTooLong { length: usize, maximum: usize },
    RootNotDirectory { path: PathBuf },
    CanonicalizeRoot { path: PathBuf, source: io::Error },
    ReadDirectory { path: PathBuf, source: io::Error },
    ReadDirectoryEntry { source: io::Error },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootTooLong { length, maximum } => {
                write!(
                    formatter,
                    "Skills root is {length} bytes; maximum is {maximum}"
                )
            }
            Self::RootNotDirectory { path } => {
                write!(
                    formatter,
                    "Skills root is not a directory: {}",
                    path.display()
                )
            }
            Self::CanonicalizeRoot { path, source } => {
                write!(
                    formatter,
                    "cannot resolve Skills root {}: {source}",
                    path.display()
                )
            }
            Self::ReadDirectory { path, source } => {
                write!(
                    formatter,
                    "cannot read Skills root {}: {source}",
                    path.display()
                )
            }
            Self::ReadDirectoryEntry { source } => {
                write!(formatter, "cannot read a Skills directory entry: {source}")
            }
        }
    }
}

impl Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CanonicalizeRoot { source, .. }
            | Self::ReadDirectory { source, .. }
            | Self::ReadDirectoryEntry { source } => Some(source),
            Self::RootTooLong { .. } | Self::RootNotDirectory { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yunxi-skills-{stamp}"));
        fs::create_dir_all(&root).expect("root");
        root
    }

    #[test]
    fn discovers_frontmatter_and_metadata_only_tools() {
        let root = temp_root();
        let skill = root.join("review");
        fs::create_dir_all(&skill).expect("skill");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: Code Review\ndescription: Review source\n---\nRead the diff.\n",
        )
        .expect("skill file");
        fs::write(
            skill.join("tools.json"),
            r#"[{"name":"check","description":"Check metadata","input_schema":{"type":"object"}}]"#,
        )
        .expect("tools file");
        let config = SkillsConfig::new(&root, Vec::<String>::new()).expect("config");
        let snapshot = discover(&config).expect("discover");
        assert_eq!(snapshot.skills.len(), 1);
        assert_eq!(snapshot.skills[0].metadata.id(), "review");
        assert_eq!(snapshot.skills[0].metadata.tools().len(), 1);
        assert!(snapshot.skills[0].instructions.contains("Read the diff"));
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_skill_without_frontmatter() {
        let root = temp_root();
        let skill = root.join("review");
        fs::create_dir_all(&skill).expect("skill dir");
        fs::write(skill.join("SKILL.md"), "review instructions\n").expect("skill file");
        fs::write(
            skill.join("tools.json"),
            r#"[{"name":"check","description":"Inspect metadata","input_schema":{"type":"object"}}]"#,
        )
        .expect("tools file");
        let config = SkillsConfig::new(&root, Vec::<String>::new()).expect("config");
        let snapshot = discover(&config).expect("discover");
        assert_eq!(
            snapshot.skills.len(),
            1,
            "warnings: {:?}",
            snapshot.warnings
        );
        assert_eq!(snapshot.skills[0].metadata.tools().len(), 1);
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn traversal_and_executable_tool_metadata_are_ignored() {
        let root = temp_root();
        let skill = root.join("review");
        fs::create_dir_all(&skill).expect("skill");
        fs::write(skill.join("SKILL.md"), "instructions").expect("skill file");
        fs::write(
            skill.join("tools.json"),
            r#"[{"name":"run","description":"bad","input_schema":{"type":"object","command":"echo"}}]"#,
        )
        .expect("tools file");
        let config = SkillsConfig::new(&root, Vec::<String>::new()).expect("config");
        let snapshot = discover(&config).expect("discover");
        assert!(snapshot.skills.is_empty());
        assert!(!snapshot.warnings.is_empty());
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn control_characters_in_instructions_are_rejected_during_discovery() {
        let root = temp_root();
        let skill = root.join("review");
        fs::create_dir_all(&skill).expect("skill");
        fs::write(skill.join("SKILL.md"), b"invalid\0instructions").expect("skill file");
        let config = SkillsConfig::new(&root, Vec::<String>::new()).expect("config");
        let snapshot = discover(&config).expect("discover");
        assert!(snapshot.skills.is_empty());
        assert!(!snapshot.warnings.is_empty());
        let _ignored = fs::remove_dir_all(root);
    }
}
