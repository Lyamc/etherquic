//! Quinn endpoints whose datagrams ride a borderless-style pcap/Npcap link.

use std::fmt;
use std::io::{self, IoSliceMut};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{
    AsyncUdpSocket, ClientConfig, Endpoint, EndpointConfig, ServerConfig, TokioRuntime,
    TransportConfig, UdpSender,
};

use crate::addr::socket_addr_to_peer;
use crate::endpoint::EndpointError;
use crate::fabric::Fabric;
use crate::frame::Frame;
use crate::pcap_link;
use crate::socket::EtherSocket;
use crate::tls;
use crate::Mac;

/// Shared outbound queue + fabric used by L2 Quinn sockets.
pub struct L2Net {
    pub fabric: Arc<Fabric>,
    out: std::sync::mpsc::Sender<Vec<u8>>,
    ethertype: u16,
}

impl L2Net {
    pub fn open(nic: Option<&str>, mtu: usize, ethertype: u16) -> Result<Arc<Self>, EndpointError> {
        let fabric = Fabric::with_ether_type(mtu, ethertype);
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let mut io = open_packet_io(nic, ethertype)?;
        eprintln!(
            "etherquic L2: backend={} ethertype=0x{ethertype:04x} mtu={mtu}",
            io.name()
        );

        let fabric_rx = fabric.clone();
        thread::Builder::new()
            .name("etherquic-l2-io".into())
            .spawn(move || loop {
                // Wait on the send queue first. A pcap read can block until a
                // frame arrives, so a read-first loop never transmits.
                // 1ms instead of 5ms: that wait was added to every round trip.
                match rx.recv_timeout(Duration::from_millis(1)) {
                    Ok(frame) => {
                        if let Err(e) = io.send(&frame) {
                            eprintln!("L2 send error: {e}");
                        }
                        while let Ok(more) = rx.try_recv() {
                            if let Err(e) = io.send(&more) {
                                eprintln!("L2 send error: {e}");
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let batch = match io.recv_batch(64) {
                    Ok(batch) => batch,
                    Err(e) => {
                        eprintln!("L2 recv error: {e}");
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                };
                for raw in batch {
                    if let Ok(frame) = Frame::decode_filtered(&raw, ethertype) {
                        fabric_rx.inject(frame);
                    }
                }
            })
            .map_err(|e| EndpointError::Other(e.to_string()))?;

        Ok(Arc::new(Self {
            fabric,
            out: tx,
            ethertype,
        }))
    }

    pub fn ethertype(&self) -> u16 {
        self.ethertype
    }
}

fn transport_for_mtu(max_datagram: u16) -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport.initial_mtu(max_datagram);
    transport.min_mtu(1200.min(max_datagram));
    transport.mtu_discovery_config(None);
    transport
}

pub fn l2_server_endpoint(
    net: Arc<L2Net>,
    local_mac: Mac,
    service_id: u16,
    cert_pem: &Path,
    key_pem: &Path,
) -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let max = net.fabric.max_quic_datagram() as u16;

    let rustls_server = tls::load_server_config(cert_pem, key_pem)?;
    let mut server_config =
        ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(rustls_server).map_err(
            |e| EndpointError::Other(format!("QuicServerConfig: {e}")),
        )?));
    server_config.transport_config(Arc::new(transport_for_mtu(max)));

    let mut endpoint_config = EndpointConfig::default();
    endpoint_config.max_udp_payload_size(max)?;

    let socket = L2Socket::open(net, local_mac, service_id);
    let runtime = Arc::new(TokioRuntime);
    Ok(Endpoint::new_with_abstract_socket(
        endpoint_config,
        Some(server_config),
        socket,
        runtime,
    )?)
}

pub fn l2_client_endpoint(
    net: Arc<L2Net>,
    local_mac: Mac,
    service_id: u16,
) -> Result<Endpoint, EndpointError> {
    tls::install_pure_crypto();
    let max = net.fabric.max_quic_datagram() as u16;

    let rustls_client = tls::load_client_config(true)?;
    let mut client_config =
        ClientConfig::new(Arc::new(QuicClientConfig::try_from(rustls_client).map_err(
            |e| EndpointError::Other(format!("QuicClientConfig: {e}")),
        )?));
    client_config.transport_config(Arc::new(transport_for_mtu(max)));

    let mut endpoint_config = EndpointConfig::default();
    endpoint_config.max_udp_payload_size(max)?;

    let socket = L2Socket::open(net, local_mac, service_id);
    let runtime = Arc::new(TokioRuntime);
    let endpoint = Endpoint::new_with_abstract_socket(endpoint_config, None, socket, runtime)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

struct L2Socket {
    inner: Box<EtherSocket>,
    net: Arc<L2Net>,
    mac: Mac,
    service_id: u16,
}

impl L2Socket {
    fn open(net: Arc<L2Net>, mac: Mac, service_id: u16) -> Box<Self> {
        let inner = EtherSocket::open(net.fabric.clone(), mac, service_id);
        Box::new(Self {
            inner,
            net,
            mac,
            service_id,
        })
    }
}

impl fmt::Debug for L2Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("L2Socket")
            .field("mac", &self.mac)
            .field("service_id", &self.service_id)
            .finish()
    }
}

impl AsyncUdpSocket for L2Socket {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        Box::pin(L2Sender {
            net: self.net.clone(),
            mac: self.mac,
            service_id: self.service_id,
            max_datagram: self.net.fabric.max_quic_datagram(),
            ethertype: self.net.ethertype(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }

    fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.inner.local_addr()
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

struct L2Sender {
    net: Arc<L2Net>,
    mac: Mac,
    service_id: u16,
    max_datagram: usize,
    ethertype: u16,
}

impl fmt::Debug for L2Sender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("L2Sender").field("mac", &self.mac).finish()
    }
}

impl UdpSender for L2Sender {
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        _cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(self.try_send(transmit))
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }
}

impl L2Sender {
    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        let (dst_mac, dst_service) = socket_addr_to_peer(transmit.destination).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "not an etherquic peer")
        })?;
        if transmit.contents.len() > self.max_datagram {
            return Err(io::Error::from_raw_os_error(90));
        }
        let frame = Frame {
            dst: dst_mac,
            src: self.mac,
            ether_type: self.ethertype,
            dst_service,
            src_service: self.service_id,
            payload: transmit.contents.to_vec(),
        };
        let bytes = frame.encode();
        self.net
            .out
            .send(bytes)
            .map_err(|e| io::Error::other(format!("L2 outbound queue closed: {e}")))?;
        Ok(())
    }
}

enum PacketIo {
    Pcap(pcap_link::Link),
    #[cfg(windows)]
    Ndis(crate::ndis_link::NdisLink),
}

impl PacketIo {
    fn name(&self) -> &'static str {
        match self {
            PacketIo::Pcap(_) => "pcap",
            #[cfg(windows)]
            PacketIo::Ndis(_) => "rust-ndis",
        }
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), String> {
        match self {
            PacketIo::Pcap(link) => link.send(frame).map_err(|e| e.to_string()),
            #[cfg(windows)]
            PacketIo::Ndis(link) => link.send(frame).map_err(|e| e.to_string()),
        }
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.recv_batch(1)
            .map(|mut batch| batch.pop())
    }

    fn recv_batch(&mut self, max: usize) -> Result<Vec<Vec<u8>>, String> {
        let mut out = Vec::new();
        let limit = match self {
            // A pcap read blocks until the capture timeout. One frame per
            // turn keeps the send queue moving.
            PacketIo::Pcap(_) => 1,
            #[cfg(windows)]
            PacketIo::Ndis(_) => max.max(1),
        };
        for _ in 0..limit {
            match self.recv_one()? {
                Some(frame) => out.push(frame),
                None => break,
            }
        }
        Ok(out)
    }

    fn recv_one(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self {
            PacketIo::Pcap(link) => link.recv().map_err(|e| e.to_string()),
            #[cfg(windows)]
            PacketIo::Ndis(link) => {
                let mut buf = vec![0u8; 2048];
                match link.recv(&mut buf) {
                    Ok(Some(n)) => {
                        buf.truncate(n);
                        Ok(Some(buf))
                    }
                    Ok(None) => Ok(None),
                    Err(e) => Err(e.to_string()),
                }
            }
        }
    }
}

fn open_packet_io(nic: Option<&str>, ethertype: u16) -> Result<PacketIo, EndpointError> {
    #[cfg(windows)]
    if let Some(ndis) =
        crate::ndis_link::NdisLink::try_open().map_err(|e| EndpointError::Other(e.to_string()))?
    {
        return Ok(PacketIo::Ndis(ndis));
    }
    let link = pcap_link::open(nic, ethertype).map_err(|e| EndpointError::Other(e.to_string()))?;
    Ok(PacketIo::Pcap(link))
}

