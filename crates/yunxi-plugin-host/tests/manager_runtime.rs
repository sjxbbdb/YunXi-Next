//! End-to-end discovery and lifecycle tests using a real isolated process.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use yunxi_kernel::PluginId;
use yunxi_plugin_host::{
    PLUGIN_MANIFEST_FILE, PluginDirectory, PluginDiscoveryManager, ProcessPluginHost,
};
use yunxi_protocol::{CapabilityDescriptor, PROTOCOL_VERSION};

fn temporary_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "yunxi-manager-runtime-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn fixture_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yunxi-plugin-fixture"))
}

fn executable_name(version: &str) -> String {
    if cfg!(windows) {
        format!("fixture-{version}.exe")
    } else {
        format!("fixture-{version}")
    }
}

fn install_package(
    root: &Path,
    directory_name: &str,
    plugin_id: &str,
    capability_id: &str,
    mode: &str,
    version: &str,
    default_enabled: bool,
) {
    let directory = root.join(directory_name);
    fs::create_dir_all(&directory).expect("create package directory");
    let executable_name = executable_name(version);
    let executable = directory.join(&executable_name);
    fs::copy(fixture_binary(), &executable).expect("copy fixture executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&executable)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).expect("make fixture executable");
    }
    let manifest = json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "display_name": plugin_id,
        "plugin_version": version,
        "default_enabled": default_enabled,
        "executable": {
            "path": executable_name,
            "args": [
                "--id", plugin_id,
                "--capability", capability_id,
                "--mode", mode,
                "--version", version,
            ],
            "protocol_version": PROTOCOL_VERSION,
        },
        "capabilities": [{"id": capability_id, "version": 1}],
    });
    fs::write(
        directory.join(PLUGIN_MANIFEST_FILE),
        serde_json::to_vec(&manifest).expect("serialize package manifest"),
    )
    .expect("write package manifest");
}

fn id(value: &str) -> PluginId {
    PluginId::new(value).expect("valid plugin id")
}

fn capability(value: &str) -> CapabilityDescriptor {
    CapabilityDescriptor::new(value, 1).expect("valid capability")
}

#[test]
fn dynamic_reload_isolates_failure_and_supports_disable_enable_and_replace() {
    let root = temporary_root("lifecycle");
    fs::create_dir_all(&root).expect("create plugin root");
    let healthy_id = "fixture.dynamic.healthy";
    let healthy_capability = "fixture.dynamic.echo";
    let broken_id = "fixture.dynamic.broken";
    let broken_capability = "fixture.dynamic.broken-capability";
    install_package(
        &root,
        "healthy",
        healthy_id,
        healthy_capability,
        "echo",
        "1.0.0",
        true,
    );
    install_package(
        &root,
        "broken",
        broken_id,
        broken_capability,
        "malformed",
        "1.0.0",
        true,
    );

    let mut manager = PluginDiscoveryManager::new(PluginDirectory::new(&root));
    let mut host = ProcessPluginHost::new();
    let first = manager
        .reload(&mut host, &BTreeMap::new())
        .expect("initial reload");
    assert_eq!(first.discovered(), 2);
    assert!(
        first
            .launched()
            .iter()
            .any(|value| value.as_str() == healthy_id)
    );
    assert!(first.failures().iter().any(|failure| {
        failure
            .id()
            .is_some_and(|value| value.as_str() == broken_id)
    }));
    assert_eq!(
        host.invoke::<_, String>(
            &capability(healthy_capability),
            "echo",
            &"healthy-before-toggle".to_string(),
        )
        .expect("healthy sibling invocation"),
        "healthy-before-toggle"
    );

    let second = manager
        .reload(&mut host, &BTreeMap::new())
        .expect("stable reload");
    assert!(
        second
            .retained()
            .iter()
            .any(|value| value.as_str() == healthy_id)
    );
    assert!(
        second
            .retained()
            .iter()
            .any(|value| value.as_str() == broken_id)
    );
    assert!(second.failures().is_empty());

    let mut disabled = BTreeMap::new();
    disabled.insert(healthy_id.to_string(), false);
    let stopped = manager
        .reload(&mut host, &disabled)
        .expect("disable reload");
    assert!(
        stopped
            .disabled()
            .iter()
            .any(|value| value.as_str() == healthy_id)
    );
    assert!(!host.is_registered(&id(healthy_id)));
    assert!(host.catalog().providers(healthy_capability, 1).is_empty());

    let mut enabled = BTreeMap::new();
    enabled.insert(healthy_id.to_string(), true);
    let restarted = manager.reload(&mut host, &enabled).expect("enable reload");
    assert!(
        restarted
            .launched()
            .iter()
            .any(|value| value.as_str() == healthy_id)
    );
    assert_eq!(
        host.invoke::<_, String>(
            &capability(healthy_capability),
            "echo",
            &"healthy-after-enable".to_string(),
        )
        .expect("re-enabled sibling invocation"),
        "healthy-after-enable"
    );

    install_package(
        &root,
        "healthy",
        healthy_id,
        healthy_capability,
        "echo",
        "1.0.1",
        true,
    );
    let replaced = manager
        .reload(&mut host, &enabled)
        .expect("replacement reload");
    assert!(
        replaced
            .replaced()
            .iter()
            .any(|value| value.as_str() == healthy_id)
    );
    assert!(
        replaced
            .launched()
            .iter()
            .any(|value| value.as_str() == healthy_id)
    );
    assert_eq!(
        host.catalog()
            .plugin(&id(healthy_id))
            .expect("replacement catalog record")
            .version(),
        "1.0.1"
    );

    let unloaded = manager.unload_all(&mut host);
    assert!(
        unloaded
            .removed()
            .iter()
            .any(|value| value.as_str() == healthy_id)
    );
    assert!(
        unloaded
            .removed()
            .iter()
            .any(|value| value.as_str() == broken_id)
    );
    assert!(!host.is_registered(&id(healthy_id)));
    assert!(!host.is_registered(&id(broken_id)));
    assert!(host.catalog().providers(healthy_capability, 1).is_empty());

    host.shutdown();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unload_does_not_remove_a_same_id_registration_owned_elsewhere() {
    let root = temporary_root("ownership");
    fs::create_dir_all(&root).expect("create plugin root");
    let plugin_id = "fixture.dynamic.external-owner";
    let capability_id = "fixture.dynamic.external-owner-capability";
    install_package(
        &root,
        "disabled",
        plugin_id,
        capability_id,
        "echo",
        "1.0.0",
        false,
    );

    let mut manager = PluginDiscoveryManager::new(PluginDirectory::new(&root));
    let mut host = ProcessPluginHost::new();
    manager
        .reload(&mut host, &BTreeMap::new())
        .expect("discover disabled package");
    let external_launch = manager
        .packages()
        .get(&id(plugin_id))
        .expect("discovered package")
        .launch();
    host.launch(external_launch).expect("external registration");

    let unloaded = manager.unload_all(&mut host);
    assert!(
        unloaded
            .removed()
            .iter()
            .any(|value| value.as_str() == plugin_id)
    );
    assert!(host.is_registered(&id(plugin_id)));
    assert_eq!(
        host.invoke::<_, String>(
            &capability(capability_id),
            "echo",
            &"external-owner-still-running".to_string(),
        )
        .expect("external registration remains routable"),
        "external-owner-still-running"
    );

    host.unregister(&id(plugin_id))
        .expect("clean up external registration");
    host.shutdown();
    let _ = fs::remove_dir_all(root);
}
