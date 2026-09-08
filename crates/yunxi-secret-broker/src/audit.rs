use std::fmt;

use crate::PluginId;

/// The operation that crossed the secret boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditOperation {
    Resolve,
    Inject,
}

impl AuditOperation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Inject => "inject",
        }
    }
}

/// The metadata-only result recorded for an access attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditOutcome {
    Success,
    Missing,
    Denied,
    InvalidReference,
    AlreadyResolved,
    StoreError,
    CallbackPanicked,
}

impl AuditOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Missing => "missing",
            Self::Denied => "denied",
            Self::InvalidReference => "invalid-reference",
            Self::AlreadyResolved => "already-resolved",
            Self::StoreError => "store-error",
            Self::CallbackPanicked => "callback-panicked",
        }
    }
}

/// A metadata-only audit record. It intentionally has no secret-value field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEvent {
    pub sequence: u64,
    pub operation: AuditOperation,
    pub plugin: PluginId,
    pub reference_id: u64,
    pub outcome: AuditOutcome,
    pub value_len: Option<usize>,
}

impl AuditEvent {
    /// Returns a small JSON object containing metadata only.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"sequence\":{},\"operation\":\"{}\",\"plugin\":\"{}\",\"reference\":\"opaque\",\"outcome\":\"{}\",\"value_len\":{}}}",
            self.sequence,
            self.operation.as_str(),
            escape_json(self.plugin.as_str()),
            self.outcome.as_str(),
            self.value_len
                .map_or_else(|| "null".to_owned(), |length| length.to_string())
        )
    }
}

impl fmt::Display for AuditEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "audit#{} {} plugin={} ref=opaque outcome={} value_len={}",
            self.sequence,
            self.operation.as_str(),
            self.plugin,
            self.outcome.as_str(),
            self.value_len
                .map_or_else(|| "-".to_owned(), |length| length.to_string())
        )
    }
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write;
                let _ = write!(escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}
