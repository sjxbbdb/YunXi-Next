//! Shared bounds for browser-facing JSON messages.

use serde_json::Value;

use crate::WebContractError;

pub const MAX_RPC_ID_BYTES: usize = 128;
pub const MAX_METHOD_BYTES: usize = 128;
pub const MAX_ERROR_MESSAGE_BYTES: usize = 4 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
pub const MAX_DETAILS_BYTES: usize = 32 * 1024;
pub const MAX_FRAME_BYTES: usize = 512 * 1024;

pub(crate) fn validate_token(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), WebContractError> {
    if value.is_empty() {
        return Err(WebContractError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(WebContractError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    if value.chars().any(|character| {
        !character.is_ascii() || character.is_control() || character.is_whitespace()
    }) {
        return Err(WebContractError::InvalidField { field });
    }
    Ok(())
}

pub(crate) fn validate_json(
    field: &'static str,
    value: &Value,
    maximum: usize,
) -> Result<(), WebContractError> {
    let bytes = serde_json::to_vec(value).map_err(|error| WebContractError::Json {
        message: error.to_string(),
    })?;
    if bytes.len() > maximum {
        return Err(WebContractError::JsonTooLarge {
            field,
            length: bytes.len(),
            maximum,
        });
    }
    Ok(())
}
