//! Quinn endpoints over real UDP (LAN Ethernet path).
//!
//! Used for cross-host tests where an in-memory L2 fabric cannot reach the peer.
//! Crypto stays on the pure rustls-rustcrypto provider.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, IdleTimeout, ServerConfig, TransportConfig, VarInt};

use crate::endpoint::EndpointError;
use crate::tls;

fn transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    // LAN Ethernet. Path MTU discovery stays off. The cap is 1452 bytes.
    transport.initial_mtu(1452);
    transport.min_mtu(1200);
    transport.mtu_discovery_config(None);
    // Mode 1, including the next listener after a short sample, stays at
    // 12,000 bytes and 333 ms. Mode 2 retunes the connection that measured
    // the RTT in [`apply_agreed_mode`].
    crate::carrier::apply_mode(
        &mut transport,
        crate::carrier::QUIC_STREAMS,
        false,
        None,
    );
    transport.datagram_receive_buffer_size(Some(1024 * 1024));
    transport.datagram_send_buffer_size(1024 * 1024);
    transport
}

/// Mode 2 on UDP takes the short path only when this connection's own RTT is
/// under 2 ms. Mode 1 is unchanged. A stored sample from another connection
/// does not widen this one.
pub(crate) fn apply_agreed_mode(conn: &Connection, mode: u8) {
    if mode != crate::carrier::QUIC_DATAGRAMS {
        return;
    }
    if conn.congestion_state().initial_window() == crate::carrier::MODE2_WINDOW {
        return;
    }
    // `rtt()` is the smoothed estimate and can sit above 2 ms while the
    // ack-delay-free minimum is a few hundred microseconds. The minimum is
    // the first path sample. Before any sample it is still the 333 ms initial.
    let profile = crate::carrier::profile_for(
        crate::carrier::QUIC_DATAGRAMS,
        false,
        Some(conn.min_rtt()),
    );
    if !profile.short {
        return;
    }
    conn.retune_cubic(
        profile.initial_rtt,
        profile.initial_window,
        profile.max_ack_delay,
        u32::try_from(profile.ack_eliciting).unwrap_or(0),
    );
}

/// IPv6 and IPv4 listeners for one play port.
///
/// Both sockets are bound on the same port, and the IPv6 one is IPv6-only, so
/// each family has its own socket. Before, IPv6 was bound dual-stack first and
/// an IPv4 "address in use" was taken to mean that socket already covered
/// IPv4. On Windows that is wrong: `[::]` is IPv6-only there, so the error
/// meant another program held `0.0.0.0` on the port, and every IPv4 player
/// reached that program instead. On AN515 RoSE holds UDP `0.0.0.0:4433`, and
/// its QUIC stack rejected ours with "peer doesn't support any known protocol".
pub struct UdpListeners {
    pub v4: Option<Endpoint>,
    pub v6: Option<Endpoint>,
    /// The port both listen on. It differs from the asked port when another
    /// program held that one; announce this one.
    pub port: u16,
    pub note: String,
}

/// Ports tried after the asked one when another program holds it.
pub const UDP_PORT_TRIES: u16 = 16;

fn std_socket(addr: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())?;
    // Windows gives a UDP socket 64 KB. A PyroWave picture on QUIC datagrams
    // is a few hundred KB inside one congestion window, so the tail of every
    // burst was dropped by the kernel and no picture ever reassembled.
    let _ = socket.set_recv_buffer_size(QUIC_UDP_BUFFER);
    let _ = socket.set_send_buffer_size(QUIC_UDP_BUFFER);
    Ok(socket.into())
}

/// Kernel buffer for a QUIC UDP socket. Matches the RTP media sockets.
const QUIC_UDP_BUFFER: usize = 4 * 1024 * 1024;

/// The sockets for one port: IPv4 must bind, IPv6 is kept when it binds.
/// `Err` only when IPv4 is taken, so the caller moves to the next port.
fn bind_pair(port: u16) -> std::io::Result<(std::net::UdpSocket, Option<std::net::UdpSocket>)> {
    let v4 = std_socket(SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)))?;
    let v6 = std_socket(SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port))).ok();
    Ok((v4, v6))
}

/// The first port from `port` on whose IPv4 address no other program
/// listens. Discovery announces this, and [`udp_listeners`] binds it.
pub fn udp_port_for(port: u16) -> u16 {
    for candidate in port..=port.saturating_add(UDP_PORT_TRIES) {
        if bind_pair(candidate).is_ok() {
            return candidate;
        }
    }
    port
}

pub fn udp_listeners(port: u16, cert_pem: &Path, key_pem: &Path) -> UdpListeners {
    let mut taken = false;
    let mut last_error = String::new();
    for candidate in port..=port.saturating_add(UDP_PORT_TRIES) {
        let (v4_socket, v6_socket) = match bind_pair(candidate) {
            Ok(pair) => pair,
            Err(error) => {
                taken |= error.kind() == std::io::ErrorKind::AddrInUse
                    || already_bound(&error.to_string());
                last_error = error.to_string();
                continue;
            }
        };
        let v4 = server_on(v4_socket, cert_pem, key_pem, "IPv4");
        let v6 = v6_socket.and_then(|socket| server_on(socket, cert_pem, key_pem, "IPv6"));
        let families = match (v4.is_some(), v6.is_some()) {
            (true, true) => "IPv4 and IPv6",
            (true, false) => "IPv4",
            (false, true) => "IPv6",
            (false, false) => continue,
        };
        let note = if candidate == port {
            format!("{families} on {candidate}.")
        } else {
            format!("{families} on {candidate}: another program uses UDP {port}.")
        };
        return UdpListeners {
            v4,
            v6,
            port: candidate,
            note,
        };
    }
    let why = if taken { "is in use" } else { "did not open" };
    eprintln!("etherquic: udp {port}: {last_error}");
    UdpListeners {
        v4: None,
        v6: None,
        port,
        note: format!("UDP {port} {why}, and the next {UDP_PORT_TRIES} ports too."),
    }
}

fn server_on(
    socket: std::net::UdpSocket,
    cert_pem: &Path,
    key_pem: &Path,
    family: &str,
) -> Option<Endpoint> {
    let result = server_config(cert_pem, key_pem).and_then(|config| {
        let runtime = quinn::default_runtime()
            .ok_or_else(|| EndpointError::Other("no async runtime".into()))?;
        crate::note_quic_bind();
        Ok(Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(config),
            socket,
            runtime,
        )?)
    });
    match result {
        Ok(endpoint) => Some(endpoint),
        Err(error) => {
            eprintln!("etherquic: udp {family}: {error}");
            None
        }
    }
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

fn server_config(cert_pem: &Path, key_pem: &Path) -> Result<ServerConfig, EndpointError> {
    tls::install_pure_crypto();
    let rustls_server = tls::load_server_config(cert_pem, key_pem)?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(rustls_server)
            .map_err(|e| EndpointError::Other(format!("QuicServerConfig: {e}")))?,
    ));
    server_config.transport_config(Arc::new(transport()));
    Ok(server_config)
}

/// UDP QUIC server bound to `addr`.
pub fn udp_server_endpoint(
    addr: SocketAddr,
    cert_pem: &Path,
    key_pem: &Path,
) -> Result<Endpoint, EndpointError> {
    crate::note_quic_bind();
    Ok(Endpoint::server(server_config(cert_pem, key_pem)?, addr)?)
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

    crate::note_quic_bind();
    let runtime = quinn::default_runtime()
        .ok_or_else(|| EndpointError::Other("no async runtime".into()))?;
    let endpoint = Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        std_socket(addr)?,
        runtime,
    )?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, UdpSocket};
    use std::path::PathBuf;

    fn certs() -> (PathBuf, PathBuf) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("certs");
        (root.join("cert.pem"), root.join("key.pem"))
    }

    /// A port the OS hands out, with room for the tries above it.
    fn free_port() -> u16 {
        loop {
            let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
            let port = probe.local_addr().unwrap().port();
            drop(probe);
            if port < u16::MAX - UDP_PORT_TRIES {
                return port;
            }
        }
    }

    async fn handshake(port: u16, server: &Endpoint) -> Result<Vec<u8>, String> {
        let client =
            udp_client_endpoint_on("127.0.0.1:0".parse().unwrap()).map_err(|e| e.to_string())?;
        let dial = client
            .connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), "localhost")
            .map_err(|e| e.to_string())?;
        let accept = async {
            let incoming = server.accept().await.ok_or("listener closed")?;
            incoming.await.map_err(|e| e.to_string())
        };
        let (client_side, server_side) = tokio::join!(dial, accept);
        let connection = client_side.map_err(|e| e.to_string())?;
        server_side?;
        let alpn = connection
            .handshake_data()
            .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
            .and_then(|data| data.protocol)
            .ok_or("no ALPN")?;
        connection.close(0u32.into(), b"done");
        Ok(alpn)
    }

    #[tokio::test]
    async fn a_short_sample_does_not_widen_mode_1_and_mode_2_retunes_the_measured_connection() {
        crate::carrier::reset_udp_rtt();
        crate::carrier::observe_udp_rtt(Duration::from_micros(800), true);
        let (cert, key) = certs();
        let listeners = udp_listeners(free_port(), &cert, &key);
        let server = listeners.v4.expect("v4 listener");
        let client = udp_client_endpoint_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let dial = client
            .connect(
                SocketAddr::from((Ipv4Addr::LOCALHOST, listeners.port)),
                "localhost",
            )
            .unwrap();
        let accept = async {
            let incoming = server.accept().await.expect("listener open");
            incoming.await.expect("handshake")
        };
        let (dialed, server_conn) = tokio::join!(dial, accept);
        let client_conn = dialed.expect("client handshake");
        assert_eq!(
            client_conn.congestion_state().initial_window(),
            crate::carrier::MODE1_WINDOW,
            "mode 1 listener grew after a short sample"
        );
        assert_eq!(
            server_conn.congestion_state().initial_window(),
            crate::carrier::MODE1_WINDOW
        );
        apply_agreed_mode(&server_conn, crate::carrier::QUIC_STREAMS);
        assert_eq!(
            server_conn.congestion_state().initial_window(),
            crate::carrier::MODE1_WINDOW
        );
        // One real round trip. A smoothed sample around 5901 µs must not hide
        // a minimum under 2 ms, and nothing here writes a fake RTT.
        let mut send = client_conn.open_uni().await.unwrap();
        send.write_all(&[1u8; 32]).await.unwrap();
        send.finish().unwrap();
        let mut recv = server_conn.accept_uni().await.unwrap();
        let mut buf = [0u8; 32];
        recv.read_exact(&mut buf).await.unwrap();
        let smoothed = server_conn.rtt();
        let fastest = server_conn.min_rtt();
        assert!(
            smoothed >= Duration::from_millis(2) && fastest < Duration::from_millis(2),
            "smoothed {} us min {} us",
            smoothed.as_micros(),
            fastest.as_micros()
        );
        assert_eq!(
            server_conn.congestion_state().initial_window(),
            crate::carrier::MODE1_WINDOW,
            "smoothed RTT above 2 ms must not by itself leave the built window"
        );
        apply_agreed_mode(&server_conn, crate::carrier::QUIC_DATAGRAMS);
        assert_eq!(
            server_conn.congestion_state().initial_window(),
            crate::carrier::MODE2_WINDOW
        );
        assert_eq!(server_conn.rtt(), Duration::from_millis(1));
        assert_eq!(
            server_conn.configured_max_ack_delay(),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            client_conn.congestion_state().initial_window(),
            crate::carrier::MODE1_WINDOW,
            "retuning the server must not widen the client"
        );
        client_conn.close(0u32.into(), b"done");
        crate::carrier::reset_udp_rtt();
    }

    #[tokio::test]
    async fn ipv4_player_reaches_us_when_another_program_holds_the_port() {
        let (cert, key) = certs();
        let port = free_port();
        // Stands in for RoSE on AN515: a plain IPv4 socket on our port.
        let other = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).unwrap();
        // Discovery asks before the listeners bind, and must name their port.
        let announced = udp_port_for(port);
        assert_ne!(announced, port);
        let listeners = udp_listeners(port, &cert, &key);
        assert_eq!(listeners.port, announced, "{}", listeners.note);
        assert!(
            listeners.note.contains("another program"),
            "{}",
            listeners.note
        );
        let server = listeners.v4.as_ref().expect("IPv4 listener");
        let alpn = handshake(listeners.port, server).await.unwrap();
        assert_eq!(alpn, crate::negotiate::ALPN_NEGOTIATE);
        drop(other);
    }

    #[tokio::test]
    async fn free_port_is_kept_and_ipv4_has_its_own_socket() {
        let (cert, key) = certs();
        let port = free_port();
        let listeners = udp_listeners(port, &cert, &key);
        assert_eq!(listeners.port, port, "{}", listeners.note);
        let server = listeners.v4.as_ref().expect("IPv4 listener");
        assert_eq!(
            handshake(port, server).await.unwrap(),
            crate::negotiate::ALPN_NEGOTIATE
        );
    }

    /// A player built before negotiation lists only `etherquic`. The new
    /// host must still take it, on the legacy ALPN.
    #[tokio::test]
    async fn old_player_gets_the_legacy_alpn() {
        let (cert, key) = certs();
        let port = free_port();
        let listeners = udp_listeners(port, &cert, &key);
        let server = listeners.v4.as_ref().expect("IPv4 listener");
        let mut rustls_client = tls::load_client_config(true).unwrap();
        rustls_client.alpn_protocols = vec![tls::ALPN_ETHERQUIC.to_vec()];
        let client_config =
            ClientConfig::new(Arc::new(QuicClientConfig::try_from(rustls_client).unwrap()));
        crate::note_quic_bind();
        let client = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let dial = client
            .connect_with(
                client_config,
                SocketAddr::from((Ipv4Addr::LOCALHOST, listeners.port)),
                "localhost",
            )
            .unwrap();
        let accept = async { server.accept().await.unwrap().await.unwrap() };
        let (client_side, server_side) = tokio::join!(dial, accept);
        let client_side = client_side.unwrap();
        assert!(!tls::negotiates(&client_side));
        assert!(!tls::negotiates(&server_side));
        assert_eq!(tls::agreed_alpn(&server_side).unwrap(), tls::ALPN_ETHERQUIC);
    }
}
