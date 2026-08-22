//! Bounded parser and transactional applicator for YunXi patch text.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use yunxi_protocol::{
    ActionGrantError, PatchApplyRequest, PatchApplyResult, PatchChangeKind, PatchFileChange,
};

const MAX_PATCH_BYTES: usize = 2 * 1024 * 1024;
const MAX_PATCH_FILES: usize = 128;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub fn apply_patch(request: &PatchApplyRequest) -> Result<PatchApplyResult, PatchError> {
    let grant = request.grant();
    grant.validate().map_err(PatchError::InvalidGrant)?;
    if !grant.workspace().allows_legacy_read() {
        return Err(PatchError::ReadNotGranted);
    }
    if !grant.allow_write() {
        return Err(PatchError::WriteNotGranted);
    }

    let root =
        fs::canonicalize(grant.workspace().root()).map_err(|error| PatchError::Workspace {
            path: grant.workspace().root().to_path_buf(),
            message: error.to_string(),
        })?;
    let working_directory =
        fs::canonicalize(grant.working_directory()).map_err(|error| PatchError::Workspace {
            path: grant.working_directory().to_path_buf(),
            message: error.to_string(),
        })?;
    if !working_directory.starts_with(&root) {
        return Err(PatchError::OutsideWorkspace {
            path: working_directory,
            root,
        });
    }

    let changes = parse_patch(request.patch())?;
    let plans = build_plans(&root, changes)?;
    apply_plans(&plans)
}

#[derive(Clone, Debug)]
struct ParsedChange {
    path: PathBuf,
    kind: ParsedChangeKind,
}

#[derive(Clone, Debug)]
enum ParsedChangeKind {
    Add { lines: Vec<String> },
    Delete,
    Update { hunks: Vec<Vec<PatchLine>> },
}

#[derive(Clone, Debug)]
enum PatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

#[derive(Clone, Debug)]
struct PlannedChange {
    target: PathBuf,
    relative: String,
    kind: PatchChangeKind,
    content: Option<Vec<u8>>,
    original: Option<Vec<u8>>,
    added_lines: usize,
    removed_lines: usize,
}

fn parse_patch(input: &str) -> Result<Vec<ParsedChange>, PatchError> {
    if input.is_empty() {
        return Err(PatchError::EmptyPatch);
    }
    if input.len() > MAX_PATCH_BYTES {
        return Err(PatchError::PatchTooLarge {
            length: input.len(),
            maximum: MAX_PATCH_BYTES,
        });
    }
    let lines = input.lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some("*** Begin Patch") {
        return Err(PatchError::InvalidPatch(
            "first line must be `*** Begin Patch`".to_string(),
        ));
    }

    let mut index = 1;
    let mut changes = Vec::new();
    let mut ended = false;
    while index < lines.len() {
        let line = lines[index].trim_end_matches('\r');
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if line.trim() == "*** End Patch" {
            ended = true;
            index += 1;
            break;
        }
        if let Some(path) = line.strip_prefix("*** Add File:") {
            let path = validate_relative_path(path.trim())?;
            index += 1;
            let mut content = Vec::new();
            while index < lines.len() && !is_section_header(lines[index]) {
                let body = lines[index].trim_end_matches('\r');
                if body == "*** End of File" {
                    index += 1;
                    continue;
                }
                let Some(body) = body.strip_prefix('+') else {
                    return Err(PatchError::InvalidPatch(format!(
                        "add file `{}` contains a line without `+`",
                        path.display()
                    )));
                };
                content.push(body.to_string());
                index += 1;
            }
            changes.push(ParsedChange {
                path,
                kind: ParsedChangeKind::Add { lines: content },
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File:") {
            changes.push(ParsedChange {
                path: validate_relative_path(path.trim())?,
                kind: ParsedChangeKind::Delete,
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File:") {
            let path = validate_relative_path(path.trim())?;
            index += 1;
            let mut hunks = Vec::<Vec<PatchLine>>::new();
            let mut current = Vec::new();
            while index < lines.len() && !is_section_header(lines[index]) {
                let body = lines[index].trim_end_matches('\r');
                if body.starts_with("*** Move to:") {
                    return Err(PatchError::Unsupported(
                        "`*** Move to:` is not supported in the first patch baseline".to_string(),
                    ));
                }
                if body.starts_with("@@") {
                    if !current.is_empty() {
                        hunks.push(std::mem::take(&mut current));
                    }
                    index += 1;
                    continue;
                }
                if body == "*** End of File" {
                    index += 1;
                    continue;
                }
                if let Some(value) = body.strip_prefix('+') {
                    current.push(PatchLine::Add(value.to_string()));
                } else if let Some(value) = body.strip_prefix('-') {
                    current.push(PatchLine::Remove(value.to_string()));
                } else if let Some(value) = body.strip_prefix(' ') {
                    current.push(PatchLine::Context(value.to_string()));
                } else if body.is_empty() {
                    current.push(PatchLine::Context(String::new()));
                } else {
                    return Err(PatchError::InvalidPatch(format!(
                        "update file `{}` contains an invalid line `{body}`",
                        path.display()
                    )));
                }
                index += 1;
            }
            if !current.is_empty() {
                hunks.push(current);
            }
            if hunks.is_empty() {
                return Err(PatchError::InvalidPatch(format!(
                    "update file `{}` has no hunk lines",
                    path.display()
                )));
            }
            changes.push(ParsedChange {
                path,
                kind: ParsedChangeKind::Update { hunks },
            });
            continue;
        }
        return Err(PatchError::InvalidPatch(format!(
            "unsupported patch section `{line}`"
        )));
    }

    if !ended {
        return Err(PatchError::InvalidPatch(
            "patch must end with `*** End Patch`".to_string(),
        ));
    }
    if lines[index..].iter().any(|line| !line.trim().is_empty()) {
        return Err(PatchError::InvalidPatch(
            "no non-empty content is allowed after `*** End Patch`".to_string(),
        ));
    }
    if changes.is_empty() {
        return Err(PatchError::InvalidPatch(
            "patch must contain at least one file section".to_string(),
        ));
    }
    if changes.len() > MAX_PATCH_FILES {
        return Err(PatchError::TooManyFiles {
            count: changes.len(),
            maximum: MAX_PATCH_FILES,
        });
    }
    let mut paths = std::collections::BTreeSet::new();
    for change in &changes {
        if !paths.insert(change.path.clone()) {
            return Err(PatchError::DuplicatePath(change.path.clone()));
        }
    }
    Ok(changes)
}

fn is_section_header(line: &str) -> bool {
    let line = line.trim_end_matches('\r');
    line.starts_with("*** Add File:")
        || line.starts_with("*** Delete File:")
        || line.starts_with("*** Update File:")
        || line.trim() == "*** End Patch"
}

fn validate_relative_path(value: &str) -> Result<PathBuf, PatchError> {
    if value.is_empty() || value.contains('\0') {
        return Err(PatchError::InvalidPath(value.to_string()));
    }
    let path = PathBuf::from(value.replace('\\', "/"));
    if path.is_absolute() {
        return Err(PatchError::InvalidPath(value.to_string()));
    }
    if value.contains(':') {
        return Err(PatchError::InvalidPath(value.to_string()));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(PatchError::InvalidPath(value.to_string()));
            }
        }
    }
    Ok(path)
}

fn build_plans(root: &Path, changes: Vec<ParsedChange>) -> Result<Vec<PlannedChange>, PatchError> {
    changes
        .into_iter()
        .map(|change| {
            let relative = change.path.to_string_lossy().replace('\\', "/");
            match change.kind {
                ParsedChangeKind::Add { lines } => {
                    let target = resolve_target(root, &change.path, false)?;
                    if target.exists() {
                        return Err(PatchError::AlreadyExists(relative));
                    }
                    let added_lines = lines.len();
                    let content = render_lines(&lines, "\n", true).into_bytes();
                    Ok(PlannedChange {
                        target,
                        relative,
                        kind: PatchChangeKind::Added,
                        content: Some(content),
                        original: None,
                        added_lines,
                        removed_lines: 0,
                    })
                }
                ParsedChangeKind::Delete => {
                    let target = resolve_target(root, &change.path, true)?;
                    let original = read_bounded_file(&target)?;
                    let removed_lines = count_lines(&original);
                    Ok(PlannedChange {
                        target,
                        relative,
                        kind: PatchChangeKind::Deleted,
                        content: None,
                        original: Some(original),
                        added_lines: 0,
                        removed_lines,
                    })
                }
                ParsedChangeKind::Update { hunks } => {
                    let target = resolve_target(root, &change.path, true)?;
                    let original = read_bounded_file(&target)?;
                    let (content, added_lines, removed_lines) = apply_hunks(&original, &hunks)?;
                    Ok(PlannedChange {
                        target,
                        relative,
                        kind: PatchChangeKind::Updated,
                        content: Some(content),
                        original: Some(original),
                        added_lines,
                        removed_lines,
                    })
                }
            }
        })
        .collect()
}

fn resolve_target(root: &Path, relative: &Path, must_exist: bool) -> Result<PathBuf, PatchError> {
    let candidate = root.join(relative);
    if must_exist {
        let target = fs::canonicalize(&candidate).map_err(|error| PatchError::Workspace {
            path: candidate.clone(),
            message: error.to_string(),
        })?;
        if !target.starts_with(root) || !target.is_file() {
            return Err(PatchError::OutsideWorkspace {
                path: target,
                root: root.to_path_buf(),
            });
        }
        return Ok(target);
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| PatchError::InvalidPath(relative.to_string_lossy().into_owned()))?;
    let parent = fs::canonicalize(parent).map_err(|error| PatchError::Workspace {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;
    if !parent.starts_with(root) {
        return Err(PatchError::OutsideWorkspace {
            path: parent,
            root: root.to_path_buf(),
        });
    }
    Ok(parent.join(
        relative
            .file_name()
            .ok_or_else(|| PatchError::InvalidPath(relative.to_string_lossy().into_owned()))?,
    ))
}

fn read_bounded_file(path: &Path) -> Result<Vec<u8>, PatchError> {
    let metadata = fs::metadata(path).map_err(|error| PatchError::Workspace {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(PatchError::FileTooLarge {
            path: path.to_path_buf(),
            maximum: MAX_FILE_BYTES,
        });
    }
    fs::read(path).map_err(|error| PatchError::Workspace {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn apply_hunks(
    original: &[u8],
    hunks: &[Vec<PatchLine>],
) -> Result<(Vec<u8>, usize, usize), PatchError> {
    let text = String::from_utf8(original.to_vec())
        .map_err(|_| PatchError::InvalidUtf8("updated files must be UTF-8".to_string()))?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let normalized = text.replace("\r\n", "\n");
    let trailing_newline = normalized.ends_with('\n');
    let mut lines = split_lines(&normalized);
    let mut cursor = 0;
    let mut added = 0;
    let mut removed = 0;

    for hunk in hunks {
        let old = hunk
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(value) | PatchLine::Remove(value) => Some(value.clone()),
                PatchLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let new = hunk
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(value) | PatchLine::Add(value) => Some(value.clone()),
                PatchLine::Remove(_) => None,
            })
            .collect::<Vec<_>>();
        let position = if old.is_empty() {
            cursor.min(lines.len())
        } else {
            find_unique_match(&lines, &old, cursor)?
        };
        lines.splice(position..position + old.len(), new.clone());
        cursor = position + new.len();
        added += hunk
            .iter()
            .filter(|line| matches!(line, PatchLine::Add(_)))
            .count();
        removed += hunk
            .iter()
            .filter(|line| matches!(line, PatchLine::Remove(_)))
            .count();
    }

    Ok((
        render_lines(&lines, newline, trailing_newline).into_bytes(),
        added,
        removed,
    ))
}

fn split_lines(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    let mut lines = value
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn render_lines(lines: &[String], newline: &str, trailing_newline: bool) -> String {
    let mut rendered = lines.join(newline);
    if trailing_newline && !rendered.is_empty() {
        rendered.push_str(newline);
    }
    rendered
}

fn find_unique_match(
    lines: &[String],
    wanted: &[String],
    start: usize,
) -> Result<usize, PatchError> {
    if wanted.len() > lines.len() || start > lines.len().saturating_sub(wanted.len()) {
        return Err(PatchError::HunkContextNotFound);
    }
    let mut found = None;
    for index in start..=lines.len().saturating_sub(wanted.len()) {
        if lines[index..index + wanted.len()] == *wanted {
            if found.is_some() {
                return Err(PatchError::AmbiguousHunk);
            }
            found = Some(index);
        }
    }
    found.ok_or(PatchError::HunkContextNotFound)
}

fn count_lines(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes).lines().count()
}

fn apply_plans(plans: &[PlannedChange]) -> Result<PatchApplyResult, PatchError> {
    let mut applied = Vec::new();
    for plan in plans {
        if let Err(error) = apply_one(plan) {
            rollback(&applied);
            return Err(PatchError::Transaction(error.to_string()));
        }
        applied.push(plan);
    }
    Ok(PatchApplyResult::new(
        plans
            .iter()
            .map(|plan| {
                PatchFileChange::new(
                    plan.relative.clone(),
                    plan.kind,
                    plan.added_lines,
                    plan.removed_lines,
                )
            })
            .collect(),
    ))
}

fn apply_one(plan: &PlannedChange) -> Result<(), std::io::Error> {
    match &plan.content {
        Some(content) => {
            let temporary = temporary_path(&plan.target);
            fs::write(&temporary, content)?;
            if plan.target.exists() {
                fs::remove_file(&plan.target)?;
            }
            if let Err(error) = fs::rename(&temporary, &plan.target) {
                let _ignored = fs::remove_file(&temporary);
                return Err(error);
            }
        }
        None => fs::remove_file(&plan.target)?,
    }
    Ok(())
}

fn rollback(applied: &[&PlannedChange]) {
    for plan in applied.iter().rev() {
        match &plan.original {
            Some(original) => {
                let _ignored = fs::write(&plan.target, original);
            }
            None => {
                let _ignored = fs::remove_file(&plan.target);
            }
        }
    }
}

fn temporary_path(target: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    target.with_file_name(format!(".yunxi-next-patch-{stamp}.tmp"))
}

#[derive(Debug)]
pub enum PatchError {
    InvalidGrant(ActionGrantError),
    ReadNotGranted,
    WriteNotGranted,
    EmptyPatch,
    PatchTooLarge { length: usize, maximum: usize },
    TooManyFiles { count: usize, maximum: usize },
    InvalidPatch(String),
    Unsupported(String),
    InvalidPath(String),
    DuplicatePath(PathBuf),
    AlreadyExists(String),
    Workspace { path: PathBuf, message: String },
    OutsideWorkspace { path: PathBuf, root: PathBuf },
    FileTooLarge { path: PathBuf, maximum: u64 },
    InvalidUtf8(String),
    HunkContextNotFound,
    AmbiguousHunk,
    Transaction(String),
}

impl PatchError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidGrant(_) | Self::ReadNotGranted | Self::WriteNotGranted => "grant_denied",
            Self::EmptyPatch
            | Self::PatchTooLarge { .. }
            | Self::TooManyFiles { .. }
            | Self::InvalidPatch(_) => "invalid_patch",
            Self::Unsupported(_) => "unsupported_patch",
            Self::InvalidPath(_) | Self::OutsideWorkspace { .. } => "path_denied",
            Self::DuplicatePath(_) | Self::AlreadyExists(_) => "conflict",
            Self::Workspace { .. } | Self::FileTooLarge { .. } | Self::InvalidUtf8(_) => {
                "file_error"
            }
            Self::HunkContextNotFound | Self::AmbiguousHunk => "hunk_mismatch",
            Self::Transaction(_) => "transaction_failed",
        }
    }
}

impl fmt::Display for PatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGrant(error) => write!(formatter, "invalid action grant: {error}"),
            Self::ReadNotGranted => {
                formatter.write_str("workspace read permission was not granted")
            }
            Self::WriteNotGranted => {
                formatter.write_str("workspace write permission was not granted")
            }
            Self::EmptyPatch => formatter.write_str("patch cannot be empty"),
            Self::PatchTooLarge { length, maximum } => {
                write!(formatter, "patch is {length} bytes; maximum is {maximum}")
            }
            Self::TooManyFiles { count, maximum } => write!(
                formatter,
                "patch contains {count} files; maximum is {maximum}"
            ),
            Self::InvalidPatch(message) => formatter.write_str(message),
            Self::Unsupported(message) => formatter.write_str(message),
            Self::InvalidPath(path) => write!(
                formatter,
                "patch path is not a safe workspace-relative path: `{path}`"
            ),
            Self::DuplicatePath(path) => write!(
                formatter,
                "patch contains duplicate path `{}`",
                path.display()
            ),
            Self::AlreadyExists(path) => {
                write!(formatter, "add-file target already exists: `{path}`")
            }
            Self::Workspace { path, message } => write!(
                formatter,
                "file {} is unavailable: {message}",
                path.display()
            ),
            Self::OutsideWorkspace { path, root } => write!(
                formatter,
                "file {} is outside workspace {}",
                path.display(),
                root.display()
            ),
            Self::FileTooLarge { path, maximum } => {
                write!(formatter, "file {} exceeds {maximum} bytes", path.display())
            }
            Self::InvalidUtf8(message) => formatter.write_str(message),
            Self::HunkContextNotFound => formatter.write_str("patch hunk context was not found"),
            Self::AmbiguousHunk => formatter.write_str("patch hunk context matched more than once"),
            Self::Transaction(message) => {
                write!(formatter, "patch transaction rolled back: {message}")
            }
        }
    }
}

impl Error for PatchError {
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
    fn add_update_and_delete_stay_inside_workspace() {
        let root = test_root("round-trip");
        fs::write(root.join("existing.txt"), "one\ntwo\n").expect("write fixture");
        let patch = "*** Begin Patch\n*** Add File: added.txt\n+hello\n*** Update File: existing.txt\n@@\n-one\n+ONE\n*** End Patch\n";
        let result =
            apply_patch(&PatchApplyRequest::new(grant(&root), patch)).expect("apply patch");
        assert_eq!(result.changes().len(), 2);
        assert_eq!(
            fs::read_to_string(root.join("added.txt")).expect("read added"),
            "hello\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("existing.txt")).expect("read updated"),
            "ONE\ntwo\n"
        );

        let delete = "*** Begin Patch\n*** Delete File: added.txt\n*** End Patch\n";
        apply_patch(&PatchApplyRequest::new(grant(&root), delete)).expect("delete patch");
        assert!(!root.join("added.txt").exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn traversal_and_denied_grants_never_modify_files() {
        let root = test_root("denied");
        fs::write(root.join("existing.txt"), "one\n").expect("write fixture");
        let traversal = "*** Begin Patch\n*** Add File: ../escape.txt\n+bad\n*** End Patch\n";
        let error = apply_patch(&PatchApplyRequest::new(grant(&root), traversal))
            .expect_err("traversal must fail");
        assert_eq!(error.code(), "path_denied");

        let denied = yunxi_protocol::ActionGrant::pending(WorkspaceGrant::read_only(&root), &root);
        let patch =
            "*** Begin Patch\n*** Update File: existing.txt\n@@\n-one\n+two\n*** End Patch\n";
        let error = apply_patch(&PatchApplyRequest::new(denied, patch))
            .expect_err("unapproved patch must fail");
        assert_eq!(error.code(), "grant_denied");
        assert_eq!(
            fs::read_to_string(root.join("existing.txt")).expect("read fixture"),
            "one\n"
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn a_later_hunk_failure_rolls_back_earlier_files() {
        let root = test_root("rollback");
        fs::write(root.join("first.txt"), "one\n").expect("write first");
        let patch = "*** Begin Patch\n*** Update File: first.txt\n@@\n-one\n+ONE\n*** Update File: missing.txt\n@@\n-nope\n+value\n*** End Patch\n";
        let error = apply_patch(&PatchApplyRequest::new(grant(&root), patch))
            .expect_err("missing file must fail");
        assert_eq!(error.code(), "file_error");
        assert_eq!(
            fs::read_to_string(root.join("first.txt")).expect("read first"),
            "one\n"
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    fn test_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yunxi-patch-{label}-{stamp}"));
        fs::create_dir_all(&root).expect("create root");
        root
    }
}
