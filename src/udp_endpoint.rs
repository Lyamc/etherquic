//! Quinn endpoints over real UDP (LAN Ethernet path).
//!
//! Used for cross-host tests where an in-memory L2 fabric cannot reach the peer.
//! Crypto stays on the pure rustls-rustcrypto provider.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, IdleTimeout, ServerConfig, TransportConfig, VarInt};

use crate::endpoint::EndpointError;
use crate::tls;

fn transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    // Typical Ethernet path MTU; keep discovery on for real NICs.
    transport.initial_mtu(1452);
    transport.min_mtu(1200);
    transport
}

/// IPv6 and IPv4 listeners for one play port.
///
/// IPv6 is bound first. On Windows that socket often accepts IPv4 as well, so
/// a following IPv4 bind can fail with the address already in use. That case
/// still counts as both families.
pub struct UdpListeners {
    pub v4: Option<Endpoint>,
    pub v6: Option<Endpoint>,
    pub note: String,
}

pub fn udp_listeners(port: u16, cert_pem: &Path, key_pem: &Path) -> UdpListeners {
    let v6_addr = SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port));
    let v4_addr = SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port));
    let v6 = match udp_server_endpoint(v6_addr, cert_pem, key_pem) {
        Ok(endpoint) => Some(endpoint),
        Err(error) => {
            eprintln!("etherquic: udp {v6_addr}: {error}");
            None
        }
    };
    let v4_result = udp_server_endpoint(v4_addr, cert_pem, key_pem);
    let v4_error = v4_result.as_ref().err().map(ToString::to_string);
    let v4 = v4_result.ok();
    if let Some(error) = v4_error.as_deref() {
        if !(v6.is_some() && already_bound(error)) {
            eprintln!("etherquic: udp {v4_addr}: {error}");
        }
    }
    let v4_open = v4.is_some() || (v4_error.as_deref().is_some_and(already_bound) && v6.is_some());
    let note = match (v4_open, v6.is_some()) {
        (true, true) => format!("IPv4 and IPv6 on {port}."),
        (true, false) => format!("IPv4 on {port}."),
        (false, true) => format!("IPv6 on {port}."),
        (false, false) => format!("UDP {port} did not open."),
    };
    UdpListeners { v4, v6, note }
}

/// `accept` on a listener, or wait forever when that family did not bind.
pub async fn udp_accept(endpoint: Option<&Endpoint>) -> Option<quinn::Incoming> {
    match endpoint {
        Some(endpoint) => endpoint.accept().await,
        None => std::future::pending().await,
    }
}

fn already_bound(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("in use") || error.contains("10048") || error.contains("only one usage")
}

/// UDP QUIC server bound to `addr`.
pub fn udp_server_endpoint(
    addr: SocketAddr,
    cert_pem: &Path,
    key_pem: &Path,
) -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let rustls_server = tls::load_server_config(cert_pem, key_pem)?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(rustls_server)
            .map_err(|e| EndpointError::Other(format!("QuicServerConfig: {e}")))?,
    ));
    server_config.transport_config(Arc::new(transport()));
    Ok(Endpoint::server(server_config, addr)?)
}

/// UDP QUIC client bound to an ephemeral IPv4 port.
pub fn udp_client_endpoint() -> Result<Endpoint, EndpointError> {
    udp_client_endpoint_on("0.0.0.0:0".parse().expect("unspecified ipv4"))
}

/// UDP QUIC client bound to `addr`.
///
/// `127.0.0.1:0` and `[::1]:0` each stay in one family, so a loopback
/// session can be IPv4 or IPv6.
pub fn udp_client_endpoint_on(addr: SocketAddr) -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let rustls_client = tls::load_client_config(true)?;
    let mut client_config = ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(rustls_client)
            .map_err(|e| EndpointError::Other(format!("QuicClientConfig: {e}")))?,
    ));
    // Same 6s idle limit and 2s ping as the L2 client.
    let mut transport = transport();
    transport.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(6_000))));
    transport.keep_alive_interval(Some(Duration::from_secs(2)));
    client_config.transport_config(Arc::new(transport));

    let endpoint = Endpoint::client(addr)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}
