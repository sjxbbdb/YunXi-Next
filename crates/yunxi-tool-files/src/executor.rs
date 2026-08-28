//! Bounded read-only workspace search and file viewing.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use yunxi_protocol::{
    FileReadRequest, FileReadResult, FileSearchMatch, FileSearchRequest, FileSearchResult,
};

const MAX_QUERY_BYTES: usize = 256;
const MAX_SEARCH_ENTRIES: usize = 4096;
const MAX_SEARCH_DEPTH: usize = 32;
const MAX_READ_BYTES: usize = 1024 * 1024;

pub fn search_files(request: &FileSearchRequest) -> Result<FileSearchResult, FileToolError> {
    ensure_read_grant(request.grant())?;
    let query = request.query().trim();
    if query.is_empty() {
        return Err(FileToolError::EmptyQuery);
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err(FileToolError::QueryTooLong {
            length: query.len(),
            maximum: MAX_QUERY_BYTES,
        });
    }
    if request.max_results() == 0 {
        return Err(FileToolError::InvalidLimit);
    }
    let workspace_root =
        fs::canonicalize(request.grant().root()).map_err(|source| FileToolError::Path {
            path: request.grant().root().to_path_buf(),
            message: source.to_string(),
        })?;
    let root = resolve_directory(&workspace_root, request.root())?;
    let query = query.to_ascii_lowercase();
    let mut stack = vec![(root.clone(), 0usize)];
    let mut visited = BTreeSet::new();
    let mut matches = Vec::new();
    let mut inspected = 0usize;
    let mut truncated = false;

    while let Some((directory, depth)) = stack.pop() {
        let canonical_directory =
            fs::canonicalize(&directory).map_err(|source| FileToolError::Path {
                path: directory.clone(),
                message: source.to_string(),
            })?;
        if !canonical_directory.starts_with(&root) || !visited.insert(canonical_directory.clone()) {
            continue;
        }
        let mut entries = fs::read_dir(&canonical_directory)
            .map_err(|source| FileToolError::Read {
                path: canonical_directory.clone(),
                message: source.to_string(),
            })?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            inspected = inspected.saturating_add(1);
            if inspected > MAX_SEARCH_ENTRIES {
                truncated = true;
                break;
            }
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let canonical = match fs::canonicalize(&path) {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !canonical.starts_with(&root) {
                continue;
            }
            let is_directory = canonical.is_dir();
            if name.to_string_lossy().to_ascii_lowercase().contains(&query) {
                matches.push(FileSearchMatch::new(
                    relative_path(&workspace_root, &canonical),
                    is_directory,
                ));
                if matches.len() >= request.max_results() {
                    truncated = true;
                    break;
                }
            }
            if is_directory && depth < MAX_SEARCH_DEPTH {
                stack.push((canonical, depth + 1));
            }
        }
        if truncated || matches.len() >= request.max_results() {
            break;
        }
    }

    Ok(FileSearchResult::new(matches, truncated))
}

pub fn read_file(request: &FileReadRequest) -> Result<FileReadResult, FileToolError> {
    ensure_read_grant(request.grant())?;
    if request.max_bytes() == 0 || request.max_bytes() > MAX_READ_BYTES {
        return Err(FileToolError::InvalidLimit);
    }
    let root = fs::canonicalize(request.grant().root()).map_err(|source| FileToolError::Path {
        path: request.grant().root().to_path_buf(),
        message: source.to_string(),
    })?;
    let target = resolve_file(&root, request.path())?;
    let file = fs::File::open(&target).map_err(|source| FileToolError::Read {
        path: target.clone(),
        message: source.to_string(),
    })?;
    let mut bytes = Vec::new();
    file.take((request.max_bytes() + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| FileToolError::Read {
            path: target.clone(),
            message: source.to_string(),
        })?;
    let truncated = bytes.len() > request.max_bytes();
    if truncated {
        bytes.truncate(request.max_bytes());
    }
    let returned_bytes = bytes.len();
    let content = String::from_utf8(bytes).map_err(|_| FileToolError::NotUtf8(target.clone()))?;
    Ok(FileReadResult::new(
        relative_path(&root, &target),
        content,
        returned_bytes,
        truncated,
    ))
}

fn ensure_read_grant(grant: &yunxi_protocol::WorkspaceGrant) -> Result<(), FileToolError> {
    if grant.root().as_os_str().is_empty() {
        return Err(FileToolError::InvalidGrant);
    }
    if !grant.allows_legacy_read() {
        return Err(FileToolError::ReadNotGranted);
    }
    Ok(())
}

fn resolve_directory(root: &Path, requested: &Path) -> Result<PathBuf, FileToolError> {
    let root = fs::canonicalize(root).map_err(|source| FileToolError::Path {
        path: root.to_path_buf(),
        message: source.to_string(),
    })?;
    let requested = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let requested = fs::canonicalize(&requested).map_err(|source| FileToolError::Path {
        path: requested.clone(),
        message: source.to_string(),
    })?;
    if !requested.starts_with(&root) || !requested.is_dir() {
        return Err(FileToolError::OutsideWorkspace(requested));
    }
    Ok(requested)
}

fn resolve_file(root: &Path, requested: &Path) -> Result<PathBuf, FileToolError> {
    let requested = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let target = fs::canonicalize(&requested).map_err(|source| FileToolError::Path {
        path: requested.clone(),
        message: source.to_string(),
    })?;
    if !target.starts_with(root) || !target.is_file() {
        return Err(FileToolError::OutsideWorkspace(target));
    }
    Ok(target)
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Debug)]
pub enum FileToolError {
    Read { path: PathBuf, message: String },
    Path { path: PathBuf, message: String },
    OutsideWorkspace(PathBuf),
    NotUtf8(PathBuf),
    EmptyQuery,
    QueryTooLong { length: usize, maximum: usize },
    InvalidLimit,
    InvalidGrant,
    ReadNotGranted,
}

impl FileToolError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Read { .. } => "file_read_error",
            Self::Path { .. } => "file_path_error",
            Self::OutsideWorkspace(_) => "outside_workspace",
            Self::NotUtf8(_) => "file_not_utf8",
            Self::EmptyQuery => "empty_search_query",
            Self::QueryTooLong { .. } => "search_query_too_long",
            Self::InvalidLimit => "invalid_file_limit",
            Self::InvalidGrant => "invalid_grant",
            Self::ReadNotGranted => "read_not_granted",
        }
    }
}

impl fmt::Display for FileToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, message } => {
                write!(formatter, "failed to read {}: {message}", path.display())
            }
            Self::Path { path, message } => {
                write!(formatter, "failed to resolve {}: {message}", path.display())
            }
            Self::OutsideWorkspace(path) => {
                write!(
                    formatter,
                    "path is outside the workspace: {}",
                    path.display()
                )
            }
            Self::NotUtf8(path) => write!(formatter, "file is not UTF-8 text: {}", path.display()),
            Self::EmptyQuery => formatter.write_str("file search query cannot be empty"),
            Self::QueryTooLong { length, maximum } => write!(
                formatter,
                "file search query is {length} bytes; maximum is {maximum}"
            ),
            Self::InvalidLimit => formatter.write_str("file tool limit is invalid"),
            Self::InvalidGrant => {
                formatter.write_str("file tool received an invalid workspace grant")
            }
            Self::ReadNotGranted => {
                formatter.write_str("workspace read permission was not granted")
            }
        }
    }
}

impl Error for FileToolError {}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use yunxi_protocol::{FileReadRequest, FileSearchRequest, WorkspaceGrant};

    use super::*;

    fn fixture_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "yunxi-files-{label}-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn search_and_read_stay_inside_the_granted_workspace() {
        let root = fixture_root("basic");
        fs::create_dir_all(root.join("src")).expect("create fixture");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write source");
        fs::write(root.join("notes.txt"), "hello").expect("write notes");
        let grant = WorkspaceGrant::read_only(&root);

        let search = search_files(&FileSearchRequest::new(grant.clone(), &root, "main"))
            .expect("search files");
        assert_eq!(search.matches().len(), 1);
        assert_eq!(search.matches()[0].path(), "src/main.rs");

        let read =
            read_file(&FileReadRequest::new(grant.clone(), "src/main.rs")).expect("read file");
        assert_eq!(read.content(), "fn main() {}\n");
        assert!(matches!(
            read_file(&FileReadRequest::new(grant, "../outside.txt")),
            Err(FileToolError::Path { .. }) | Err(FileToolError::OutsideWorkspace(_))
        ));
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn read_limit_reports_truncation_without_exceeding_the_bound() {
        let root = fixture_root("limit");
        fs::create_dir_all(&root).expect("create fixture");
        fs::write(root.join("large.txt"), "1234567890").expect("write large fixture");
        let request =
            FileReadRequest::new(WorkspaceGrant::read_only(&root), "large.txt").with_max_bytes(4);
        let result = read_file(&request).expect("bounded read");
        assert_eq!(result.content(), "1234");
        assert!(result.truncated());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn binary_files_are_rejected_instead_of_being_lossily_decoded() {
        let root = fixture_root("binary");
        fs::create_dir_all(&root).expect("create fixture");
        fs::write(root.join("binary.dat"), [0xff, 0xfe, 0xfd]).expect("write binary fixture");

        let result = read_file(&FileReadRequest::new(
            WorkspaceGrant::read_only(&root),
            "binary.dat",
        ));
        assert!(matches!(result, Err(FileToolError::NotUtf8(_))));
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
