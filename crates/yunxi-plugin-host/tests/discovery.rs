use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use yunxi_plugin_host::{
    DiscoveryError, MAX_MANIFEST_FILE_BYTES, PLUGIN_MANIFEST_FILE, PluginDirectory,
};
use yunxi_protocol::PROTOCOL_VERSION;

fn temp_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "yunxi-public-discovery-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn package(root: &Path, name: &str, manifest: &str, executable: &str) {
    let directory = root.join(name);
    fs::create_dir_all(&directory).expect("create package");
    fs::write(directory.join(PLUGIN_MANIFEST_FILE), manifest).expect("write manifest");
    fs::write(directory.join(executable), b"fixture").expect("write executable");
}

fn manifest(id: &str, version: &str, executable: &str, dependencies: &str) -> String {
    format!(
        r#"{{"schema_version":1,"plugin_id":"{id}","display_name":"{id}","plugin_version":"{version}","executable":{{"path":"{executable}","protocol_version":{protocol}}},"capabilities":[{{"id":"fixture.{id}","version":1}}]{dependencies}}}"#,
        protocol = PROTOCOL_VERSION,
    )
}

#[test]
fn public_discovery_isolates_duplicate_and_escape_packages() {
    let root = temp_root("isolation");
    fs::create_dir_all(&root).expect("create root");
    package(
        &root,
        "good",
        &manifest("good.plugin", "1.0.0", "run", ""),
        "run",
    );
    package(
        &root,
        "duplicate",
        &manifest("good.plugin", "1.0.0", "run", ""),
        "run",
    );
    package(
        &root,
        "escape",
        &manifest("escape.plugin", "1.0.0", "../outside", ""),
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
    assert!(
        report
            .failures()
            .iter()
            .any(|failure| matches!(failure.error(), DiscoveryError::ExecutablePath { .. }))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn public_load_order_checks_exact_dependency_versions() {
    let root = temp_root("dependencies");
    fs::create_dir_all(&root).expect("create root");
    package(
        &root,
        "base",
        &manifest("base.plugin", "2.0.0", "run", ""),
        "run",
    );
    package(
        &root,
        "dependent",
        &manifest(
            "dependent.plugin",
            "1.0.0",
            "run",
            ",\"dependencies\":[{\"id\":\"base.plugin\",\"version\":\"2.0.0\"}]",
        ),
        "run",
    );

    let report = PluginDirectory::new(&root).discover().expect("discover");
    let order = report.load_order().expect("dependency order");
    assert_eq!(order[0].as_str(), "base.plugin");
    assert_eq!(order[1].as_str(), "dependent.plugin");

    let mismatch = manifest(
        "dependent.plugin",
        "1.0.0",
        "run",
        ",\"dependencies\":[{\"id\":\"base.plugin\",\"version\":\"1.0.0\"}]",
    );
    fs::write(root.join("dependent").join(PLUGIN_MANIFEST_FILE), mismatch)
        .expect("rewrite manifest");
    let report = PluginDirectory::new(&root).discover().expect("rediscover");
    assert!(matches!(
        report.load_order(),
        Err(DiscoveryError::DependencyVersionMismatch { .. })
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn public_discovery_applies_manifest_size_limit_before_json_parsing() {
    let root = temp_root("bounds");
    fs::create_dir_all(root.join("large")).expect("create package");
    fs::write(
        root.join("large").join(PLUGIN_MANIFEST_FILE),
        vec![b'x'; MAX_MANIFEST_FILE_BYTES as usize + 1],
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
fn executable_replacement_changes_the_discovered_package_identity() {
    let root = temp_root("executable-replacement");
    fs::create_dir_all(&root).expect("create root");
    package(
        &root,
        "replaceable",
        &manifest("replaceable.plugin", "1.0.0", "run", ""),
        "run",
    );

    let first = PluginDirectory::new(&root)
        .discover()
        .expect("first discovery");
    fs::write(
        root.join("replaceable").join("run"),
        b"replacement-with-a-different-size",
    )
    .expect("replace executable");
    let second = PluginDirectory::new(&root)
        .discover()
        .expect("second discovery");

    assert_ne!(first.plugins()[0], second.plugins()[0]);
    let _ = fs::remove_dir_all(root);
}
