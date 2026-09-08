//! Voice process-Host launch policy.
//!
//! Provider implementations live in `yunxi-voice` and execute only in the
//! supervised plugin process. The CLI keeps just the grant and deadline policy
//! needed to launch that process.

use std::time::Duration;

use yunxi_protocol::GrantKind;
use yunxi_voice::ProcessSidecarConfig;

const SIDECAR_TIMEOUT_ENV: &str = "YUNXI_VOICE_SIDECAR_TIMEOUT_MS";

/// A configured sidecar is device-facing and therefore requires an explicit
/// Device grant. Deterministic loopback has no device authority.
pub(crate) fn session_required_grants(external_plugin_configured: bool) -> Vec<GrantKind> {
    session_required_grants_for_configuration(
        external_plugin_configured,
        ProcessSidecarConfig::from_environment()
            .ok()
            .flatten()
            .is_some(),
    )
}

fn session_required_grants_for_configuration(
    _external_plugin_configured: bool,
    sidecar_configured: bool,
) -> Vec<GrantKind> {
    if sidecar_configured {
        vec![GrantKind::Device]
    } else {
        Vec::new()
    }
}

pub(crate) fn session_response_timeout() -> Duration {
    let timeout = std::env::var(SIDECAR_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(yunxi_voice::DEFAULT_SIDECAR_TIMEOUT)
        .min(yunxi_voice::MAX_SIDECAR_TIMEOUT);
    timeout
        .checked_add(Duration::from_secs(5))
        .unwrap_or(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_without_sidecar_does_not_require_device_grant() {
        assert!(session_required_grants_for_configuration(false, false).is_empty());
    }

    #[test]
    fn configured_sidecar_provider_requires_device_grant() {
        assert_eq!(
            session_required_grants_for_configuration(true, true),
            vec![GrantKind::Device]
        );
        assert_eq!(
            session_required_grants_for_configuration(false, true),
            vec![GrantKind::Device]
        );
    }

    #[test]
    fn missing_sidecar_does_not_add_a_device_requirement() {
        assert!(session_required_grants_for_configuration(true, false).is_empty());
    }

    #[test]
    fn session_timeout_includes_a_host_response_margin() {
        assert!(session_response_timeout() >= Duration::from_secs(5));
    }
}
