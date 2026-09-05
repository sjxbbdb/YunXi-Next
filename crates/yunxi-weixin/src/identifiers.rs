use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

use crate::error::{WeixinContractError, validate_token};

pub const MAX_MESSAGE_ID_BYTES: usize = 128;
pub const MAX_SESSION_ID_BYTES: usize = 128;
pub const MAX_REQUEST_ID_BYTES: usize = 128;
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
pub const MAX_PARTICIPANT_ID_BYTES: usize = 256;

macro_rules! bounded_id {
    ($name:ident, $field:literal, $maximum:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, WeixinContractError> {
                let value = value.into();
                validate_token($field, &value, $maximum)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
            }
        }
    };
}

bounded_id!(MessageId, "message id", MAX_MESSAGE_ID_BYTES);
bounded_id!(SessionId, "session id", MAX_SESSION_ID_BYTES);
bounded_id!(RequestId, "request id", MAX_REQUEST_ID_BYTES);
bounded_id!(IdempotencyKey, "idempotency key", MAX_IDEMPOTENCY_KEY_BYTES);
bounded_id!(ParticipantId, "participant id", MAX_PARTICIPANT_ID_BYTES);

pub const MAX_MEDIA_ID_BYTES: usize = 256;
bounded_id!(MediaId, "media id", MAX_MEDIA_ID_BYTES);
