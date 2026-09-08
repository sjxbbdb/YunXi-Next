//! Bounded event carriers and the optional durable replay journal.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yunxi_web_contract::{
    EventChannel, MAX_FRAME_BYTES, RpcId, RpcMessage, WebContractError, event_message,
    parse_event_message,
};

use crate::GatewayError;

pub const MAX_PENDING_EVENTS: usize = 256;
pub const MAX_REPLAY_BYTES: usize = 512 * 1024;
/// Maximum size of an on-disk event log before an atomic rewrite is required.
pub const MAX_PERSISTED_EVENT_LOG_BYTES: u64 = 8 * 1024 * 1024;
/// A persisted JSONL frame may contain one bounded Web RPC frame plus its envelope.
pub const MAX_PERSISTED_EVENT_RECORD_BYTES: usize = MAX_FRAME_BYTES + 1024;

#[derive(Debug)]
pub enum EventJournalError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    InvalidRecord {
        line: usize,
        message: String,
    },
    FileTooLarge {
        length: u64,
        maximum: u64,
    },
    RecordTooLarge {
        length: usize,
        maximum: usize,
    },
    Contract(WebContractError),
}

impl fmt::Display for EventJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} event journal {path:?}: {source}"
            ),
            Self::InvalidRecord { line, message } => {
                write!(
                    formatter,
                    "invalid event journal record at line {line}: {message}"
                )
            }
            Self::FileTooLarge { length, maximum } => write!(
                formatter,
                "event journal is {length} bytes; maximum supported size is {maximum}"
            ),
            Self::RecordTooLarge { length, maximum } => write!(
                formatter,
                "event journal record is {length} bytes; maximum is {maximum}"
            ),
            Self::Contract(error) => {
                write!(formatter, "event journal rejected a Web event: {error}")
            }
        }
    }
}

impl Error for EventJournalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Contract(error) => Some(error),
            Self::InvalidRecord { .. }
            | Self::FileTooLarge { .. }
            | Self::RecordTooLarge { .. } => None,
        }
    }
}

impl From<WebContractError> for EventJournalError {
    fn from(error: WebContractError) -> Self {
        Self::Contract(error)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EventRecord {
    pub(crate) sequence: u64,
    pub(crate) message: RpcMessage,
    pub(crate) encoded_bytes: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ReplayPage {
    pub(crate) after_sequence: u64,
    pub(crate) oldest_sequence: u64,
    pub(crate) latest_sequence: u64,
    pub(crate) events: Vec<EventRecord>,
    pub(crate) has_more: bool,
    pub(crate) replay_gap: Option<ReplayGap>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReplayGap {
    pub(crate) after_sequence: u64,
    pub(crate) oldest_sequence: u64,
    pub(crate) latest_sequence: u64,
}

#[derive(Debug)]
struct PersistedJournal {
    path: PathBuf,
    file: File,
    length: u64,
}

#[derive(Deserialize, Serialize)]
struct PersistedRecord {
    version: u8,
    channel: String,
    sequence: u64,
    message: RpcMessage,
}

/// A bounded per-channel replay log with an optional crash-tolerant JSONL adapter.
///
/// `new` retains the historical in-memory behavior. `open` or `attach_path`
/// enables durable replay. The file is appended and synced after every ingest;
/// a malformed final partial line is truncated on load, while a complete bad
/// line is reported. When the file reaches `MAX_PERSISTED_EVENT_LOG_BYTES`, it
/// is atomically rewritten from the currently retained in-memory window.
#[derive(Debug)]
pub struct EventJournal {
    mux: VecDeque<EventRecord>,
    host: VecDeque<EventRecord>,
    mux_bytes: usize,
    host_bytes: usize,
    next_mux_sequence: u64,
    next_host_sequence: u64,
    persistence: Option<PersistedJournal>,
}

impl EventJournal {
    pub fn new() -> Self {
        Self {
            mux: VecDeque::new(),
            host: VecDeque::new(),
            mux_bytes: 0,
            host_bytes: 0,
            next_mux_sequence: 1,
            next_host_sequence: 1,
            persistence: None,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, EventJournalError> {
        let path = path.as_ref().to_path_buf();
        let (records, repair_to, add_newline, needs_redaction_rewrite) = load_records(&path)?;
        let mut journal = Self::new();
        for (channel, sequence, message) in records {
            journal.push_loaded(channel, sequence, message)?;
        }
        let mut persistence = PersistedJournal::open(path.clone())?;
        if let Some(valid_end) = repair_to {
            persistence.repair_tail(valid_end, add_newline)?;
        }
        journal.persistence = Some(persistence);
        if needs_redaction_rewrite {
            journal.compact_persisted()?;
        }
        Ok(journal)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, EventJournalError> {
        Self::open(path)
    }

    /// Attach a file and replace the in-memory window with its validated contents.
    /// Attaching the same path repeatedly is idempotent and never duplicates events.
    pub fn attach_path(&mut self, path: impl AsRef<Path>) -> Result<(), EventJournalError> {
        let path = path.as_ref().to_path_buf();
        if self
            .persistence
            .as_ref()
            .is_some_and(|current| current.path == path)
        {
            return Ok(());
        }
        *self = Self::open(path)?;
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.persistence
            .as_ref()
            .map(|persistence| persistence.path.as_path())
    }

    pub(crate) fn latest_sequence(&self, channel: EventChannel) -> u64 {
        self.next_sequence(channel).saturating_sub(1)
    }

    pub(crate) fn ingest(
        &mut self,
        channel: EventChannel,
        messages: Vec<RpcMessage>,
    ) -> Result<(), GatewayError> {
        // Validate and encode the complete batch before changing memory or disk.
        let mut encoded_messages = Vec::with_capacity(messages.len());
        for message in messages {
            let (message_channel, rpc_id, payload) = parse_event_message(&message)?;
            if message_channel != channel {
                return Err(GatewayError::EventJournal(
                    EventJournalError::InvalidRecord {
                        line: 0,
                        message: format!(
                            "event belongs to `{}` but was ingested into `{}`",
                            message_channel.method(),
                            channel.method()
                        ),
                    },
                ));
            }
            let encoded_bytes = message.encode()?.len();
            let persisted_message = redacted_event_message(channel, rpc_id, payload)?;
            encoded_messages.push((message, persisted_message, encoded_bytes));
        }
        if !encoded_messages.is_empty() {
            let count = u64::try_from(encoded_messages.len()).unwrap_or(u64::MAX);
            if self.next_sequence(channel).checked_add(count).is_none() {
                return Err(GatewayError::EventSequenceExhausted { channel });
            }
        }

        let persisted = encoded_messages
            .iter()
            .enumerate()
            .map(|(index, (_, message, _))| {
                let sequence = self
                    .next_sequence(channel)
                    .checked_add(u64::try_from(index).unwrap_or(u64::MAX))
                    .ok_or(GatewayError::EventSequenceExhausted { channel })?;
                Ok(PersistedRecord {
                    version: 1,
                    channel: channel.method().to_string(),
                    sequence,
                    message: message.clone(),
                })
            })
            .collect::<Result<Vec<_>, GatewayError>>()?;
        let retained = self.retained_snapshot()?;
        if let Some(persistence) = self.persistence.as_mut() {
            persistence.append(&persisted, &retained)?;
        }

        for (message, _, encoded_bytes) in encoded_messages {
            let sequence = self.allocate_sequence(channel)?;
            self.queue_mut(channel).push_back(EventRecord {
                sequence,
                message,
                encoded_bytes,
            });
            *self.bytes_mut(channel) = self.bytes(channel).saturating_add(encoded_bytes);
            self.trim(channel);
        }
        Ok(())
    }

    pub(crate) fn page_after(
        &self,
        channel: EventChannel,
        after_sequence: u64,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> ReplayPage {
        let queue = self.queue(channel);
        let latest_sequence = self.latest_sequence(channel);
        let oldest_sequence = queue.front().map_or(0, |record| record.sequence);
        let replay_gap = match queue.front() {
            Some(record) if after_sequence.saturating_add(1) < record.sequence => {
                Some(record.sequence)
            }
            Some(_) => None,
            None if latest_sequence > after_sequence => Some(latest_sequence.saturating_add(1)),
            None => None,
        }
        .map(|oldest_sequence| ReplayGap {
            after_sequence,
            oldest_sequence,
            latest_sequence,
        });
        let page_limit = maximum_events.saturating_sub(usize::from(replay_gap.is_some()));
        let mut events = Vec::new();
        let mut encoded_bytes = 0usize;
        for record in queue
            .iter()
            .filter(|record| record.sequence > after_sequence)
        {
            if events.len() >= page_limit {
                break;
            }
            let Some(next_bytes) = encoded_bytes.checked_add(record.encoded_bytes) else {
                break;
            };
            if next_bytes > maximum_encoded_bytes {
                break;
            }
            encoded_bytes = next_bytes;
            events.push(record.clone());
        }
        let last_sequence = events.last().map_or(after_sequence, |last| last.sequence);
        ReplayPage {
            after_sequence,
            oldest_sequence,
            latest_sequence,
            events,
            has_more: queue.iter().any(|record| record.sequence > last_sequence),
            replay_gap,
        }
    }

    fn push_loaded(
        &mut self,
        channel: EventChannel,
        sequence: u64,
        message: RpcMessage,
    ) -> Result<(), EventJournalError> {
        if sequence == 0 || sequence == u64::MAX {
            return Err(EventJournalError::InvalidRecord {
                line: 0,
                message: "sequence must be in the range 1..u64::MAX".to_string(),
            });
        }
        let encoded_bytes = message.encode()?.len();
        let next = self.next_sequence(channel);
        if next != 1 && sequence < next.saturating_sub(1) {
            return Err(EventJournalError::InvalidRecord {
                line: 0,
                message: format!("sequence {sequence} is not monotonic after {}", next - 1),
            });
        }
        if sequence >= next {
            *self.next_sequence_mut(channel) = sequence + 1;
        }
        self.queue_mut(channel).push_back(EventRecord {
            sequence,
            message,
            encoded_bytes,
        });
        *self.bytes_mut(channel) = self.bytes(channel).saturating_add(encoded_bytes);
        self.trim(channel);
        Ok(())
    }

    fn retained_snapshot(&self) -> Result<Vec<PersistedRecord>, GatewayError> {
        let mut records = Vec::with_capacity(self.mux.len() + self.host.len());
        for (channel, queue) in [
            (EventChannel::Mux, &self.mux),
            (EventChannel::Host, &self.host),
        ] {
            for record in queue {
                let (_, rpc_id, payload) = parse_event_message(&record.message)?;
                records.push(PersistedRecord {
                    version: 1,
                    channel: channel.method().to_string(),
                    sequence: record.sequence,
                    message: redacted_event_message(channel, rpc_id, payload)?,
                });
            }
        }
        Ok(records)
    }

    fn compact_persisted(&mut self) -> Result<(), EventJournalError> {
        let records =
            self.retained_snapshot()
                .map_err(|error| EventJournalError::InvalidRecord {
                    line: 0,
                    message: error.to_string(),
                })?;
        if let Some(persistence) = self.persistence.as_mut() {
            persistence.compact(&records)?;
        }
        Ok(())
    }

    fn trim(&mut self, channel: EventChannel) {
        while self.queue(channel).len() > MAX_PENDING_EVENTS
            || self.bytes(channel) > MAX_REPLAY_BYTES
        {
            let Some(record) = self.queue_mut(channel).pop_front() else {
                break;
            };
            *self.bytes_mut(channel) = self.bytes(channel).saturating_sub(record.encoded_bytes);
        }
    }

    fn allocate_sequence(&mut self, channel: EventChannel) -> Result<u64, GatewayError> {
        let next = self.next_sequence_mut(channel);
        let sequence = *next;
        let Some(next_sequence) = sequence.checked_add(1) else {
            return Err(GatewayError::EventSequenceExhausted { channel });
        };
        *next = next_sequence;
        Ok(sequence)
    }

    fn next_sequence(&self, channel: EventChannel) -> u64 {
        match channel {
            EventChannel::Mux => self.next_mux_sequence,
            EventChannel::Host => self.next_host_sequence,
        }
    }

    fn next_sequence_mut(&mut self, channel: EventChannel) -> &mut u64 {
        match channel {
            EventChannel::Mux => &mut self.next_mux_sequence,
            EventChannel::Host => &mut self.next_host_sequence,
        }
    }

    fn queue(&self, channel: EventChannel) -> &VecDeque<EventRecord> {
        match channel {
            EventChannel::Mux => &self.mux,
            EventChannel::Host => &self.host,
        }
    }

    fn queue_mut(&mut self, channel: EventChannel) -> &mut VecDeque<EventRecord> {
        match channel {
            EventChannel::Mux => &mut self.mux,
            EventChannel::Host => &mut self.host,
        }
    }

    fn bytes(&self, channel: EventChannel) -> usize {
        match channel {
            EventChannel::Mux => self.mux_bytes,
            EventChannel::Host => self.host_bytes,
        }
    }

    fn bytes_mut(&mut self, channel: EventChannel) -> &mut usize {
        match channel {
            EventChannel::Mux => &mut self.mux_bytes,
            EventChannel::Host => &mut self.host_bytes,
        }
    }
}

impl Default for EventJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl PersistedJournal {
    fn open(path: PathBuf) -> Result<Self, EventJournalError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .map_err(|source| EventJournalError::Io {
                operation: "open",
                path: path.clone(),
                source,
            })?;
        let length = file
            .metadata()
            .map_err(|source| EventJournalError::Io {
                operation: "stat",
                path: path.clone(),
                source,
            })?
            .len();
        Ok(Self { path, file, length })
    }

    fn append(
        &mut self,
        records: &[PersistedRecord],
        retained: &[PersistedRecord],
    ) -> Result<(), EventJournalError> {
        let mut encoded = Vec::new();
        for record in records {
            let line = serde_json::to_vec(record).map_err(|source| EventJournalError::Io {
                operation: "encode",
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, source),
            })?;
            if line.len() + 1 > MAX_PERSISTED_EVENT_RECORD_BYTES {
                return Err(EventJournalError::RecordTooLarge {
                    length: line.len() + 1,
                    maximum: MAX_PERSISTED_EVENT_RECORD_BYTES,
                });
            }
            encoded.extend_from_slice(&line);
            encoded.push(b'\n');
        }
        let encoded_len = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        if encoded_len > MAX_PERSISTED_EVENT_LOG_BYTES {
            return Err(EventJournalError::FileTooLarge {
                length: encoded_len,
                maximum: MAX_PERSISTED_EVENT_LOG_BYTES,
            });
        }
        if self.length.saturating_add(encoded_len) > MAX_PERSISTED_EVENT_LOG_BYTES {
            self.compact(retained)?;
        }
        if self.length.saturating_add(encoded_len) > MAX_PERSISTED_EVENT_LOG_BYTES {
            return Err(EventJournalError::FileTooLarge {
                length: self.length.saturating_add(encoded_len),
                maximum: MAX_PERSISTED_EVENT_LOG_BYTES,
            });
        }
        self.file
            .write_all(&encoded)
            .and_then(|_| self.file.sync_data())
            .map_err(|source| EventJournalError::Io {
                operation: "append and flush",
                path: self.path.clone(),
                source,
            })?;
        self.length = self.length.saturating_add(encoded_len);
        Ok(())
    }

    fn repair_tail(
        &mut self,
        valid_end: usize,
        add_newline: bool,
    ) -> Result<(), EventJournalError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|source| EventJournalError::Io {
                operation: "open for tail repair",
                path: self.path.clone(),
                source,
            })?;
        file.set_len(u64::try_from(valid_end).unwrap_or(u64::MAX))
            .and_then(|_| {
                if add_newline {
                    file.seek(std::io::SeekFrom::End(0))?;
                    file.write_all(b"\n")?;
                }
                file.sync_all()
            })
            .map_err(|source| EventJournalError::Io {
                operation: "repair and flush",
                path: self.path.clone(),
                source,
            })?;
        self.length = u64::try_from(valid_end).unwrap_or(u64::MAX) + u64::from(add_newline);
        Ok(())
    }

    fn compact(&mut self, records: &[PersistedRecord]) -> Result<(), EventJournalError> {
        let mut encoded = Vec::new();
        for record in records {
            let line = serde_json::to_vec(record).map_err(|source| EventJournalError::Io {
                operation: "encode compacted",
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, source),
            })?;
            if line.len() + 1 > MAX_PERSISTED_EVENT_RECORD_BYTES {
                return Err(EventJournalError::RecordTooLarge {
                    length: line.len() + 1,
                    maximum: MAX_PERSISTED_EVENT_RECORD_BYTES,
                });
            }
            encoded.extend_from_slice(&line);
            encoded.push(b'\n');
        }
        atomic_replace(&self.path, &encoded)?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)
            .map_err(|source| EventJournalError::Io {
                operation: "reopen compacted",
                path: self.path.clone(),
                source,
            })?;
        self.length = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        Ok(())
    }
}

type LoadedJournalRecords = (
    Vec<(EventChannel, u64, RpcMessage)>,
    Option<usize>,
    bool,
    bool,
);

fn load_records(path: &Path) -> Result<LoadedJournalRecords, EventJournalError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(EventJournalError::Io {
                operation: "read",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if length > MAX_PERSISTED_EVENT_LOG_BYTES {
        return Err(EventJournalError::FileTooLarge {
            length,
            maximum: MAX_PERSISTED_EVENT_LOG_BYTES,
        });
    }
    let mut records = Vec::new();
    let mut last_sequence = [0_u64; 2];
    let mut offset = 0usize;
    let mut repair_to = None;
    let mut add_newline = false;
    let mut needs_redaction_rewrite = false;
    for (line_index, segment) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let has_newline = segment.ends_with(b"\n");
        let content = segment.strip_suffix(b"\n").unwrap_or(segment);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        let is_last = offset + segment.len() == bytes.len();
        if content.is_empty() {
            if is_last && has_newline {
                offset += segment.len();
                continue;
            }
            return Err(EventJournalError::InvalidRecord {
                line: line_index + 1,
                message: "empty JSONL record".to_string(),
            });
        }
        if content.len() > MAX_PERSISTED_EVENT_RECORD_BYTES {
            if is_last && !has_newline {
                repair_to = Some(offset);
                break;
            }
            return Err(EventJournalError::RecordTooLarge {
                length: content.len(),
                maximum: MAX_PERSISTED_EVENT_RECORD_BYTES,
            });
        }
        let parsed = std::str::from_utf8(content)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                serde_json::from_str::<PersistedRecord>(text).map_err(|error| error.to_string())
            });
        let record = match parsed {
            Ok(record) => record,
            Err(_message) if is_last && !has_newline => {
                repair_to = Some(offset);
                break;
            }
            Err(message) => {
                return Err(EventJournalError::InvalidRecord {
                    line: line_index + 1,
                    message,
                });
            }
        };
        if record.version != 1 {
            return Err(EventJournalError::InvalidRecord {
                line: line_index + 1,
                message: format!("unsupported journal version {}", record.version),
            });
        }
        let channel = match record.channel.as_str() {
            "events.mux" => EventChannel::Mux,
            "events.host" => EventChannel::Host,
            _ => {
                return Err(EventJournalError::InvalidRecord {
                    line: line_index + 1,
                    message: format!("invalid event channel `{}`", record.channel),
                });
            }
        };
        let slot = match channel {
            EventChannel::Mux => 0,
            EventChannel::Host => 1,
        };
        if record.sequence == 0 || record.sequence <= last_sequence[slot] {
            return Err(EventJournalError::InvalidRecord {
                line: line_index + 1,
                message: format!(
                    "sequence {} is not strictly increasing after {}",
                    record.sequence, last_sequence[slot]
                ),
            });
        }
        let (message_channel, rpc_id, payload) =
            parse_event_message(&record.message).map_err(|error| {
                EventJournalError::InvalidRecord {
                    line: line_index + 1,
                    message: error.to_string(),
                }
            })?;
        if message_channel != channel {
            return Err(EventJournalError::InvalidRecord {
                line: line_index + 1,
                message: "record channel does not match message method".to_string(),
            });
        }
        let redacted = redacted_event_message(channel, rpc_id, payload)?;
        needs_redaction_rewrite |= redacted != record.message;
        last_sequence[slot] = record.sequence;
        records.push((channel, record.sequence, redacted));
        offset += segment.len();
    }
    if offset < bytes.len() {
        repair_to = Some(offset);
    } else if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        // A valid final line without a newline is safe, but normalize it so
        // the next append cannot merge two JSON objects.
        repair_to = Some(bytes.len());
        add_newline = true;
    }
    Ok((records, repair_to, add_newline, needs_redaction_rewrite))
}

fn redacted_event_message(
    channel: EventChannel,
    rpc_id: &RpcId,
    payload: &Value,
) -> Result<RpcMessage, EventJournalError> {
    let mut payload = payload.clone();
    redact_value(None, &mut payload);
    Ok(event_message(channel, rpc_id.clone(), payload)?)
}

fn redact_value(key: Option<&str>, value: &mut Value) {
    if key.is_some_and(is_sensitive_key) {
        *value = Value::String("[REDACTED]".to_string());
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                redact_value(None, item);
            }
        }
        Value::Object(fields) => {
            for (field, value) in fields {
                redact_value(Some(field), value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.ends_with("token")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.starts_with("audio")
        || normalized.contains("pcm")
        || normalized.contains("sample")
        || matches!(
            normalized.as_str(),
            "apikey"
                | "accesskey"
                | "authorization"
                | "cookie"
                | "credential"
                | "credentials"
                | "idtoken"
                | "password"
                | "privatekey"
                | "refreshtoken"
                | "secret"
                | "secrets"
                | "token"
                | "audio"
                | "audiobase64"
                | "pcm"
                | "wav"
                | "mp3"
                | "opus"
                | "samples"
        )
}

fn atomic_replace(path: &Path, content: &[u8]) -> Result<(), EventJournalError> {
    let temporary = path.with_extension(format!("journal-tmp-{}", std::process::id()));
    let backup = path.with_extension(format!("journal-bak-{}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| EventJournalError::Io {
                operation: "create compaction file",
                path: temporary.clone(),
                source,
            })?;
        file.write_all(content)
            .and_then(|_| file.sync_all())
            .map_err(|source| EventJournalError::Io {
                operation: "write compaction file",
                path: temporary.clone(),
                source,
            })?;
        if path.exists() {
            fs::rename(path, &backup).map_err(|source| EventJournalError::Io {
                operation: "backup event journal",
                path: path.to_path_buf(),
                source,
            })?;
        }
        if let Err(source) = fs::rename(&temporary, path) {
            if backup.exists() {
                let _ = fs::rename(&backup, path);
            }
            return Err(EventJournalError::Io {
                operation: "install compacted event journal",
                path: path.to_path_buf(),
                source,
            });
        }
        let _ = fs::remove_file(backup);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Debug)]
pub(crate) struct EventBuffer {
    mux: VecDeque<RpcMessage>,
    host: VecDeque<RpcMessage>,
    next_id: u64,
}

impl EventBuffer {
    pub(crate) fn new() -> Self {
        Self {
            mux: VecDeque::new(),
            host: VecDeque::new(),
            next_id: 1,
        }
    }

    pub(crate) fn publish(
        &mut self,
        channel: EventChannel,
        payload: Value,
    ) -> Result<(), GatewayError> {
        let rpc_id = RpcId::new(format!("event-{}", self.next_id))?;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.publish_with_id(channel, rpc_id, payload)
    }

    pub(crate) fn publish_with_id(
        &mut self,
        channel: EventChannel,
        rpc_id: RpcId,
        payload: Value,
    ) -> Result<(), GatewayError> {
        let message = event_message(channel, rpc_id, payload)?;
        if self.queue(channel).len() >= MAX_PENDING_EVENTS {
            return Err(GatewayError::EventQueueFull {
                channel,
                capacity: MAX_PENDING_EVENTS,
            });
        }
        self.queue(channel).push_back(message);
        Ok(())
    }

    pub(crate) fn drain(&mut self, channel: EventChannel) -> Vec<RpcMessage> {
        self.queue(channel).drain(..).collect()
    }

    pub(crate) fn has_events(&self, channel: EventChannel) -> bool {
        match channel {
            EventChannel::Mux => !self.mux.is_empty(),
            EventChannel::Host => !self.host.is_empty(),
        }
    }

    pub(crate) fn take_bounded(
        &mut self,
        channel: EventChannel,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> Result<Vec<RpcMessage>, GatewayError> {
        let mut events = Vec::new();
        let mut encoded_bytes = 0usize;
        while events.len() < maximum_events {
            let Some(next_bytes) = self
                .queue(channel)
                .front()
                .map(RpcMessage::encode)
                .transpose()?
            else {
                break;
            };
            let Some(next_total) = encoded_bytes.checked_add(next_bytes.len()) else {
                break;
            };
            if next_total > maximum_encoded_bytes {
                break;
            }
            let Some(message) = self.queue(channel).pop_front() else {
                break;
            };
            encoded_bytes = next_total;
            events.push(message);
        }
        Ok(events)
    }

    fn queue(&mut self, channel: EventChannel) -> &mut VecDeque<RpcMessage> {
        match channel {
            EventChannel::Mux => &mut self.mux,
            EventChannel::Host => &mut self.host,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn event(channel: EventChannel, id: &str, payload: Value) -> RpcMessage {
        event_message(channel, RpcId::new(id).expect("rpc id"), payload).expect("event")
    }

    fn temp_path(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "yunxi-web-gateway-{name}-{}-{stamp}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn journal_sequence_allocation_does_not_wrap() {
        let mut journal = EventJournal::new();
        journal.next_host_sequence = u64::MAX;
        let error = journal
            .ingest(
                EventChannel::Host,
                vec![event(
                    EventChannel::Host,
                    "event",
                    json!({ "type": "host/event" }),
                )],
            )
            .expect_err("sequence exhaustion must be explicit");
        assert!(matches!(
            error,
            GatewayError::EventSequenceExhausted {
                channel: EventChannel::Host
            }
        ));
        assert_eq!(journal.latest_sequence(EventChannel::Host), u64::MAX - 1);
    }

    #[test]
    fn durable_journal_replays_after_reopen_and_attach_is_idempotent() {
        let path = temp_path("reopen");
        let mut journal = EventJournal::open(&path).expect("open journal");
        journal
            .ingest(
                EventChannel::Host,
                vec![event(EventChannel::Host, "one", json!({ "n": 1 }))],
            )
            .expect("append");
        let mut reopened = EventJournal::from_path(&path).expect("reopen journal");
        assert_eq!(reopened.latest_sequence(EventChannel::Host), 1);
        assert_eq!(
            reopened
                .page_after(EventChannel::Host, 0, 10, 4096)
                .events
                .len(),
            1
        );
        reopened.attach_path(&path).expect("idempotent attach");
        assert_eq!(reopened.latest_sequence(EventChannel::Host), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn incomplete_tail_is_truncated_but_bad_middle_record_is_reported() {
        let tail_path = temp_path("tail");
        let message = event(EventChannel::Host, "one", json!({ "n": 1 }));
        let record = PersistedRecord {
            version: 1,
            channel: EventChannel::Host.method().to_string(),
            sequence: 1,
            message,
        };
        let mut encoded = serde_json::to_vec(&record).expect("record");
        encoded.push(b'\n');
        encoded.extend_from_slice(b"{\"version\":1,\"channel\":\"events.host\"");
        fs::write(&tail_path, encoded).expect("write tail");
        let reopened = EventJournal::open(&tail_path).expect("recover tail");
        assert_eq!(reopened.latest_sequence(EventChannel::Host), 1);
        let bytes = fs::read(&tail_path).expect("read repaired tail");
        assert!(bytes.ends_with(b"\n"));
        let _ = fs::remove_file(tail_path);

        let middle_path = temp_path("middle");
        let valid = serde_json::to_vec(&record).expect("record");
        fs::write(
            &middle_path,
            [valid.as_slice(), b"\n{bad}\n", valid.as_slice(), b"\n"].concat(),
        )
        .expect("write middle");
        let error = EventJournal::open(&middle_path).expect_err("bad middle must fail");
        assert!(matches!(
            error,
            EventJournalError::InvalidRecord { line: 2, .. }
        ));
        let _ = fs::remove_file(middle_path);
    }

    #[test]
    fn persisted_records_redact_credentials_and_audio() {
        let path = temp_path("redaction");
        let mut journal = EventJournal::open(&path).expect("open journal");
        journal
            .ingest(
                EventChannel::Mux,
                vec![event(
                    EventChannel::Mux,
                    "redact",
                    json!({ "token": "secret", "audio": "base64" }),
                )],
            )
            .expect("append");
        let text = fs::read_to_string(&path).expect("read journal");
        assert!(!text.contains("secret"));
        assert!(!text.contains("base64"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn durable_log_compacts_at_capacity_without_resetting_sequence() {
        let path = temp_path("capacity");
        let mut journal = EventJournal::open(&path).expect("open journal");
        let payload = "x".repeat(64 * 1024);
        let iterations = (MAX_PERSISTED_EVENT_LOG_BYTES / 65_000) as usize + 8;
        for index in 0..iterations {
            journal
                .ingest(
                    EventChannel::Host,
                    vec![event(
                        EventChannel::Host,
                        format!("event-{index}").as_str(),
                        json!({ "payload": payload.clone() }),
                    )],
                )
                .expect("append and compact");
        }
        let length = fs::metadata(&path).expect("journal metadata").len();
        assert!(length <= MAX_PERSISTED_EVENT_LOG_BYTES);
        assert_eq!(
            journal.latest_sequence(EventChannel::Host),
            iterations as u64
        );
        let reopened = EventJournal::open(&path).expect("reopen compacted journal");
        assert_eq!(
            reopened.latest_sequence(EventChannel::Host),
            iterations as u64
        );
        let _ = fs::remove_file(path);
    }
}
