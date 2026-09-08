//! Replaceable secret storage for the Weixin production boundary.
//!
//! The channel stores references in configuration and keeps secret bytes behind
//! this trait.  The in-memory implementation is deterministic and intended for
//! tests, loopback, and hosts that provide their own persistence boundary.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, Payload},
};

use crate::adapter::{
    MAX_SECRET_BYTES, MAX_SECRET_REF_BYTES, SecretError, SecretMaterial, SecretRef, SecretResolver,
};

pub const MAX_SECRET_ENTRIES: usize = 64;
pub const MAX_TOTAL_SECRET_BYTES: usize = 256 * 1024;
pub const MASTER_KEY_BYTES: usize = 32;

const FILE_MAGIC: &[u8] = b"YUNXI-WEIXIN-SECRETS";
const FILE_VERSION: u8 = 1;
const NONCE_BYTES: usize = 12;
const AEAD_TAG_BYTES: usize = 16;
const SNAPSHOT_PREFIX_BYTES: usize = 4;
const SNAPSHOT_ENTRY_OVERHEAD_BYTES: usize = 8;
const MAX_SNAPSHOT_BYTES: usize = SNAPSHOT_PREFIX_BYTES
    + MAX_TOTAL_SECRET_BYTES
    + MAX_SECRET_ENTRIES * (MAX_SECRET_REF_BYTES + SNAPSHOT_ENTRY_OVERHEAD_BYTES);
const FILE_HEADER_BYTES: usize = FILE_MAGIC.len() + 1 + NONCE_BYTES;
pub const MAX_SECRET_FILE_BYTES: usize = FILE_HEADER_BYTES + MAX_SNAPSHOT_BYTES + AEAD_TAG_BYTES;

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct SecretValues(BTreeMap<SecretRef, Vec<u8>>);

impl Deref for SecretValues {
    type Target = BTreeMap<SecretRef, Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for SecretValues {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for SecretValues {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            wipe_bytes(value);
        }
    }
}

/// Host-owned secret storage. Implementations must never include secret bytes
/// in errors or diagnostic output.
pub trait SecretStore: Send + Sync {
    fn get(&self, reference: &SecretRef) -> Result<Option<SecretMaterial>, SecretStoreError>;

    fn put(&self, reference: SecretRef, value: SecretMaterial) -> Result<(), SecretStoreError>;

    fn remove(&self, reference: &SecretRef) -> Result<bool, SecretStoreError>;

    fn contains(&self, reference: &SecretRef) -> Result<bool, SecretStoreError> {
        Ok(self.get(reference)?.is_some())
    }
}

/// A bounded, process-local store. It is deliberately not presented as
/// durable or OS-backed credential storage.
#[derive(Clone, Default)]
pub struct MemorySecretStore {
    values: Arc<Mutex<SecretValues>>,
}

impl fmt::Debug for MemorySecretStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.values.lock().map(|values| values.len()).unwrap_or(0);
        formatter
            .debug_struct("MemorySecretStore")
            .field("entries", &count)
            .field("values", &"<redacted>")
            .finish()
    }
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> Result<usize, SecretStoreError> {
        Ok(self.lock()?.len())
    }

    pub fn is_empty(&self) -> Result<bool, SecretStoreError> {
        Ok(self.len()? == 0)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, SecretValues>, SecretStoreError> {
        self.values.lock().map_err(|_| SecretStoreError::Poisoned)
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, reference: &SecretRef) -> Result<Option<SecretMaterial>, SecretStoreError> {
        let values = self.lock()?;
        values
            .get(reference)
            .map(|value| {
                SecretMaterial::from_bytes(value.clone()).map_err(SecretStoreError::Secret)
            })
            .transpose()
    }

    fn put(&self, reference: SecretRef, value: SecretMaterial) -> Result<(), SecretStoreError> {
        let bytes = value.as_bytes();
        if bytes.is_empty() {
            return Err(SecretStoreError::Secret(SecretError::Empty));
        }
        if bytes.len() > MAX_SECRET_BYTES {
            return Err(SecretStoreError::Secret(SecretError::TooLarge));
        }
        let mut values = self.lock()?;
        let current_total = values.values().map(Vec::len).sum::<usize>();
        let replaced = values.get(&reference).map_or(0, Vec::len);
        let next_total = current_total
            .saturating_sub(replaced)
            .saturating_add(bytes.len());
        if !values.contains_key(&reference) && values.len() >= MAX_SECRET_ENTRIES {
            return Err(SecretStoreError::CapacityExceeded {
                maximum_entries: MAX_SECRET_ENTRIES,
            });
        }
        if next_total > MAX_TOTAL_SECRET_BYTES {
            return Err(SecretStoreError::CapacityExceeded {
                maximum_entries: MAX_TOTAL_SECRET_BYTES,
            });
        }
        if let Some(mut previous) = values.insert(reference, bytes.to_vec()) {
            wipe_bytes(&mut previous);
        }
        Ok(())
    }

    fn remove(&self, reference: &SecretRef) -> Result<bool, SecretStoreError> {
        let removed = self.lock()?.remove(reference);
        if let Some(mut removed) = removed {
            wipe_bytes(&mut removed);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// A durable store encrypted with a caller-provided 32-byte master key.
///
/// The file contains one authenticated encrypted snapshot. The master key is
/// never written to disk, and the snapshot includes references as well as
/// values so the file does not disclose its secret inventory.
pub struct FileSecretStore {
    path: PathBuf,
    master_key: [u8; MASTER_KEY_BYTES],
    operation_lock: Mutex<()>,
}

impl fmt::Debug for FileSecretStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileSecretStore")
            .field("path", &self.path)
            .field("master_key", &"<redacted>")
            .finish()
    }
}

impl FileSecretStore {
    /// Opens a file-backed store. The key is copied into protected store state
    /// and is never inferred from the filesystem or environment.
    pub fn new(
        path: impl Into<PathBuf>,
        master_key: impl AsRef<[u8]>,
    ) -> Result<Self, SecretStoreError> {
        let master_key = master_key.as_ref();
        if master_key.len() != MASTER_KEY_BYTES {
            return Err(SecretStoreError::InvalidMasterKey {
                expected_bytes: MASTER_KEY_BYTES,
                actual_bytes: master_key.len(),
            });
        }
        let mut key = [0_u8; MASTER_KEY_BYTES];
        key.copy_from_slice(master_key);
        Ok(Self {
            path: path.into(),
            master_key: key,
            operation_lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> Result<usize, SecretStoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::Poisoned)?;
        Ok(self.load_values()?.len())
    }

    pub fn is_empty(&self) -> Result<bool, SecretStoreError> {
        Ok(self.len()? == 0)
    }

    fn load_values(&self) -> Result<SecretValues, SecretStoreError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SecretValues::default());
            }
            Err(_) => return Err(SecretStoreError::Io { operation: "read" }),
        };
        let mut encoded = Vec::new();
        file.take(MAX_SECRET_FILE_BYTES as u64 + 1)
            .read_to_end(&mut encoded)
            .map_err(|_| SecretStoreError::Io { operation: "read" })?;
        if encoded.len() > MAX_SECRET_FILE_BYTES {
            return Err(SecretStoreError::Corrupt);
        }
        let mut plaintext = decrypt_snapshot(&self.master_key, &encoded)?;
        let result = decode_snapshot(&plaintext);
        wipe_bytes(&mut plaintext);
        result
    }

    fn save_values(&self, values: &SecretValues) -> Result<(), SecretStoreError> {
        if values.is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(_) => {
                    return Err(SecretStoreError::Io {
                        operation: "remove",
                    });
                }
            }
        }
        let mut encoded = encrypt_snapshot(&self.master_key, values)?;
        let result = atomic_write(&self.path, &encoded);
        wipe_bytes(&mut encoded);
        result
    }
}

impl Drop for FileSecretStore {
    fn drop(&mut self) {
        for byte in &mut self.master_key {
            *byte = 0;
        }
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, reference: &SecretRef) -> Result<Option<SecretMaterial>, SecretStoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::Poisoned)?;
        self.load_values()?
            .get(reference)
            .map(|value| {
                SecretMaterial::from_bytes(value.clone()).map_err(SecretStoreError::Secret)
            })
            .transpose()
    }

    fn put(&self, reference: SecretRef, value: SecretMaterial) -> Result<(), SecretStoreError> {
        validate_secret(value.as_bytes())?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::Poisoned)?;
        let mut values = self.load_values()?;
        let current_total = values.values().map(Vec::len).sum::<usize>();
        let replaced = values.get(&reference).map_or(0, Vec::len);
        let next_total = current_total
            .saturating_sub(replaced)
            .saturating_add(value.as_bytes().len());
        if !values.contains_key(&reference) && values.len() >= MAX_SECRET_ENTRIES {
            return Err(SecretStoreError::CapacityExceeded {
                maximum_entries: MAX_SECRET_ENTRIES,
            });
        }
        if next_total > MAX_TOTAL_SECRET_BYTES {
            return Err(SecretStoreError::CapacityExceeded {
                maximum_entries: MAX_TOTAL_SECRET_BYTES,
            });
        }
        if let Some(mut previous) = values.insert(reference, value.as_bytes().to_vec()) {
            wipe_bytes(&mut previous);
        }
        self.save_values(&values)
    }

    fn remove(&self, reference: &SecretRef) -> Result<bool, SecretStoreError> {
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::Poisoned)?;
        let mut values = self.load_values()?;
        if let Some(mut removed) = values.remove(reference) {
            wipe_bytes(&mut removed);
        } else {
            return Ok(false);
        }
        self.save_values(&values)?;
        Ok(true)
    }
}

fn validate_secret(value: &[u8]) -> Result<(), SecretStoreError> {
    if value.is_empty() {
        return Err(SecretStoreError::Secret(SecretError::Empty));
    }
    if value.len() > MAX_SECRET_BYTES {
        return Err(SecretStoreError::Secret(SecretError::TooLarge));
    }
    Ok(())
}

fn encrypt_snapshot(
    master_key: &[u8; MASTER_KEY_BYTES],
    values: &SecretValues,
) -> Result<Vec<u8>, SecretStoreError> {
    let mut plaintext = encode_snapshot(values)?;
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| SecretStoreError::Crypto)?;
    let mut aad = Vec::with_capacity(FILE_HEADER_BYTES);
    aad.extend_from_slice(FILE_MAGIC);
    aad.push(FILE_VERSION);
    aad.extend_from_slice(&nonce);
    let cipher = match ChaCha20Poly1305::new_from_slice(master_key) {
        Ok(cipher) => cipher,
        Err(_) => {
            wipe_bytes(&mut plaintext);
            return Err(SecretStoreError::Crypto);
        }
    };
    let encrypted = match cipher.encrypt(
        Nonce::from_slice(&nonce),
        Payload {
            msg: &plaintext,
            aad: &aad,
        },
    ) {
        Ok(encrypted) => encrypted,
        Err(_) => {
            wipe_bytes(&mut plaintext);
            return Err(SecretStoreError::Crypto);
        }
    };
    wipe_bytes(&mut plaintext);
    let ciphertext = encrypted;
    let mut encoded = aad;
    encoded.extend_from_slice(&ciphertext);
    Ok(encoded)
}

fn decrypt_snapshot(
    master_key: &[u8; MASTER_KEY_BYTES],
    encoded: &[u8],
) -> Result<Vec<u8>, SecretStoreError> {
    if encoded.len() < FILE_HEADER_BYTES + AEAD_TAG_BYTES
        || encoded.len() > MAX_SECRET_FILE_BYTES
        || &encoded[..FILE_MAGIC.len()] != FILE_MAGIC
        || encoded[FILE_MAGIC.len()] != FILE_VERSION
    {
        return Err(SecretStoreError::Corrupt);
    }
    let header = &encoded[..FILE_HEADER_BYTES];
    let nonce = &encoded[FILE_MAGIC.len() + 1..FILE_HEADER_BYTES];
    let cipher =
        ChaCha20Poly1305::new_from_slice(master_key).map_err(|_| SecretStoreError::Crypto)?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: &encoded[FILE_HEADER_BYTES..],
                aad: header,
            },
        )
        .map_err(|_| SecretStoreError::Corrupt)
}

fn encode_snapshot(values: &SecretValues) -> Result<Vec<u8>, SecretStoreError> {
    if values.len() > MAX_SECRET_ENTRIES {
        return Err(SecretStoreError::CapacityExceeded {
            maximum_entries: MAX_SECRET_ENTRIES,
        });
    }
    let total = values.values().try_fold(0_usize, |total, value| {
        validate_secret(value)?;
        total
            .checked_add(value.len())
            .ok_or(SecretStoreError::CapacityExceeded {
                maximum_entries: MAX_TOTAL_SECRET_BYTES,
            })
    })?;
    if total > MAX_TOTAL_SECRET_BYTES {
        return Err(SecretStoreError::CapacityExceeded {
            maximum_entries: MAX_TOTAL_SECRET_BYTES,
        });
    }
    let mut encoded = Vec::with_capacity(
        MAX_SNAPSHOT_BYTES.min(
            SNAPSHOT_PREFIX_BYTES
                + values
                    .iter()
                    .map(|(reference, value)| reference.as_str().len() + value.len() + 8)
                    .sum::<usize>(),
        ),
    );
    push_u32(&mut encoded, values.len());
    for (reference, value) in values.iter() {
        if reference.as_str().len() > MAX_SECRET_REF_BYTES {
            return Err(SecretStoreError::Corrupt);
        }
        push_u32(&mut encoded, reference.as_str().len());
        encoded.extend_from_slice(reference.as_str().as_bytes());
        push_u32(&mut encoded, value.len());
        encoded.extend_from_slice(value);
    }
    if encoded.len() > MAX_SNAPSHOT_BYTES {
        return Err(SecretStoreError::CapacityExceeded {
            maximum_entries: MAX_TOTAL_SECRET_BYTES,
        });
    }
    Ok(encoded)
}

fn decode_snapshot(encoded: &[u8]) -> Result<SecretValues, SecretStoreError> {
    if encoded.len() > MAX_SNAPSHOT_BYTES {
        return Err(SecretStoreError::Corrupt);
    }
    let mut cursor = 0;
    let count = read_u32(encoded, &mut cursor)? as usize;
    if count > MAX_SECRET_ENTRIES {
        return Err(SecretStoreError::Corrupt);
    }
    let mut values = SecretValues::default();
    let mut total = 0_usize;
    for _ in 0..count {
        let reference_len = read_u32(encoded, &mut cursor)? as usize;
        if reference_len == 0 || reference_len > MAX_SECRET_REF_BYTES {
            return Err(SecretStoreError::Corrupt);
        }
        let reference_bytes = take(encoded, &mut cursor, reference_len)?;
        let reference = SecretRef::new(
            String::from_utf8(reference_bytes.to_vec()).map_err(|_| SecretStoreError::Corrupt)?,
        )
        .map_err(|_| SecretStoreError::Corrupt)?;
        let secret_len = read_u32(encoded, &mut cursor)? as usize;
        if secret_len == 0 || secret_len > MAX_SECRET_BYTES {
            return Err(SecretStoreError::Corrupt);
        }
        let secret = take(encoded, &mut cursor, secret_len)?.to_vec();
        total = total
            .checked_add(secret_len)
            .ok_or(SecretStoreError::Corrupt)?;
        let duplicate = if let Some(mut previous) = values.insert(reference, secret) {
            wipe_bytes(&mut previous);
            true
        } else {
            false
        };
        if total > MAX_TOTAL_SECRET_BYTES || duplicate {
            return Err(SecretStoreError::Corrupt);
        }
    }
    if cursor != encoded.len() {
        return Err(SecretStoreError::Corrupt);
    }
    Ok(values)
}

fn push_u32(buffer: &mut Vec<u8>, value: usize) {
    buffer.extend_from_slice(&(value as u32).to_be_bytes());
}

fn read_u32(encoded: &[u8], cursor: &mut usize) -> Result<u32, SecretStoreError> {
    let bytes = take(encoded, cursor, 4)?;
    Ok(u32::from_be_bytes(
        bytes.try_into().map_err(|_| SecretStoreError::Corrupt)?,
    ))
}

fn take<'a>(
    encoded: &'a [u8],
    cursor: &mut usize,
    length: usize,
) -> Result<&'a [u8], SecretStoreError> {
    let end = cursor
        .checked_add(length)
        .ok_or(SecretStoreError::Corrupt)?;
    let value = encoded.get(*cursor..end).ok_or(SecretStoreError::Corrupt)?;
    *cursor = end;
    Ok(value)
}

fn wipe_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        *byte = 0;
    }
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), SecretStoreError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|_| SecretStoreError::Io {
        operation: "create",
    })?;
    let file_name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("secret-store"));
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| SecretStoreError::Io {
                operation: "create",
            })?;
        created = true;
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|_| SecretStoreError::Io {
                operation: "permissions",
            })?;
        file.write_all(content)
            .and_then(|_| file.sync_all())
            .map_err(|_| SecretStoreError::Io { operation: "write" })?;
        drop(file);
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(error)
                if cfg!(windows) && matches!(error.kind(), std::io::ErrorKind::AlreadyExists) =>
            {
                // Windows does not replace an existing destination with the
                // standard-library rename operation. The temp file is still
                // fully synced before this narrow compatibility fallback.
                fs::remove_file(path).map_err(|_| SecretStoreError::Io {
                    operation: "replace",
                })?;
                fs::rename(&temporary, path).map_err(|_| SecretStoreError::Io {
                    operation: "replace",
                })
            }
            Err(_) => Err(SecretStoreError::Io {
                operation: "replace",
            }),
        }
    })();
    if created && result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

impl<T> SecretResolver for T
where
    T: SecretStore,
{
    fn resolve(&self, reference: &SecretRef) -> Result<SecretMaterial, SecretError> {
        self.get(reference)
            .map_err(|_| SecretError::Unavailable)?
            .ok_or(SecretError::Unavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretStoreError {
    Secret(SecretError),
    CapacityExceeded {
        maximum_entries: usize,
    },
    InvalidMasterKey {
        expected_bytes: usize,
        actual_bytes: usize,
    },
    Corrupt,
    Crypto,
    Io {
        operation: &'static str,
    },
    Poisoned,
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Secret(error) => error.fmt(formatter),
            Self::CapacityExceeded { maximum_entries } => {
                write!(
                    formatter,
                    "secret store capacity limit {maximum_entries} was reached"
                )
            }
            Self::InvalidMasterKey {
                expected_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "master key must contain {expected_bytes} bytes (received {actual_bytes})"
            ),
            Self::Corrupt => {
                formatter.write_str("secret store file is corrupt or failed authentication")
            }
            Self::Crypto => formatter.write_str("secret store encryption failed"),
            Self::Io { operation } => {
                write!(formatter, "secret store file {operation} operation failed")
            }
            Self::Poisoned => formatter.write_str("secret store lock is unavailable"),
        }
    }
}

impl std::error::Error for SecretStoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

    fn test_store() -> (FileSecretStore, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "yunxi-weixin-secret-store-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).expect("test directory");
        let path = directory.join("secrets.bin");
        let store = FileSecretStore::new(&path, [7_u8; MASTER_KEY_BYTES]).expect("store");
        (store, directory)
    }

    fn remove_test_directory(directory: &Path) {
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn memory_store_round_trips_without_exposing_value_in_debug() {
        let store = MemorySecretStore::new();
        let reference = SecretRef::new("host:weixin/token").expect("reference");
        store
            .put(
                reference.clone(),
                SecretMaterial::from_text("private-token").expect("secret"),
            )
            .expect("put");
        assert!(store.contains(&reference).expect("contains"));
        let material = store.get(&reference).expect("get").expect("material");
        assert_eq!(material.as_bytes(), b"private-token");
        assert!(!format!("{store:?}").contains("private-token"));
        assert!(store.remove(&reference).expect("remove"));
        assert!(!store.contains(&reference).expect("missing"));
    }

    #[test]
    fn file_store_round_trips_encrypted_data_across_instances() {
        let (store, directory) = test_store();
        let path = store.path().to_path_buf();
        let reference = SecretRef::new("host:weixin/file-token").expect("reference");
        let secret = "private-file-token";
        store
            .put(
                reference.clone(),
                SecretMaterial::from_text(secret).expect("secret"),
            )
            .expect("put");
        let on_disk = fs::read(&path).expect("encrypted file");
        assert!(!String::from_utf8_lossy(&on_disk).contains(secret));
        assert!(!format!("{store:?}").contains(secret));

        let reopened = FileSecretStore::new(path, [7_u8; MASTER_KEY_BYTES]).expect("reopen");
        let material = reopened.get(&reference).expect("get").expect("material");
        assert_eq!(material.as_bytes(), secret.as_bytes());
        assert_eq!(reopened.len().expect("len"), 1);
        remove_test_directory(&directory);
    }

    #[test]
    fn file_store_rejects_wrong_key_tamper_and_corrupt_files() {
        let (store, directory) = test_store();
        let path = store.path().to_path_buf();
        let reference = SecretRef::new("host:weixin/token").expect("reference");
        let secret = "do-not-leak";
        store
            .put(
                reference.clone(),
                SecretMaterial::from_text(secret).expect("secret"),
            )
            .expect("put");

        let wrong_key = FileSecretStore::new(&path, [8_u8; MASTER_KEY_BYTES]).expect("store");
        let error = wrong_key.get(&reference).expect_err("wrong key");
        assert_eq!(error, SecretStoreError::Corrupt);
        assert!(!error.to_string().contains(secret));

        let mut tampered = fs::read(&path).expect("read");
        let last = tampered.last_mut().expect("ciphertext");
        *last ^= 1;
        fs::write(&path, tampered).expect("tamper");
        assert_eq!(
            store.get(&reference).expect_err("tamper must fail closed"),
            SecretStoreError::Corrupt
        );

        fs::write(&path, b"not-a-secret-store").expect("corrupt");
        assert_eq!(store.len(), Err(SecretStoreError::Corrupt));
        remove_test_directory(&directory);
    }

    #[test]
    fn file_store_enforces_key_entry_and_file_bounds() {
        let error =
            FileSecretStore::new("unused", [1_u8; MASTER_KEY_BYTES - 1]).expect_err("short key");
        assert!(matches!(error, SecretStoreError::InvalidMasterKey { .. }));

        let (store, directory) = test_store();
        for index in 0..MAX_SECRET_ENTRIES {
            let reference = SecretRef::new(format!("host:key-{index}")).expect("reference");
            store
                .put(
                    reference,
                    SecretMaterial::from_text("value").expect("secret"),
                )
                .expect("within entry bound");
        }
        let error = store.put(
            SecretRef::new("host:key-overflow").expect("reference"),
            SecretMaterial::from_text("value").expect("secret"),
        );
        assert_eq!(
            error,
            Err(SecretStoreError::CapacityExceeded {
                maximum_entries: MAX_SECRET_ENTRIES,
            })
        );

        fs::write(store.path(), vec![0_u8; MAX_SECRET_FILE_BYTES + 1]).expect("oversized file");
        assert_eq!(store.is_empty(), Err(SecretStoreError::Corrupt));
        remove_test_directory(&directory);
    }

    #[test]
    fn file_store_removes_last_entry_and_cleans_up_file() {
        let (store, directory) = test_store();
        let path = store.path().to_path_buf();
        let reference = SecretRef::new("host:cleanup").expect("reference");
        store
            .put(
                reference.clone(),
                SecretMaterial::from_text("temporary").expect("secret"),
            )
            .expect("put");
        assert!(path.exists());
        assert!(store.remove(&reference).expect("remove"));
        assert!(!path.exists());
        assert!(!store.contains(&reference).expect("missing"));
        assert!(!store.remove(&reference).expect("remove missing"));
        remove_test_directory(&directory);
    }
}
