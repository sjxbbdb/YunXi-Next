//! Loopback connection setup and readiness negotiation.

use std::collections::BTreeSet;
use std::env;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{
    CapabilityDescriptor, GrantRequirement, HostMessage, JsonLineTransport, PROTOCOL_VERSION,
    PluginManifest, PluginMessage, ProtocolError,
};

pub const CONNECT_ADDRESS_ENV: &str = "YUNXI_PLUGIN_CONNECT_ADDRESS";
pub const CONNECT_TOKEN_ENV: &str = "YUNXI_PLUGIN_CONNECT_TOKEN";

static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(1);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_PLUGIN_METADATA_BYTES: usize = 128;
const MAX_PLUGIN_CAPABILITIES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginConnectionInfo {
    plugin_id: String,
    display_name: String,
    plugin_version: String,
    capabilities: Vec<CapabilityDescriptor>,
    manifest: Option<PluginManifest>,
}

impl PluginConnectionInfo {
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn plugin_version(&self) -> &str {
        &self.plugin_version
    }

    pub fn capabilities(&self) -> &[CapabilityDescriptor] {
        &self.capabilities
    }

    pub fn manifest(&self) -> Option<&PluginManifest> {
        self.manifest.as_ref()
    }

    pub fn supports(&self, capability: &str, version: u32) -> bool {
        self.capabilities.iter().any(|descriptor| {
            descriptor.id().as_str() == capability && descriptor.version() == version
        })
    }
}

pub struct PluginAcceptor {
    listener: TcpListener,
    connection_token: String,
}

impl PluginAcceptor {
    pub fn bind() -> Result<Self, ProtocolError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            connection_token: new_connection_token(),
        })
    }

    pub fn address(&self) -> Result<SocketAddr, ProtocolError> {
        Ok(self.listener.local_addr()?)
    }

    pub fn connection_token(&self) -> &str {
        &self.connection_token
    }

    pub fn accept(
        &self,
        expected_plugin_id: &str,
        timeout: Duration,
    ) -> Result<HostPluginSession, ProtocolError> {
        self.accept_with_max_frame_bytes(
            expected_plugin_id,
            timeout,
            crate::DEFAULT_MAX_FRAME_BYTES,
        )
    }

    /// Accepts a plugin while applying the caller's frame bound before the
    /// first hello is decoded.  The plain [`Self::accept`] method keeps the
    /// historical protocol default for low-level callers; process hosts use
    /// this method to enforce their launch policy across the handshake too.
    pub fn accept_with_max_frame_bytes(
        &self,
        expected_plugin_id: &str,
        timeout: Duration,
        max_frame_bytes: usize,
    ) -> Result<HostPluginSession, ProtocolError> {
        let deadline = Instant::now() + timeout;
        let stream = loop {
            match self.listener.accept() {
                Ok((stream, _peer)) => {
                    stream.set_nonblocking(false)?;
                    break stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(ProtocolError::Handshake(format!(
                            "timed out after {} ms waiting for `{expected_plugin_id}`",
                            timeout.as_millis()
                        )));
                    }
                    thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) => return Err(error.into()),
            }
        };

        let remaining = deadline.saturating_duration_since(Instant::now());
        let mut transport = JsonLineTransport::new(stream)?.with_max_frame_bytes(max_frame_bytes);
        transport.set_timeouts(Some(remaining), Some(remaining))?;
        let hello = transport.receive::<PluginMessage>()?;
        let info = match hello {
            PluginMessage::Hello {
                protocol_version,
                plugin_id,
                connection_token,
                display_name,
                plugin_version,
                capabilities,
                manifest,
            } => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(ProtocolError::Handshake(format!(
                        "plugin uses protocol {protocol_version}; host uses {PROTOCOL_VERSION}"
                    )));
                }
                if plugin_id != expected_plugin_id {
                    return Err(ProtocolError::Handshake(format!(
                        "expected plugin `{expected_plugin_id}`, received `{plugin_id}`"
                    )));
                }
                if connection_token != self.connection_token {
                    return Err(ProtocolError::Handshake(
                        "connection token did not match the launched plugin".to_string(),
                    ));
                }
                validate_declaration(&display_name, &plugin_version, &capabilities)?;
                if let Some(manifest) = &manifest {
                    manifest
                        .validate()
                        .map_err(|error| ProtocolError::Handshake(error.to_string()))?;
                    validate_manifest_identity(
                        manifest,
                        &plugin_id,
                        &display_name,
                        &plugin_version,
                        &capabilities,
                    )?;
                }
                PluginConnectionInfo {
                    plugin_id,
                    display_name,
                    plugin_version,
                    capabilities,
                    manifest,
                }
            }
            message => {
                return Err(ProtocolError::Handshake(format!(
                    "expected hello, received {message:?}"
                )));
            }
        };

        transport.send(&HostMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
        })?;
        match transport.receive::<PluginMessage>()? {
            PluginMessage::Ready => {}
            message => {
                return Err(ProtocolError::Handshake(format!(
                    "expected ready, received {message:?}"
                )));
            }
        }
        transport.set_timeouts(None, None)?;
        Ok(HostPluginSession { transport, info })
    }
}

pub struct HostPluginSession {
    transport: JsonLineTransport,
    info: PluginConnectionInfo,
}

impl HostPluginSession {
    pub fn info(&self) -> &PluginConnectionInfo {
        &self.info
    }

    pub fn set_timeouts(
        &self,
        read_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
    ) -> Result<(), ProtocolError> {
        self.transport.set_timeouts(read_timeout, write_timeout)
    }

    pub fn set_max_frame_bytes(&mut self, max_frame_bytes: usize) {
        self.transport.set_max_frame_bytes(max_frame_bytes);
    }

    pub const fn max_frame_bytes(&self) -> usize {
        self.transport.max_frame_bytes()
    }

    pub fn send(&mut self, message: &HostMessage) -> Result<(), ProtocolError> {
        self.transport.send(message)
    }

    pub fn receive(&mut self) -> Result<PluginMessage, ProtocolError> {
        self.transport.receive()
    }
}

pub struct PluginSession {
    transport: JsonLineTransport,
}

impl PluginSession {
    /// Sets bounded socket timeouts while the plugin multiplexes host control
    /// frames with a worker performing provider I/O.
    pub fn set_timeouts(
        &self,
        read_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
    ) -> Result<(), ProtocolError> {
        self.transport.set_timeouts(read_timeout, write_timeout)
    }

    pub fn set_max_frame_bytes(&mut self, max_frame_bytes: usize) {
        self.transport.set_max_frame_bytes(max_frame_bytes);
    }

    pub const fn max_frame_bytes(&self) -> usize {
        self.transport.max_frame_bytes()
    }

    pub fn receive(&mut self) -> Result<HostMessage, ProtocolError> {
        self.transport.receive()
    }

    pub fn send(&mut self, message: &PluginMessage) -> Result<(), ProtocolError> {
        self.transport.send(message)
    }
}

pub fn connect_plugin(
    plugin_id: impl Into<String>,
    display_name: impl Into<String>,
    plugin_version: impl Into<String>,
    capabilities: Vec<CapabilityDescriptor>,
    timeout: Duration,
) -> Result<PluginSession, ProtocolError> {
    connect_plugin_with_grants(
        plugin_id,
        display_name,
        plugin_version,
        capabilities,
        Vec::new(),
        timeout,
    )
}

pub fn connect_plugin_with_grants(
    plugin_id: impl Into<String>,
    display_name: impl Into<String>,
    plugin_version: impl Into<String>,
    capabilities: Vec<CapabilityDescriptor>,
    grants: Vec<GrantRequirement>,
    timeout: Duration,
) -> Result<PluginSession, ProtocolError> {
    let plugin_id = plugin_id.into();
    let display_name = display_name.into();
    let plugin_version = plugin_version.into();
    let manifest = PluginManifest::new(
        &plugin_id,
        &display_name,
        &plugin_version,
        capabilities.clone(),
    )
    .with_grants(grants);
    connect_plugin_with_manifest(manifest, timeout)
}

pub fn connect_plugin_with_manifest(
    manifest: PluginManifest,
    timeout: Duration,
) -> Result<PluginSession, ProtocolError> {
    manifest
        .validate()
        .map_err(|error| ProtocolError::Handshake(error.to_string()))?;
    validate_declaration(
        manifest.display_name(),
        manifest.plugin_version(),
        manifest.capabilities(),
    )?;
    let plugin_id = manifest.plugin_id().to_string();
    let display_name = manifest.display_name().to_string();
    let plugin_version = manifest.plugin_version().to_string();
    let capabilities = manifest.capabilities().to_vec();
    let address = env::var(CONNECT_ADDRESS_ENV).map_err(|_| {
        ProtocolError::Handshake(format!(
            "environment variable {CONNECT_ADDRESS_ENV} is missing"
        ))
    })?;
    let address = address.parse::<SocketAddr>().map_err(|error| {
        ProtocolError::Handshake(format!("invalid plugin connection address: {error}"))
    })?;
    let connection_token = env::var(CONNECT_TOKEN_ENV).map_err(|_| {
        ProtocolError::Handshake(format!(
            "environment variable {CONNECT_TOKEN_ENV} is missing"
        ))
    })?;

    let stream = TcpStream::connect_timeout(&address, timeout)?;
    let mut transport = JsonLineTransport::new(stream)?;
    transport.set_timeouts(Some(timeout), Some(timeout))?;
    transport.send(&PluginMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        plugin_id,
        connection_token,
        display_name,
        plugin_version,
        capabilities,
        manifest: Some(manifest),
    })?;
    match transport.receive::<HostMessage>()? {
        HostMessage::Welcome { protocol_version } if protocol_version == PROTOCOL_VERSION => {}
        HostMessage::Welcome { protocol_version } => {
            return Err(ProtocolError::Handshake(format!(
                "host selected protocol {protocol_version}; plugin uses {PROTOCOL_VERSION}"
            )));
        }
        message => {
            return Err(ProtocolError::Handshake(format!(
                "expected welcome, received {message:?}"
            )));
        }
    }
    transport.send(&PluginMessage::Ready)?;
    transport.set_timeouts(None, None)?;
    Ok(PluginSession { transport })
}

fn validate_manifest_identity(
    manifest: &PluginManifest,
    plugin_id: &str,
    display_name: &str,
    plugin_version: &str,
    capabilities: &[CapabilityDescriptor],
) -> Result<(), ProtocolError> {
    for (field, expected, received) in [
        ("plugin id", plugin_id, manifest.plugin_id()),
        ("display name", display_name, manifest.display_name()),
        ("plugin version", plugin_version, manifest.plugin_version()),
    ] {
        if expected != received {
            return Err(ProtocolError::Handshake(format!(
                "manifest {field} `{received}` does not match hello `{expected}`"
            )));
        }
    }

    let hello_capabilities = capabilities
        .iter()
        .map(|capability| (capability.id().as_str(), capability.version()))
        .collect::<BTreeSet<_>>();
    let manifest_capabilities = manifest
        .capabilities()
        .iter()
        .map(|capability| (capability.id().as_str(), capability.version()))
        .collect::<BTreeSet<_>>();
    if hello_capabilities != manifest_capabilities {
        return Err(ProtocolError::Handshake(
            "manifest capabilities do not match hello capabilities".to_string(),
        ));
    }
    Ok(())
}

fn validate_declaration(
    display_name: &str,
    plugin_version: &str,
    capabilities: &[CapabilityDescriptor],
) -> Result<(), ProtocolError> {
    for (field, value) in [
        ("display name", display_name),
        ("plugin version", plugin_version),
    ] {
        if value.trim().is_empty() {
            return Err(ProtocolError::Handshake(format!(
                "plugin {field} cannot be empty"
            )));
        }
        if value.len() > MAX_PLUGIN_METADATA_BYTES {
            return Err(ProtocolError::Handshake(format!(
                "plugin {field} exceeds {MAX_PLUGIN_METADATA_BYTES} bytes"
            )));
        }
    }
    if capabilities.is_empty() {
        return Err(ProtocolError::Handshake(
            "plugin must announce at least one capability".to_string(),
        ));
    }
    if capabilities.len() > MAX_PLUGIN_CAPABILITIES {
        return Err(ProtocolError::Handshake(format!(
            "plugin announces more than {MAX_PLUGIN_CAPABILITIES} capabilities"
        )));
    }
    let mut seen = BTreeSet::new();
    for descriptor in capabilities {
        if !seen.insert(descriptor.id().as_str()) {
            return Err(ProtocolError::Handshake(format!(
                "plugin announces capability `{}` more than once",
                descriptor.id()
            )));
        }
    }
    Ok(())
}

fn new_connection_token() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{timestamp:x}-{counter:x}", process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities;

    #[test]
    fn host_and_plugin_complete_readiness_handshake() {
        let acceptor = PluginAcceptor::bind().expect("bind plugin acceptor");
        let address = acceptor.address().expect("read listener address");
        let token = acceptor.connection_token().to_string();
        let plugin = thread::spawn(move || {
            let stream = TcpStream::connect(address).expect("connect to host");
            let mut transport = JsonLineTransport::new(stream).expect("create plugin transport");
            transport
                .send(&PluginMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    plugin_id: "yunxi.test".to_string(),
                    connection_token: token,
                    display_name: "Fixture plugin".to_string(),
                    plugin_version: "1.0.0".to_string(),
                    capabilities: vec![
                        CapabilityDescriptor::new(
                            capabilities::MODEL_CHAT,
                            capabilities::MODEL_CHAT_VERSION,
                        )
                        .expect("valid capability"),
                    ],
                    manifest: None,
                })
                .expect("send hello");
            assert!(matches!(
                transport.receive::<HostMessage>().expect("receive welcome"),
                HostMessage::Welcome {
                    protocol_version: PROTOCOL_VERSION
                }
            ));
            transport.send(&PluginMessage::Ready).expect("send ready");
            assert_eq!(
                transport
                    .receive::<HostMessage>()
                    .expect("receive shutdown"),
                HostMessage::Shutdown
            );
        });

        let mut session = acceptor
            .accept("yunxi.test", Duration::from_secs(2))
            .expect("accept ready plugin");
        assert_eq!(session.info().display_name(), "Fixture plugin");
        assert_eq!(session.info().plugin_version(), "1.0.0");
        assert!(
            session
                .info()
                .supports(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)
        );
        session.send(&HostMessage::Shutdown).expect("send shutdown");
        plugin.join().expect("join plugin thread");
    }

    #[test]
    fn duplicate_capability_declarations_are_rejected() {
        let capability =
            CapabilityDescriptor::new(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)
                .expect("valid capability");

        let error =
            validate_declaration("Fixture plugin", "1.0.0", &[capability.clone(), capability])
                .expect_err("duplicate capability must fail");

        assert!(error.to_string().contains("more than once"));
    }
}
