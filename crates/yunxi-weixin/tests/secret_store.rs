use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use yunxi_weixin::{
    FileSecretStore, MASTER_KEY_BYTES, SecretMaterial, SecretRef, SecretStore, SecretStoreError,
};

static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

fn test_path() -> (PathBuf, PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "yunxi-weixin-secret-store-integration-{}-{}",
        std::process::id(),
        NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).expect("test directory");
    (directory.join("secrets.bin"), directory)
}

#[test]
fn file_store_is_a_persistent_secret_store_with_fail_closed_errors() {
    let (path, directory) = test_path();
    let reference = SecretRef::new("host:integration/token").expect("reference");
    let store = FileSecretStore::new(&path, [3_u8; MASTER_KEY_BYTES]).expect("store");
    store
        .put(
            reference.clone(),
            SecretMaterial::from_text("integration-secret").expect("secret"),
        )
        .expect("put");
    store
        .put(
            reference.clone(),
            SecretMaterial::from_text("rotated-secret").expect("rotated secret"),
        )
        .expect("replace existing entry");
    assert!(store.contains(&reference).expect("contains"));

    let reopened = FileSecretStore::new(&path, [3_u8; MASTER_KEY_BYTES]).expect("reopen");
    assert!(reopened.contains(&reference).expect("persisted contains"));

    let wrong_key = FileSecretStore::new(&path, [4_u8; MASTER_KEY_BYTES]).expect("wrong key");
    assert_eq!(
        wrong_key
            .get(&reference)
            .expect_err("wrong key must fail closed"),
        SecretStoreError::Corrupt
    );

    let mut bytes = fs::read(&path).expect("read encrypted file");
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&path, bytes).expect("tamper");
    let error = reopened
        .contains(&reference)
        .expect_err("tamper must fail closed");
    assert_eq!(error, SecretStoreError::Corrupt);
    assert!(!error.to_string().contains("integration-secret"));
    let _ = fs::remove_dir_all(directory);
}
