//! Build Quinn endpoints whose datagram path is the Ethernet fabric.

use std::path::Path;
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig, TokioRuntime, TransportConfig};

use crate::fabric::Fabric;
use crate::socket::EtherSocket;
use crate::tls::{self, ALPN_ETHERQUIC};
use crate::Mac;

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Tls(#[from] rustls::Error),
    #[error(transparent)]
    Config(#[from] quinn::ConfigError),
    #[error(transparent)]
    Connect(#[from] quinn::ConnectError),
    #[error("{0}")]
    Other(String),
}

/// Server endpoint listening on `mac` / `service_id` over `fabric`.
pub fn server_endpoint(
    fabric: Arc<Fabric>,
    mac: Mac,
    service_id: u16,
    cert_pem: &Path,
    key_pem: &Path,
) -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let max = fabric.max_quic_datagram() as u16;

    let mut transport = TransportConfig::default();
    transport.initial_mtu(max);
    transport.min_mtu(1200.min(max));
    transport.mtu_discovery_config(None);

    let rustls_server = tls::load_server_config(cert_pem, key_pem)?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(rustls_server)
            .map_err(|e| EndpointError::Other(format!("QuicServerConfig: {e}")))?,
    ));
    server_config.transport_config(Arc::new(transport));

    let mut endpoint_config = EndpointConfig::default();
    endpoint_config.max_udp_payload_size(max)?;

    let socket = EtherSocket::open(fabric, mac, service_id);
    let runtime = Arc::new(TokioRuntime);
    crate::note_quic_bind();
    Ok(Endpoint::new_with_abstract_socket(
        endpoint_config,
        Some(server_config),
        socket,
        runtime,
    )?)
}

/// Client endpoint bound to `mac` / `service_id` over `fabric`.
pub fn client_endpoint(
    fabric: Arc<Fabric>,
    mac: Mac,
    service_id: u16,
) -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let max = fabric.max_quic_datagram() as u16;

    let mut transport = TransportConfig::default();
    transport.initial_mtu(max);
    transport.min_mtu(1200.min(max));
    transport.mtu_discovery_config(None);

    let rustls_client = tls::load_client_config(true)?;
    let mut client_config = ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(rustls_client)
            .map_err(|e| EndpointError::Other(format!("QuicClientConfig: {e}")))?,
    ));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint_config = EndpointConfig::default();
    endpoint_config.max_udp_payload_size(max)?;

    let socket = EtherSocket::open(fabric, mac, service_id);
    let runtime = Arc::new(TokioRuntime);
    crate::note_quic_bind();
    let endpoint = Endpoint::new_with_abstract_socket(endpoint_config, None, socket, runtime)?;
    endpoint.set_default_client_config(client_config);
    let _ = ALPN_ETHERQUIC; // documented constant; already set on the rustls configs
    Ok(endpoint)
}
