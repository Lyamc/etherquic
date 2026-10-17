//! Quinn endpoints whose datagrams ride a borderless-style pcap/Npcap link.

use std::fmt;
use std::io::{self, IoSliceMut};
use std::path::Path;
use std::pin::Pin;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{
    AsyncUdpSocket, ClientConfig, Endpoint, EndpointConfig, IdleTimeout, ServerConfig,
    TokioRuntime, TransportConfig, UdpSender, VarInt,
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
    l2_backend: u8,
    hw_mac: Option<Mac>,
    adapter: Option<String>,
}

impl L2Net {
    pub fn open(nic: Option<&str>, mtu: usize, ethertype: u16) -> Result<Arc<Self>, EndpointError> {
        let fabric = Fabric::with_ether_type(mtu, ethertype);
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let io = open_packet_io(nic, ethertype)?;
        let l2_backend = io.l2_backend();
        let hw_mac = io.hw_mac();
        let adapter = io.adapter();
        eprintln!(
            "etherquic L2: backend={} ethertype=0x{ethertype:04x} mtu={mtu} adapter={}",
            io.name(),
            hw_mac
                .map(|m| m
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(":"))
                .unwrap_or_else(|| "unknown".into())
        );

        let fabric_rx = fabric.clone();
        // Membrane's read waits inside the driver until a frame arrives.
        // pcap returns at once. A read on the send thread leaves the first
        // picture queued until the peer sends again.
        #[cfg(windows)]
        if io.blocks_in_recv() {
            match open_dedicated_reader() {
                Ok(reader) => spawn_split_io(rx, io, reader, fabric_rx, ethertype)?,
                Err(error) => {
                    eprintln!(
                        "L2 recv could not open a second Membrane handle ({error}); sends wait for reads"
                    );
                    spawn_coupled_io(rx, io, fabric_rx, ethertype)?;
                }
            }
        } else {
            spawn_coupled_io(rx, io, fabric_rx, ethertype)?;
        }
        #[cfg(not(windows))]
        spawn_coupled_io(rx, io, fabric_rx, ethertype)?;

        Ok(Arc::new(Self {
            fabric,
            out: tx,
            ethertype,
            l2_backend,
            hw_mac,
            adapter,
        }))
    }

    /// One fabric, no adapter. Outbound frames are delivered back into that fabric.
    ///
    /// A host and a player that share this net talk without opening a NIC.
    pub fn memory() -> Arc<Self> {
        let ethertype = crate::ETHERTYPE_QUIC;
        let fabric = Fabric::with_ether_type(1500, ethertype);
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let fabric_rx = fabric.clone();
        thread::Builder::new()
            .name("etherquic-l2-memory".into())
            .spawn(move || {
                while let Ok(bytes) = rx.recv() {
                    if let Ok(frame) = Frame::decode_filtered(&bytes, ethertype) {
                        fabric_rx.inject(frame);
                    }
                }
            })
            .expect("memory l2 thread");
        Arc::new(Self {
            fabric,
            out: tx,
            ethertype,
            l2_backend: 0,
            hw_mac: None,
            adapter: None,
        })
    }

    pub fn ethertype(&self) -> u16 {
        self.ethertype
    }

    /// [`crate::discover::L2_LIBPCAP`], [`crate::discover::L2_NPCAP`], or
    /// [`crate::discover::L2_MEMBRANE`]. This is the driver the session opened.
    pub fn l2_backend(&self) -> u8 {
        self.l2_backend
    }

    /// The adapter's own MAC, when the OS reports it.
    pub fn hardware_mac(&self) -> Option<Mac> {
        self.hw_mac
    }

    /// The pcap device name this net opened. `None` for Membrane.
    pub fn adapter(&self) -> Option<&str> {
        self.adapter.as_deref()
    }
}

/// Writes queued frames on this thread. A blocked recv must be a different thread.
///
/// Linux pcap does not call this. The unit test does, and Windows Membrane does.
#[cfg_attr(not(windows), allow(dead_code))]
fn drive_send(rx: Receiver<Vec<u8>>, mut send: impl FnMut(&[u8])) {
    while let Ok(frame) = rx.recv() {
        send(&frame);
        while let Ok(more) = rx.try_recv() {
            send(&more);
        }
    }
}

fn deliver_raw(fabric: &Fabric, ethertype: u16, raw: &[u8]) {
    if let Ok(frame) = Frame::decode_filtered(raw, ethertype) {
        // Announcements share this EtherType. They are not QUIC datagrams.
        if !crate::discover::is_discovery(&frame) {
            fabric.inject(frame);
        }
    }
}

fn spawn_coupled_io(
    rx: Receiver<Vec<u8>>,
    mut io: PacketIo,
    fabric: Arc<Fabric>,
    ethertype: u16,
) -> Result<(), EndpointError> {
    thread::Builder::new()
        .name("etherquic-l2-io".into())
        .spawn(move || {
            loop {
                // Wait on the send queue first. A read that blocks until a
                // frame arrives must not run on this thread: the queued
                // picture would sit until the peer sends again.
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
                    deliver_raw(&fabric, ethertype, &raw);
                }
            }
        })
        .map_err(|e| EndpointError::Other(e.to_string()))?;
    Ok(())
}

#[cfg(windows)]
fn open_dedicated_reader() -> Result<crate::ndis_link::NdisLink, String> {
    match crate::ndis_link::NdisLink::try_open().map_err(|e| e.to_string())? {
        Some(link) => Ok(link),
        None => Err("Membrane device is not open".into()),
    }
}

#[cfg(windows)]
fn spawn_split_io(
    rx: Receiver<Vec<u8>>,
    mut io: PacketIo,
    reader: crate::ndis_link::NdisLink,
    fabric: Arc<Fabric>,
    ethertype: u16,
) -> Result<(), EndpointError> {
    let stop = Arc::new(RecvStop::default());
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let stop_recv = Arc::clone(&stop);
    thread::Builder::new()
        .name("etherquic-l2-recv".into())
        .spawn(move || {
            recv_on_its_own(&reader, &stop_recv, &fabric, ethertype);
            let _ = done_tx.send(());
        })
        .map_err(|e| EndpointError::Other(e.to_string()))?;
    let stop_send = Arc::clone(&stop);
    thread::Builder::new()
        .name("etherquic-l2-io".into())
        .spawn(move || {
            drive_send(rx, move |frame| {
                if let Err(e) = io.send(frame) {
                    eprintln!("L2 send error: {e}");
                }
            });
            stop_send.cancel_until(&done_rx, Duration::from_millis(500));
        })
        .map_err(|e| EndpointError::Other(e.to_string()))?;
    Ok(())
}

#[cfg(windows)]
fn recv_on_its_own(
    reader: &crate::ndis_link::NdisLink,
    stop: &RecvStop,
    fabric: &Fabric,
    ethertype: u16,
) {
    stop.arm(reader.raw_handle());
    let mut buf = vec![0u8; 2048];
    while !stop.stopped() {
        let mut got = 0usize;
        for _ in 0..64 {
            if stop.stopped() {
                return;
            }
            match reader.recv(&mut buf) {
                Ok(Some(n)) => {
                    let n = n.min(buf.len());
                    got += 1;
                    deliver_raw(fabric, ethertype, &buf[..n]);
                }
                Ok(None) => break,
                Err(error) => {
                    if stop.stopped() {
                        return;
                    }
                    eprintln!("L2 recv error: {error}");
                    thread::sleep(Duration::from_millis(50));
                    break;
                }
            }
        }
        // An immediate empty read must not spin. A read that waits in the
        // driver returns only when a frame arrives, and never reaches here.
        if got == 0 && !stop.stopped() {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(windows)]
struct RecvStop {
    stop: std::sync::atomic::AtomicBool,
    handle: std::sync::atomic::AtomicPtr<core::ffi::c_void>,
}

#[cfg(windows)]
impl Default for RecvStop {
    fn default() -> Self {
        Self {
            stop: std::sync::atomic::AtomicBool::new(false),
            handle: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

#[cfg(windows)]
impl RecvStop {
    fn arm(&self, handle: windows_sys::Win32::Foundation::HANDLE) {
        self.handle
            .store(handle, std::sync::atomic::Ordering::SeqCst);
    }

    fn stopped(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Wake a read that is waiting in the driver, then wait until it leaves.
    fn cancel_until(&self, done: &Receiver<()>, budget: Duration) {
        use std::sync::atomic::Ordering;
        use std::sync::mpsc::RecvTimeoutError;
        use std::time::Instant;

        self.stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + budget;
        loop {
            let handle = self.handle.load(Ordering::SeqCst);
            if !handle.is_null() {
                crate::ndis_link::NdisLink::cancel_io(handle);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match done.recv_timeout(left.min(Duration::from_millis(10))) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
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
    let mut server_config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(rustls_server)
            .map_err(|e| EndpointError::Other(format!("QuicServerConfig: {e}")))?,
    ));
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
    let mut client_config = ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(rustls_client)
            .map_err(|e| EndpointError::Other(format!("QuicClientConfig: {e}")))?,
    ));
    let mut transport = transport_for_mtu(max);
    // Six seconds with no packet ends the session, so a stopped service or a
    // dead cable closes the view. The ping is shorter than that, so a quiet
    // desktop still stays up. The host offers 30s; QUIC uses the smaller one.
    transport.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(6_000))));
    transport.keep_alive_interval(Some(Duration::from_secs(2)));
    client_config.transport_config(Arc::new(transport));

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
        // On Wi-Fi the peer sees, and answers, the adapter's MAC.
        if let Some(hardware) = net.hw_mac {
            net.fabric.alias_hardware(hardware, mac);
        }
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
        let (dst_mac, dst_service) = socket_addr_to_peer(transmit.destination)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not an etherquic peer"))?;
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

    fn l2_backend(&self) -> u8 {
        match self {
            #[cfg(windows)]
            PacketIo::Pcap(_) => crate::discover::L2_NPCAP,
            #[cfg(not(windows))]
            PacketIo::Pcap(_) => crate::discover::L2_LIBPCAP,
            #[cfg(windows)]
            PacketIo::Ndis(_) => crate::discover::L2_MEMBRANE,
        }
    }

    fn adapter(&self) -> Option<String> {
        match self {
            PacketIo::Pcap(link) => Some(link.nic.name.clone()),
            #[cfg(windows)]
            PacketIo::Ndis(_) => None,
        }
    }

    fn hw_mac(&self) -> Option<Mac> {
        match self {
            PacketIo::Pcap(link) => link.hw_mac,
            #[cfg(windows)]
            PacketIo::Ndis(_) => None,
        }
    }

    /// True when [`Self::recv_one`] can wait until a frame arrives.
    #[cfg(windows)]
    fn blocks_in_recv(&self) -> bool {
        match self {
            PacketIo::Pcap(_) => false,
            #[cfg(windows)]
            PacketIo::Ndis(_) => true,
        }
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), String> {
        match self {
            PacketIo::Pcap(link) => link.send(frame).map_err(|e| e.to_string()),
            #[cfg(windows)]
            PacketIo::Ndis(link) => link.send(frame).map_err(|e| e.to_string()),
        }
    }

    fn recv_batch(&mut self, max: usize) -> Result<Vec<Vec<u8>>, String> {
        let mut out = Vec::new();
        // pcap reads are non-blocking (see open_nic), so an empty capture
        // returns immediately and the send queue is serviced every turn.
        let limit = max.max(1);
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

static L2_PREFERENCE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Driver the next [`L2Net::open`] uses: 0 picks Membrane when its driver is
/// installed and pcap otherwise, [`crate::discover::L2_MEMBRANE`] needs
/// Membrane, and [`crate::discover::L2_NPCAP`] or
/// [`crate::discover::L2_LIBPCAP`] skips it. Off Windows there is only pcap.
pub fn set_l2_preference(driver: u8) {
    L2_PREFERENCE.store(driver, std::sync::atomic::Ordering::Relaxed);
}

pub fn l2_preference() -> u8 {
    L2_PREFERENCE.load(std::sync::atomic::Ordering::Relaxed)
}

fn open_packet_io(nic: Option<&str>, ethertype: u16) -> Result<PacketIo, EndpointError> {
    #[cfg(windows)]
    {
        let wanted = l2_preference();
        let pcap_only =
            wanted == crate::discover::L2_NPCAP || wanted == crate::discover::L2_LIBPCAP;
        if !pcap_only {
            match crate::ndis_link::NdisLink::try_open()
                .map_err(|e| EndpointError::Other(e.to_string()))?
            {
                Some(ndis) => return Ok(PacketIo::Ndis(ndis)),
                None if wanted == crate::discover::L2_MEMBRANE => {
                    return Err(EndpointError::Other(
                        "the Membrane driver is not installed; pick Automatic or Npcap".into(),
                    ))
                }
                None => {}
            }
        }
    }
    let link = pcap_link::open(nic, ethertype).map_err(|e| EndpointError::Other(e.to_string()))?;
    Ok(PacketIo::Pcap(link))
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::drive_send;

    /// Membrane's read waits in the driver. The send side of that split is
    /// [`drive_send`]: a frame queued while recv has not returned is still written.
    #[test]
    fn a_queued_send_is_written_while_recv_blocks() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sent_thread = Arc::clone(&sent);

        let recv = thread::Builder::new()
            .name("blocked-recv".into())
            .spawn(move || {
                entered_tx.send(()).expect("entered");
                let _ = release_rx.recv();
            })
            .expect("recv thread");

        let send = thread::Builder::new()
            .name("etherquic-l2-io".into())
            .spawn(move || {
                drive_send(rx, move |frame| {
                    sent_thread
                        .lock()
                        .expect("sent")
                        .push(frame.to_vec());
                });
            })
            .expect("send thread");

        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("recv entered");
        tx.send(b"picture".to_vec()).expect("queue");

        let deadline = Instant::now() + Duration::from_secs(1);
        let wrote = loop {
            let ready = !sent.lock().expect("sent").is_empty();
            if ready || Instant::now() >= deadline {
                break ready;
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(wrote, "send stayed behind the blocked recv");
        assert_eq!(sent.lock().expect("sent").as_slice(), &[b"picture".to_vec()]);

        drop(tx);
        let _ = release_tx.send(());
        recv.join().expect("recv joined");
        send.join().expect("send joined");
    }
}
