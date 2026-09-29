//! Quinn endpoints over real UDP (LAN Ethernet path).
//!
//! Used for cross-host tests where an in-memory L2 fabric cannot reach the peer.
//! Crypto stays on the pure rustls-rustcrypto provider.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig};

use crate::endpoint::EndpointError;
use crate::tls;

fn transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    // Typical Ethernet path MTU; keep discovery on for real NICs.
    transport.initial_mtu(1452);
    transport.min_mtu(1200);
    transport
}

/// UDP QUIC server bound to `addr`.
pub fn udp_server_endpoint(
    addr: SocketAddr,
    cert_pem: &Path,
    key_pem: &Path,
) -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let rustls_server = tls::load_server_config(cert_pem, key_pem)?;
    let mut server_config =
        ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(rustls_server).map_err(
            |e| EndpointError::Other(format!("QuicServerConfig: {e}")),
        )?));
    server_config.transport_config(Arc::new(transport()));
    Ok(Endpoint::server(server_config, addr)?)
}

/// UDP QUIC client bound to an ephemeral local port.
pub fn udp_client_endpoint() -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let rustls_client = tls::load_client_config(true)?;
    let mut client_config =
        ClientConfig::new(Arc::new(QuicClientConfig::try_from(rustls_client).map_err(
            |e| EndpointError::Other(format!("QuicClientConfig: {e}")),
        )?));
    client_config.transport_config(Arc::new(transport()));

    let endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}
