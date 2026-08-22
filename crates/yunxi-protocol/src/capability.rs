//! Stable capability identifiers announced by isolated plugins.

use std::borrow::Borrow;
use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const MAX_CAPABILITY_ID_BYTES: usize = 128;

pub mod capabilities {
    //! Capability identifiers reserved for YunXi's built-in plugin contracts.

    pub const MODEL_CHAT: &str = "model.chat";
    pub const MODEL_CHAT_VERSION: u32 = 1;
    pub const CONTEXT_COMPOSE: &str = "context.compose";
    pub const CONTEXT_COMPOSE_VERSION: u32 = 1;
    pub const PERSONA_CONTEXT: &str = "persona.context";
    pub const PERSONA_CONTEXT_VERSION: u32 = 1;
    pub const MEMORY_RECALL: &str = "memory.recall";
    pub const MEMORY_RECALL_VERSION: u32 = 1;
    pub const MEMORY_WRITE: &str = "memory.write";
    pub const COMPANION_DECIDE: &str = "companion.decide";
    pub const COMPANION_MAILBOX: &str = "companion.mailbox";
    pub const SCHEDULER_PROACTIVE: &str = "scheduler.proactive";
    pub const STORAGE_SESSIONS: &str = "storage.sessions";
    pub const TOOL_SHELL: &str = "tool.shell";
    pub const TOOL_PATCH: &str = "tool.patch";
    pub const TOOL_MCP: &str = "tool.mcp";
    pub const TOOL_SKILLS: &str = "tool.skills";
    pub const TOOL_MULTI_AGENT: &str = "tool.multi-agent";
    pub const CHANNEL_WEIXIN: &str = "channel.weixin";
    pub const VOICE_TRANSCRIBE: &str = "voice.transcribe";
    pub const VOICE_SYNTHESIZE: &str = "voice.synthesize";

    pub const ALL: &[&str] = &[
        MODEL_CHAT,
        CONTEXT_COMPOSE,
        PERSONA_CONTEXT,
        MEMORY_RECALL,
        MEMORY_WRITE,
        COMPANION_DECIDE,
        COMPANION_MAILBOX,
        SCHEDULER_PROACTIVE,
        STORAGE_SESSIONS,
        TOOL_SHELL,
        TOOL_PATCH,
        TOOL_MCP,
        TOOL_SKILLS,
        TOOL_MULTI_AGENT,
        CHANNEL_WEIXIN,
        VOICE_TRANSCRIBE,
        VOICE_SYNTHESIZE,
    ];
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CapabilityId(String);

impl CapabilityId {
    pub fn new(value: impl Into<String>) -> Result<Self, CapabilityIdError> {
        let value = value.into();
        validate_capability_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for CapabilityId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for CapabilityId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for CapabilityId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CapabilityId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityDescriptor {
    id: CapabilityId,
    version: u32,
}

impl CapabilityDescriptor {
    pub fn new(id: impl Into<String>, version: u32) -> Result<Self, CapabilityError> {
        let id = CapabilityId::new(id).map_err(CapabilityError::InvalidId)?;
        Self::from_id(id, version)
    }

    pub fn from_id(id: CapabilityId, version: u32) -> Result<Self, CapabilityError> {
        if version == 0 {
            return Err(CapabilityError::ZeroVersion);
        }
        Ok(Self { id, version })
    }

    pub fn id(&self) -> &CapabilityId {
        &self.id
    }

    pub fn version(&self) -> u32 {
        self.version
    }
}

impl<'de> Deserialize<'de> for CapabilityDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireDescriptor {
            id: CapabilityId,
            version: u32,
        }

        let descriptor = WireDescriptor::deserialize(deserializer)?;
        Self::from_id(descriptor.id, descriptor.version).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityIdError {
    Empty,
    TooLong { length: usize, maximum: usize },
    MissingNamespace,
    EmptySegment,
    InvalidSegmentStart { segment: String },
    InvalidSegmentEnd { segment: String },
    InvalidCharacter { index: usize, character: char },
}

impl fmt::Display for CapabilityIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("capability id cannot be empty"),
            Self::TooLong { length, maximum } => write!(
                formatter,
                "capability id is {length} bytes; maximum is {maximum}"
            ),
            Self::MissingNamespace => formatter
                .write_str("capability id must contain at least two dot-separated segments"),
            Self::EmptySegment => formatter.write_str("capability id contains an empty segment"),
            Self::InvalidSegmentStart { segment } => write!(
                formatter,
                "capability id segment `{segment}` must start with a lowercase ASCII letter"
            ),
            Self::InvalidSegmentEnd { segment } => write!(
                formatter,
                "capability id segment `{segment}` must not end with a hyphen"
            ),
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "capability id contains unsupported character `{character}` at byte {index}"
            ),
        }
    }
}

impl Error for CapabilityIdError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    InvalidId(CapabilityIdError),
    ZeroVersion,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(error) => error.fmt(formatter),
            Self::ZeroVersion => {
                formatter.write_str("capability version must be greater than zero")
            }
        }
    }
}

impl Error for CapabilityError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidId(error) => Some(error),
            Self::ZeroVersion => None,
        }
    }
}

fn validate_capability_id(value: &str) -> Result<(), CapabilityIdError> {
    if value.is_empty() {
        return Err(CapabilityIdError::Empty);
    }
    if value.len() > MAX_CAPABILITY_ID_BYTES {
        return Err(CapabilityIdError::TooLong {
            length: value.len(),
            maximum: MAX_CAPABILITY_ID_BYTES,
        });
    }

    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() < 2 {
        return Err(CapabilityIdError::MissingNamespace);
    }
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(CapabilityIdError::EmptySegment);
    }

    for segment in segments {
        if !segment
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
        {
            return Err(CapabilityIdError::InvalidSegmentStart {
                segment: segment.to_string(),
            });
        }
        if segment.ends_with('-') {
            return Err(CapabilityIdError::InvalidSegmentEnd {
                segment: segment.to_string(),
            });
        }
    }

    for (index, character) in value.char_indices() {
        if !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && !matches!(character, '.' | '-')
        {
            return Err(CapabilityIdError::InvalidCharacter { index, character });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_descriptors_round_trip_with_validation() {
        let descriptor = CapabilityDescriptor::new(capabilities::MODEL_CHAT, 1)
            .expect("create capability descriptor");
        let json = serde_json::to_string(&descriptor).expect("serialize descriptor");
        assert_eq!(json, r#"{"id":"model.chat","version":1}"#);
        assert_eq!(
            serde_json::from_str::<CapabilityDescriptor>(&json).expect("deserialize descriptor"),
            descriptor
        );
    }

    #[test]
    fn capability_ids_reject_unstable_wire_names() {
        assert!(matches!(
            CapabilityId::new("chat"),
            Err(CapabilityIdError::MissingNamespace)
        ));
        assert!(matches!(
            CapabilityId::new("Model.Chat"),
            Err(CapabilityIdError::InvalidSegmentStart { .. })
        ));
        assert!(matches!(
            CapabilityId::new("model.chat_legacy"),
            Err(CapabilityIdError::InvalidCharacter { .. })
        ));
    }

    #[test]
    fn capability_versions_cannot_be_zero_on_the_wire() {
        let error =
            serde_json::from_str::<CapabilityDescriptor>(r#"{"id":"model.chat","version":0}"#)
                .expect_err("zero version must fail");
        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn built_in_capability_names_are_valid_and_unique() {
        let ids = capabilities::ALL
            .iter()
            .map(|value| CapabilityId::new(*value).expect("valid built-in capability"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), capabilities::ALL.len());
    }
}
