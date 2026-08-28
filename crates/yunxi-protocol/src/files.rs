//! Bounded, read-only workspace file search and viewing contracts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::WorkspaceGrant;

pub const TOOL_FILES_SEARCH_OPERATION: &str = "search";
pub const TOOL_FILES_READ_OPERATION: &str = "read";

const DEFAULT_FILE_SEARCH_RESULTS: usize = 50;
const MAX_FILE_SEARCH_RESULTS: usize = 200;
const DEFAULT_FILE_READ_BYTES: usize = 256 * 1024;
const MAX_FILE_READ_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileSearchRequest {
    grant: WorkspaceGrant,
    root: PathBuf,
    query: String,
    max_results: usize,
}

impl FileSearchRequest {
    pub fn new(grant: WorkspaceGrant, root: impl Into<PathBuf>, query: impl Into<String>) -> Self {
        Self {
            grant,
            root: root.into(),
            query: query.into(),
            max_results: DEFAULT_FILE_SEARCH_RESULTS,
        }
    }

    pub fn with_max_results(mut self, max_results: usize) -> Self {
        self.max_results = max_results.min(MAX_FILE_SEARCH_RESULTS);
        self
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn max_results(&self) -> usize {
        self.max_results
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileSearchMatch {
    path: String,
    is_directory: bool,
}

impl FileSearchMatch {
    pub fn new(path: impl Into<String>, is_directory: bool) -> Self {
        Self {
            path: path.into(),
            is_directory,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn is_directory(&self) -> bool {
        self.is_directory
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileSearchResult {
    matches: Vec<FileSearchMatch>,
    truncated: bool,
}

impl FileSearchResult {
    pub fn new(matches: Vec<FileSearchMatch>, truncated: bool) -> Self {
        Self { matches, truncated }
    }

    pub fn matches(&self) -> &[FileSearchMatch] {
        &self.matches
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileReadRequest {
    grant: WorkspaceGrant,
    path: PathBuf,
    max_bytes: usize,
}

impl FileReadRequest {
    pub fn new(grant: WorkspaceGrant, path: impl Into<PathBuf>) -> Self {
        Self {
            grant,
            path: path.into(),
            max_bytes: DEFAULT_FILE_READ_BYTES,
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes.min(MAX_FILE_READ_BYTES);
        self
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileReadResult {
    path: String,
    content: String,
    bytes: usize,
    truncated: bool,
}

impl FileReadResult {
    pub fn new(
        path: impl Into<String>,
        content: impl Into<String>,
        bytes: usize,
        truncated: bool,
    ) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
            bytes,
            truncated,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_requests_keep_read_limits_bounded() {
        let search = FileSearchRequest::new(WorkspaceGrant::read_only("C:\\workspace"), ".", "src")
            .with_max_results(usize::MAX);
        assert_eq!(search.max_results(), MAX_FILE_SEARCH_RESULTS);

        let read = FileReadRequest::new(WorkspaceGrant::read_only("C:\\workspace"), "README.md")
            .with_max_bytes(usize::MAX);
        assert_eq!(read.max_bytes(), MAX_FILE_READ_BYTES);
    }

    #[test]
    fn file_results_round_trip_with_paths_and_truncation() {
        let result = FileReadResult::new("src/lib.rs", "fn main() {}", 12, false);
        let json = serde_json::to_string(&result).expect("serialize file result");
        assert_eq!(
            serde_json::from_str::<FileReadResult>(&json).expect("deserialize file result"),
            result
        );
    }
}
