//! Bounded root-to-cwd loading of project instruction files.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use yunxi_protocol::ContextComposeResult;

pub const DEFAULT_INSTRUCTIONS_FILENAME: &str = "AGENTS.md";
const MAX_INSTRUCTION_FILE_BYTES: u64 = 1024 * 1024;
const MAX_COMBINED_INSTRUCTION_BYTES: usize = 4 * 1024 * 1024;

pub fn compose_context(cwd: &Path) -> Result<ContextComposeResult, ContextComposeError> {
    let canonical = fs::canonicalize(cwd).map_err(|source| ContextComposeError::Canonicalize {
        path: cwd.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(ContextComposeError::NotDirectory(canonical));
    }

    let mut directories = canonical
        .ancestors()
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    directories.reverse();
    let mut sources = Vec::new();
    let mut sections = Vec::new();
    let mut combined_bytes: usize = 0;

    for directory in directories {
        let path = directory.join(DEFAULT_INSTRUCTIONS_FILENAME);
        if !path.is_file() {
            continue;
        }
        let metadata = fs::metadata(&path).map_err(|source| ContextComposeError::Read {
            path: path.clone(),
            source,
        })?;
        if metadata.len() > MAX_INSTRUCTION_FILE_BYTES {
            return Err(ContextComposeError::FileTooLarge {
                path,
                bytes: metadata.len(),
                maximum: MAX_INSTRUCTION_FILE_BYTES,
            });
        }
        let content = fs::read_to_string(&path).map_err(|source| ContextComposeError::Read {
            path: path.clone(),
            source,
        })?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        combined_bytes = combined_bytes
            .checked_add(trimmed.len())
            .and_then(|bytes| bytes.checked_add(2))
            .ok_or(ContextComposeError::CombinedTooLarge {
                bytes: usize::MAX,
                maximum: MAX_COMBINED_INSTRUCTION_BYTES,
            })?;
        if combined_bytes > MAX_COMBINED_INSTRUCTION_BYTES {
            return Err(ContextComposeError::CombinedTooLarge {
                bytes: combined_bytes,
                maximum: MAX_COMBINED_INSTRUCTION_BYTES,
            });
        }
        sources.push(path);
        sections.push(trimmed.to_string());
    }

    Ok(ContextComposeResult::new(sections.join("\n\n"), sources))
}

#[derive(Debug)]
pub enum ContextComposeError {
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    NotDirectory(PathBuf),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    FileTooLarge {
        path: PathBuf,
        bytes: u64,
        maximum: u64,
    },
    CombinedTooLarge {
        bytes: usize,
        maximum: usize,
    },
}

impl fmt::Display for ContextComposeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canonicalize { path, source } => {
                write!(formatter, "failed to resolve {}: {source}", path.display())
            }
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "context root is not a directory: {}",
                    path.display()
                )
            }
            Self::Read { path, source } => {
                write!(formatter, "failed to read {}: {source}", path.display())
            }
            Self::FileTooLarge {
                path,
                bytes,
                maximum,
            } => write!(
                formatter,
                "{} is {bytes} bytes; maximum is {maximum}",
                path.display()
            ),
            Self::CombinedTooLarge { bytes, maximum } => write!(
                formatter,
                "combined project instructions are {bytes} bytes; maximum is {maximum}"
            ),
        }
    }
}

impl Error for ContextComposeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Canonicalize { source, .. } | Self::Read { source, .. } => Some(source),
            Self::NotDirectory(_) | Self::FileTooLarge { .. } | Self::CombinedTooLarge { .. } => {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn instructions_are_composed_from_parent_to_child() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("yunxi-context-{}-{unique}", std::process::id()));
        let child = root.join("child");
        fs::create_dir_all(&child).expect("create fixture directories");
        fs::write(
            root.join(DEFAULT_INSTRUCTIONS_FILENAME),
            "root instructions",
        )
        .expect("write root instructions");
        fs::write(
            child.join(DEFAULT_INSTRUCTIONS_FILENAME),
            "child instructions",
        )
        .expect("write child instructions");
        let canonical_root = fs::canonicalize(&root).expect("canonicalize fixture root");
        let canonical_child = fs::canonicalize(&child).expect("canonicalize fixture child");

        let result = compose_context(&child).expect("compose context");

        assert!(
            result
                .instructions()
                .ends_with("root instructions\n\nchild instructions")
        );
        assert_eq!(
            result.sources()[result.sources().len() - 2..],
            [
                canonical_root.join(DEFAULT_INSTRUCTIONS_FILENAME),
                canonical_child.join(DEFAULT_INSTRUCTIONS_FILENAME)
            ]
        );
        fs::remove_dir_all(root).expect("remove fixture directories");
    }
}
