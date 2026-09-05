use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{WeixinContractError, validate_text};
use crate::identifiers::MediaId;

pub const MAX_MIME_TYPE_BYTES: usize = 128;
pub const MAX_FILE_NAME_BYTES: usize = 512;
pub const MAX_DECLARED_MEDIA_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const MAX_MEDIA_DURATION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const MAX_DIMENSION: u32 = 32_768;
pub const MAX_MEDIA_ITEMS: usize = 16;
pub const MAX_MEDIA_METADATA_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MediaMetadata {
    pub media_id: MediaId,
    pub kind: MediaKind,
    pub mime_type: String,
    pub file_name: Option<String>,
    pub byte_length: u64,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl MediaMetadata {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        media_id: MediaId,
        kind: MediaKind,
        mime_type: impl Into<String>,
        file_name: Option<String>,
        byte_length: u64,
        duration_ms: Option<u64>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<Self, WeixinContractError> {
        let metadata = Self {
            media_id,
            kind,
            mime_type: mime_type.into(),
            file_name,
            byte_length,
            duration_ms,
            width,
            height,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn validate(&self) -> Result<(), WeixinContractError> {
        if self.mime_type.is_empty() || self.mime_type.len() > MAX_MIME_TYPE_BYTES {
            return Err(WeixinContractError::InvalidValue {
                field: "mime_type",
                message: "must be a non-empty MIME type no longer than 128 bytes",
            });
        }
        let Some((mime_major, mime_minor)) = self.mime_type.split_once('/') else {
            return Err(WeixinContractError::InvalidValue {
                field: "mime_type",
                message: "must contain one ASCII slash and no whitespace",
            });
        };
        if !self.mime_type.is_ascii()
            || mime_major.is_empty()
            || mime_minor.is_empty()
            || self
                .mime_type
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || mime_minor.contains('/')
        {
            return Err(WeixinContractError::InvalidValue {
                field: "mime_type",
                message: "must contain one ASCII slash and no whitespace",
            });
        }
        if let Some(file_name) = &self.file_name {
            validate_text("file name", file_name, MAX_FILE_NAME_BYTES, false)?;
            if file_name.contains(['/', '\\']) {
                return Err(WeixinContractError::InvalidValue {
                    field: "file_name",
                    message: "must not contain path separators",
                });
            }
        }
        if self.byte_length > MAX_DECLARED_MEDIA_BYTES {
            return Err(WeixinContractError::InvalidValue {
                field: "byte_length",
                message: "exceeds the declared media size bound",
            });
        }
        if self
            .duration_ms
            .is_some_and(|value| value > MAX_MEDIA_DURATION_MS)
        {
            return Err(WeixinContractError::InvalidValue {
                field: "duration_ms",
                message: "exceeds the duration bound",
            });
        }
        if self
            .width
            .is_some_and(|value| value == 0 || value > MAX_DIMENSION)
            || self
                .height
                .is_some_and(|value| value == 0 || value > MAX_DIMENSION)
            || self.width.is_some() != self.height.is_some()
        {
            return Err(WeixinContractError::InvalidValue {
                field: "width/height",
                message: "must be both absent or non-zero dimensions within the bound",
            });
        }
        Ok(())
    }

    pub(crate) fn bounded_size_bytes(&self) -> usize {
        self.media_id.as_str().len()
            + self.mime_type.len()
            + self.file_name.as_deref().map_or(0, str::len)
            + 64
    }
}

impl<'de> Deserialize<'de> for MediaMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireMediaMetadata {
            media_id: MediaId,
            kind: MediaKind,
            mime_type: String,
            file_name: Option<String>,
            byte_length: u64,
            duration_ms: Option<u64>,
            width: Option<u32>,
            height: Option<u32>,
        }

        let wire = WireMediaMetadata::deserialize(deserializer)?;
        Self::new(
            wire.media_id,
            wire.kind,
            wire.mime_type,
            wire.file_name,
            wire.byte_length,
            wire.duration_ms,
            wire.width,
            wire.height,
        )
        .map_err(D::Error::custom)
    }
}
