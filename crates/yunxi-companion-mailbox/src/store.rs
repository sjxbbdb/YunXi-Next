//! Workspace-scoped encrypted mailbox storage.

use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use serde::{Deserialize, Serialize};
use yunxi_protocol::{
    MailboxEnqueueRequest, MailboxEntry, MailboxGetRequest, MailboxGetResult, MailboxItemKind,
    MailboxListRequest, MailboxListResult, MailboxMarkReadRequest, MailboxMutationResult,
    MailboxSummary, WorkspaceGrant,
};

const MAILBOX_SCHEMA_VERSION: u32 = 1;
const MAX_ITEM_FILE_BYTES: u64 = 256 * 1024;
const MAX_CONTENT_CHARS: usize = 64 * 1024;
const MAX_SUBJECT_CHARS: usize = 200;
const MAX_REASON_CHARS: usize = 500;
const MAX_IDEMPOTENCY_CHARS: usize = 256;
const MAX_SCANNED_ITEMS: usize = 2_000;
const MAX_LIST_LIMIT: usize = 200;
const NONCE_BYTES: usize = 12;

#[derive(Clone, Debug)]
pub struct MailboxStore {
    workspace_root: PathBuf,
    root: PathBuf,
    writable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct EncryptedItem {
    schema_version: u32,
    id: String,
    kind: MailboxItemKind,
    subject: String,
    reason: String,
    idempotency_key: String,
    read: bool,
    created_at_millis: u128,
    updated_at_millis: u128,
    algorithm: String,
    nonce: String,
    ciphertext: String,
}

impl EncryptedItem {
    fn summary(&self) -> MailboxSummary {
        MailboxSummary::new(
            self.id.clone(),
            self.kind,
            self.subject.clone(),
            self.reason.clone(),
            self.read,
            self.created_at_millis,
        )
    }

    fn aad(&self) -> Vec<u8> {
        format!(
            "yunxi-next-mailbox-v1|{}|{}",
            self.id, self.created_at_millis
        )
        .into_bytes()
    }

    fn validate(&self) -> Result<(), MailboxError> {
        if self.schema_version != MAILBOX_SCHEMA_VERSION {
            return Err(MailboxError::InvalidItem(format!(
                "unsupported mailbox schema_version={}",
                self.schema_version
            )));
        }
        validate_id(&self.id)?;
        validate_text("subject", &self.subject, MAX_SUBJECT_CHARS)?;
        validate_text("reason", &self.reason, MAX_REASON_CHARS)?;
        validate_text(
            "idempotency key",
            &self.idempotency_key,
            MAX_IDEMPOTENCY_CHARS,
        )?;
        if self.algorithm != "chacha20poly1305" {
            return Err(MailboxError::InvalidItem(
                "unsupported mailbox encryption algorithm".to_string(),
            ));
        }
        Ok(())
    }
}

impl MailboxStore {
    pub fn from_grant(grant: &WorkspaceGrant) -> Result<Self, MailboxError> {
        let workspace =
            fs::canonicalize(grant.root()).map_err(|source| MailboxError::Workspace {
                path: grant.root().to_path_buf(),
                source,
            })?;
        if !workspace.is_dir() {
            return Err(MailboxError::NotDirectory(workspace));
        }
        Ok(Self {
            workspace_root: workspace.clone(),
            root: workspace.join(".yunxi-next").join("mailbox"),
            writable: grant.allows_next_write(),
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn enqueue(
        &self,
        request: &MailboxEnqueueRequest,
    ) -> Result<MailboxMutationResult, MailboxError> {
        self.require_write()?;
        validate_text("subject", request.subject(), MAX_SUBJECT_CHARS)?;
        validate_text("content", request.content(), MAX_CONTENT_CHARS)?;
        validate_text("reason", request.reason(), MAX_REASON_CHARS)?;
        validate_text(
            "idempotency key",
            request.idempotency_key(),
            MAX_IDEMPOTENCY_CHARS,
        )?;
        let (items, warnings) = self.load_items()?;
        if let Some(existing) = items
            .iter()
            .find(|item| item.idempotency_key == request.idempotency_key())
        {
            return Ok(MailboxMutationResult::new(
                Some(existing.summary()),
                false,
                warnings,
            ));
        }
        fs::create_dir_all(&self.root).map_err(|source| MailboxError::Io {
            path: self.root.clone(),
            source,
        })?;
        let now = now_millis();
        let id = format!(
            "mail-{:016x}",
            stable_hash64(request.idempotency_key().as_bytes())
        );
        if let Some(existing) = items.iter().find(|item| item.id == id) {
            return Err(MailboxError::IdempotencyCollision {
                existing: existing.idempotency_key.clone(),
            });
        }
        let mut item = EncryptedItem {
            schema_version: MAILBOX_SCHEMA_VERSION,
            id,
            kind: request.kind(),
            subject: request.subject().to_string(),
            reason: request.reason().to_string(),
            idempotency_key: request.idempotency_key().to_string(),
            read: false,
            created_at_millis: now,
            updated_at_millis: now,
            algorithm: "chacha20poly1305".to_string(),
            nonce: String::new(),
            ciphertext: String::new(),
        };
        let key = self.load_or_create_key()?;
        let (nonce, ciphertext) = encrypt(&key, request.content().as_bytes(), &item.aad())?;
        item.nonce = STANDARD.encode(nonce);
        item.ciphertext = STANDARD.encode(ciphertext);
        item.validate()?;
        self.save_item(&item)?;
        Ok(MailboxMutationResult::new(
            Some(item.summary()),
            true,
            warnings,
        ))
    }

    pub fn list(&self, request: &MailboxListRequest) -> Result<MailboxListResult, MailboxError> {
        let (items, warnings) = self.load_items()?;
        let unread_count = items.iter().filter(|item| !item.read).count();
        let mut items = items
            .into_iter()
            .filter(|item| !request.only_unread() || !item.read)
            .collect::<Vec<_>>();
        items.sort_by(|left, right| {
            right
                .created_at_millis
                .cmp(&left.created_at_millis)
                .then_with(|| right.id.cmp(&left.id))
        });
        let limit = request.limit().clamp(1, MAX_LIST_LIMIT);
        let truncated = items.len() > limit;
        items.truncate(limit);
        Ok(MailboxListResult::new(
            items.iter().map(EncryptedItem::summary).collect(),
            unread_count,
            warnings,
            truncated,
        ))
    }

    pub fn get(&self, request: &MailboxGetRequest) -> Result<MailboxGetResult, MailboxError> {
        validate_id(request.item_id())?;
        let (items, warnings) = self.load_items()?;
        let Some(item) = items.into_iter().find(|item| item.id == request.item_id()) else {
            return Ok(MailboxGetResult::new(None, warnings));
        };
        let key = self.load_key()?;
        let nonce = STANDARD
            .decode(item.nonce.as_bytes())
            .map_err(|_| MailboxError::Crypto("mailbox nonce is invalid".to_string()))?;
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| MailboxError::Crypto("mailbox nonce length is invalid".to_string()))?;
        let ciphertext = STANDARD
            .decode(item.ciphertext.as_bytes())
            .map_err(|_| MailboxError::Crypto("mailbox ciphertext is invalid".to_string()))?;
        let plaintext = decrypt(&key, &nonce, &ciphertext, &item.aad())?;
        let content = String::from_utf8(plaintext)
            .map_err(|_| MailboxError::Crypto("mailbox content is not UTF-8".to_string()))?;
        Ok(MailboxGetResult::new(
            Some(MailboxEntry::new(item.summary(), content)),
            warnings,
        ))
    }

    pub fn mark_read(
        &self,
        request: &MailboxMarkReadRequest,
    ) -> Result<MailboxMutationResult, MailboxError> {
        self.require_write()?;
        validate_id(request.item_id())?;
        let (items, warnings) = self.load_items()?;
        let Some(mut item) = items.into_iter().find(|item| item.id == request.item_id()) else {
            return Ok(MailboxMutationResult::new(None, false, warnings));
        };
        item.read = request.read();
        item.updated_at_millis = now_millis();
        self.save_item(&item)?;
        Ok(MailboxMutationResult::new(
            Some(item.summary()),
            false,
            warnings,
        ))
    }

    fn require_write(&self) -> Result<(), MailboxError> {
        if self.writable {
            Ok(())
        } else {
            Err(MailboxError::WriteNotGranted)
        }
    }

    fn load_items(&self) -> Result<(Vec<EncryptedItem>, Vec<String>), MailboxError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), Vec::new()));
            }
            Err(source) => {
                return Err(MailboxError::Io {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        let mut items = Vec::new();
        let mut warnings = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= MAX_SCANNED_ITEMS {
                warnings.push(format!(
                    "mailbox scan stopped after {MAX_SCANNED_ITEMS} entries"
                ));
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!("failed to read mailbox entry: {error}"));
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match read_item(&path) {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {}
                Err(error) => warnings.push(error.to_string()),
            }
        }
        Ok((items, warnings))
    }

    fn save_item(&self, item: &EncryptedItem) -> Result<(), MailboxError> {
        item.validate()?;
        fs::create_dir_all(&self.root).map_err(|source| MailboxError::Io {
            path: self.root.clone(),
            source,
        })?;
        let content = serde_json::to_vec_pretty(item).map_err(MailboxError::Serialize)?;
        if content.len() as u64 > MAX_ITEM_FILE_BYTES {
            return Err(MailboxError::ItemTooLarge {
                maximum: MAX_ITEM_FILE_BYTES,
            });
        }
        replace_file(&self.root.join(format!("{}.json", item.id)), &content)
    }

    fn key_path(&self) -> PathBuf {
        self.root.join("data-key.hex")
    }

    fn load_key(&self) -> Result<[u8; 32], MailboxError> {
        for name in ["YUNXI_NEXT_MAILBOX_KEY_HEX", "YUNXI_MAILBOX_KEY_HEX"] {
            if let Some(value) = std::env::var_os(name) {
                return decode_hex_key(&value.to_string_lossy());
            }
        }
        let path = self.key_path();
        let value = fs::read_to_string(&path).map_err(|source| MailboxError::Io {
            path: path.clone(),
            source,
        })?;
        decode_hex_key(value.trim())
    }

    fn load_or_create_key(&self) -> Result<[u8; 32], MailboxError> {
        match self.load_key() {
            Ok(key) => return Ok(key),
            Err(MailboxError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::create_dir_all(&self.root).map_err(|source| MailboxError::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut key = [0_u8; 32];
        getrandom::fill(&mut key)
            .map_err(|error| MailboxError::Crypto(format!("key generation failed: {error}")))?;
        let encoded = encode_hex_key(&key);
        let path = self.key_path();
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(encoded.as_bytes())
                    .and_then(|_| file.sync_all())
                    .map_err(|source| MailboxError::Io {
                        path: path.clone(),
                        source,
                    })?;
                Ok(key)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => self.load_key(),
            Err(source) => Err(MailboxError::Io { path, source }),
        }
    }
}

fn read_item(path: &Path) -> Result<Option<EncryptedItem>, MailboxError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MailboxError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.len() > MAX_ITEM_FILE_BYTES {
        return Err(MailboxError::ItemTooLarge {
            maximum: MAX_ITEM_FILE_BYTES,
        });
    }
    let content = fs::read(path).map_err(|source| MailboxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let item = serde_json::from_slice::<EncryptedItem>(&content).map_err(|error| {
        MailboxError::InvalidItem(format!("failed to parse {}: {error}", path.display()))
    })?;
    item.validate()?;
    Ok(Some(item))
}

fn encrypt(
    key: &[u8; 32],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<([u8; NONCE_BYTES], Vec<u8>), MailboxError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| MailboxError::Crypto("mailbox key is invalid".to_string()))?;
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut nonce)
        .map_err(|error| MailboxError::Crypto(format!("nonce generation failed: {error}")))?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| MailboxError::Crypto("mailbox encryption failed".to_string()))?;
    Ok((nonce, ciphertext))
}

fn decrypt(
    key: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, MailboxError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| MailboxError::Crypto("mailbox key is invalid".to_string()))?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| MailboxError::Crypto("mailbox authentication failed".to_string()))
}

fn validate_text(name: &str, value: &str, maximum: usize) -> Result<(), MailboxError> {
    let length = value.chars().count();
    if value.trim().is_empty() || length > maximum {
        return Err(MailboxError::InvalidItem(format!(
            "{name} must contain 1 to {maximum} characters"
        )));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), MailboxError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(MailboxError::InvalidItem(
            "mailbox item id is invalid".to_string(),
        ));
    }
    Ok(())
}

fn decode_hex_key(value: &str) -> Result<[u8; 32], MailboxError> {
    if value.len() != 64 {
        return Err(MailboxError::Crypto(
            "mailbox key must contain 64 hex characters".to_string(),
        ));
    }
    let mut key = [0_u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| MailboxError::Crypto("mailbox key contains invalid hex".to_string()))?;
    }
    Ok(key)
}

fn encode_hex_key(key: &[u8; 32]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn stable_hash64(value: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn replace_file(target: &Path, content: &[u8]) -> Result<(), MailboxError> {
    let parent = target
        .parent()
        .ok_or_else(|| MailboxError::InvalidItem("mailbox item path has no parent".to_string()))?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".mailbox-{}-{unique}.tmp", process::id()));
    let backup = target.with_extension("json.bak");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| MailboxError::Io {
            path: temporary.clone(),
            source,
        })?;
    if let Err(source) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ignored = fs::remove_file(&temporary);
        return Err(MailboxError::Io {
            path: temporary,
            source,
        });
    }
    drop(file);
    if target.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|source| MailboxError::Io {
                path: backup.clone(),
                source,
            })?;
        }
        fs::rename(target, &backup).map_err(|source| MailboxError::Io {
            path: target.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(&temporary, target) {
        if backup.exists() {
            let _ignored = fs::rename(&backup, target);
        }
        let _ignored = fs::remove_file(&temporary);
        return Err(MailboxError::Io {
            path: target.to_path_buf(),
            source,
        });
    }
    if backup.exists() {
        let _ignored = fs::remove_file(backup);
    }
    Ok(())
}

#[derive(Debug)]
pub enum MailboxError {
    Workspace {
        path: PathBuf,
        source: std::io::Error,
    },
    NotDirectory(PathBuf),
    WriteNotGranted,
    InvalidItem(String),
    ItemTooLarge {
        maximum: u64,
    },
    IdempotencyCollision {
        existing: String,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Serialize(serde_json::Error),
    Crypto(String),
}

impl fmt::Display for MailboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace { path, source } => {
                write!(
                    formatter,
                    "failed to resolve workspace {}: {source}",
                    path.display()
                )
            }
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "workspace is not a directory: {}",
                    path.display()
                )
            }
            Self::WriteNotGranted => formatter.write_str("mailbox write access was not granted"),
            Self::InvalidItem(message) => formatter.write_str(message),
            Self::ItemTooLarge { maximum } => {
                write!(formatter, "mailbox item exceeds {maximum} bytes")
            }
            Self::IdempotencyCollision { existing } => write!(
                formatter,
                "mailbox idempotency hash collided with existing key `{existing}`"
            ),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "mailbox I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::Serialize(error) => write!(formatter, "mailbox serialization failed: {error}"),
            Self::Crypto(message) => formatter.write_str(message),
        }
    }
}

impl Error for MailboxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Workspace { source, .. } | Self::Io { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
            Self::NotDirectory(_)
            | Self::WriteNotGranted
            | Self::InvalidItem(_)
            | Self::ItemTooLarge { .. }
            | Self::IdempotencyCollision { .. }
            | Self::Crypto(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_content_round_trips_and_idempotency_prevents_duplicates() {
        let root = test_root();
        let grant = WorkspaceGrant::read_write(&root);
        let store = MailboxStore::from_grant(&grant).expect("create store");
        let request = MailboxEnqueueRequest::new(
            grant.clone(),
            MailboxItemKind::ProactiveMessage,
            "Follow-up",
            "private mailbox body",
            "unfinished task",
            "task-1",
        );
        let first = store.enqueue(&request).expect("enqueue item");
        let second = store.enqueue(&request).expect("deduplicate item");
        assert!(first.created());
        assert!(!second.created());
        let id = first.item().expect("mailbox item").id();
        let raw = fs::read_to_string(root.join(format!(".yunxi-next/mailbox/{id}.json")))
            .expect("read encrypted item");
        assert!(!raw.contains("private mailbox body"));
        let entry = store
            .get(&MailboxGetRequest::new(grant, id))
            .expect("get item")
            .entry()
            .expect("mailbox entry")
            .content()
            .to_string();
        assert_eq!(entry, "private mailbox body");
        fs::remove_dir_all(root).expect("remove test root");
    }

    fn test_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yunxi-mailbox-{}-{unique}", process::id()));
        fs::create_dir_all(&root).expect("create test root");
        root
    }
}
