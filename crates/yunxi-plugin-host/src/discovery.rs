//! Bounded discovery and validation for Rust process plugins.
//!
//! A package is a directory containing a `plugin.json` manifest and one
//! executable.  Discovery is deliberately separate from process startup:
//! reading an invalid package never starts a child process, and a bad package
//! is retained as a per-entry failure while healthy siblings remain usable.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use yunxi_kernel::{PluginCommand, PluginId, PluginIdError};
use yunxi_protocol::{
    CapabilityDescriptor, GrantRequirement, PROTOCOL_VERSION, PluginManifest, PluginRiskLevel,
    PluginRuntimeMetadata,
};

use crate::PluginLaunch;

/// The manifest filename used by a plugin package.
pub const PLUGIN_MANIFEST_FILE: &str = "plugin.json";
/// Current on-disk package schema.
pub const PLUGIN_MANIFEST_SCHEMA_VERSION: u32 = 1;
/// Hard upper bound for packages considered in one scan.
pub const MAX_DISCOVERY_PLUGINS: usize = 256;
/// Hard upper bound for one manifest file.
pub const MAX_MANIFEST_FILE_BYTES: u64 = 64 * 1024;
/// Hard upper bound for a path retained by discovery.
pub const MAX_PLUGIN_PATH_BYTES: usize = 4096;
/// Hard upper bound for a plugin version string.
pub const MAX_PLUGIN_VERSION_BYTES: usize = 64;
/// Hard upper bound for dependencies declared by one package.
pub const MAX_MANIFEST_DEPENDENCIES: usize = 64;
/// Hard upper bound for executable arguments in one package.
pub const MAX_EXECUTABLE_ARGS: usize = 64;
/// Hard upper bound for one executable argument.
pub const MAX_EXECUTABLE_ARG_BYTES: usize = 1024;

const MAX_MANIFEST_FILENAME_BYTES: usize = 128;

// A discovered package never inherits provider keys, shell settings, or
// arbitrary process state.  The host adds its short-lived connection values
// after this baseline is created.
const SAFE_PLUGIN_ENVIRONMENT: &[&str] = &[
    "PATH",
    "Path",
    "PATHEXT",
    "SystemRoot",
    "WINDIR",
    "ComSpec",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USERPROFILE",
    "HOME",
    "YUNXI_HOME",
    "YUNXI_NEXT_HOME",
    "LANG",
    "LC_ALL",
];

/// A validated semantic version used by package metadata and exact
/// dependency requirements.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginVersion {
    raw: String,
    major: u64,
    minor: u64,
    patch: u64,
}

impl PluginVersion {
    /// Parse a SemVer-like `major.minor.patch` value.
    pub fn parse(value: impl Into<String>) -> Result<Self, VersionError> {
        let raw = value.into();
        if raw.is_empty() {
            return Err(VersionError::Empty);
        }
        if raw.len() > MAX_PLUGIN_VERSION_BYTES {
            return Err(VersionError::TooLong {
                length: raw.len(),
                maximum: MAX_PLUGIN_VERSION_BYTES,
            });
        }
        if raw
            .chars()
            .any(|character| !character.is_ascii() || character.is_control())
        {
            return Err(VersionError::Invalid {
                value: bounded_text(&raw),
            });
        }

        let (without_build, build) = match raw.split_once('+') {
            Some((core, build)) => (core, Some(build)),
            None => (raw.as_str(), None),
        };
        if let Some(build) = build {
            validate_identifiers(build, false).map_err(|_| VersionError::Invalid {
                value: bounded_text(&raw),
            })?;
        }

        let (core, prerelease) = match without_build.split_once('-') {
            Some((core, prerelease)) => (core, Some(prerelease)),
            None => (without_build, None),
        };
        if let Some(prerelease) = prerelease {
            validate_identifiers(prerelease, true).map_err(|_| VersionError::Invalid {
                value: bounded_text(&raw),
            })?;
        }

        let components = core.split('.').collect::<Vec<_>>();
        if components.len() != 3 || components.iter().any(|component| component.is_empty()) {
            return Err(VersionError::Invalid {
                value: bounded_text(&raw),
            });
        }
        let numbers = components
            .iter()
            .map(|component| {
                if (component.len() > 1 && component.starts_with('0'))
                    || !component.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(());
                }
                component.parse::<u64>().map_err(|_| ())
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| VersionError::Invalid {
                value: bounded_text(&raw),
            })?;

        Ok(Self {
            raw,
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub const fn major(&self) -> u64 {
        self.major
    }

    pub const fn minor(&self) -> u64 {
        self.minor
    }

    pub const fn patch(&self) -> u64 {
        self.patch
    }
}

impl fmt::Display for PluginVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn validate_identifiers(value: &str, reject_numeric_leading_zero: bool) -> Result<(), ()> {
    if value.is_empty() {
        return Err(());
    }
    for identifier in value.split('.') {
        if identifier.is_empty()
            || !identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || (reject_numeric_leading_zero
                && identifier.len() > 1
                && identifier.bytes().all(|byte| byte.is_ascii_digit())
                && identifier.starts_with('0'))
        {
            return Err(());
        }
    }
    Ok(())
}

/// Error returned when parsing a package version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionError {
    Empty,
    TooLong { length: usize, maximum: usize },
    Invalid { value: String },
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("version cannot be empty"),
            Self::TooLong { length, maximum } => {
                write!(formatter, "version is {length} bytes; maximum is {maximum}")
            }
            Self::Invalid { value } => write!(formatter, "invalid semantic version `{value}`"),
        }
    }
}

impl Error for VersionError {}

/// An exact dependency on another package version.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginDependency {
    id: PluginId,
    version: PluginVersion,
}

impl PluginDependency {
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Result<Self, DiscoveryError> {
        let id = PluginId::new(id.into()).map_err(DiscoveryError::InvalidPluginId)?;
        let version =
            PluginVersion::parse(version).map_err(|source| DiscoveryError::InvalidVersion {
                field: "dependency version",
                value: bounded_text(source_value(&source)),
                source,
            })?;
        Ok(Self { id, version })
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn version(&self) -> &PluginVersion {
        &self.version
    }
}

/// Versioned executable data resolved from one package directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableDefinition {
    path: PathBuf,
    args: Vec<String>,
    plugin_version: PluginVersion,
    protocol_version: u32,
}

impl ExecutableDefinition {
    pub fn new(
        path: impl Into<PathBuf>,
        plugin_version: PluginVersion,
        protocol_version: u32,
    ) -> Result<Self, DiscoveryError> {
        let path = path.into();
        validate_path_size(&path)?;
        if path.as_os_str().is_empty() {
            return Err(DiscoveryError::ExecutablePath {
                path: String::new(),
                reason: "path cannot be empty".to_string(),
            });
        }
        if protocol_version != PROTOCOL_VERSION {
            return Err(DiscoveryError::UnsupportedProtocolVersion {
                found: protocol_version,
                expected: PROTOCOL_VERSION,
            });
        }
        Ok(Self {
            path,
            args: Vec::new(),
            plugin_version,
            protocol_version,
        })
    }

    pub fn from_version(
        path: impl Into<PathBuf>,
        plugin_version: impl Into<String>,
        protocol_version: u32,
    ) -> Result<Self, DiscoveryError> {
        let version = PluginVersion::parse(plugin_version).map_err(|source| {
            DiscoveryError::InvalidVersion {
                field: "plugin version",
                value: bounded_text(source_value(&source)),
                source,
            }
        })?;
        Self::new(path, version, protocol_version)
    }

    pub fn with_args<I, S>(mut self, args: I) -> Result<Self, DiscoveryError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = validate_arguments(args)?;
        Ok(self)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn plugin_version(&self) -> &PluginVersion {
        &self.plugin_version
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
}

/// Validated metadata loaded from one package manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPackageManifest {
    schema_version: u32,
    id: PluginId,
    display_name: String,
    version: PluginVersion,
    capabilities: Vec<CapabilityDescriptor>,
    grants: Vec<GrantRequirement>,
    runtime: Option<PluginRuntimeMetadata>,
    default_enabled: bool,
    dependencies: Vec<PluginDependency>,
}

impl PluginPackageManifest {
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn version(&self) -> &PluginVersion {
        &self.version
    }

    pub fn capabilities(&self) -> &[CapabilityDescriptor] {
        &self.capabilities
    }

    pub fn grants(&self) -> &[GrantRequirement] {
        &self.grants
    }

    pub fn runtime_metadata(&self) -> Option<&PluginRuntimeMetadata> {
        self.runtime.as_ref()
    }

    pub const fn default_enabled(&self) -> bool {
        self.default_enabled
    }

    pub fn dependencies(&self) -> &[PluginDependency] {
        &self.dependencies
    }

    /// Convert package metadata to the already-versioned handshake manifest.
    pub fn protocol_manifest(&self) -> PluginManifest {
        let manifest = PluginManifest::new(
            self.id.as_str(),
            &self.display_name,
            self.version.as_str(),
            self.capabilities.clone(),
        )
        .with_grants(self.grants.clone());
        match &self.runtime {
            Some(runtime) => manifest.with_runtime_metadata(runtime.clone()),
            None => manifest,
        }
    }
}

/// One fully validated package, including its resolved executable path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPlugin {
    manifest: PluginPackageManifest,
    executable: ExecutableDefinition,
    package_dir: PathBuf,
    manifest_path: PathBuf,
    launch: PluginLaunch,
}

impl DiscoveredPlugin {
    pub fn manifest(&self) -> &PluginPackageManifest {
        &self.manifest
    }

    pub fn executable(&self) -> &ExecutableDefinition {
        &self.executable
    }

    pub fn package_dir(&self) -> &Path {
        &self.package_dir
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Produce an independent launch description for a host registration.
    pub fn launch(&self) -> PluginLaunch {
        self.launch.clone()
    }
}

/// Bounds applied before any package data is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryLimits {
    max_plugins: usize,
    max_manifest_bytes: u64,
    max_path_bytes: usize,
    max_dependencies: usize,
    max_arguments: usize,
    max_argument_bytes: usize,
}

impl DiscoveryLimits {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_plugins(mut self, value: usize) -> Self {
        self.max_plugins = value.clamp(1, MAX_DISCOVERY_PLUGINS);
        self
    }

    pub fn with_max_manifest_bytes(mut self, value: u64) -> Self {
        self.max_manifest_bytes = value.clamp(1, MAX_MANIFEST_FILE_BYTES);
        self
    }

    pub fn with_max_path_bytes(mut self, value: usize) -> Self {
        self.max_path_bytes = value.clamp(1, MAX_PLUGIN_PATH_BYTES);
        self
    }

    pub fn with_max_dependencies(mut self, value: usize) -> Self {
        self.max_dependencies = value.clamp(1, MAX_MANIFEST_DEPENDENCIES);
        self
    }

    pub fn with_max_arguments(mut self, value: usize) -> Self {
        self.max_arguments = value.clamp(1, MAX_EXECUTABLE_ARGS);
        self
    }

    pub fn with_max_argument_bytes(mut self, value: usize) -> Self {
        self.max_argument_bytes = value.clamp(1, MAX_EXECUTABLE_ARG_BYTES);
        self
    }

    pub const fn max_plugins(self) -> usize {
        self.max_plugins
    }

    pub const fn max_manifest_bytes(self) -> u64 {
        self.max_manifest_bytes
    }
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            max_plugins: MAX_DISCOVERY_PLUGINS,
            max_manifest_bytes: MAX_MANIFEST_FILE_BYTES,
            max_path_bytes: MAX_PLUGIN_PATH_BYTES,
            max_dependencies: MAX_MANIFEST_DEPENDENCIES,
            max_arguments: MAX_EXECUTABLE_ARGS,
            max_argument_bytes: MAX_EXECUTABLE_ARG_BYTES,
        }
    }
}

/// Explicit root directory for package discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginDirectory {
    root: PathBuf,
    manifest_file_name: String,
    limits: DiscoveryLimits,
}

/// Descriptive alias for callers that prefer the operation's name.
pub type PluginDiscovery = PluginDirectory;

impl PluginDirectory {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            manifest_file_name: PLUGIN_MANIFEST_FILE.to_string(),
            limits: DiscoveryLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: DiscoveryLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_manifest_file_name(
        mut self,
        name: impl Into<String>,
    ) -> Result<Self, DiscoveryError> {
        let name = name.into();
        if name.is_empty()
            || name.len() > MAX_MANIFEST_FILENAME_BYTES
            || Path::new(&name)
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(DiscoveryError::InvalidManifestFileName {
                name: bounded_text(&name),
            });
        }
        self.manifest_file_name = name;
        Ok(self)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn limits(&self) -> DiscoveryLimits {
        self.limits
    }

    /// Scan immediate child directories in deterministic name order.
    pub fn discover(&self) -> Result<DiscoveryReport, DiscoveryError> {
        validate_path_size_with_limit(&self.root, self.limits.max_path_bytes)?;
        let root_metadata =
            fs::symlink_metadata(&self.root).map_err(|error| DiscoveryError::Io {
                path: bounded_path(&self.root),
                message: bounded_text(&error.to_string()),
            })?;
        if root_metadata.file_type().is_symlink() {
            return Err(DiscoveryError::RootIsSymlink {
                path: bounded_path(&self.root),
            });
        }
        if !root_metadata.is_dir() {
            return Err(DiscoveryError::RootNotDirectory {
                path: bounded_path(&self.root),
            });
        }
        let root = fs::canonicalize(&self.root).map_err(|error| DiscoveryError::Io {
            path: bounded_path(&self.root),
            message: bounded_text(&error.to_string()),
        })?;
        validate_path_size_with_limit(&root, self.limits.max_path_bytes)?;

        let mut entries = Vec::new();
        let read_dir = fs::read_dir(&root).map_err(|error| DiscoveryError::Io {
            path: bounded_path(&root),
            message: bounded_text(&error.to_string()),
        })?;
        let mut over_limit = false;
        for entry in read_dir {
            let entry = entry.map_err(|error| DiscoveryError::Io {
                path: bounded_path(&root),
                message: bounded_text(&error.to_string()),
            })?;
            if entries.len() >= self.limits.max_plugins {
                over_limit = true;
                break;
            }
            entries.push(entry);
        }
        entries.sort_by_key(|entry| entry.file_name());

        let mut report = DiscoveryReport {
            root: root.clone(),
            plugins: Vec::new(),
            failures: Vec::new(),
        };
        if over_limit {
            report.failures.push(DiscoveryFailure::new(
                root.clone(),
                DiscoveryError::TooManyPlugins {
                    maximum: self.limits.max_plugins,
                },
            ));
        }

        let mut seen_ids = BTreeSet::new();
        for entry in entries {
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    report.failures.push(DiscoveryFailure::new(
                        path,
                        DiscoveryError::Io {
                            path: bounded_path(&entry.path()),
                            message: bounded_text(&error.to_string()),
                        },
                    ));
                    continue;
                }
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            match self.discover_package(&root, &path, &metadata) {
                Ok(plugin) => {
                    if !seen_ids.insert(plugin.manifest.id().clone()) {
                        report.failures.push(DiscoveryFailure::new(
                            plugin.package_dir().to_path_buf(),
                            DiscoveryError::DuplicatePluginId {
                                id: plugin.manifest.id().to_string(),
                            },
                        ));
                    } else {
                        report.plugins.push(plugin);
                    }
                }
                Err(error) => report.failures.push(DiscoveryFailure::new(path, error)),
            }
        }
        report
            .plugins
            .sort_by(|left, right| left.manifest.id().cmp(right.manifest.id()));
        Ok(report)
    }

    /// Discover an optional plugin root without turning a not-yet-created
    /// directory into a host-wide startup failure.
    ///
    /// A missing root is returned as an empty report with one bounded
    /// diagnostic. Other root errors retain the strict [`Self::discover`]
    /// behavior so permission and path problems remain visible to callers.
    pub fn discover_or_empty(&self) -> Result<DiscoveryReport, DiscoveryError> {
        if let Err(error) = fs::symlink_metadata(&self.root) {
            if error.kind() == io::ErrorKind::NotFound {
                let diagnostic = DiscoveryError::Io {
                    path: bounded_path(&self.root),
                    message: bounded_text(&error.to_string()),
                };
                return Ok(DiscoveryReport {
                    root: self.root.clone(),
                    plugins: Vec::new(),
                    failures: vec![DiscoveryFailure::new(self.root.clone(), diagnostic)],
                });
            }
        }
        self.discover()
    }

    fn discover_package(
        &self,
        root: &Path,
        package_dir: &Path,
        package_metadata: &Metadata,
    ) -> Result<DiscoveredPlugin, DiscoveryError> {
        if package_metadata.file_type().is_symlink() || !package_metadata.is_dir() {
            return Err(DiscoveryError::PackageNotDirectory {
                path: bounded_path(package_dir),
            });
        }
        let canonical_package =
            fs::canonicalize(package_dir).map_err(|error| DiscoveryError::Io {
                path: bounded_path(package_dir),
                message: bounded_text(&error.to_string()),
            })?;
        ensure_within(root, &canonical_package, package_dir)?;
        validate_path_size_with_limit(&canonical_package, self.limits.max_path_bytes)?;

        let manifest_path = package_dir.join(&self.manifest_file_name);
        validate_path_size_with_limit(&manifest_path, self.limits.max_path_bytes)?;
        let manifest_metadata = fs::symlink_metadata(&manifest_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                DiscoveryError::ManifestMissing {
                    path: bounded_path(&manifest_path),
                }
            } else {
                DiscoveryError::Io {
                    path: bounded_path(&manifest_path),
                    message: bounded_text(&error.to_string()),
                }
            }
        })?;
        if manifest_metadata.file_type().is_symlink() {
            return Err(DiscoveryError::ManifestIsSymlink {
                path: bounded_path(&manifest_path),
            });
        }
        if !manifest_metadata.is_file() {
            return Err(DiscoveryError::ManifestNotFile {
                path: bounded_path(&manifest_path),
            });
        }
        let bytes = read_bounded_file(&manifest_path, self.limits.max_manifest_bytes)?;
        let file_manifest = serde_json::from_slice::<FileManifest>(&bytes).map_err(|error| {
            DiscoveryError::InvalidJson {
                path: bounded_path(&manifest_path),
                message: bounded_text(&error.to_string()),
            }
        })?;
        self.resolve_package(root, &canonical_package, &manifest_path, file_manifest)
    }

    fn resolve_package(
        &self,
        root: &Path,
        package_dir: &Path,
        manifest_path: &Path,
        file: FileManifest,
    ) -> Result<DiscoveredPlugin, DiscoveryError> {
        if file.schema_version != PLUGIN_MANIFEST_SCHEMA_VERSION {
            return Err(DiscoveryError::UnsupportedSchema {
                found: file.schema_version,
                expected: PLUGIN_MANIFEST_SCHEMA_VERSION,
            });
        }
        let id = PluginId::new(file.plugin_id.clone()).map_err(DiscoveryError::InvalidPluginId)?;
        let version = PluginVersion::parse(file.plugin_version.clone()).map_err(|source| {
            DiscoveryError::InvalidVersion {
                field: "plugin version",
                value: bounded_text(&file.plugin_version),
                source,
            }
        })?;
        if file.dependencies.len() > self.limits.max_dependencies {
            return Err(DiscoveryError::TooManyDependencies {
                count: file.dependencies.len(),
                maximum: self.limits.max_dependencies,
            });
        }
        let mut dependencies = Vec::with_capacity(file.dependencies.len());
        let mut dependency_ids = BTreeSet::new();
        for dependency in file.dependencies {
            let (dependency_id, dependency_version) = dependency.into_parts()?;
            let parsed = PluginDependency::new(dependency_id, dependency_version)?;
            if !dependency_ids.insert(parsed.id().clone()) {
                return Err(DiscoveryError::DuplicateDependency {
                    plugin_id: id.to_string(),
                    dependency_id: parsed.id().to_string(),
                });
            }
            dependencies.push(parsed);
        }

        let runtime = file.runtime;
        let default_enabled = file.default_enabled.unwrap_or_else(|| {
            runtime
                .as_ref()
                .is_none_or(|metadata| metadata.default_enabled())
        });
        if default_enabled
            && runtime
                .as_ref()
                .is_some_and(|metadata| !matches!(metadata.risk(), PluginRiskLevel::Safe))
        {
            return Err(DiscoveryError::UnsafeDefaultEnabled {
                plugin_id: id.to_string(),
            });
        }

        let executable = file.executable.into_definition(&version, &self.limits)?;
        validate_relative_executable_path(executable.path())?;
        let executable_path = package_dir.join(executable.path());
        validate_path_size_with_limit(&executable_path, self.limits.max_path_bytes)?;
        let executable_metadata = fs::symlink_metadata(&executable_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                DiscoveryError::ExecutableMissing {
                    path: bounded_path(&executable_path),
                }
            } else {
                DiscoveryError::Io {
                    path: bounded_path(&executable_path),
                    message: bounded_text(&error.to_string()),
                }
            }
        })?;
        if executable_metadata.file_type().is_symlink() {
            return Err(DiscoveryError::ExecutableIsSymlink {
                path: bounded_path(&executable_path),
            });
        }
        if !executable_metadata.is_file() {
            return Err(DiscoveryError::ExecutableNotFile {
                path: bounded_path(&executable_path),
            });
        }
        let canonical_executable =
            fs::canonicalize(&executable_path).map_err(|error| DiscoveryError::Io {
                path: bounded_path(&executable_path),
                message: bounded_text(&error.to_string()),
            })?;
        ensure_within(root, &canonical_executable, &executable_path)?;
        ensure_within(package_dir, &canonical_executable, &executable_path)?;

        let capabilities = file.capabilities;
        let grants = file.grants;
        let mut protocol_manifest = PluginManifest::new(
            id.as_str(),
            file.display_name.clone(),
            version.as_str(),
            capabilities.clone(),
        )
        .with_grants(grants.clone());
        if let Some(runtime) = &runtime {
            protocol_manifest = protocol_manifest.with_runtime_metadata(runtime.clone());
        }
        protocol_manifest
            .validate()
            .map_err(|error| DiscoveryError::InvalidProtocolManifest {
                plugin_id: id.to_string(),
                message: bounded_text(&error.to_string()),
            })?;

        let manifest = PluginPackageManifest {
            schema_version: file.schema_version,
            id: id.clone(),
            display_name: file.display_name,
            version: version.clone(),
            capabilities: capabilities.clone(),
            grants: grants.clone(),
            runtime,
            default_enabled,
            dependencies,
        };
        let executable = ExecutableDefinition {
            path: canonical_executable.clone(),
            args: executable.args,
            plugin_version: version,
            protocol_version: executable.protocol_version,
        };
        let mut command = PluginCommand::new(canonical_executable)
            .args(executable.args.iter())
            .current_dir(package_dir)
            .clear_environment();
        for name in SAFE_PLUGIN_ENVIRONMENT {
            if let Some(value) = env::var_os(name) {
                command = command.env(*name, value);
            }
        }
        let required_grants = grants
            .iter()
            .copied()
            .filter(|requirement| requirement.is_required())
            .map(GrantRequirement::kind);
        let launch = PluginLaunch::new(id.clone(), command)
            .with_display_name(manifest.display_name().to_string())
            .with_expected_capabilities(capabilities)
            .with_required_grants(required_grants)
            .with_expected_plugin_version(manifest.version().as_str().to_string());

        Ok(DiscoveredPlugin {
            manifest,
            executable,
            package_dir: package_dir.to_path_buf(),
            manifest_path: manifest_path.to_path_buf(),
            launch,
        })
    }
}

/// Result of one package directory scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryReport {
    root: PathBuf,
    plugins: Vec<DiscoveredPlugin>,
    failures: Vec<DiscoveryFailure>,
}

impl DiscoveryReport {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn plugins(&self) -> &[DiscoveredPlugin] {
        &self.plugins
    }

    pub fn failures(&self) -> &[DiscoveryFailure] {
        &self.failures
    }

    pub fn accepted_count(&self) -> usize {
        self.plugins.len()
    }

    pub fn rejected_count(&self) -> usize {
        self.failures.len()
    }

    /// Resolve all accepted packages in deterministic dependency order.
    pub fn load_order(&self) -> Result<Vec<PluginId>, DiscoveryError> {
        let by_id = self
            .plugins
            .iter()
            .enumerate()
            .map(|(index, plugin)| (plugin.manifest.id().clone(), index))
            .collect::<BTreeMap<_, _>>();
        let mut indegree = BTreeMap::<PluginId, usize>::new();
        let mut outgoing = BTreeMap::<PluginId, Vec<PluginId>>::new();

        for plugin in &self.plugins {
            let id = plugin.manifest.id().clone();
            indegree.insert(id.clone(), plugin.manifest.dependencies().len());
            for dependency in plugin.manifest.dependencies() {
                let Some(index) = by_id.get(dependency.id()) else {
                    return Err(DiscoveryError::MissingDependency {
                        plugin_id: id.to_string(),
                        dependency_id: dependency.id().to_string(),
                    });
                };
                let target = &self.plugins[*index];
                if target.manifest.version() != dependency.version() {
                    return Err(DiscoveryError::DependencyVersionMismatch {
                        plugin_id: id.to_string(),
                        dependency_id: dependency.id().to_string(),
                        required: dependency.version().to_string(),
                        available: target.manifest.version().to_string(),
                    });
                }
                outgoing
                    .entry(dependency.id().clone())
                    .or_default()
                    .push(id.clone());
            }
        }

        let mut ready = BTreeSet::new();
        for (id, count) in &indegree {
            if *count == 0 {
                ready.insert(id.clone());
            }
        }
        let mut order = Vec::with_capacity(self.plugins.len());
        while let Some(id) = ready.pop_first() {
            order.push(id.clone());
            if let Some(children) = outgoing.get(&id) {
                for child in children {
                    let count = indegree
                        .get_mut(child)
                        .expect("dependency graph child is indexed");
                    *count -= 1;
                    if *count == 0 {
                        ready.insert(child.clone());
                    }
                }
            }
        }
        if order.len() != self.plugins.len() {
            let cycle = indegree
                .into_iter()
                .filter_map(|(id, count)| (count > 0).then_some(id.to_string()))
                .collect();
            return Err(DiscoveryError::DependencyCycle { plugins: cycle });
        }
        Ok(order)
    }

    pub fn ordered_plugins(&self) -> Result<Vec<DiscoveredPlugin>, DiscoveryError> {
        let by_id = self
            .plugins
            .iter()
            .map(|plugin| (plugin.manifest.id(), plugin))
            .collect::<BTreeMap<_, _>>();
        self.load_order()?
            .into_iter()
            .map(|id| {
                by_id
                    .get(&id)
                    .map(|plugin| (*plugin).clone())
                    .ok_or_else(|| DiscoveryError::MissingDependency {
                        plugin_id: id.to_string(),
                        dependency_id: id.to_string(),
                    })
            })
            .collect()
    }
}

/// One package-level failure retained without aborting sibling discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryFailure {
    path: PathBuf,
    error: DiscoveryError,
}

impl DiscoveryFailure {
    fn new(path: PathBuf, error: DiscoveryError) -> Self {
        Self {
            path: bounded_path(&path),
            error,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn error(&self) -> &DiscoveryError {
        &self.error
    }
}

/// Errors raised by root discovery or package validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    Io {
        path: PathBuf,
        message: String,
    },
    RootIsSymlink {
        path: PathBuf,
    },
    RootNotDirectory {
        path: PathBuf,
    },
    PackageNotDirectory {
        path: PathBuf,
    },
    InvalidManifestFileName {
        name: String,
    },
    ManifestMissing {
        path: PathBuf,
    },
    ManifestIsSymlink {
        path: PathBuf,
    },
    ManifestNotFile {
        path: PathBuf,
    },
    ManifestTooLarge {
        path: PathBuf,
        limit: u64,
    },
    ManifestRead {
        path: PathBuf,
        message: String,
    },
    InvalidJson {
        path: PathBuf,
        message: String,
    },
    UnsupportedSchema {
        found: u32,
        expected: u32,
    },
    InvalidPluginId(PluginIdError),
    InvalidVersion {
        field: &'static str,
        value: String,
        source: VersionError,
    },
    UnsupportedProtocolVersion {
        found: u32,
        expected: u32,
    },
    InvalidProtocolManifest {
        plugin_id: String,
        message: String,
    },
    ExecutablePath {
        path: String,
        reason: String,
    },
    ExecutableMissing {
        path: PathBuf,
    },
    ExecutableIsSymlink {
        path: PathBuf,
    },
    ExecutableNotFile {
        path: PathBuf,
    },
    PathEscapesPackage {
        path: PathBuf,
    },
    PathTooLong {
        path: PathBuf,
        maximum: usize,
    },
    TooManyPlugins {
        maximum: usize,
    },
    TooManyDependencies {
        count: usize,
        maximum: usize,
    },
    DuplicateDependency {
        plugin_id: String,
        dependency_id: String,
    },
    DuplicatePluginId {
        id: String,
    },
    MissingDependency {
        plugin_id: String,
        dependency_id: String,
    },
    DependencyVersionMismatch {
        plugin_id: String,
        dependency_id: String,
        required: String,
        available: String,
    },
    DependencyCycle {
        plugins: Vec<String>,
    },
    InvalidDependency {
        value: String,
    },
    UnsafeDefaultEnabled {
        plugin_id: String,
    },
    ExecutableVersionMismatch {
        expected: String,
        actual: String,
    },
    TooManyArguments {
        count: usize,
        maximum: usize,
    },
    ArgumentTooLong {
        length: usize,
        maximum: usize,
    },
    InvalidArgument,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(formatter, "I/O failed for `{}`: {message}", path.display())
            }
            Self::RootIsSymlink { path } => {
                write!(formatter, "plugin root `{}` is a symlink", path.display())
            }
            Self::RootNotDirectory { path } => write!(
                formatter,
                "plugin root `{}` is not a directory",
                path.display()
            ),
            Self::PackageNotDirectory { path } => {
                write!(formatter, "package `{}` is not a directory", path.display())
            }
            Self::InvalidManifestFileName { name } => {
                write!(formatter, "invalid manifest filename `{name}`")
            }
            Self::ManifestMissing { path } => {
                write!(formatter, "manifest `{}` is missing", path.display())
            }
            Self::ManifestIsSymlink { path } => {
                write!(formatter, "manifest `{}` is a symlink", path.display())
            }
            Self::ManifestNotFile { path } => {
                write!(formatter, "manifest `{}` is not a file", path.display())
            }
            Self::ManifestTooLarge { path, limit } => write!(
                formatter,
                "manifest `{}` exceeds {limit} bytes",
                path.display()
            ),
            Self::ManifestRead { path, message } => write!(
                formatter,
                "cannot read manifest `{}`: {message}",
                path.display()
            ),
            Self::InvalidJson { path, message } => write!(
                formatter,
                "manifest `{}` is invalid JSON: {message}",
                path.display()
            ),
            Self::UnsupportedSchema { found, expected } => write!(
                formatter,
                "manifest schema {found} is unsupported; expected {expected}"
            ),
            Self::InvalidPluginId(error) => write!(formatter, "invalid plugin id: {error}"),
            Self::InvalidVersion { field, source, .. } => {
                write!(formatter, "invalid {field}: {source}")
            }
            Self::UnsupportedProtocolVersion { found, expected } => write!(
                formatter,
                "executable protocol {found} is unsupported; expected {expected}"
            ),
            Self::InvalidProtocolManifest { plugin_id, message } => write!(
                formatter,
                "manifest for `{plugin_id}` is invalid: {message}"
            ),
            Self::ExecutablePath { path, reason } => {
                write!(formatter, "invalid executable path `{path}`: {reason}")
            }
            Self::ExecutableMissing { path } => {
                write!(formatter, "executable `{}` is missing", path.display())
            }
            Self::ExecutableIsSymlink { path } => {
                write!(formatter, "executable `{}` is a symlink", path.display())
            }
            Self::ExecutableNotFile { path } => {
                write!(formatter, "executable `{}` is not a file", path.display())
            }
            Self::PathEscapesPackage { path } => write!(
                formatter,
                "path `{}` escapes its package directory",
                path.display()
            ),
            Self::PathTooLong { path, maximum } => write!(
                formatter,
                "path `{}` exceeds {maximum} bytes",
                path.display()
            ),
            Self::TooManyPlugins { maximum } => {
                write!(formatter, "plugin directory exceeds {maximum} packages")
            }
            Self::TooManyDependencies { count, maximum } => write!(
                formatter,
                "manifest declares {count} dependencies; maximum is {maximum}"
            ),
            Self::DuplicateDependency {
                plugin_id,
                dependency_id,
            } => write!(
                formatter,
                "plugin `{plugin_id}` declares dependency `{dependency_id}` more than once"
            ),
            Self::DuplicatePluginId { id } => {
                write!(formatter, "plugin id `{id}` is declared more than once")
            }
            Self::MissingDependency {
                plugin_id,
                dependency_id,
            } => write!(
                formatter,
                "plugin `{plugin_id}` depends on missing plugin `{dependency_id}`"
            ),
            Self::DependencyVersionMismatch {
                plugin_id,
                dependency_id,
                required,
                available,
            } => write!(
                formatter,
                "plugin `{plugin_id}` requires `{dependency_id}` version {required}; found {available}"
            ),
            Self::DependencyCycle { plugins } => {
                write!(formatter, "plugin dependency cycle: {}", plugins.join(", "))
            }
            Self::InvalidDependency { value } => write!(
                formatter,
                "invalid dependency `{value}`; expected id@version or an object"
            ),
            Self::UnsafeDefaultEnabled { plugin_id } => write!(
                formatter,
                "unsafe plugin `{plugin_id}` cannot be enabled by default"
            ),
            Self::ExecutableVersionMismatch { expected, actual } => write!(
                formatter,
                "executable version {actual} does not match manifest version {expected}"
            ),
            Self::TooManyArguments { count, maximum } => write!(
                formatter,
                "executable declares {count} arguments; maximum is {maximum}"
            ),
            Self::ArgumentTooLong { length, maximum } => write!(
                formatter,
                "executable argument is {length} bytes; maximum is {maximum}"
            ),
            Self::InvalidArgument => {
                formatter.write_str("executable argument contains a NUL or control character")
            }
        }
    }
}

impl Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPluginId(error) => Some(error),
            Self::InvalidVersion { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileManifest {
    schema_version: u32,
    #[serde(alias = "id")]
    plugin_id: String,
    display_name: String,
    plugin_version: String,
    executable: FileExecutable,
    capabilities: Vec<CapabilityDescriptor>,
    #[serde(default)]
    grants: Vec<GrantRequirement>,
    #[serde(default)]
    runtime: Option<PluginRuntimeMetadata>,
    #[serde(default)]
    default_enabled: Option<bool>,
    #[serde(default)]
    dependencies: Vec<FileDependency>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FileExecutable {
    Detailed(FileExecutableFields),
    Legacy(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileExecutableFields {
    path: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    protocol_version: Option<u32>,
    #[serde(default)]
    version: Option<String>,
}

impl FileExecutable {
    fn into_definition(
        self,
        plugin_version: &PluginVersion,
        limits: &DiscoveryLimits,
    ) -> Result<ExecutableDefinition, DiscoveryError> {
        let (path, args, protocol_version, version) = match self {
            Self::Detailed(fields) => (
                fields.path,
                fields.args,
                fields.protocol_version,
                fields.version,
            ),
            Self::Legacy(path) => (path, Vec::new(), None, None),
        };
        let protocol_version = protocol_version.unwrap_or(PROTOCOL_VERSION);
        let mut definition =
            ExecutableDefinition::new(path, plugin_version.clone(), protocol_version)?;
        definition.args =
            validate_arguments_limited(args, limits.max_arguments, limits.max_argument_bytes)?;
        if let Some(version) = version {
            let actual = PluginVersion::parse(version.clone()).map_err(|source| {
                DiscoveryError::InvalidVersion {
                    field: "executable version",
                    value: bounded_text(&version),
                    source,
                }
            })?;
            if actual != *plugin_version {
                return Err(DiscoveryError::ExecutableVersionMismatch {
                    expected: plugin_version.to_string(),
                    actual: actual.to_string(),
                });
            }
        }
        Ok(definition)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FileDependency {
    Detailed(FileDependencyFields),
    Exact(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDependencyFields {
    id: String,
    version: String,
}

impl FileDependency {
    fn into_parts(self) -> Result<(String, String), DiscoveryError> {
        match self {
            Self::Detailed(fields) => Ok((fields.id, fields.version)),
            Self::Exact(value) => value.split_once('@').map_or_else(
                || {
                    Err(DiscoveryError::InvalidDependency {
                        value: bounded_text(&value),
                    })
                },
                |(id, version)| Ok((id.to_string(), version.to_string())),
            ),
        }
    }
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, DiscoveryError> {
    let mut file = File::open(path).map_err(|error| DiscoveryError::ManifestRead {
        path: bounded_path(path),
        message: bounded_text(&error.to_string()),
    })?;
    let capacity = usize::try_from(maximum.saturating_add(1)).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity.min(usize::MAX / 2));
    file.by_ref()
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| DiscoveryError::ManifestRead {
            path: bounded_path(path),
            message: bounded_text(&error.to_string()),
        })?;
    if bytes.len() as u64 > maximum {
        return Err(DiscoveryError::ManifestTooLarge {
            path: bounded_path(path),
            limit: maximum,
        });
    }
    Ok(bytes)
}

fn validate_arguments<I, S>(args: I) -> Result<Vec<String>, DiscoveryError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    validate_arguments_limited(args, MAX_EXECUTABLE_ARGS, MAX_EXECUTABLE_ARG_BYTES)
}

fn validate_arguments_limited<I, S>(
    args: I,
    maximum: usize,
    maximum_bytes: usize,
) -> Result<Vec<String>, DiscoveryError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut output = Vec::new();
    for argument in args {
        if output.len() >= maximum {
            return Err(DiscoveryError::TooManyArguments {
                count: output.len() + 1,
                maximum,
            });
        }
        let argument = argument.into();
        if argument.len() > maximum_bytes {
            return Err(DiscoveryError::ArgumentTooLong {
                length: argument.len(),
                maximum: maximum_bytes,
            });
        }
        if argument
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        {
            return Err(DiscoveryError::InvalidArgument);
        }
        output.push(argument);
    }
    Ok(output)
}

fn validate_relative_executable_path(path: &Path) -> Result<(), DiscoveryError> {
    if path.as_os_str().is_empty() || path.is_absolute() || path.has_root() {
        return Err(DiscoveryError::ExecutablePath {
            path: bounded_path(path).to_string_lossy().into_owned(),
            reason: "path must be relative to the package".to_string(),
        });
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(DiscoveryError::ExecutablePath {
                path: bounded_path(path).to_string_lossy().into_owned(),
                reason: "path may not contain parent, root, or current-directory components"
                    .to_string(),
            });
        }
    }
    Ok(())
}

fn ensure_within(root: &Path, candidate: &Path, original: &Path) -> Result<(), DiscoveryError> {
    if candidate.starts_with(root) {
        Ok(())
    } else {
        Err(DiscoveryError::PathEscapesPackage {
            path: bounded_path(original),
        })
    }
}

fn validate_path_size(path: &Path) -> Result<(), DiscoveryError> {
    validate_path_size_with_limit(path, MAX_PLUGIN_PATH_BYTES)
}

fn validate_path_size_with_limit(path: &Path, maximum: usize) -> Result<(), DiscoveryError> {
    if path.to_string_lossy().len() > maximum {
        return Err(DiscoveryError::PathTooLong {
            path: bounded_path(path),
            maximum,
        });
    }
    Ok(())
}

fn bounded_path(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if value.len() <= MAX_PLUGIN_PATH_BYTES {
        return path.to_path_buf();
    }
    let mut end = MAX_PLUGIN_PATH_BYTES.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    PathBuf::from(format!("{}...", &value[..end]))
}

fn bounded_text(value: &str) -> String {
    const MAX_ERROR_BYTES: usize = 512;
    if value.len() <= MAX_ERROR_BYTES {
        return value.to_string();
    }
    let mut end = MAX_ERROR_BYTES.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn source_value(error: &VersionError) -> &str {
    match error {
        VersionError::Invalid { value } => value,
        VersionError::Empty | VersionError::TooLong { .. } => "<invalid>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "yunxi-discovery-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn write_package(root: &Path, name: &str, manifest: &str, executable: &str) -> PathBuf {
        let package = root.join(name);
        fs::create_dir_all(&package).expect("create package");
        fs::write(package.join(PLUGIN_MANIFEST_FILE), manifest).expect("write manifest");
        fs::write(package.join(executable), b"fixture").expect("write executable");
        package
    }

    fn manifest(id: &str, version: &str, executable: &str) -> String {
        format!(
            r#"{{
                "schema_version": 1,
                "plugin_id": "{id}",
                "display_name": "{id}",
                "plugin_version": "{version}",
                "executable": {{"path": "{executable}", "version": "{version}", "protocol_version": {protocol}}},
                "capabilities": [{{"id":"fixture.{id}","version":1}}]
            }}"#,
            protocol = PROTOCOL_VERSION,
        )
    }

    #[test]
    fn valid_packages_are_sorted_by_dependency_not_directory_order() {
        let root = temp_root("order");
        fs::create_dir_all(&root).expect("create root");
        let dependent_manifest = format!(
            r#"{{"schema_version":1,"plugin_id":"z.dependent","display_name":"z.dependent","plugin_version":"1.0.0","executable":{{"path":"run","protocol_version":{protocol}}},"capabilities":[{{"id":"fixture.z.dependent","version":1}}],"dependencies":[{{"id":"a.base","version":"1.0.0"}}]}}"#,
            protocol = PROTOCOL_VERSION
        );
        write_package(&root, "z-dependent", &dependent_manifest, "run");
        let dependency_manifest = format!(
            r#"{{"schema_version":1,"plugin_id":"a.base","display_name":"a.base","plugin_version":"1.0.0","executable":{{"path":"run","protocol_version":{protocol}}},"capabilities":[{{"id":"fixture.a.base","version":1}}]}}"#,
            protocol = PROTOCOL_VERSION
        );
        write_package(&root, "a-base", &dependency_manifest, "run");
        let report = PluginDirectory::new(&root).discover().expect("discover");
        assert_eq!(report.accepted_count(), 2);
        assert_eq!(
            report
                .load_order()
                .expect("order")
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["a.base", "z.dependent"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_sibling_is_reported_without_hiding_a_valid_package() {
        let root = temp_root("sibling");
        fs::create_dir_all(&root).expect("create root");
        write_package(
            &root,
            "good",
            &manifest("good.plugin", "1.0.0", "run"),
            "run",
        );
        write_package(&root, "bad", "{not-json", "run");
        let report = PluginDirectory::new(&root).discover().expect("discover");
        assert_eq!(report.accepted_count(), 1);
        assert_eq!(report.rejected_count(), 1);
        assert!(matches!(
            report.failures()[0].error(),
            DiscoveryError::InvalidJson { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_ids_are_rejected_deterministically() {
        let root = temp_root("duplicate");
        fs::create_dir_all(&root).expect("create root");
        write_package(
            &root,
            "first",
            &manifest("same.plugin", "1.0.0", "run"),
            "run",
        );
        write_package(
            &root,
            "second",
            &manifest("same.plugin", "1.0.0", "run"),
            "run",
        );
        let report = PluginDirectory::new(&root).discover().expect("discover");
        assert_eq!(report.accepted_count(), 1);
        assert!(
            report
                .failures()
                .iter()
                .any(|failure| matches!(failure.error(), DiscoveryError::DuplicatePluginId { .. }))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relative_path_escape_is_rejected() {
        let root = temp_root("escape");
        fs::create_dir_all(&root).expect("create root");
        let manifest = manifest("escape.plugin", "1.0.0", "../outside.exe");
        write_package(&root, "escape", &manifest, "inside.exe");
        let report = PluginDirectory::new(&root).discover().expect("discover");
        assert!(matches!(
            report.failures()[0].error(),
            DiscoveryError::ExecutablePath { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn version_and_dependency_errors_are_structured() {
        let root = temp_root("dependency");
        fs::create_dir_all(&root).expect("create root");
        let manifest = format!(
            r#"{{"schema_version":1,"plugin_id":"dependent.plugin","display_name":"Dependent","plugin_version":"bad","executable":{{"path":"run","protocol_version":{protocol}}},"capabilities":[{{"id":"fixture.dependent","version":1}}],"dependencies":[{{"id":"missing.plugin","version":"1.0.0"}}]}}"#,
            protocol = PROTOCOL_VERSION
        );
        write_package(&root, "dependent", &manifest, "run");
        let report = PluginDirectory::new(&root).discover().expect("discover");
        assert!(matches!(
            report.failures()[0].error(),
            DiscoveryError::InvalidVersion { .. }
        ));

        let valid_dependency = format!(
            r#"{{"schema_version":1,"plugin_id":"dependent.plugin","display_name":"Dependent","plugin_version":"1.0.0","executable":{{"path":"run","protocol_version":{protocol}}},"capabilities":[{{"id":"fixture.dependent","version":1}}],"dependencies":[{{"id":"missing.plugin","version":"1.0.0"}}]}}"#,
            protocol = PROTOCOL_VERSION
        );
        fs::write(
            root.join("dependent").join(PLUGIN_MANIFEST_FILE),
            valid_dependency,
        )
        .expect("rewrite manifest");
        let report = PluginDirectory::new(&root).discover().expect("rediscover");
        assert_eq!(report.accepted_count(), 1);
        assert!(matches!(
            report.load_order(),
            Err(DiscoveryError::MissingDependency { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounds_reject_oversized_manifest_and_arguments() {
        let too_many = (0..=MAX_EXECUTABLE_ARGS)
            .map(|_| "\"x\"")
            .collect::<Vec<_>>()
            .join(",");
        assert!(matches!(
            validate_arguments(too_many.split(',').map(|value| value.to_string())),
            Err(DiscoveryError::TooManyArguments { .. })
        ));
        let root = temp_root("size");
        fs::create_dir_all(&root).expect("create root");
        let package = root.join("large");
        fs::create_dir_all(&package).expect("create package");
        fs::write(
            package.join(PLUGIN_MANIFEST_FILE),
            vec![b'x'; (MAX_MANIFEST_FILE_BYTES + 1) as usize],
        )
        .expect("write large manifest");
        let report = PluginDirectory::new(&root).discover().expect("discover");
        assert!(matches!(
            report.failures()[0].error(),
            DiscoveryError::ManifestTooLarge { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn versions_accept_prerelease_and_reject_short_forms() {
        assert!(PluginVersion::parse("1.2.3-alpha.1+build.4").is_ok());
        assert!(PluginVersion::parse("1.2").is_err());
        assert!(PluginVersion::parse("01.2.3").is_err());
    }

    #[test]
    fn optional_discovery_reports_a_missing_root_without_aborting() {
        let root = temp_root("missing");
        let report = PluginDirectory::new(&root)
            .discover_or_empty()
            .expect("missing optional root is empty");
        assert_eq!(report.accepted_count(), 0);
        assert_eq!(report.rejected_count(), 1);
        assert!(matches!(
            report.failures()[0].error(),
            DiscoveryError::Io { .. }
        ));
    }

    fn _keep_validate_path_used(path: &Path) -> Result<(), DiscoveryError> {
        validate_path_size(path)
    }
}
