#![doc = "Native command router for legacy YunXi and YunXi Next."]
#![forbid(unsafe_code)]

use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const NEXT_ROUTE_FILE: &str = "yunxi-next.path";
const LEGACY_ROUTE_FILE: &str = "yunxi-legacy.path";

pub fn run_from_env() -> Result<i32, LauncherError> {
    let launcher = env::current_exe().map_err(LauncherError::CurrentExecutable)?;
    let directory = launcher
        .parent()
        .ok_or_else(|| LauncherError::MissingParent(launcher.clone()))?;
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    let route = select_route(&arguments);
    let target = load_target(directory, route)?;

    let launcher = fs::canonicalize(&launcher).unwrap_or(launcher);
    if target == launcher {
        return Err(LauncherError::RecursiveRoute(target));
    }

    let status = Command::new(&target)
        .args(&arguments)
        .status()
        .map_err(|source| LauncherError::Spawn {
            target: target.clone(),
            source,
        })?;
    Ok(status.code().unwrap_or(1))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    Next,
    Legacy,
}

fn select_route(arguments: &[OsString]) -> Route {
    let is_next = arguments
        .first()
        .and_then(|argument| argument.to_str())
        .is_some_and(|argument| argument.eq_ignore_ascii_case("next"));
    if is_next { Route::Next } else { Route::Legacy }
}

fn load_target(directory: &Path, route: Route) -> Result<PathBuf, LauncherError> {
    let file_name = match route {
        Route::Next => NEXT_ROUTE_FILE,
        Route::Legacy => LEGACY_ROUTE_FILE,
    };
    let route_file = directory.join(file_name);
    let configured = match fs::read_to_string(&route_file) {
        Ok(value) => value,
        Err(source) if source.kind() == io::ErrorKind::NotFound && route == Route::Legacy => {
            return Err(LauncherError::LegacyNotInstalled(route_file));
        }
        Err(source) => {
            return Err(LauncherError::ReadRoute {
                path: route_file,
                source,
            });
        }
    };
    let configured = configured.trim_end_matches(['\r', '\n']);
    if configured.is_empty() {
        return Err(LauncherError::EmptyRoute(route_file));
    }

    let configured = PathBuf::from(OsStr::new(configured));
    let target = if configured.is_absolute() {
        configured
    } else {
        directory.join(configured)
    };
    fs::canonicalize(&target).map_err(|source| LauncherError::ResolveTarget {
        path: target,
        source,
    })
}

#[derive(Debug)]
pub enum LauncherError {
    CurrentExecutable(io::Error),
    MissingParent(PathBuf),
    ReadRoute { path: PathBuf, source: io::Error },
    EmptyRoute(PathBuf),
    LegacyNotInstalled(PathBuf),
    ResolveTarget { path: PathBuf, source: io::Error },
    RecursiveRoute(PathBuf),
    Spawn { target: PathBuf, source: io::Error },
}

impl fmt::Display for LauncherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExecutable(error) => {
                write!(formatter, "failed to locate the YunXi launcher: {error}")
            }
            Self::MissingParent(path) => {
                write!(formatter, "launcher path has no parent: {}", path.display())
            }
            Self::ReadRoute { path, source } => {
                write!(
                    formatter,
                    "failed to read route {}: {source}",
                    path.display()
                )
            }
            Self::EmptyRoute(path) => write!(formatter, "route file is empty: {}", path.display()),
            Self::LegacyNotInstalled(path) => write!(
                formatter,
                "legacy YunXi is not configured; run `yunxi next` or create {}",
                path.display()
            ),
            Self::ResolveTarget { path, source } => write!(
                formatter,
                "configured YunXi executable is unavailable at {}: {source}",
                path.display()
            ),
            Self::RecursiveRoute(path) => write!(
                formatter,
                "route points back to the launcher itself: {}",
                path.display()
            ),
            Self::Spawn { target, source } => write!(
                formatter,
                "failed to start configured YunXi executable {}: {source}",
                target.display()
            ),
        }
    }
}

impl Error for LauncherError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CurrentExecutable(error) => Some(error),
            Self::ReadRoute { source, .. }
            | Self::ResolveTarget { source, .. }
            | Self::Spawn { source, .. } => Some(source),
            Self::MissingParent(_)
            | Self::EmptyRoute(_)
            | Self::LegacyNotInstalled(_)
            | Self::RecursiveRoute(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_is_the_only_intercepted_subcommand() {
        assert_eq!(select_route(&[OsString::from("next")]), Route::Next);
        assert_eq!(select_route(&[OsString::from("NEXT")]), Route::Next);
        assert_eq!(select_route(&[OsString::from("run")]), Route::Legacy);
        assert_eq!(select_route(&[]), Route::Legacy);
    }
}
