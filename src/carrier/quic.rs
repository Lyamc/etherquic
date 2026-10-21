//! QUIC carriers. Mode 1 is streams and does not emit datagrams. Mode 2
//! puts picture and audio on datagrams only after `carrier 2` is agreed.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// First mode-2 UDP RTT sample, in microseconds. `u64::MAX` means unset.
static UDP_RTT_US: AtomicU64 = AtomicU64::new(u64::MAX);

/// Remember the first mode-2 UDP RTT. A later listener uses it for the window.
pub fn observe_udp_rtt(rtt: Duration, datagrams: bool) {
    if !datagrams || rtt.is_zero() {
        return;
    }
    let us = u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX);
    let _ = UDP_RTT_US.compare_exchange(u64::MAX, us, Ordering::Relaxed, Ordering::Relaxed);
}

pub fn udp_rtt_sample() -> Option<Duration> {
    let us = UDP_RTT_US.load(Ordering::Relaxed);
    if us == u64::MAX {
        None
    } else {
        Some(Duration::from_micros(us))
    }
}

pub fn reset_udp_rtt() {
    UDP_RTT_US.store(u64::MAX, Ordering::Relaxed);
}

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{
    congestion::CubicConfig, AckFrequencyConfig, ClientConfig, Connection, Endpoint,
    EndpointConfig, ServerConfig, TokioRuntime, TransportConfig, VarInt,
};

use super::assemble::{split_record, Assembler, Lane, Slice};
use super::{agree, QUIC_DATAGRAMS, QUIC_STREAMS};
use crate::fabric::Fabric;
use crate::socket::EtherSocket;
use crate::tls::{self, ALPN_ETHERQUIC};
use crate::Mac;

/// Quinn's mode-1 window stays the small default. This is the value mode 2
/// must not use on a long path, and must not replace in mode 1.
pub const MODE1_WINDOW: u64 = 12_000;
pub const MODE2_WINDOW: u64 = 256 * 1024;
const MODE1_RTT: Duration = Duration::from_millis(333);
const MODE2_RTT: Duration = Duration::from_millis(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathProfile {
    pub initial_rtt: Duration,
    pub initial_window: u64,
    pub max_ack_delay: Duration,
    pub ack_eliciting: u64,
    pub short: bool,
}

/// Mode 1 keeps the long-path numbers. Mode 2 uses them only when the first
/// UDP RTT is 2 ms or more. L2, and a faster UDP sample, use the short path.
pub fn profile_for(mode: u8, l2: bool, first_rtt: Option<Duration>) -> PathProfile {
    let short = mode == QUIC_DATAGRAMS
        && (l2 || first_rtt.is_some_and(|rtt| rtt < Duration::from_millis(2)));
    if short {
        PathProfile {
            initial_rtt: MODE2_RTT,
            initial_window: MODE2_WINDOW,
            max_ack_delay: MODE2_RTT,
            ack_eliciting: 0,
            short: true,
        }
    } else {
        PathProfile {
            initial_rtt: MODE1_RTT,
            initial_window: MODE1_WINDOW,
            max_ack_delay: MODE1_RTT,
            ack_eliciting: 1,
            short: false,
        }
    }
}

/// Apply a mode-2 short path. Mode 1 leaves the congestion window alone.
pub fn apply_mode(transport: &mut TransportConfig, mode: u8, l2: bool, first_rtt: Option<Duration>) {
    let profile = profile_for(mode, l2, first_rtt);
    if mode == QUIC_DATAGRAMS {
        transport.initial_rtt(profile.initial_rtt);
        let mut cubic = CubicConfig::default();
        cubic.initial_window(profile.initial_window);
        transport.congestion_controller_factory(Arc::new(cubic));
        if profile.short {
            let mut ack = AckFrequencyConfig::default();
            ack.max_ack_delay(Some(profile.max_ack_delay));
            ack.ack_eliciting_threshold(VarInt::from_u32(0));
            transport.ack_frequency_config(Some(ack));
        }
        transport.datagram_receive_buffer_size(Some(1024 * 1024));
        transport.datagram_send_buffer_size(1024 * 1024);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    pub datagrams: usize,
    pub stream_bytes: Vec<u8>,
    pub record: Option<Vec<u8>>,
    pub refused: bool,
}

/// Fresh connection on the in-memory fabric. Mode 1 writes one stream record
/// and sends no datagrams. Mode 2 sends datagrams only when carrier 2 agreed.
pub async fn exchange_picture(
    cert: &Path,
    key: &Path,
    mode: u8,
    agreed: bool,
    record: &[u8],
) -> Result<Exchange, String> {
    if mode == QUIC_DATAGRAMS && !agreed {
        // Carrier 2 is refused unless the offer asked for it and the host
        // announced it. Nothing is connected, so no datagram can leave.
        if agree(true, Some(QUIC_DATAGRAMS), &[QUIC_STREAMS]).is_err() {
            return Ok(Exchange {
                datagrams: 0,
                stream_bytes: Vec::new(),
                record: None,
                refused: true,
            });
        }
    }
    if mode == QUIC_DATAGRAMS {
        agree(true, Some(QUIC_DATAGRAMS), &[QUIC_STREAMS, QUIC_DATAGRAMS])?;
    }
    let fabric = Fabric::new(1500);
    let (server, client, addr) = endpoints(fabric, cert, key, mode)?;
    let accept = tokio::spawn(async move {
        let incoming = server.accept().await.ok_or("no connection")?;
        let conn = incoming.await.map_err(|e| e.to_string())?;
        read_exchange(&conn, mode).await
    });
    let conn = client
        .connect(addr, "etherquic")
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    send_picture(&conn, mode, record).await?;
    let got = accept.await.map_err(|e| e.to_string())??;
    conn.close(0u32.into(), b"done");
    Ok(got)
}

async fn send_picture(conn: &Connection, mode: u8, record: &[u8]) -> Result<(), String> {
    if mode != QUIC_DATAGRAMS {
        let mut send = conn.open_uni().await.map_err(|e| e.to_string())?;
        send.write_all(record).await.map_err(|e| e.to_string())?;
        send.finish().map_err(|e| e.to_string())?;
        return Ok(());
    }
    // Config finishes on the reliable stream before the first picture datagram.
    let mut send = conn.open_uni().await.map_err(|e| e.to_string())?;
    send.write_all(b"eqneg 1\ncarrier 2\n")
        .await
        .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    let budget = conn.max_datagram_size().unwrap_or(1200).saturating_sub(16).max(32);
    for slice in split_record(1, record, budget) {
        let mut body = vec![Lane::Picture as u8];
        body.extend(slice.encode());
        conn.send_datagram(body.into()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn read_exchange(conn: &Connection, mode: u8) -> Result<Exchange, String> {
    if mode != QUIC_DATAGRAMS {
        let mut recv = conn.accept_uni().await.map_err(|e| e.to_string())?;
        let mut stream_bytes = Vec::new();
        recv.read_to_end(1024 * 1024)
            .await
            .map_err(|e| e.to_string())?
            .clone_into(&mut stream_bytes);
        let datagrams = match tokio::time::timeout(Duration::from_millis(40), conn.read_datagram()).await
        {
            Ok(Ok(_)) => 1,
            _ => 0,
        };
        return Ok(Exchange {
            datagrams,
            record: Some(stream_bytes.clone()),
            stream_bytes,
            refused: false,
        });
    }
    let mut recv = conn.accept_uni().await.map_err(|e| e.to_string())?;
    let stream_bytes = recv
        .read_to_end(1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    if !stream_bytes.windows(9).any(|window| window == b"carrier 2") {
        return Err("config did not finish before the picture datagrams".into());
    }
    let mut asm = Assembler::new();
    let mut datagrams = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut record = None;
    while tokio::time::Instant::now() < deadline {
        let packet = tokio::time::timeout(Duration::from_millis(200), conn.read_datagram())
            .await
            .map_err(|_| "datagram timeout")?
            .map_err(|e| e.to_string())?;
        datagrams += 1;
        if packet.first().copied() != Some(Lane::Picture as u8) {
            continue;
        }
        let Some(slice) = Slice::decode(&packet[1..]) else {
            continue;
        };
        let now = std::time::Instant::now();
        if let Some(done) = asm.push(slice, now, now + Duration::from_secs(2)) {
            record = Some(done);
            break;
        }
    }
    Ok(Exchange {
        datagrams,
        stream_bytes,
        record,
        refused: false,
    })
}

fn endpoints(
    fabric: Arc<Fabric>,
    cert: &Path,
    key: &Path,
    mode: u8,
) -> Result<(Endpoint, Endpoint, std::net::SocketAddr), String> {
    tls::install_pure_crypto();
    let server = endpoint(fabric.clone(), [2, 0, 0, 0, 0, 1], true, cert, key, mode)?;
    let client = endpoint(fabric, [2, 0, 0, 0, 0, 2], false, cert, key, mode)?;
    let addr = crate::peer_to_socket_addr([2, 0, 0, 0, 0, 1], 4433);
    Ok((server, client, addr))
}

fn endpoint(
    fabric: Arc<Fabric>,
    mac: Mac,
    server: bool,
    cert: &Path,
    key: &Path,
    mode: u8,
) -> Result<Endpoint, String> {
    let max = fabric.max_quic_datagram() as u16;
    let mut transport = TransportConfig::default();
    transport.initial_mtu(max);
    transport.min_mtu(1200.min(max));
    transport.mtu_discovery_config(None);
    apply_mode(&mut transport, mode, true, None);
    let mut endpoint_config = EndpointConfig::default();
    endpoint_config
        .max_udp_payload_size(max)
        .map_err(|e| e.to_string())?;
    let socket = EtherSocket::open(fabric, mac, 4433);
    let runtime = Arc::new(TokioRuntime);
    if server {
        let rustls_server = tls::load_server_config(cert, key).map_err(|e| e.to_string())?;
        let crypto = QuicServerConfig::try_from(rustls_server).map_err(|e| e.to_string())?;
        let mut server_config = ServerConfig::with_crypto(Arc::new(crypto));
        server_config.transport_config(Arc::new(transport));
        crate::note_quic_bind();
        Endpoint::new_with_abstract_socket(endpoint_config, Some(server_config), socket, runtime)
            .map_err(|e| e.to_string())
    } else {
        let rustls_client = tls::load_client_config(true).map_err(|e| e.to_string())?;
        let crypto = QuicClientConfig::try_from(rustls_client).map_err(|e| e.to_string())?;
        let mut client_config = ClientConfig::new(Arc::new(crypto));
        client_config.transport_config(Arc::new(transport));
        crate::note_quic_bind();
        let endpoint =
            Endpoint::new_with_abstract_socket(endpoint_config, None, socket, runtime)
                .map_err(|e| e.to_string())?;
        endpoint.set_default_client_config(client_config);
        let _ = ALPN_ETHERQUIC;
        Ok(endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_1_keeps_the_long_window_and_mode_2_shortens_only_a_short_path() {
        let mode1 = profile_for(QUIC_STREAMS, true, Some(Duration::from_micros(500)));
        assert!(!mode1.short);
        assert_eq!(mode1.initial_window, MODE1_WINDOW);
        assert_eq!(mode1.initial_rtt, MODE1_RTT);
        let short = profile_for(QUIC_DATAGRAMS, false, Some(Duration::from_micros(500)));
        assert!(short.short);
        assert_eq!(short.initial_window, MODE2_WINDOW);
        assert_eq!(short.max_ack_delay, Duration::from_millis(1));
        assert_eq!(short.ack_eliciting, 0);
        let l2 = profile_for(QUIC_DATAGRAMS, true, None);
        assert!(l2.short);
        let long = profile_for(QUIC_DATAGRAMS, false, Some(Duration::from_millis(2)));
        assert!(!long.short);
        assert_eq!(long.initial_window, MODE1_WINDOW);
        assert_eq!(long.initial_rtt, MODE1_RTT);
        reset_udp_rtt();
        assert!(udp_rtt_sample().is_none());
        observe_udp_rtt(Duration::from_millis(5), false);
        assert!(udp_rtt_sample().is_none(), "mode 1 does not arm the short path");
        observe_udp_rtt(Duration::from_millis(5), true);
        let slow = profile_for(QUIC_DATAGRAMS, false, udp_rtt_sample());
        assert!(!slow.short);
        reset_udp_rtt();
        observe_udp_rtt(Duration::from_micros(800), true);
        let fast = profile_for(QUIC_DATAGRAMS, false, udp_rtt_sample());
        assert!(fast.short);
        assert_eq!(fast.initial_window, MODE2_WINDOW);
        assert_eq!(fast.initial_rtt, MODE2_RTT);
        reset_udp_rtt();
    }

    #[tokio::test]
    async fn l2_mode_2_retunes_that_connection_and_mode_1_stays_at_12000() {
        use std::path::PathBuf;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("certs");
        let fabric = Fabric::new(1500);
        let (server, client, addr) = endpoints(
            fabric,
            &root.join("cert.pem"),
            &root.join("key.pem"),
            QUIC_STREAMS,
        )
        .unwrap();
        let accept = tokio::spawn(async move { server.accept().await.unwrap().await.unwrap() });
        let client = client
            .connect(addr, "etherquic")
            .unwrap()
            .await
            .unwrap();
        let server = accept.await.unwrap();
        assert_eq!(client.congestion_state().initial_window(), MODE1_WINDOW);
        assert_eq!(server.congestion_state().initial_window(), MODE1_WINDOW);
        crate::l2_endpoint::apply_agreed_mode(&server, QUIC_STREAMS);
        assert_eq!(server.congestion_state().initial_window(), MODE1_WINDOW);
        crate::l2_endpoint::apply_agreed_mode(&server, QUIC_DATAGRAMS);
        assert_eq!(server.congestion_state().initial_window(), MODE2_WINDOW);
        assert_eq!(server.rtt(), MODE2_RTT);
        assert_eq!(
            server.configured_max_ack_delay(),
            Some(Duration::from_millis(1))
        );
        assert_eq!(client.congestion_state().initial_window(), MODE1_WINDOW);
        client.close(0u32.into(), b"done");
    }
}
