//! Four-quadrant RPC messages shared by dsh HTTP and stream carriers.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::WebContractError;
use crate::bounds::{
    MAX_DETAILS_BYTES, MAX_ERROR_MESSAGE_BYTES, MAX_FRAME_BYTES, MAX_METHOD_BYTES,
    MAX_PAYLOAD_BYTES, MAX_RPC_ID_BYTES, validate_json, validate_token,
};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RpcId(String);

impl RpcId {
    pub fn new(value: impl Into<String>) -> Result<Self, WebContractError> {
        let value = value.into();
        validate_token("rpcId", &value, MAX_RPC_ID_BYTES)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for RpcId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for RpcId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for RpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RpcId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcError {
    code: String,
    message: String,
    details: Value,
}

impl RpcError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Value,
    ) -> Result<Self, WebContractError> {
        let code = code.into();
        let message = message.into();
        validate_token("error code", &code, MAX_METHOD_BYTES)?;
        if message.trim().is_empty() {
            return Err(WebContractError::EmptyField {
                field: "error message",
            });
        }
        if message.len() > MAX_ERROR_MESSAGE_BYTES {
            return Err(WebContractError::FieldTooLong {
                field: "error message",
                length: message.len(),
                maximum: MAX_ERROR_MESSAGE_BYTES,
            });
        }
        validate_json("error details", &details, MAX_DETAILS_BYTES)?;
        if !details.is_object() {
            return Err(WebContractError::InvalidMessage {
                message: "error details must be a JSON object".to_string(),
            });
        }
        Ok(Self {
            code,
            message,
            details,
        })
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn details(&self) -> &Value {
        &self.details
    }
}

#[derive(Serialize, Deserialize)]
struct WireRpcError {
    code: String,
    message: String,
    details: Value,
}

impl Serialize for RpcError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireRpcError {
            code: self.code.clone(),
            message: self.message.clone(),
            details: self.details.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RpcError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireRpcError::deserialize(deserializer)?;
        Self::new(wire.code, wire.message, wire.details).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RpcResult<T> {
    Success(T),
    Failure(RpcError),
}

impl<T> RpcResult<T> {
    pub fn success(value: T) -> Self {
        Self::Success(value)
    }

    pub fn failure(error: RpcError) -> Self {
        Self::Failure(error)
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Success(value) => Some(value),
            Self::Failure(_) => None,
        }
    }

    pub fn error(&self) -> Option<&RpcError> {
        match self {
            Self::Success(_) => None,
            Self::Failure(error) => Some(error),
        }
    }
}

impl<T: Serialize> Serialize for RpcResult<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Success(value) => {
                #[derive(Serialize)]
                struct WireSuccess<'a, T> {
                    ok: bool,
                    value: &'a T,
                }
                WireSuccess { ok: true, value }.serialize(serializer)
            }
            Self::Failure(error) => {
                #[derive(Serialize)]
                struct WireFailure<'a> {
                    ok: bool,
                    error: &'a RpcError,
                }
                WireFailure { ok: false, error }.serialize(serializer)
            }
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for RpcResult<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let object = Value::deserialize(deserializer)?;
        let object = object
            .as_object()
            .ok_or_else(|| D::Error::custom("RPC result must be a JSON object"))?;
        let ok = object
            .get("ok")
            .and_then(Value::as_bool)
            .ok_or_else(|| D::Error::custom("RPC result must contain boolean `ok`"))?;
        if ok {
            let value = object
                .get("value")
                .cloned()
                .ok_or_else(|| D::Error::custom("successful RPC result lacks `value`"))?;
            serde_json::from_value(value)
                .map(Self::Success)
                .map_err(D::Error::custom)
        } else {
            let error = object
                .get("error")
                .cloned()
                .ok_or_else(|| D::Error::custom("failed RPC result lacks `error`"))?;
            serde_json::from_value(error)
                .map(Self::Failure)
                .map_err(D::Error::custom)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientRequest {
    rpc_id: RpcId,
    method: String,
    payload: Value,
}

impl ClientRequest {
    pub fn new(
        rpc_id: RpcId,
        method: impl Into<String>,
        payload: Value,
    ) -> Result<Self, WebContractError> {
        let method = method.into();
        validate_method(&method)?;
        validate_payload(&payload)?;
        Ok(Self {
            rpc_id,
            method,
            payload,
        })
    }

    pub fn rpc_id(&self) -> &RpcId {
        &self.rpc_id
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerRequest {
    rpc_id: RpcId,
    method: String,
    payload: Value,
}

impl ServerRequest {
    pub fn new(
        rpc_id: RpcId,
        method: impl Into<String>,
        payload: Value,
    ) -> Result<Self, WebContractError> {
        let method = method.into();
        validate_method(&method)?;
        validate_payload(&payload)?;
        Ok(Self {
            rpc_id,
            method,
            payload,
        })
    }

    pub fn rpc_id(&self) -> &RpcId {
        &self.rpc_id
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerResponse {
    rpc_id: RpcId,
    result: RpcResult<Value>,
}

impl ServerResponse {
    pub fn new(rpc_id: RpcId, result: RpcResult<Value>) -> Result<Self, WebContractError> {
        validate_result(&result)?;
        Ok(Self { rpc_id, result })
    }

    pub fn rpc_id(&self) -> &RpcId {
        &self.rpc_id
    }

    pub fn result(&self) -> &RpcResult<Value> {
        &self.result
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientResponse {
    rpc_id: RpcId,
    result: RpcResult<Value>,
}

impl ClientResponse {
    pub fn new(rpc_id: RpcId, result: RpcResult<Value>) -> Result<Self, WebContractError> {
        validate_result(&result)?;
        Ok(Self { rpc_id, result })
    }

    pub fn rpc_id(&self) -> &RpcId {
        &self.rpc_id
    }

    pub fn result(&self) -> &RpcResult<Value> {
        &self.result
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RpcMessage {
    ClientRequest(ClientRequest),
    ServerResponse(ServerResponse),
    ServerRequest(ServerRequest),
    ClientResponse(ClientResponse),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum WireRpcMessage {
    #[serde(rename = "client-request")]
    ClientRequest {
        #[serde(rename = "rpcId")]
        rpc_id: RpcId,
        method: String,
        payload: Value,
    },
    #[serde(rename = "server-response")]
    ServerResponse {
        #[serde(rename = "rpcId")]
        rpc_id: RpcId,
        result: RpcResult<Value>,
    },
    #[serde(rename = "server-request")]
    ServerRequest {
        #[serde(rename = "rpcId")]
        rpc_id: RpcId,
        method: String,
        payload: Value,
    },
    #[serde(rename = "client-response")]
    ClientResponse {
        #[serde(rename = "rpcId")]
        rpc_id: RpcId,
        result: RpcResult<Value>,
    },
}

impl Serialize for ClientRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireRpcMessage::ClientRequest {
            rpc_id: self.rpc_id.clone(),
            method: self.method.clone(),
            payload: self.payload.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ClientRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match WireRpcMessage::deserialize(deserializer)? {
            WireRpcMessage::ClientRequest {
                rpc_id,
                method,
                payload,
            } => Self::new(rpc_id, method, payload).map_err(D::Error::custom),
            _ => Err(D::Error::custom("expected a client-request message")),
        }
    }
}

impl Serialize for ServerRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireRpcMessage::ServerRequest {
            rpc_id: self.rpc_id.clone(),
            method: self.method.clone(),
            payload: self.payload.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ServerRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match WireRpcMessage::deserialize(deserializer)? {
            WireRpcMessage::ServerRequest {
                rpc_id,
                method,
                payload,
            } => Self::new(rpc_id, method, payload).map_err(D::Error::custom),
            _ => Err(D::Error::custom("expected a server-request message")),
        }
    }
}

impl Serialize for ServerResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireRpcMessage::ServerResponse {
            rpc_id: self.rpc_id.clone(),
            result: self.result.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ServerResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match WireRpcMessage::deserialize(deserializer)? {
            WireRpcMessage::ServerResponse { rpc_id, result } => {
                Self::new(rpc_id, result).map_err(D::Error::custom)
            }
            _ => Err(D::Error::custom("expected a server-response message")),
        }
    }
}

impl Serialize for ClientResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireRpcMessage::ClientResponse {
            rpc_id: self.rpc_id.clone(),
            result: self.result.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ClientResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match WireRpcMessage::deserialize(deserializer)? {
            WireRpcMessage::ClientResponse { rpc_id, result } => {
                Self::new(rpc_id, result).map_err(D::Error::custom)
            }
            _ => Err(D::Error::custom("expected a client-response message")),
        }
    }
}

impl Serialize for RpcMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let wire = match self {
            Self::ClientRequest(request) => WireRpcMessage::ClientRequest {
                rpc_id: request.rpc_id.clone(),
                method: request.method.clone(),
                payload: request.payload.clone(),
            },
            Self::ServerResponse(response) => WireRpcMessage::ServerResponse {
                rpc_id: response.rpc_id.clone(),
                result: response.result.clone(),
            },
            Self::ServerRequest(request) => WireRpcMessage::ServerRequest {
                rpc_id: request.rpc_id.clone(),
                method: request.method.clone(),
                payload: request.payload.clone(),
            },
            Self::ClientResponse(response) => WireRpcMessage::ClientResponse {
                rpc_id: response.rpc_id.clone(),
                result: response.result.clone(),
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireRpcMessage::deserialize(deserializer)?;
        match wire {
            WireRpcMessage::ClientRequest {
                rpc_id,
                method,
                payload,
            } => ClientRequest::new(rpc_id, method, payload)
                .map(Self::ClientRequest)
                .map_err(D::Error::custom),
            WireRpcMessage::ServerResponse { rpc_id, result } => {
                ServerResponse::new(rpc_id, result)
                    .map(Self::ServerResponse)
                    .map_err(D::Error::custom)
            }
            WireRpcMessage::ServerRequest {
                rpc_id,
                method,
                payload,
            } => ServerRequest::new(rpc_id, method, payload)
                .map(Self::ServerRequest)
                .map_err(D::Error::custom),
            WireRpcMessage::ClientResponse { rpc_id, result } => {
                ClientResponse::new(rpc_id, result)
                    .map(Self::ClientResponse)
                    .map_err(D::Error::custom)
            }
        }
    }
}

impl RpcMessage {
    pub fn client_request(
        rpc_id: RpcId,
        method: impl Into<String>,
        payload: Value,
    ) -> Result<Self, WebContractError> {
        ClientRequest::new(rpc_id, method, payload).map(Self::ClientRequest)
    }

    pub fn server_request(
        rpc_id: RpcId,
        method: impl Into<String>,
        payload: Value,
    ) -> Result<Self, WebContractError> {
        ServerRequest::new(rpc_id, method, payload).map(Self::ServerRequest)
    }

    pub fn server_response(
        rpc_id: RpcId,
        result: RpcResult<Value>,
    ) -> Result<Self, WebContractError> {
        ServerResponse::new(rpc_id, result).map(Self::ServerResponse)
    }

    pub fn client_response(
        rpc_id: RpcId,
        result: RpcResult<Value>,
    ) -> Result<Self, WebContractError> {
        ClientResponse::new(rpc_id, result).map(Self::ClientResponse)
    }

    pub fn encode(&self) -> Result<Vec<u8>, WebContractError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|error| WebContractError::Json {
            message: error.to_string(),
        })?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(WebContractError::FrameTooLarge {
                length: bytes.len(),
                maximum: MAX_FRAME_BYTES,
            });
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, WebContractError> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(WebContractError::FrameTooLarge {
                length: bytes.len(),
                maximum: MAX_FRAME_BYTES,
            });
        }
        let message: Self =
            serde_json::from_slice(bytes).map_err(|error| WebContractError::Json {
                message: error.to_string(),
            })?;
        message.validate()?;
        Ok(message)
    }

    pub fn validate(&self) -> Result<(), WebContractError> {
        match self {
            Self::ClientRequest(request) => {
                validate_request(request.rpc_id(), request.method(), request.payload())
            }
            Self::ServerRequest(request) => {
                validate_request(request.rpc_id(), request.method(), request.payload())
            }
            Self::ServerResponse(response) => {
                validate_response(response.rpc_id(), response.result())
            }
            Self::ClientResponse(response) => {
                validate_response(response.rpc_id(), response.result())
            }
        }
    }
}

fn validate_request(rpc_id: &RpcId, method: &str, payload: &Value) -> Result<(), WebContractError> {
    validate_rpc_id(rpc_id)?;
    validate_method(method)?;
    validate_payload(payload)
}

fn validate_response(rpc_id: &RpcId, result: &RpcResult<Value>) -> Result<(), WebContractError> {
    validate_rpc_id(rpc_id)?;
    validate_result(result)
}

fn validate_rpc_id(rpc_id: &RpcId) -> Result<(), WebContractError> {
    validate_token("rpcId", rpc_id.as_str(), MAX_RPC_ID_BYTES)
}

fn validate_method(method: &str) -> Result<(), WebContractError> {
    validate_token("method", method, MAX_METHOD_BYTES)
}

fn validate_payload(payload: &Value) -> Result<(), WebContractError> {
    validate_json("payload", payload, MAX_PAYLOAD_BYTES)
}

fn validate_result(result: &RpcResult<Value>) -> Result<(), WebContractError> {
    match result {
        RpcResult::Success(value) => validate_payload(value),
        RpcResult::Failure(error) => {
            validate_token("error code", error.code(), MAX_METHOD_BYTES)?;
            validate_json("error details", error.details(), MAX_DETAILS_BYTES)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rpc_id() -> RpcId {
        RpcId::new("rpc-1").expect("valid rpc id")
    }

    #[test]
    fn dsh_client_request_round_trips_exact_wire_names() {
        let message =
            RpcMessage::client_request(rpc_id(), "session.list", json!({})).expect("request");
        let bytes = message.encode().expect("encode");
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).expect("json"),
            json!({
                "type": "client-request",
                "rpcId": "rpc-1",
                "method": "session.list",
                "payload": {}
            })
        );
        assert_eq!(RpcMessage::decode(&bytes).expect("decode"), message);
    }

    #[test]
    fn named_quadrant_types_keep_their_discriminants() {
        let request = ClientRequest::new(rpc_id(), "session.list", json!({})).expect("request");
        let encoded = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(encoded["type"], "client-request");
        assert_eq!(
            serde_json::from_value::<ClientRequest>(encoded).expect("decode request"),
            request
        );

        let response = ServerResponse::new(
            rpc_id(),
            RpcResult::success(json!({ "items": [] })),
        )
        .expect("response");
        let encoded = serde_json::to_value(&response).expect("serialize response");
        assert_eq!(encoded["type"], "server-response");
        assert_eq!(
            serde_json::from_value::<ServerResponse>(encoded).expect("decode response"),
            response
        );
    }

    #[test]
    fn rpc_results_use_dsh_success_and_failure_quadrants() {
        let success = RpcResult::success(json!({ "items": [] }));
        assert_eq!(
            serde_json::to_value(&success).expect("serialize success"),
            json!({ "ok": true, "value": { "items": [] } })
        );
        let error = RpcError::new("internal", "failed", json!({})).expect("error");
        let failure = RpcResult::<Value>::failure(error);
        let encoded = serde_json::to_value(&failure).expect("serialize failure");
        assert_eq!(encoded["ok"], false);
        assert_eq!(
            serde_json::from_value::<RpcResult<Value>>(encoded).expect("decode failure"),
            failure
        );
    }

    #[test]
    fn oversized_payloads_are_rejected_before_transport() {
        let payload = json!({ "text": "x".repeat(MAX_PAYLOAD_BYTES) });
        let error = RpcMessage::client_request(rpc_id(), "session.list", payload)
            .expect_err("payload must be bounded");
        assert!(matches!(
            error,
            WebContractError::JsonTooLarge {
                field: "payload",
                ..
            }
        ));
    }

    #[test]
    fn malformed_rpc_result_tags_are_not_silently_accepted() {
        let error = serde_json::from_str::<RpcResult<Value>>(r#"{"ok":"yes","value":{}}"#)
            .expect_err("invalid ok tag");
        assert!(error.to_string().contains("boolean `ok`"));
    }
}
