//! Dynamic package discovery and lifecycle reconciliation.
//!
//! `PluginDiscoveryManager` is deliberately a coordinator above
//! `ProcessPluginHost`.  Discovery, dependency ordering, user enablement, and
//! replacement are kept separate from the process protocol so a malformed or
//! crashing package cannot tear down the kernel or its healthy siblings.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use yunxi_kernel::PluginId;

use crate::{
    DiscoveredPlugin, DiscoveryError, DiscoveryFailure, PluginDirectory, ProcessPluginHost,
};

/// Upper bound for package-level lifecycle diagnostics retained by one scan.
pub const MAX_DYNAMIC_PLUGIN_DIAGNOSTICS: usize = 256;
const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 1024;

/// One package that could not be started, stopped, or ordered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginLifecycleFailure {
    id: Option<PluginId>,
    path: PathBuf,
    reason: String,
}

impl PluginLifecycleFailure {
    pub fn id(&self) -> Option<&PluginId> {
        self.id.as_ref()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// The bounded result of one successful discovery/reconciliation pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginReloadReport {
    root: PathBuf,
    discovered: usize,
    enabled: Vec<PluginId>,
    disabled: Vec<PluginId>,
    launched: Vec<PluginId>,
    retained: Vec<PluginId>,
    removed: Vec<PluginId>,
    replaced: Vec<PluginId>,
    skipped: Vec<PluginLifecycleFailure>,
    failures: Vec<PluginLifecycleFailure>,
    discovery_failures: Vec<DiscoveryFailure>,
}

impl PluginReloadReport {
    fn new(root: PathBuf, discovered: usize, discovery_failures: Vec<DiscoveryFailure>) -> Self {
        Self {
            root,
            discovered,
            enabled: Vec::new(),
            disabled: Vec::new(),
            launched: Vec::new(),
            retained: Vec::new(),
            removed: Vec::new(),
            replaced: Vec::new(),
            skipped: Vec::new(),
            failures: Vec::new(),
            discovery_failures,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn discovered(&self) -> usize {
        self.discovered
    }

    pub fn enabled(&self) -> &[PluginId] {
        &self.enabled
    }

    pub fn disabled(&self) -> &[PluginId] {
        &self.disabled
    }

    pub fn launched(&self) -> &[PluginId] {
        &self.launched
    }

    pub fn retained(&self) -> &[PluginId] {
        &self.retained
    }

    pub fn removed(&self) -> &[PluginId] {
        &self.removed
    }

    pub fn replaced(&self) -> &[PluginId] {
        &self.replaced
    }

    pub fn skipped(&self) -> &[PluginLifecycleFailure] {
        &self.skipped
    }

    pub fn failures(&self) -> &[PluginLifecycleFailure] {
        &self.failures
    }

    pub fn discovery_failures(&self) -> &[DiscoveryFailure] {
        &self.discovery_failures
    }

    pub fn has_failures(&self) -> bool {
        !self.discovery_failures.is_empty() || !self.skipped.is_empty() || !self.failures.is_empty()
    }

    fn push_skipped(&mut self, failure: PluginLifecycleFailure) {
        if self.skipped.len() < MAX_DYNAMIC_PLUGIN_DIAGNOSTICS {
            self.skipped.push(failure);
        }
    }

    fn push_failure(&mut self, failure: PluginLifecycleFailure) {
        if self.failures.len() < MAX_DYNAMIC_PLUGIN_DIAGNOSTICS {
            self.failures.push(failure);
        }
    }
}

/// Errors that prevent a scan from being reconciled at all.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginDiscoveryManagerError {
    Discovery(DiscoveryError),
}

impl fmt::Display for PluginDiscoveryManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(error) => write!(formatter, "dynamic plugin discovery failed: {error}"),
        }
    }
}

impl Error for PluginDiscoveryManagerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Discovery(error) => Some(error),
        }
    }
}

impl From<DiscoveryError> for PluginDiscoveryManagerError {
    fn from(error: DiscoveryError) -> Self {
        Self::Discovery(error)
    }
}

/// Owns the package set for one configured plugin directory.
///
/// The manager is intentionally not responsible for settings persistence.
/// Callers pass the effective `plugin_id -> enabled` map on every pass, which
/// makes a disabled package receive no process, connection, or capability
/// route by construction.
#[derive(Clone, Debug)]
pub struct PluginDiscoveryManager {
    directory: PluginDirectory,
    packages: BTreeMap<PluginId, DiscoveredPlugin>,
    managed_ids: BTreeSet<PluginId>,
    last_report: Option<PluginReloadReport>,
}

#[derive(Default)]
struct ReconciliationState {
    replacement_blocked: BTreeSet<PluginId>,
    deferred: BTreeMap<PluginId, DiscoveredPlugin>,
}

impl PluginDiscoveryManager {
    pub fn new(directory: PluginDirectory) -> Self {
        Self {
            directory,
            packages: BTreeMap::new(),
            managed_ids: BTreeSet::new(),
            last_report: None,
        }
    }

    pub fn directory(&self) -> &PluginDirectory {
        &self.directory
    }

    pub fn packages(&self) -> &BTreeMap<PluginId, DiscoveredPlugin> {
        &self.packages
    }

    pub fn managed_ids(&self) -> &BTreeSet<PluginId> {
        &self.managed_ids
    }

    pub fn last_report(&self) -> Option<&PluginReloadReport> {
        self.last_report.as_ref()
    }

    /// Stop and unregister every package owned by this manager.
    ///
    /// This is used when the configured directory is removed or replaced.
    /// Dropping a manager alone cannot clean up the host registrations because
    /// the host owns the process supervisors and capability catalog.  Any
    /// package that cannot be unregistered remains in the manager so the
    /// caller can report the failure instead of silently leaking a route.
    pub fn unload_all(&mut self, host: &mut ProcessPluginHost) -> PluginReloadReport {
        host.refresh();
        let mut report =
            PluginReloadReport::new(self.directory.root().to_path_buf(), 0, Vec::new());
        let mut ids = self.managed_ids.clone();
        ids.extend(self.packages.keys().cloned());

        for id in ids {
            let path = self.packages.get(&id).map_or_else(
                || self.directory.root().to_path_buf(),
                |plugin| plugin.package_dir().to_path_buf(),
            );
            if self.remove_managed(host, &id, &path, &mut report) {
                report.removed.push(id);
            }
        }

        let remaining = self.managed_ids.clone();
        self.packages.retain(|id, _| remaining.contains(id));
        self.last_report = Some(report.clone());
        report
    }

    /// Scan and reconcile packages with a live process host.
    pub fn reload(
        &mut self,
        host: &mut ProcessPluginHost,
        overrides: &BTreeMap<String, bool>,
    ) -> Result<PluginReloadReport, PluginDiscoveryManagerError> {
        // Let the Host settle terminal events before deciding whether an
        // existing package can be retained or must be replaced.
        host.refresh();
        let discovered = self.directory.discover_or_empty()?;
        let current = discovered
            .plugins()
            .iter()
            .map(|plugin| (plugin.manifest().id().clone(), plugin.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut report = PluginReloadReport::new(
            discovered.root().to_path_buf(),
            discovered.accepted_count(),
            discovered.failures().to_vec(),
        );

        let previous = self.packages.clone();
        let mut reconciliation = ReconciliationState::default();
        self.reconcile_old_packages(
            host,
            &previous,
            &current,
            overrides,
            &mut reconciliation,
            &mut report,
        );
        // Keep an old package as the authoritative manager entry until its
        // Host registration has actually been removed.  Otherwise a failed
        // unregister would make the next scan compare the new package with
        // itself and silently retain the old process forever.
        let mut next = current;
        next.extend(reconciliation.deferred);
        self.packages = next;

        let (order, blocked) = dependency_order(&self.packages);
        for (id, reason) in blocked {
            if let Some(plugin) = self.packages.get(&id) {
                if enabled(plugin, overrides) {
                    report.enabled.push(id.clone());
                    report.push_skipped(lifecycle_failure(Some(id), plugin.package_dir(), reason));
                } else {
                    report.disabled.push(id);
                }
            }
        }

        let mut available = BTreeSet::new();
        for id in order {
            let Some(plugin) = self.packages.get(&id).cloned() else {
                continue;
            };
            if enabled(&plugin, overrides) {
                report.enabled.push(id.clone());
            } else {
                report.disabled.push(id);
                continue;
            }
            if reconciliation.replacement_blocked.contains(&id) {
                continue;
            }

            if !dependencies_available(host, &self.packages, &plugin, overrides, &available) {
                let reason =
                    dependency_failure_reason(host, &self.packages, &plugin, overrides, &available);
                report.push_skipped(lifecycle_failure(Some(id), plugin.package_dir(), reason));
                continue;
            }

            if self.managed_ids.contains(&id) && host.is_registered(&id) {
                report.retained.push(id.clone());
                if host.catalog().plugin(&id).is_some() {
                    available.insert(id);
                }
                continue;
            }
            if host.is_registered(&id) {
                report.push_failure(lifecycle_failure(
                    Some(id),
                    plugin.package_dir(),
                    "plugin id is already owned by another host registration",
                ));
                continue;
            }

            let was_registered = host.is_registered(&id);
            match host.launch(plugin.launch()) {
                Ok(_) => {
                    self.managed_ids.insert(id.clone());
                    available.insert(id.clone());
                    report.launched.push(id);
                }
                Err(error) => {
                    // `launch` keeps a failed slot for explicit recovery.
                    // Claim that slot when it was created by this manager so
                    // a routine Web refresh does not spawn the same broken
                    // package over and over. A package change or explicit
                    // disable/enable cycle still removes it and retries.
                    if !was_registered && host.is_registered(&id) {
                        self.managed_ids.insert(id.clone());
                    }
                    report.push_failure(lifecycle_failure(
                        Some(id),
                        plugin.package_dir(),
                        error.to_string(),
                    ));
                }
            }
        }

        self.last_report = Some(report.clone());
        Ok(report)
    }

    fn reconcile_old_packages(
        &mut self,
        host: &mut ProcessPluginHost,
        previous: &BTreeMap<PluginId, DiscoveredPlugin>,
        current: &BTreeMap<PluginId, DiscoveredPlugin>,
        overrides: &BTreeMap<String, bool>,
        reconciliation: &mut ReconciliationState,
        report: &mut PluginReloadReport,
    ) {
        for (id, old_plugin) in previous {
            let Some(new_plugin) = current.get(id) else {
                if self.remove_managed(host, id, old_plugin.package_dir(), report) {
                    report.removed.push(id.clone());
                } else {
                    reconciliation.replacement_blocked.insert(id.clone());
                    reconciliation
                        .deferred
                        .insert(id.clone(), old_plugin.clone());
                }
                continue;
            };

            let changed = old_plugin != new_plugin;
            let should_be_running = enabled(new_plugin, overrides);
            if !changed && should_be_running {
                continue;
            }

            if self.managed_ids.contains(id)
                && !self.remove_managed(host, id, old_plugin.package_dir(), report)
            {
                reconciliation.replacement_blocked.insert(id.clone());
                reconciliation
                    .deferred
                    .insert(id.clone(), old_plugin.clone());
                continue;
            }
            if changed {
                report.replaced.push(id.clone());
            }
        }
    }

    fn remove_managed(
        &mut self,
        host: &mut ProcessPluginHost,
        id: &PluginId,
        path: &Path,
        report: &mut PluginReloadReport,
    ) -> bool {
        // A discovered-but-disabled package does not own a Host slot.  Never
        // unregister a same-ID registration created by another coordinator.
        if !self.managed_ids.contains(id) {
            return true;
        }
        if host.is_registered(id)
            && let Err(error) = host.unregister(id)
        {
            report.push_failure(lifecycle_failure(Some(id.clone()), path, error.to_string()));
            return false;
        }
        self.managed_ids.remove(id);
        true
    }
}

impl Default for PluginDiscoveryManager {
    fn default() -> Self {
        Self::new(PluginDirectory::new("plugins"))
    }
}

fn enabled(plugin: &DiscoveredPlugin, overrides: &BTreeMap<String, bool>) -> bool {
    overrides
        .get(plugin.manifest().id().as_str())
        .copied()
        .unwrap_or_else(|| plugin.manifest().default_enabled())
}

/// Return a deterministic topological order and retain dependency errors per
/// package. A broken graph therefore blocks only the affected package chain.
fn dependency_order(
    packages: &BTreeMap<PluginId, DiscoveredPlugin>,
) -> (Vec<PluginId>, BTreeMap<PluginId, String>) {
    let mut blocked = BTreeMap::new();
    loop {
        let mut newly_blocked = Vec::new();
        for (id, plugin) in packages {
            if blocked.contains_key(id) {
                continue;
            }
            let reason = plugin
                .manifest()
                .dependencies()
                .iter()
                .find_map(|dependency| {
                    let Some(target) = packages.get(dependency.id()) else {
                        return Some(format!("dependency `{}` is not installed", dependency.id()));
                    };
                    if target.manifest().version() != dependency.version() {
                        return Some(format!(
                            "dependency `{}` requires version {}; found {}",
                            dependency.id(),
                            dependency.version(),
                            target.manifest().version()
                        ));
                    }
                    blocked.get(dependency.id()).map(|dependency_reason| {
                        format!(
                            "dependency `{}` is unavailable: {dependency_reason}",
                            dependency.id()
                        )
                    })
                });
            if let Some(reason) = reason {
                newly_blocked.push((id.clone(), reason));
            }
        }
        if newly_blocked.is_empty() {
            break;
        }
        for (id, reason) in newly_blocked {
            blocked.insert(id, reason);
        }
    }

    let mut indegree = BTreeMap::<PluginId, usize>::new();
    let mut outgoing = BTreeMap::<PluginId, Vec<PluginId>>::new();
    for (id, plugin) in packages {
        if blocked.contains_key(id) {
            continue;
        }
        indegree.insert(id.clone(), plugin.manifest().dependencies().len());
        for dependency in plugin.manifest().dependencies() {
            outgoing
                .entry(dependency.id().clone())
                .or_default()
                .push(id.clone());
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(id.clone()))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(indegree.len());
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
    if order.len() != indegree.len() {
        let cycle_ids = indegree
            .into_iter()
            .filter_map(|(id, count)| (count > 0).then_some(id))
            .collect::<Vec<_>>();
        let cycle = cycle_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        for id in cycle_ids {
            blocked.insert(
                id,
                format!("dependency cycle or blocked cycle member: {cycle}"),
            );
        }
    }
    (order, blocked)
}

fn dependencies_available(
    host: &ProcessPluginHost,
    packages: &BTreeMap<PluginId, DiscoveredPlugin>,
    plugin: &DiscoveredPlugin,
    overrides: &BTreeMap<String, bool>,
    available: &BTreeSet<PluginId>,
) -> bool {
    plugin.manifest().dependencies().iter().all(|dependency| {
        if let Some(package) = packages.get(dependency.id()) {
            enabled(package, overrides) && available.contains(dependency.id())
        } else {
            host.catalog()
                .plugin(dependency.id())
                .is_some_and(|record| record.version() == dependency.version().as_str())
        }
    })
}

fn dependency_failure_reason(
    host: &ProcessPluginHost,
    packages: &BTreeMap<PluginId, DiscoveredPlugin>,
    plugin: &DiscoveredPlugin,
    overrides: &BTreeMap<String, bool>,
    available: &BTreeSet<PluginId>,
) -> String {
    for dependency in plugin.manifest().dependencies() {
        if let Some(package) = packages.get(dependency.id()) {
            if !enabled(package, overrides) {
                return format!("dependency `{}` is disabled", dependency.id());
            }
            if !available.contains(dependency.id()) {
                return format!("dependency `{}` did not become active", dependency.id());
            }
        } else if host
            .catalog()
            .plugin(dependency.id())
            .is_none_or(|record| record.version() != dependency.version().as_str())
        {
            return format!(
                "dependency `{}` is not active at the required version",
                dependency.id()
            );
        }
    }
    "one or more dependencies are unavailable".to_string()
}

fn lifecycle_failure(
    id: Option<PluginId>,
    path: &Path,
    reason: impl Into<String>,
) -> PluginLifecycleFailure {
    PluginLifecycleFailure {
        id,
        path: path.to_path_buf(),
        reason: bounded_message(&reason.into()),
    }
}

fn bounded_message(value: &str) -> String {
    if value.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES {
        return value.to_string();
    }
    let mut end = MAX_DIAGNOSTIC_MESSAGE_BYTES.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{PLUGIN_MANIFEST_FILE, PluginDirectory};
    use yunxi_protocol::PROTOCOL_VERSION;

    fn root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "yunxi-manager-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn package(root: &Path, name: &str, id: &str, version: &str, default_enabled: bool) {
        let directory = root.join(name);
        fs::create_dir_all(&directory).expect("package directory");
        fs::write(directory.join("run"), b"not a real executable").expect("executable");
        fs::write(
            directory.join(PLUGIN_MANIFEST_FILE),
            format!(
                r#"{{"schema_version":1,"plugin_id":"{id}","display_name":"{id}","plugin_version":"{version}","default_enabled":{default_enabled},"executable":{{"path":"run","protocol_version":{PROTOCOL_VERSION}}},"capabilities":[{{"id":"fixture.{id}","version":1}}]}}"#
            ),
        )
        .expect("manifest");
    }

    #[test]
    fn disabled_packages_are_discovered_without_starting_processes() {
        let root = root("disabled");
        fs::create_dir_all(&root).expect("root");
        package(&root, "external", "fixture.external", "1.0.0", false);

        let mut manager = PluginDiscoveryManager::new(PluginDirectory::new(&root));
        let mut host = ProcessPluginHost::new();
        let report = manager.reload(&mut host, &BTreeMap::new()).expect("reload");
        assert_eq!(report.discovered(), 1);
        assert_eq!(
            report.disabled(),
            &[PluginId::new("fixture.external").expect("id")]
        );
        assert!(report.launched().is_empty());
        assert_eq!(host.snapshot().plugins().len(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_removed_package_is_forgotten_on_the_next_scan() {
        let root = root("remove");
        fs::create_dir_all(&root).expect("root");
        package(&root, "external", "fixture.external", "1.0.0", false);
        let mut manager = PluginDiscoveryManager::new(PluginDirectory::new(&root));
        let mut host = ProcessPluginHost::new();
        manager
            .reload(&mut host, &BTreeMap::new())
            .expect("initial");
        fs::remove_dir_all(root.join("external")).expect("remove package");
        let report = manager.reload(&mut host, &BTreeMap::new()).expect("rescan");
        assert_eq!(report.discovered(), 0);
        assert!(manager.packages().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dependency_failure_does_not_abort_unrelated_packages() {
        let root = root("dependency");
        fs::create_dir_all(&root).expect("root");
        package(&root, "independent", "fixture.independent", "1.0.0", false);
        let dependent = root.join("dependent");
        fs::create_dir_all(&dependent).expect("dependent directory");
        fs::write(dependent.join("run"), b"fixture").expect("executable");
        fs::write(
            dependent.join(PLUGIN_MANIFEST_FILE),
            format!(
                r#"{{"schema_version":1,"plugin_id":"fixture.dependent","display_name":"fixture.dependent","plugin_version":"1.0.0","default_enabled":true,"executable":{{"path":"run","protocol_version":{PROTOCOL_VERSION}}},"dependencies":["fixture.missing@1.0.0"],"capabilities":[{{"id":"fixture.dependent","version":1}}]}}"#
            ),
        )
        .expect("manifest");

        let mut manager = PluginDiscoveryManager::new(PluginDirectory::new(&root));
        let mut host = ProcessPluginHost::new();
        let report = manager.reload(&mut host, &BTreeMap::new()).expect("reload");
        assert_eq!(report.discovered(), 2);
        assert!(
            report
                .disabled()
                .iter()
                .any(|id| id.as_str() == "fixture.independent")
        );
        assert!(report.skipped().iter().any(|failure| {
            failure
                .id()
                .is_some_and(|id| id.as_str() == "fixture.dependent")
        }));
        let _ = fs::remove_dir_all(root);
    }
}
