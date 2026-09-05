use std::time::Duration;

use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{CapabilityDescriptor, GrantKind, capabilities};
use yunxi_weixin::{
    ACK_OPERATION, ChannelMessage, INBOUND_OPERATION, MessageMutationRequest, OUTBOUND_OPERATION,
    WEIXIN_PLUGIN_ID, WeixinPluginResponse, inbound_fixture, outbound_fixture,
};

#[test]
fn weixin_fixture_uses_the_same_isolated_host_path() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(WEIXIN_PLUGIN_ID).expect("weixin plugin id");
    let capability = CapabilityDescriptor::new(
        capabilities::CHANNEL_WEIXIN,
        capabilities::CHANNEL_WEIXIN_VERSION,
    )
    .expect("channel capability");
    host.launch(
        PluginLaunch::new(
            plugin_id.clone(),
            PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-weixin-plugin-fixture")),
        )
        .with_handshake_timeout(Duration::from_secs(2))
        .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
        .with_required_grants([GrantKind::Network, GrantKind::Secret])
        .with_expected_capabilities([capability.clone()]),
    )
    .expect("weixin fixture launch");

    let inbound = inbound_fixture().expect("inbound fixture").message;
    let accepted: WeixinPluginResponse = host
        .invoke(&capability, INBOUND_OPERATION, &inbound)
        .expect("inbound route");
    assert!(matches!(
        accepted,
        WeixinPluginResponse::Accepted {
            duplicate: false,
            ..
        }
    ));

    let duplicate: WeixinPluginResponse = host
        .invoke(&capability, INBOUND_OPERATION, &inbound)
        .expect("duplicate route");
    assert!(matches!(
        duplicate,
        WeixinPluginResponse::Accepted {
            duplicate: true,
            ..
        }
    ));

    let ack: WeixinPluginResponse = host
        .invoke(
            &capability,
            ACK_OPERATION,
            &MessageMutationRequest {
                idempotency_key: inbound.envelope().idempotency_key.clone(),
                reason: None,
            },
        )
        .expect("ack route");
    assert!(matches!(ack, WeixinPluginResponse::Mutated { .. }));

    host.disable(&plugin_id).expect("disable weixin fixture");
    assert!(
        host.catalog()
            .providers(
                capabilities::CHANNEL_WEIXIN,
                capabilities::CHANNEL_WEIXIN_VERSION
            )
            .is_empty()
    );
}

#[test]
fn weixin_fixture_keeps_bad_payloads_and_wrong_routes_local() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(WEIXIN_PLUGIN_ID).expect("weixin plugin id");
    let capability = CapabilityDescriptor::new(
        capabilities::CHANNEL_WEIXIN,
        capabilities::CHANNEL_WEIXIN_VERSION,
    )
    .expect("channel capability");
    host.launch(
        PluginLaunch::new(
            plugin_id,
            PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-weixin-plugin-fixture")),
        )
        .with_handshake_timeout(Duration::from_secs(2))
        .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
        .with_required_grants([GrantKind::Network, GrantKind::Secret])
        .with_expected_capabilities([capability.clone()]),
    )
    .expect("weixin fixture launch");

    let error = host
        .invoke::<_, WeixinPluginResponse>(
            &capability,
            INBOUND_OPERATION,
            &serde_json::json!({"direction": "outbound", "message": {}}),
        )
        .expect_err("wrong payload must be rejected");
    assert!(
        matches!(error, PluginCallError::Rejected { ref code, .. } if code == "invalid_request")
    );

    let outbound = outbound_fixture().expect("outbound fixture").message;
    let accepted: WeixinPluginResponse = host
        .invoke(&capability, OUTBOUND_OPERATION, &outbound)
        .expect("outbound route remains available");
    assert!(matches!(
        accepted,
        WeixinPluginResponse::Accepted {
            message: ChannelMessage::Outbound(_),
            ..
        }
    ));
}
