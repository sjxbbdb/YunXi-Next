//! Process lifecycle and typed invocation over the capability catalog.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use yunxi_kernel::{
    KernelError, KernelSnapshot, KernelState, PluginCommand, PluginFailure, PluginId,
    PluginSnapshot, PluginSpec, YunxiKernel,
};
use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, CapabilityDescriptor, GrantKind, HostMessage,
    HostPluginSession, InvocationCodecError, InvocationRequest, PluginAcceptor,
    PluginConnectionInfo, PluginMessage, ProtocolError,
};

use crate::{CapabilityCatalog, CatalogError};

const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginLaunch {
    id: PluginId,
    display_name: String,
    command: PluginCommand,
    handshake_timeout: Duration,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    required_grants: Vec<GrantKind>,
}

impl PluginLaunch {
    pub fn new(id: PluginId, command: PluginCommand) -> Self {
        let display_name = id.to_string();
        Self {
            id,
            display_name,
            command,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            read_timeout: None,
            write_timeout: Some(DEFAULT_WRITE_TIMEOUT),
            required_grants: Vec::new(),
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        let display_name = display_name.into();
        if !display_name.trim().is_empty() {
            self.display_name = display_name;
        }
        self
    }

    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    pub fn with_io_timeouts(
        mut self,
        read_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
    ) -> Self {
        self.read_timeout = read_timeout;
        self.write_timeout = write_timeout;
        self
    }

    pub fn with_required_grants(mut self, grants: impl IntoIterator<Item = GrantKind>) -> Self {
        self.required_grants = grants.into_iter().collect();
        self.required_grants.sort_unstable();
        self.required_grants.dedup();
        self
    }
}

pub struct ProcessPluginHost {
    kernel: YunxiKernel,
    catalog: CapabilityCatalog,
    connections: BTreeMap<PluginId, HostPluginSession>,
    next_request_id: u64,
}

impl ProcessPluginHost {
    pub fn new() -> Self {
        Self {
            kernel: YunxiKernel::new(),
            catalog: CapabilityCatalog::new(),
            connections: BTreeMap::new(),
            next_request_id: 1,
        }
    }

    pub fn launch(&mut self, launch: PluginLaunch) -> Result<PluginId, PluginHostError> {
        let PluginLaunch {
            id,
            display_name,
            command,
            handshake_timeout,
            read_timeout,
            write_timeout,
            required_grants,
        } = launch;
        let acceptor = PluginAcceptor::bind()?;
        let command = command
            .env(CONNECT_ADDRESS_ENV, acceptor.address()?.to_string())
            .env(CONNECT_TOKEN_ENV, acceptor.connection_token());
        let spec = PluginSpec::new(id.clone(), command).with_display_name(display_name);
        self.kernel.register(spec)?;
        self.kernel.start(&id)?;

        let connection = match acceptor.accept(id.as_str(), handshake_timeout) {
            Ok(connection) => connection,
            Err(error) => {
                self.fail_connection(&id, error.to_string());
                return Err(PluginHostError::Protocol(error));
            }
        };
        if let Err(error) = validate_required_grants(connection.info(), &required_grants) {
            self.fail_connection(&id, error.to_string());
            return Err(PluginHostError::Protocol(error));
        }
        if let Err(error) = connection.set_timeouts(read_timeout, write_timeout) {
            self.fail_connection(&id, error.to_string());
            return Err(PluginHostError::Protocol(error));
        }
        if let Err(error) = self.catalog.register_connection(connection.info()) {
            self.fail_connection(&id, error.to_string());
            return Err(PluginHostError::Catalog(error));
        }
        self.connections.insert(id.clone(), connection);
        Ok(id)
    }

    pub fn invoke<Request, Response>(
        &mut self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, PluginCallError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        let plugin_id = self
            .catalog
            .resolve_unique(capability.id().as_str(), capability.version())
            .map_err(PluginCallError::Route)?
            .id()
            .clone();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).unwrap_or(1);
        let request = InvocationRequest::encode(request_id, capability.clone(), operation, payload)
            .map_err(PluginCallError::Codec)?;

        let Some(connection) = self.connections.get_mut(&plugin_id) else {
            self.fail_connection(&plugin_id, "plugin connection is not available".to_string());
            return Err(PluginCallError::Unavailable {
                plugin_id,
                message: "plugin connection is not available".to_string(),
            });
        };
        if let Err(error) = connection.send(&HostMessage::Invoke { request }) {
            let message = error.to_string();
            self.fail_connection(&plugin_id, message.clone());
            return Err(PluginCallError::Unavailable { plugin_id, message });
        }

        let response = match self.connections.get_mut(&plugin_id) {
            Some(connection) => match connection.receive() {
                Ok(response) => response,
                Err(error) => {
                    let message = error.to_string();
                    self.fail_connection(&plugin_id, message.clone());
                    return Err(PluginCallError::Unavailable { plugin_id, message });
                }
            },
            None => {
                let message = "plugin connection closed after request send".to_string();
                self.fail_connection(&plugin_id, message.clone());
                return Err(PluginCallError::Unavailable { plugin_id, message });
            }
        };

        match response {
            PluginMessage::InvocationCompleted { response }
                if response.request_id() == request_id =>
            {
                response.decode_payload().map_err(|error| {
                    self.fail_connection(&plugin_id, error.to_string());
                    PluginCallError::ProtocolViolation {
                        plugin_id,
                        message: error.to_string(),
                    }
                })
            }
            PluginMessage::InvocationFailed {
                request_id: response_id,
                code,
                message,
                retryable,
            } if response_id == request_id => Err(PluginCallError::Rejected {
                plugin_id,
                code,
                message,
                retryable,
            }),
            message => {
                let message =
                    format!("expected response for request {request_id}, received {message:?}");
                self.fail_connection(&plugin_id, message.clone());
                Err(PluginCallError::ProtocolViolation { plugin_id, message })
            }
        }
    }

    pub fn stop(&mut self, id: &PluginId) {
        if let Some(connection) = self.connections.get_mut(id) {
            let _ignored = connection.send(&HostMessage::Shutdown);
        }
        self.disconnect(id);
    }

    pub fn refresh(&mut self) {
        self.kernel.refresh();
        let disconnected = self
            .connections
            .keys()
            .filter(|id| {
                self.kernel
                    .plugin(id)
                    .is_none_or(|snapshot| !snapshot.state().is_active())
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in disconnected {
            self.connections.remove(&id);
            self.catalog.unregister(&id);
        }
    }

    pub fn kernel_state(&self) -> KernelState {
        self.kernel.state()
    }

    pub fn plugin(&mut self, id: &PluginId) -> Option<PluginSnapshot> {
        self.refresh();
        self.kernel.plugin(id)
    }

    pub fn snapshot(&mut self) -> KernelSnapshot {
        self.refresh();
        self.kernel.snapshot()
    }

    pub fn catalog(&self) -> &CapabilityCatalog {
        &self.catalog
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    pub fn shutdown(&mut self) {
        let ids = self.connections.keys().cloned().collect::<Vec<_>>();
        for id in &ids {
            if let Some(connection) = self.connections.get_mut(id) {
                let _ignored = connection.send(&HostMessage::Shutdown);
            }
        }
        self.connections.clear();
        for id in ids {
            self.catalog.unregister(&id);
        }
        self.kernel.shutdown();
    }

    fn disconnect(&mut self, id: &PluginId) {
        self.connections.remove(id);
        self.catalog.unregister(id);
        let _ignored = self.kernel.stop(id);
        self.kernel.refresh();
    }

    fn fail_connection(&mut self, id: &PluginId, message: String) {
        self.connections.remove(id);
        self.catalog.unregister(id);
        self.kernel.refresh();
        let already_failed = self
            .kernel
            .plugin(id)
            .is_some_and(|snapshot| snapshot.state().is_failed());
        if !already_failed {
            let _ignored = self.kernel.fail(id, PluginFailure::Protocol { message });
        }
        self.kernel.refresh();
    }
}

fn validate_required_grants(
    connection: &PluginConnectionInfo,
    required_grants: &[GrantKind],
) -> Result<(), ProtocolError> {
    if required_grants.is_empty() {
        return Ok(());
    }
    let Some(manifest) = connection.manifest() else {
        return Err(ProtocolError::Handshake(format!(
            "plugin `{}` must provide a manifest declaring required grants: {}",
            connection.plugin_id(),
            required_grants
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    for grant in required_grants {
        if !manifest.declares_required_grant(*grant) {
            return Err(ProtocolError::Handshake(format!(
                "plugin `{}` manifest does not declare required grant `{grant}`",
                connection.plugin_id()
            )));
        }
    }
    Ok(())
}

impl Default for ProcessPluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProcessPluginHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
pub enum PluginHostError {
    Kernel(KernelError),
    Catalog(CatalogError),
    Protocol(ProtocolError),
}

impl fmt::Display for PluginHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(error) => write!(formatter, "kernel operation failed: {error}"),
            Self::Catalog(error) => write!(formatter, "capability catalog failed: {error}"),
            Self::Protocol(error) => write!(formatter, "plugin protocol failed: {error}"),
        }
    }
}

impl Error for PluginHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Kernel(error) => Some(error),
            Self::Catalog(error) => Some(error),
            Self::Protocol(error) => Some(error),
        }
    }
}

impl From<KernelError> for PluginHostError {
    fn from(error: KernelError) -> Self {
        Self::Kernel(error)
    }
}

impl From<CatalogError> for PluginHostError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<ProtocolError> for PluginHostError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

#[derive(Debug)]
pub enum PluginCallError {
    Route(CatalogError),
    Codec(InvocationCodecError),
    Unavailable {
        plugin_id: PluginId,
        message: String,
    },
    Rejected {
        plugin_id: PluginId,
        code: String,
        message: String,
        retryable: bool,
    },
    ProtocolViolation {
        plugin_id: PluginId,
        message: String,
    },
}

impl fmt::Display for PluginCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Route(error) => error.fmt(formatter),
            Self::Codec(error) => error.fmt(formatter),
            Self::Unavailable { plugin_id, message } => {
                write!(formatter, "plugin `{plugin_id}` is unavailable: {message}")
            }
            Self::Rejected {
                plugin_id,
                code,
                message,
                retryable,
            } => {
                write!(
                    formatter,
                    "plugin `{plugin_id}` rejected the request ({code}): {message}"
                )?;
                if *retryable {
                    formatter.write_str(" [retryable]")?;
                }
                Ok(())
            }
            Self::ProtocolViolation { plugin_id, message } => {
                write!(
                    formatter,
                    "plugin `{plugin_id}` violated the protocol: {message}"
                )
            }
        }
    }
}

impl Error for PluginCallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Route(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::Unavailable { .. } | Self::Rejected { .. } | Self::ProtocolViolation { .. } => {
                None
            }
        }
    }
}
