//! Capability-neutral request and response envelopes with typed payload helpers.

use std::error::Error;
use std::fmt;

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::CapabilityDescriptor;

const MAX_OPERATION_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InvocationRequest {
    request_id: u64,
    capability: CapabilityDescriptor,
    operation: String,
    payload: Value,
}

impl InvocationRequest {
    pub fn encode<T>(
        request_id: u64,
        capability: CapabilityDescriptor,
        operation: impl Into<String>,
        payload: &T,
    ) -> Result<Self, InvocationCodecError>
    where
        T: Serialize,
    {
        let operation = operation.into();
        validate_request_id(request_id)?;
        validate_operation(&operation)?;
        Ok(Self {
            request_id,
            capability,
            operation,
            payload: serde_json::to_value(payload).map_err(InvocationCodecError::Serialize)?,
        })
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn capability(&self) -> &CapabilityDescriptor {
        &self.capability
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn decode_payload<T>(&self) -> Result<T, InvocationCodecError>
    where
        T: DeserializeOwned,
    {
        serde_json::from_value(self.payload.clone()).map_err(InvocationCodecError::Deserialize)
    }
}

impl<'de> Deserialize<'de> for InvocationRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRequest {
            request_id: u64,
            capability: CapabilityDescriptor,
            operation: String,
            payload: Value,
        }

        let request = WireRequest::deserialize(deserializer)?;
        validate_request_id(request.request_id).map_err(D::Error::custom)?;
        validate_operation(&request.operation).map_err(D::Error::custom)?;
        Ok(Self {
            request_id: request.request_id,
            capability: request.capability,
            operation: request.operation,
            payload: request.payload,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InvocationResponse {
    request_id: u64,
    payload: Value,
}

impl InvocationResponse {
    pub fn encode<T>(request_id: u64, payload: &T) -> Result<Self, InvocationCodecError>
    where
        T: Serialize,
    {
        validate_request_id(request_id)?;
        Ok(Self {
            request_id,
            payload: serde_json::to_value(payload).map_err(InvocationCodecError::Serialize)?,
        })
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn decode_payload<T>(&self) -> Result<T, InvocationCodecError>
    where
        T: DeserializeOwned,
    {
        serde_json::from_value(self.payload.clone()).map_err(InvocationCodecError::Deserialize)
    }
}

impl<'de> Deserialize<'de> for InvocationResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResponse {
            request_id: u64,
            payload: Value,
        }

        let response = WireResponse::deserialize(deserializer)?;
        validate_request_id(response.request_id).map_err(D::Error::custom)?;
        Ok(Self {
            request_id: response.request_id,
            payload: response.payload,
        })
    }
}

#[derive(Debug)]
pub enum InvocationCodecError {
    ZeroRequestId,
    EmptyOperation,
    OperationTooLong { length: usize, maximum: usize },
    InvalidOperationCharacter { index: usize, character: char },
    Serialize(serde_json::Error),
    Deserialize(serde_json::Error),
}

impl fmt::Display for InvocationCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroRequestId => formatter.write_str("request id must be greater than zero"),
            Self::EmptyOperation => formatter.write_str("operation cannot be empty"),
            Self::OperationTooLong { length, maximum } => {
                write!(
                    formatter,
                    "operation is {length} bytes; maximum is {maximum}"
                )
            }
            Self::InvalidOperationCharacter { index, character } => write!(
                formatter,
                "operation contains unsupported character `{character}` at byte {index}"
            ),
            Self::Serialize(error) => {
                write!(formatter, "failed to encode invocation payload: {error}")
            }
            Self::Deserialize(error) => {
                write!(formatter, "failed to decode invocation payload: {error}")
            }
        }
    }
}

impl Error for InvocationCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize(error) | Self::Deserialize(error) => Some(error),
            _ => None,
        }
    }
}

fn validate_request_id(request_id: u64) -> Result<(), InvocationCodecError> {
    if request_id == 0 {
        Err(InvocationCodecError::ZeroRequestId)
    } else {
        Ok(())
    }
}

fn validate_operation(operation: &str) -> Result<(), InvocationCodecError> {
    if operation.is_empty() {
        return Err(InvocationCodecError::EmptyOperation);
    }
    if operation.len() > MAX_OPERATION_BYTES {
        return Err(InvocationCodecError::OperationTooLong {
            length: operation.len(),
            maximum: MAX_OPERATION_BYTES,
        });
    }
    for (index, character) in operation.char_indices() {
        if !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && !matches!(character, '.' | '-' | '_')
        {
            return Err(InvocationCodecError::InvalidOperationCharacter { index, character });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct FixturePayload {
        value: String,
    }

    #[test]
    fn typed_payloads_cross_the_generic_envelope() {
        let request = InvocationRequest::encode(
            7,
            CapabilityDescriptor::new("fixture.echo", 1).expect("valid capability"),
            "run",
            &FixturePayload {
                value: "hello".to_string(),
            },
        )
        .expect("encode request");
        let json = serde_json::to_string(&request).expect("serialize request");
        let decoded =
            serde_json::from_str::<InvocationRequest>(&json).expect("deserialize request");
        assert_eq!(decoded.capability().id().as_str(), "fixture.echo");
        assert_eq!(decoded.capability().version(), 1);
        assert_eq!(
            decoded
                .decode_payload::<FixturePayload>()
                .expect("decode payload"),
            FixturePayload {
                value: "hello".to_string()
            }
        );
    }

    #[test]
    fn invalid_wire_operations_are_rejected() {
        let error = serde_json::from_str::<InvocationRequest>(
            r#"{"request_id":1,"capability":{"id":"fixture.echo","version":1},"operation":"Bad op","payload":null}"#,
        )
        .expect_err("invalid operation must fail");
        assert!(error.to_string().contains("unsupported character"));
    }
}
