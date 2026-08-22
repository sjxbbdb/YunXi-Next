//! Loopback connection setup and readiness negotiation.

use std::env;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{HostMessage, JsonLineTransport, PROTOCOL_VERSION, PluginMessage, ProtocolError};

pub const CONNECT_ADDRESS_ENV: &str = "YUNXI_PLUGIN_CONNECT_ADDRESS";
pub const CONNECT_TOKEN_ENV: &str = "YUNXI_PLUGIN_CONNECT_TOKEN";

static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(1);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginConnectionInfo {
    plugin_id: String,
    provider: String,
    model: String,
    capabilities: Vec<String>,
}

impl PluginConnectionInfo {
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
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
        let mut transport = JsonLineTransport::new(stream)?;
        transport.set_timeouts(Some(remaining), Some(remaining))?;
        let hello = transport.receive::<PluginMessage>()?;
        let info = match hello {
            PluginMessage::Hello {
                protocol_version,
                plugin_id,
                connection_token,
                provider,
                model,
                capabilities,
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
                PluginConnectionInfo {
                    plugin_id,
                    provider,
                    model,
                    capabilities,
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
    pub fn receive(&mut self) -> Result<HostMessage, ProtocolError> {
        self.transport.receive()
    }

    pub fn send(&mut self, message: &PluginMessage) -> Result<(), ProtocolError> {
        self.transport.send(message)
    }
}

pub fn connect_plugin(
    plugin_id: impl Into<String>,
    provider: impl Into<String>,
    model: impl Into<String>,
    capabilities: Vec<String>,
    timeout: Duration,
) -> Result<PluginSession, ProtocolError> {
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
        plugin_id: plugin_id.into(),
        connection_token,
        provider: provider.into(),
        model: model.into(),
        capabilities,
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
    use crate::CHAT_CAPABILITY;

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
                    provider: "fixture".to_string(),
                    model: "fixture-model".to_string(),
                    capabilities: vec![CHAT_CAPABILITY.to_string()],
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
        assert_eq!(session.info().provider(), "fixture");
        assert_eq!(session.info().model(), "fixture-model");
        assert_eq!(session.info().capabilities(), [CHAT_CAPABILITY]);
        session.send(&HostMessage::Shutdown).expect("send shutdown");
        plugin.join().expect("join plugin thread");
    }
}
