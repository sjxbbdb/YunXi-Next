use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use atomic_write_file::AtomicWriteFile;
use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use fs2::FileExt;
use zeroize::Zeroize;

use crate::{
    MASTER_KEY_BYTES, MAX_ENVIRONMENT_NAME_BYTES, MAX_SECRET_FILE_BYTES, MAX_SECRET_KEY_BYTES,
    MAX_SECRET_STORE_BYTES, MAX_SECRET_STORE_ENTRIES, MAX_SECRET_VALUE_BYTES,
    error::{BoundedItem, InputField, SecretError, SecretStoreError},
    model::{SecretKey, SecretValue},
};

/// A backing store resolves a host-side key into a bounded secret value.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &SecretKey) -> Result<SecretValue, SecretStoreError>;

    /// Stores a value. Read-only stores keep the default unavailable result.
    fn put(&self, _key: &SecretKey, _value: SecretValue) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unavailable)
    }

    /// Removes a value. Read-only stores keep the default unavailable result.
    fn remove(&self, _key: &SecretKey) -> Result<bool, SecretStoreError> {
        Err(SecretStoreError::Unavailable)
    }
}

struct MemoryState {
    values: BTreeMap<String, SecretValue>,
    total_bytes: usize,
}

/// An in-memory store suitable for tests and explicitly local deployments.
pub struct MemorySecretStore {
    state: Mutex<MemoryState>,
}

impl Default for MemorySecretStore {
    fn default() -> Self {
        Self {
            state: Mutex::new(MemoryState {
                values: BTreeMap::new(),
                total_bytes: 0,
            }),
        }
    }
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, key: &SecretKey, value: impl AsRef<[u8]>) -> Result<(), SecretError> {
        let value = SecretValue::new(value)?;
        self.put_value(key, value).map_err(map_store_error)
    }

    pub fn remove(&self, key: &SecretKey) -> Result<bool, SecretError> {
        <Self as SecretStore>::remove(self, key).map_err(map_store_error)
    }

    fn put_value(&self, key: &SecretKey, value: SecretValue) -> Result<(), SecretStoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SecretStoreError::Unavailable)?;
        let previous_len = state.values.get(key.as_str()).map_or(0, SecretValue::len);
        if !state.values.contains_key(key.as_str())
            && state.values.len() >= MAX_SECRET_STORE_ENTRIES
        {
            return Err(SecretStoreError::TooLarge {
                item: BoundedItem::StoreEntries,
                max: MAX_SECRET_STORE_ENTRIES,
            });
        }
        let new_total = state
            .total_bytes
            .checked_sub(previous_len)
            .and_then(|total| total.checked_add(value.len()))
            .ok_or(SecretStoreError::TooLarge {
                item: BoundedItem::StoreBytes,
                max: MAX_SECRET_STORE_BYTES,
            })?;
        if new_total > MAX_SECRET_STORE_BYTES {
            return Err(SecretStoreError::TooLarge {
                item: BoundedItem::StoreBytes,
                max: MAX_SECRET_STORE_BYTES,
            });
        }
        state.values.insert(key.as_str().to_owned(), value);
        state.total_bytes = new_total;
        Ok(())
    }

    fn remove_value(&self, key: &SecretKey) -> Result<bool, SecretStoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SecretStoreError::Unavailable)?;
        let removed = state.values.remove(key.as_str());
        if let Some(value) = removed.as_ref() {
            state.total_bytes = state.total_bytes.saturating_sub(value.len());
        }
        Ok(removed.is_some())
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &SecretKey) -> Result<SecretValue, SecretStoreError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SecretStoreError::Unavailable)?;
        state
            .values
            .get(key.as_str())
            .map(SecretValue::clone_value)
            .ok_or(SecretStoreError::Missing)
    }

    fn put(&self, key: &SecretKey, value: SecretValue) -> Result<(), SecretStoreError> {
        self.put_value(key, value)
    }

    fn remove(&self, key: &SecretKey) -> Result<bool, SecretStoreError> {
        self.remove_value(key)
    }
}

/// A store that reads values from the host process environment.
pub struct EnvironmentSecretStore {
    prefix: String,
}

impl EnvironmentSecretStore {
    pub fn new(prefix: impl Into<String>) -> Result<Self, SecretError> {
        let prefix = prefix.into();
        if prefix.len() > MAX_ENVIRONMENT_NAME_BYTES {
            return Err(SecretError::TooLarge {
                item: BoundedItem::EnvironmentName,
                max: MAX_ENVIRONMENT_NAME_BYTES,
            });
        }
        if prefix.chars().any(char::is_control) {
            return Err(SecretError::InvalidInput {
                field: InputField::EnvironmentName,
            });
        }
        Ok(Self { prefix })
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn environment_name(&self, key: &SecretKey) -> Result<String, SecretStoreError> {
        let name_len = self.prefix.len().checked_add(key.as_str().len()).ok_or(
            SecretStoreError::TooLarge {
                item: BoundedItem::EnvironmentName,
                max: MAX_ENVIRONMENT_NAME_BYTES,
            },
        )?;
        if name_len > MAX_ENVIRONMENT_NAME_BYTES {
            return Err(SecretStoreError::TooLarge {
                item: BoundedItem::EnvironmentName,
                max: MAX_ENVIRONMENT_NAME_BYTES,
            });
        }
        Ok(format!("{}{}", self.prefix, key.as_str()))
    }
}

impl SecretStore for EnvironmentSecretStore {
    fn get(&self, key: &SecretKey) -> Result<SecretValue, SecretStoreError> {
        let name = self.environment_name(key)?;
        let value = env::var_os(name).ok_or(SecretStoreError::Missing)?;
        let value = os_string_to_bytes(value)?;
        SecretValue::new(value).map_err(|error| match error {
            SecretError::TooLarge { item, max } => SecretStoreError::TooLarge { item, max },
            _ => SecretStoreError::Unavailable,
        })
    }
}

fn os_string_to_bytes(value: OsString) -> Result<Vec<u8>, SecretStoreError> {
    value
        .into_string()
        .map(String::into_bytes)
        .map_err(|_| SecretStoreError::InvalidEncoding)
}

const FILE_MAGIC: &[u8; 8] = b"YXSBRK01";
const FILE_VERSION: u8 = 1;
const NONCE_BYTES: usize = 12;
const AEAD_TAG_BYTES: usize = 16;
const FILE_HEADER_BYTES: usize = FILE_MAGIC.len() + 1 + 8 + NONCE_BYTES;
const LOCK_SUFFIX: &str = ".lock";

/// A durable store encrypted with a caller-provided 32-byte master key.
///
/// The file contains one authenticated encrypted snapshot. Updates are
/// serialized by a sidecar lock and committed by an atomic same-directory
/// replacement, so failed writes leave the previous snapshot untouched.
pub struct FileSecretStore {
    path: PathBuf,
    master_key: [u8; MASTER_KEY_BYTES],
    operation_lock: Mutex<()>,
}

impl std::fmt::Debug for FileSecretStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileSecretStore")
            .field("path", &self.path)
            .field("master_key", &"<redacted>")
            .finish()
    }
}

impl FileSecretStore {
    /// Opens a store with a caller-provided 32-byte master key.
    pub fn new(
        path: impl Into<PathBuf>,
        master_key: impl AsRef<[u8]>,
    ) -> Result<Self, SecretStoreError> {
        let path = path.into();
        validate_store_path(&path)?;
        let provided_key = master_key.as_ref();
        if provided_key.len() != MASTER_KEY_BYTES {
            return Err(SecretStoreError::InvalidMasterKey {
                expected: MASTER_KEY_BYTES,
                actual: provided_key.len(),
            });
        }
        let mut key = [0_u8; MASTER_KEY_BYTES];
        key.copy_from_slice(provided_key);
        let store = Self {
            path,
            master_key: key,
            operation_lock: Mutex::new(()),
        };
        store.with_file_lock(|_| store.load_snapshot().map(|_| ()))?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Inserts or replaces a value and commits the complete new snapshot.
    pub fn put(&self, key: &SecretKey, value: impl AsRef<[u8]>) -> Result<(), SecretError> {
        let value = SecretValue::new(value)?;
        <Self as SecretStore>::put(self, key, value).map_err(map_store_error)
    }

    pub fn remove(&self, key: &SecretKey) -> Result<bool, SecretError> {
        <Self as SecretStore>::remove(self, key).map_err(map_store_error)
    }

    fn with_file_lock<T>(
        &self,
        operation: impl FnOnce(&File) -> Result<T, SecretStoreError>,
    ) -> Result<T, SecretStoreError> {
        let _local_guard = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::Unavailable)?;
        let lock_path = lock_path(&self.path)?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|_| SecretStoreError::Io { operation: "lock" })?;
        #[cfg(unix)]
        set_private_permissions(&lock_file)?;
        lock_file
            .lock_exclusive()
            .map_err(|_| SecretStoreError::Io { operation: "lock" })?;
        let result = operation(&lock_file);
        let unlock_result = FileExt::unlock(&lock_file).map_err(|_| SecretStoreError::Io {
            operation: "unlock",
        });
        match (result, unlock_result) {
            (Err(error), _) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn load_snapshot(&self) -> Result<Snapshot, SecretStoreError> {
        validate_existing_file(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Snapshot::default());
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
        decrypt_snapshot(&self.master_key, &encoded)
    }

    fn save_snapshot(&self, snapshot: &Snapshot) -> Result<(), SecretStoreError> {
        let mut encoded = encrypt_snapshot(&self.master_key, snapshot)?;
        let result = write_atomically(&self.path, &encoded);
        encoded.zeroize();
        result
    }
}

impl Drop for FileSecretStore {
    fn drop(&mut self) {
        self.master_key.zeroize();
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &SecretKey) -> Result<SecretValue, SecretStoreError> {
        self.with_file_lock(|_| {
            let snapshot = self.load_snapshot()?;
            snapshot
                .values
                .into_iter()
                .find(|(stored_key, _)| stored_key == key.as_str())
                .map(|(_, value)| value)
                .ok_or(SecretStoreError::Missing)
        })
    }

    fn put(&self, key: &SecretKey, value: SecretValue) -> Result<(), SecretStoreError> {
        self.with_file_lock(|_| {
            let mut snapshot = self.load_snapshot()?;
            let is_new = !snapshot.values.contains_key(key.as_str());
            if is_new && snapshot.values.len() >= MAX_SECRET_STORE_ENTRIES {
                return Err(SecretStoreError::TooLarge {
                    item: BoundedItem::StoreEntries,
                    max: MAX_SECRET_STORE_ENTRIES,
                });
            }
            let previous_len = snapshot
                .values
                .get(key.as_str())
                .map_or(0, SecretValue::len);
            let total = snapshot
                .total_bytes()
                .checked_sub(previous_len)
                .and_then(|total| total.checked_add(value.len()))
                .ok_or(SecretStoreError::TooLarge {
                    item: BoundedItem::StoreBytes,
                    max: MAX_SECRET_STORE_BYTES,
                })?;
            if total > MAX_SECRET_STORE_BYTES {
                return Err(SecretStoreError::TooLarge {
                    item: BoundedItem::StoreBytes,
                    max: MAX_SECRET_STORE_BYTES,
                });
            }
            snapshot.values.insert(key.as_str().to_owned(), value);
            snapshot.bump_revision()?;
            self.save_snapshot(&snapshot)
        })
    }

    fn remove(&self, key: &SecretKey) -> Result<bool, SecretStoreError> {
        self.with_file_lock(|_| {
            let mut snapshot = self.load_snapshot()?;
            if snapshot.values.remove(key.as_str()).is_none() {
                return Ok(false);
            }
            snapshot.bump_revision()?;
            self.save_snapshot(&snapshot)?;
            Ok(true)
        })
    }
}

#[derive(Default)]
struct Snapshot {
    revision: u64,
    values: BTreeMap<String, SecretValue>,
}

impl Snapshot {
    fn total_bytes(&self) -> usize {
        self.values.values().map(SecretValue::len).sum()
    }

    fn bump_revision(&mut self) -> Result<(), SecretStoreError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(SecretStoreError::RevisionExhausted)?;
        Ok(())
    }
}

fn validate_store_path(path: &Path) -> Result<(), SecretStoreError> {
    if path.as_os_str().is_empty() || path.file_name().is_none() {
        return Err(SecretStoreError::InvalidPath);
    }
    validate_existing_file(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|_| SecretStoreError::Io {
        operation: "create",
    })?;
    Ok(())
}

fn validate_existing_file(path: &Path) -> Result<(), SecretStoreError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(SecretStoreError::InvalidPath);
        }
    }
    Ok(())
}

fn lock_path(path: &Path) -> Result<PathBuf, SecretStoreError> {
    let file_name = path.file_name().ok_or(SecretStoreError::InvalidPath)?;
    let mut lock_name = file_name.to_os_string();
    lock_name.push(LOCK_SUFFIX);
    Ok(path.with_file_name(lock_name))
}

fn encrypt_snapshot(
    master_key: &[u8; MASTER_KEY_BYTES],
    snapshot: &Snapshot,
) -> Result<Vec<u8>, SecretStoreError> {
    let mut plaintext = encode_snapshot(snapshot)?;
    let mut nonce = [0_u8; NONCE_BYTES];
    if getrandom::fill(&mut nonce).is_err() {
        plaintext.zeroize();
        return Err(SecretStoreError::Crypto);
    }
    let mut aad = Vec::with_capacity(FILE_HEADER_BYTES);
    aad.extend_from_slice(FILE_MAGIC);
    aad.push(FILE_VERSION);
    aad.extend_from_slice(&snapshot.revision.to_be_bytes());
    aad.extend_from_slice(&nonce);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(master_key));
    let encrypted = cipher.encrypt(
        Nonce::from_slice(&nonce),
        Payload {
            msg: &plaintext,
            aad: &aad,
        },
    );
    plaintext.zeroize();
    let ciphertext = encrypted.map_err(|_| SecretStoreError::Crypto)?;
    let mut encoded = aad;
    encoded.extend_from_slice(&ciphertext);
    if encoded.len() > MAX_SECRET_FILE_BYTES {
        return Err(SecretStoreError::TooLarge {
            item: BoundedItem::File,
            max: MAX_SECRET_FILE_BYTES,
        });
    }
    Ok(encoded)
}

fn decrypt_snapshot(
    master_key: &[u8; MASTER_KEY_BYTES],
    encoded: &[u8],
) -> Result<Snapshot, SecretStoreError> {
    if encoded.len() < FILE_HEADER_BYTES + AEAD_TAG_BYTES
        || encoded.len() > MAX_SECRET_FILE_BYTES
        || &encoded[..FILE_MAGIC.len()] != FILE_MAGIC
        || encoded[FILE_MAGIC.len()] != FILE_VERSION
    {
        return Err(SecretStoreError::Corrupt);
    }
    let revision_start = FILE_MAGIC.len() + 1;
    let revision_end = revision_start + 8;
    let revision = u64::from_be_bytes(
        encoded[revision_start..revision_end]
            .try_into()
            .map_err(|_| SecretStoreError::Corrupt)?,
    );
    if revision == 0 {
        return Err(SecretStoreError::Corrupt);
    }
    let nonce_start = revision_end;
    let nonce_end = nonce_start + NONCE_BYTES;
    let header = &encoded[..FILE_HEADER_BYTES];
    let cipher = ChaCha20Poly1305::new(Key::from_slice(master_key));
    let mut plaintext = cipher
        .decrypt(
            Nonce::from_slice(&encoded[nonce_start..nonce_end]),
            Payload {
                msg: &encoded[FILE_HEADER_BYTES..],
                aad: header,
            },
        )
        .map_err(|_| SecretStoreError::Corrupt)?;
    let result = decode_snapshot(revision, &plaintext);
    plaintext.zeroize();
    result
}

fn encode_snapshot(snapshot: &Snapshot) -> Result<Vec<u8>, SecretStoreError> {
    if snapshot.values.len() > MAX_SECRET_STORE_ENTRIES {
        return Err(SecretStoreError::TooLarge {
            item: BoundedItem::StoreEntries,
            max: MAX_SECRET_STORE_ENTRIES,
        });
    }
    if snapshot.total_bytes() > MAX_SECRET_STORE_BYTES {
        return Err(SecretStoreError::TooLarge {
            item: BoundedItem::StoreBytes,
            max: MAX_SECRET_STORE_BYTES,
        });
    }
    let mut encoded = Vec::new();
    push_u32(&mut encoded, snapshot.values.len())?;
    for (key, value) in &snapshot.values {
        if key.is_empty() || key.len() > MAX_SECRET_KEY_BYTES || key.chars().any(char::is_control) {
            return Err(SecretStoreError::Corrupt);
        }
        push_u32(&mut encoded, key.len())?;
        encoded.extend_from_slice(key.as_bytes());
        push_u32(&mut encoded, value.len())?;
        encoded.extend_from_slice(value.bytes());
    }
    if encoded.len() > MAX_SECRET_FILE_BYTES {
        return Err(SecretStoreError::TooLarge {
            item: BoundedItem::File,
            max: MAX_SECRET_FILE_BYTES,
        });
    }
    Ok(encoded)
}

fn decode_snapshot(revision: u64, encoded: &[u8]) -> Result<Snapshot, SecretStoreError> {
    let mut cursor = 0;
    let count = read_u32(encoded, &mut cursor)? as usize;
    if count > MAX_SECRET_STORE_ENTRIES {
        return Err(SecretStoreError::Corrupt);
    }
    let mut values = BTreeMap::new();
    let mut total = 0_usize;
    for _ in 0..count {
        let key_len = read_u32(encoded, &mut cursor)? as usize;
        if key_len == 0 || key_len > MAX_SECRET_KEY_BYTES {
            return Err(SecretStoreError::Corrupt);
        }
        let key_bytes = take(encoded, &mut cursor, key_len)?;
        let key = String::from_utf8(key_bytes.to_vec()).map_err(|_| SecretStoreError::Corrupt)?;
        if key.chars().any(char::is_control) || values.contains_key(&key) {
            return Err(SecretStoreError::Corrupt);
        }
        let value_len = read_u32(encoded, &mut cursor)? as usize;
        if value_len > MAX_SECRET_VALUE_BYTES {
            return Err(SecretStoreError::Corrupt);
        }
        let value = SecretValue::new(take(encoded, &mut cursor, value_len)?)
            .map_err(|_| SecretStoreError::Corrupt)?;
        total = total
            .checked_add(value_len)
            .ok_or(SecretStoreError::Corrupt)?;
        if total > MAX_SECRET_STORE_BYTES || values.insert(key, value).is_some() {
            return Err(SecretStoreError::Corrupt);
        }
    }
    if cursor != encoded.len() {
        return Err(SecretStoreError::Corrupt);
    }
    Ok(Snapshot { revision, values })
}

fn push_u32(buffer: &mut Vec<u8>, value: usize) -> Result<(), SecretStoreError> {
    let value = u32::try_from(value).map_err(|_| SecretStoreError::Corrupt)?;
    buffer.extend_from_slice(&value.to_be_bytes());
    Ok(())
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

fn write_atomically(path: &Path, content: &[u8]) -> Result<(), SecretStoreError> {
    #[cfg(unix)]
    let mut options = AtomicWriteFile::options();
    #[cfg(not(unix))]
    let options = AtomicWriteFile::options();
    #[cfg(unix)]
    {
        use atomic_write_file::unix::OpenOptionsExt;
        options.mode(0o600).preserve_mode(false);
    }
    let mut file = options.open(path).map_err(|_| SecretStoreError::Io {
        operation: "create",
    })?;
    if file.write_all(content).is_err() || file.sync_all().is_err() {
        let _ = file.discard();
        return Err(SecretStoreError::Io { operation: "write" });
    }
    file.commit().map_err(|_| SecretStoreError::Io {
        operation: "replace",
    })
}

#[cfg(unix)]
fn set_private_permissions(file: &File) -> Result<(), SecretStoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = file
        .metadata()
        .map_err(|_| SecretStoreError::Io {
            operation: "permissions",
        })?
        .permissions();
    permissions.set_mode(0o600);
    file.set_permissions(permissions)
        .map_err(|_| SecretStoreError::Io {
            operation: "permissions",
        })
}

fn map_store_error(error: SecretStoreError) -> SecretError {
    match error {
        SecretStoreError::Missing => SecretError::Missing,
        SecretStoreError::TooLarge { item, max } => SecretError::TooLarge { item, max },
        other => SecretError::Store(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        thread,
    };

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn test_path(label: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("yunxi-secret-broker-{label}-{id}.bin"))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(lock_path(path).expect("test path has a file name"));
    }

    #[test]
    fn memory_store_returns_a_copy_without_exposing_its_contents() {
        let store = MemorySecretStore::new();
        let key = SecretKey::new("test-key").expect("valid key");
        store.insert(&key, b"value").expect("insert succeeds");
        let first = store.get(&key).expect("secret exists");
        let second = store.get(&key).expect("secret still exists");
        assert!(first.with_bytes(|bytes| bytes == b"value"));
        assert_eq!(second.len(), 5);
    }

    #[test]
    fn environment_store_reports_missing_without_value_data() {
        let store =
            EnvironmentSecretStore::new("YUNXI_SECRET_BROKER_MISSING_").expect("valid prefix");
        let key = SecretKey::new("definitely-not-set").expect("valid key");
        assert!(matches!(store.get(&key), Err(SecretStoreError::Missing)));
    }

    #[test]
    fn file_store_round_trips_without_plaintext_on_disk() {
        let path = test_path("round-trip");
        let key = SecretKey::new("provider-token").expect("valid key");
        let secret = b"TOP-SECRET-FILE-VALUE";
        let store = FileSecretStore::new(&path, [7_u8; MASTER_KEY_BYTES]).expect("open store");
        store.put(&key, secret).expect("write secret");
        let bytes = fs::read(&path).expect("read ciphertext");
        assert!(!bytes.windows(secret.len()).any(|window| window == secret));
        drop(store);
        let reopened = FileSecretStore::new(&path, [7_u8; MASTER_KEY_BYTES]).expect("reopen");
        let value = reopened.get(&key).expect("read secret");
        assert!(value.with_bytes(|bytes| bytes == secret));
        cleanup(&path);
    }

    #[test]
    fn malformed_and_tampered_files_are_rejected_without_detail() {
        let path = test_path("malformed");
        fs::write(&path, b"not-a-secret-store").expect("write malformed fixture");
        let error = FileSecretStore::new(&path, [1_u8; MASTER_KEY_BYTES])
            .expect_err("malformed file must fail");
        assert_eq!(error, SecretStoreError::Corrupt);
        assert!(!error.to_string().contains("not-a-secret-store"));
        cleanup(&path);

        let key = SecretKey::new("key").expect("valid key");
        let store = FileSecretStore::new(&path, [2_u8; MASTER_KEY_BYTES]).expect("open store");
        store.put(&key, b"tamper-me").expect("write secret");
        drop(store);
        let mut bytes = fs::read(&path).expect("read ciphertext");
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&path, bytes).expect("tamper fixture");
        let error = FileSecretStore::new(&path, [2_u8; MASTER_KEY_BYTES])
            .expect_err("tamper must fail authentication");
        assert_eq!(error, SecretStoreError::Corrupt);
        cleanup(&path);
    }

    #[test]
    fn wrong_key_is_indistinguishable_from_tamper() {
        let path = test_path("wrong-key");
        let key = SecretKey::new("key").expect("valid key");
        let store = FileSecretStore::new(&path, [3_u8; MASTER_KEY_BYTES]).expect("open store");
        store.put(&key, b"secret").expect("write secret");
        drop(store);
        let error = FileSecretStore::new(&path, [4_u8; MASTER_KEY_BYTES])
            .expect_err("wrong key must not decrypt");
        assert_eq!(error, SecretStoreError::Corrupt);
        cleanup(&path);
    }

    #[test]
    fn failed_update_rolls_back_without_overwriting_previous_snapshot() {
        let path = test_path("rollback");
        let key = SecretKey::new("key").expect("valid key");
        let store = FileSecretStore::new(&path, [5_u8; MASTER_KEY_BYTES]).expect("open store");
        store.put(&key, b"old-value").expect("write old value");
        let before = fs::read(&path).expect("read old snapshot");
        let oversized = vec![b'x'; crate::MAX_SECRET_VALUE_BYTES + 1];
        assert!(matches!(
            store.put(&key, oversized),
            Err(SecretError::TooLarge { .. })
        ));
        assert_eq!(fs::read(&path).expect("read unchanged snapshot"), before);
        let value = store.get(&key).expect("old value remains");
        assert!(value.with_bytes(|bytes| bytes == b"old-value"));
        cleanup(&path);
    }

    #[test]
    fn concurrent_updates_preserve_all_committed_values() {
        let path = test_path("concurrency");
        let store =
            Arc::new(FileSecretStore::new(&path, [6_u8; MASTER_KEY_BYTES]).expect("open store"));
        let handles = (0..8)
            .map(|index| {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    let key = SecretKey::new(format!("key-{index}")).expect("valid key");
                    store
                        .put(&key, format!("value-{index}"))
                        .expect("write value");
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("writer thread completes");
        }
        drop(store);
        let reopened = FileSecretStore::new(&path, [6_u8; MASTER_KEY_BYTES]).expect("reopen");
        for index in 0..8 {
            let key = SecretKey::new(format!("key-{index}")).expect("valid key");
            let expected = format!("value-{index}");
            let value = reopened.get(&key).expect("value was preserved");
            assert!(value.with_bytes(|bytes| bytes == expected.as_bytes()));
        }
        cleanup(&path);
    }
}
