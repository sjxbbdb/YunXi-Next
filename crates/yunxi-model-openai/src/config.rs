//! Legacy-compatible provider and credential resolution.

use std::env;
use std::error::Error;
use std::fmt;
use std::time::Duration;

const DEFAULT_TIMEOUT_MILLIS: u64 = 120_000;

#[derive(Clone, Eq, PartialEq)]
pub struct ProviderConfig {
    provider: String,
    model: String,
    base_url: String,
    api_key: String,
    timeout: Duration,
}

impl ProviderConfig {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderConfigError> {
        let provider = required("provider", provider.into())?;
        let model = required("model", model.into())?;
        let base_url = required("base URL", base_url.into())?;
        let api_key = required("API key", api_key.into())?;
        Ok(Self {
            provider,
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MILLIS),
        })
    }

    pub fn from_env() -> Result<Self, ProviderConfigError> {
        Self::from_env_with(|name| env::var(name).ok())
    }

    pub fn from_env_with<F>(read_env: F) -> Result<Self, ProviderConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let explicit_profile = non_empty(&read_env, "YUNXI_PROVIDER_PROFILE");
        let profile = explicit_profile
            .or_else(|| non_empty(&read_env, "DEEPSEEK_API_KEY").map(|_| "deepseek".to_string()));
        let is_deepseek = profile.as_deref() == Some("deepseek");
        let provider = profile.unwrap_or_else(|| "openai-compatible".to_string());
        let default_model = if is_deepseek {
            "deepseek-v4-flash"
        } else {
            "gpt-4.1"
        };
        let default_base_url = if is_deepseek {
            "https://api.deepseek.com"
        } else {
            "https://api.openai.com/v1"
        };
        let model =
            non_empty(&read_env, "YUNXI_AGENT_MODEL").unwrap_or_else(|| default_model.to_string());
        let base_url = non_empty(&read_env, "YUNXI_PROVIDER_BASE_URL")
            .or_else(|| non_empty(&read_env, "OPENAI_BASE_URL"))
            .unwrap_or_else(|| default_base_url.to_string());

        let api_key = if let Some(value) = non_empty(&read_env, "YUNXI_PROVIDER_API_KEY") {
            value
        } else if let Some(name) = non_empty(&read_env, "YUNXI_PROVIDER_API_KEY_ENV") {
            non_empty(&read_env, &name)
                .ok_or(ProviderConfigError::MissingCredential { env_name: name })?
        } else if is_deepseek {
            non_empty(&read_env, "DEEPSEEK_API_KEY").ok_or_else(|| {
                ProviderConfigError::MissingCredential {
                    env_name: "DEEPSEEK_API_KEY".to_string(),
                }
            })?
        } else {
            non_empty(&read_env, "OPENAI_API_KEY").ok_or_else(|| {
                ProviderConfigError::MissingCredential {
                    env_name: "OPENAI_API_KEY".to_string(),
                }
            })?
        };

        let timeout_millis = non_empty(&read_env, "YUNXI_PROVIDER_TIMEOUT_MILLIS")
            .map(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or(ProviderConfigError::InvalidTimeout { value })
            })
            .transpose()?
            .unwrap_or(DEFAULT_TIMEOUT_MILLIS);

        Ok(Self {
            provider,
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            timeout: Duration::from_millis(timeout_millis),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    pub(crate) fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderConfigError {
    MissingValue { field: &'static str },
    MissingCredential { env_name: String },
    InvalidTimeout { value: String },
}

impl fmt::Display for ProviderConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue { field } => write!(formatter, "provider {field} cannot be empty"),
            Self::MissingCredential { env_name } => write!(
                formatter,
                "API credential is missing; set {env_name} or YUNXI_PROVIDER_API_KEY"
            ),
            Self::InvalidTimeout { value } => write!(
                formatter,
                "YUNXI_PROVIDER_TIMEOUT_MILLIS must be a positive integer, received `{value}`"
            ),
        }
    }
}

impl Error for ProviderConfigError {}

fn non_empty<F>(read_env: &F, name: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    read_env(name).filter(|value| !value.trim().is_empty())
}

fn required(field: &'static str, value: String) -> Result<String, ProviderConfigError> {
    if value.trim().is_empty() {
        Err(ProviderConfigError::MissingValue { field })
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn deepseek_key_selects_deepseek_defaults() {
        let values = BTreeMap::from([("DEEPSEEK_API_KEY", "secret")]);
        let config = ProviderConfig::from_env_with(|name| {
            values.get(name).map(|value| (*value).to_string())
        })
        .expect("resolve DeepSeek config");

        assert_eq!(config.provider(), "deepseek");
        assert_eq!(config.model(), "deepseek-v4-flash");
        assert_eq!(config.base_url(), "https://api.deepseek.com");
        assert!(format!("{config:?}").contains("[redacted]"));
        assert!(!format!("{config:?}").contains("secret"));
    }

    #[test]
    fn explicit_yunxi_values_override_compatible_defaults() {
        let values = BTreeMap::from([
            ("YUNXI_PROVIDER_PROFILE", "local"),
            ("YUNXI_PROVIDER_BASE_URL", "http://127.0.0.1:9000/v1/"),
            ("YUNXI_PROVIDER_API_KEY", "local-key"),
            ("YUNXI_AGENT_MODEL", "fixture-model"),
        ]);
        let config = ProviderConfig::from_env_with(|name| {
            values.get(name).map(|value| (*value).to_string())
        })
        .expect("resolve explicit config");

        assert_eq!(config.provider(), "local");
        assert_eq!(config.model(), "fixture-model");
        assert_eq!(config.base_url(), "http://127.0.0.1:9000/v1");
    }
}
