//! UDP/RTP carrier. No Quinn connection. Input is a sibling datagram.

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use super::assemble::{split_record, Assembler, Slice};
use super::RateCap;

pub const PROFILE_PLAIN: &str = "plain";
pub const PROFILE_GCM: &str = "gcm";
pub const PT_PICTURE: u8 = 96;
pub const PT_AUDIO: u8 = 97;
pub const DEFAULT_CAP: u64 = 100_000_000;

/// Host key bytes QUIC loads from `certs/key.pem`.
pub fn host_key_material() -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("certs/key.pem");
    std::fs::read(&path).unwrap_or_else(|_| b"etherquic-host-key".to_vec())
}

/// X25519 static share derived from the host key.
pub fn host_share() -> KeyShare {
    let mut seed = [0u8; 32];
    let hk = Hkdf::<Sha256>::new(None, &host_key_material());
    hk.expand(b"etherquic-host-x25519", &mut seed)
        .expect("hkdf expand");
    KeyShare::from_seed(seed)
}

/// In-memory player share. The seed is its own IKM, not the host public.
/// A live offer carries a fresh public and finishes the X25519 exchange.
pub fn player_share() -> KeyShare {
    let mut seed = [0u8; 32];
    let hk = Hkdf::<Sha256>::new(None, b"etherquic-player-ephemeral");
    hk.expand(b"etherquic-player-x25519", &mut seed)
        .expect("hkdf expand");
    KeyShare::from_seed(seed)
}

fn fresh_seed() -> [u8; 32] {
    // The OS RNG on every platform. Windows has no /dev/urandom, and the old
    // clock fallback copied 18 bytes into a 16-byte slot and panicked.
    let mut seed = [0u8; 32];
    if getrandom::fill(&mut seed).is_ok() {
        return seed;
    }
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(1);
    let mut seed = [0u8; 32];
    let hk = Hkdf::<Sha256>::new(Some(&tick.to_le_bytes()), b"player-ephemeral");
    hk.expand(b"etherquic-player-fallback", &mut seed)
        .expect("hkdf expand");
    seed
}

/// Default `gcm` key: X25519 against the host key, then HKDF. Not a fixed pattern.
pub fn session_key() -> [u8; 16] {
    let host = host_share();
    let player = player_share();
    derive_aes128(&player.shared(&host.public))
}

/// Picture RTP clock. `capture` is the time the frame was captured.
pub fn picture_rtp_timestamp(capture: Duration) -> u32 {
    ((capture.as_nanos() as u128) * 90_000 / 1_000_000_000) as u32
}

/// Audio RTP clock at the configured sample rate.
pub fn audio_rtp_timestamp(capture: Duration, sample_rate: u32) -> u32 {
    let rate = u128::from(sample_rate.max(1));
    ((capture.as_nanos() as u128) * rate / 1_000_000_000) as u32
}

/// One frame interval, and at least 2 ms.
pub fn picture_deadline(frame: Duration) -> Duration {
    let min = Duration::from_millis(2);
    if frame < min { min } else { frame }
}

/// How long the player waits for the rest of one picture.
///
/// A PyroWave frame is hundreds of kilobytes. One display interval expires
/// while those packets are still arriving, and the assembler then drops the
/// record. The next record id still replaces a picture that never finishes.
pub fn player_picture_wait() -> Duration {
    Duration::from_secs(1)
}

/// Record bytes in one RTP packet. Room for the RTP header, the slice header,
/// and the GCM tag under a 1500-byte Ethernet payload, and under the player's
/// 2048-byte receive buffer.
const RTP_SLICE_BUDGET: usize = 1200;

/// Kernel buffer for one picture or audio socket.
///
/// A PyroWave frame is a few hundred kilobytes and leaves in one burst. The
/// default UDP buffer is about 200 KB, so the tail of that burst is dropped
/// before the player reads it.
const MEDIA_UDP_BUFFER: usize = 4 * 1024 * 1024;

/// Send one datagram.
///
/// On Windows an unconnected send to an IPv6 address assigned on a NIC is
/// dropped, including when that address is on this machine. The socket is
/// connected for the send and then disconnected with `AF_UNSPEC`.
pub(super) fn udp_send(sock: &UdpSocket, packet: &[u8], peer: SocketAddr) -> std::io::Result<usize> {
    #[cfg(windows)]
    if peer.is_ipv6() {
        return udp_send_ipv6(sock, packet, peer);
    }
    sock.send_to(packet, peer)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn udp_send_ipv6(sock: &UdpSocket, packet: &[u8], peer: SocketAddr) -> std::io::Result<usize> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{SOCKADDR, SOCKADDR_IN6, connect, send};

    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let raw = sock.as_raw_socket() as usize;
    let dest = socket2::SockAddr::from(peer);
    let rc = unsafe { connect(raw, dest.as_ptr().cast(), dest.len() as i32) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let n = unsafe { send(raw, packet.as_ptr().cast(), packet.len() as i32, 0) };
    let result = if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    };
    let unspec = unsafe { std::mem::zeroed::<SOCKADDR_IN6>() };
    let _ = unsafe {
        connect(
            raw,
            &unspec as *const SOCKADDR_IN6 as *const SOCKADDR,
            std::mem::size_of::<SOCKADDR_IN6>() as i32,
        )
    };
    result
}

pub(super) fn widen_udp(sock: &UdpSocket) {
    let borrowed = socket2::SockRef::from(sock);
    let _ = borrowed.set_recv_buffer_size(MEDIA_UDP_BUFFER);
    let _ = borrowed.set_send_buffer_size(MEDIA_UDP_BUFFER);
}

pub fn audio_deadline() -> Duration {
    Duration::from_millis(20)
}

/// One RTCP receiver report. `lost` and `sent` since the last report.
pub fn rtcp_receiver_report(sender: u32, source: u32, sent: u64, lost: u64, ext_seq: u32) -> Vec<u8> {
    let fraction = if sent == 0 {
        0u8
    } else {
        ((lost.saturating_mul(256) / sent).min(255)) as u8
    };
    let cumulative = (lost.min(0x00ff_ffff)) as u32;
    let mut out = Vec::with_capacity(32);
    out.push(0x81);
    out.push(201);
    out.extend(7u16.to_be_bytes());
    out.extend(sender.to_be_bytes());
    out.extend(source.to_be_bytes());
    out.push(fraction);
    out.extend((cumulative & 0x00ff_ffff).to_be_bytes()[1..].iter().copied());
    out.extend(ext_seq.to_be_bytes());
    out.extend(0u32.to_be_bytes());
    out.extend(0u32.to_be_bytes());
    out.extend(0u32.to_be_bytes());
    out
}

/// Loss fraction from an RTCP RR, in the range 0.0–1.0.
pub fn rtcp_loss_fraction(packet: &[u8]) -> Option<f64> {
    if packet.len() < 32 || packet[1] != 201 {
        return None;
    }
    Some(f64::from(packet[12]) / 256.0)
}

struct RtcpSeen {
    at: Option<Instant>,
    loss: Option<f64>,
}

static RTCP_SEEN: Mutex<RtcpSeen> = Mutex::new(RtcpSeen {
    at: None,
    loss: None,
});

/// Store one receiver report from the picture socket. A report inside one second is ignored.
pub fn ingest_global_rtcp(now: Instant, packet: &[u8]) -> Option<f64> {
    let fraction = rtcp_loss_fraction(packet)?;
    let mut seen = RTCP_SEEN.lock().unwrap_or_else(|poison| poison.into_inner());
    if seen
        .at
        .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(1))
    {
        return None;
    }
    seen.at = Some(now);
    seen.loss = Some(fraction);
    Some(fraction)
}

pub fn latest_rtcp_loss() -> Option<f64> {
    RTCP_SEEN
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .loss
}

struct RtpHub {
    base: u16,
    picture: UdpSocket,
    audio: UdpSocket,
    picture_v6: Option<UdpSocket>,
    audio_v6: Option<UdpSocket>,
    peer_picture: Mutex<Option<SocketAddr>>,
    peer_audio: Mutex<Option<SocketAddr>>,
    /// Socket the current offer came from. A repeat from that socket is the
    /// player's retry. A different socket is a new player.
    offer_from: Mutex<Option<SocketAddr>>,
    exchanged: Mutex<Option<[u8; 16]>>,
    config_sent: AtomicBool,
    key_seen: AtomicU32,
    stop: AtomicBool,
}

pub(super) fn rtp_owns(port: u16) -> bool {
    let guard = RTP_HUB.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(hub) = guard.as_ref() else {
        return false;
    };
    port == hub.base
        || port == hub.base.saturating_add(1)
        || port == hub.base.saturating_add(2)
}

static RTP_HUB: Mutex<Option<Arc<RtpHub>>> = Mutex::new(None);
static SEALED: AtomicU64 = AtomicU64::new(0);
/// Bumped when the UDP sockets are released or replaced. A seam opened
/// against the previous hub must not keep sending: its packets would land
/// on the next player's GCM session and close it.
static MEDIA_EPOCH: AtomicU64 = AtomicU64::new(1);
/// Carrier id from the player hello. `0` means no hello yet.
static OFFERED: AtomicU8 = AtomicU8::new(0);

pub fn rtp_sealed() -> u64 {
    SEALED.load(Ordering::Relaxed)
}

/// The input socket this process is listening on, or 0.
pub(super) fn bound_input() -> u16 {
    let guard = RTP_HUB.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(hub) = guard.as_ref() else {
        return 0;
    };
    if hub.stop.load(Ordering::Relaxed) {
        return 0;
    }
    hub.base.saturating_add(2)
}

/// The carrier the input thread saw: UDP/RTP, AVB, or ST 2110.
pub fn offered_carrier() -> Option<u8> {
    let id = OFFERED.load(Ordering::Relaxed);
    (id != 0).then_some(id)
}

fn note_offered(id: u8) {
    if id != 0 {
        OFFERED.store(id, Ordering::Relaxed);
    }
}

pub(super) fn media_epoch() -> u64 {
    MEDIA_EPOCH.load(Ordering::Acquire)
}

/// Picture and audio SSRCs. Each seam gets its own, so a packet still in
/// flight from the previous seam cannot move this player's replay window.
pub(super) fn fresh_ssrc() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0xE7C0_0001);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    if n == 0 {
        1
    } else {
        n
    }
}

/// Held across an epoch bump and across the check-then-send of one packet.
/// A send that already passed `media_epoch` cannot leave after the bump.
static EMIT_GATE: Mutex<()> = Mutex::new(());

fn bump_media_epoch() {
    let _gate = EMIT_GATE.lock().unwrap_or_else(|poison| poison.into_inner());
    MEDIA_EPOCH.fetch_add(1, Ordering::AcqRel);
}

/// Send `packet` only while `epoch` is still the hub this seam opened on.
pub(super) fn emit_if_current(epoch: u64, picture: bool, packet: &[u8]) -> bool {
    let _gate = EMIT_GATE.lock().unwrap_or_else(|poison| poison.into_inner());
    if epoch != MEDIA_EPOCH.load(Ordering::Acquire) {
        return false;
    }
    send_udp(picture, packet);
    true
}

pub fn release_rtp() {
    bump_media_epoch();
    OFFERED.store(0, Ordering::Relaxed);
    if let Some(hub) = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner()).take() {
        hub.stop.store(true, Ordering::Relaxed);
    }
}

/// IPv6 sockets beside the IPv4 hub. `None` when this host has no IPv6 stack.
fn bind_v6_trio(port: u16) -> Option<(UdpSocket, UdpSocket, UdpSocket)> {
    let picture = bind_v6(port).ok()?;
    let audio = bind_v6(port.saturating_add(1)).ok()?;
    let input = bind_v6(port.saturating_add(2)).ok()?;
    Some((picture, audio, input))
}

fn bind_v6(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(true)?;
    socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
    Ok(socket.into())
}

/// Player socket in the same family as `host`.
pub(super) fn bind_unspecified(host: SocketAddr) -> Option<UdpSocket> {
    let sock = if host.is_ipv6() {
        bind_v6(0).ok()?
    } else {
        UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?
    };
    widen_udp(&sock);
    Some(sock)
}

/// One bind. A taken port returns without the 50-try wait.
pub(super) fn listen_once(port: u16) {
    if port == 0 {
        return;
    }
    {
        let guard = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner());
        if guard
            .as_ref()
            .is_some_and(|hub| hub.base == port && !hub.stop.load(Ordering::Relaxed))
        {
            return;
        }
    }
    let Ok(picture) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)) else {
        return;
    };
    let Ok(audio) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port.saturating_add(1))) else {
        return;
    };
    let Ok(input) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port.saturating_add(2))) else {
        return;
    };
    let (picture_v6, audio_v6, input_v6) = match bind_v6_trio(port) {
        Some((picture_v6, audio_v6, input_v6)) => {
            widen_udp(&picture_v6);
            widen_udp(&audio_v6);
            widen_udp(&input_v6);
            let _ = input_v6.set_read_timeout(Some(Duration::from_millis(20)));
            (Some(picture_v6), Some(audio_v6), Some(input_v6))
        }
        None => (None, None, None),
    };
    widen_udp(&picture);
    widen_udp(&audio);
    widen_udp(&input);
    let _ = input.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = picture.set_nonblocking(true);
    bump_media_epoch();
    let hub = Arc::new(RtpHub {
        base: port,
        picture,
        audio,
        picture_v6,
        audio_v6,
        peer_picture: Mutex::new(None),
        peer_audio: Mutex::new(None),
        offer_from: Mutex::new(None),
        exchanged: Mutex::new(None),
        config_sent: AtomicBool::new(false),
        key_seen: AtomicU32::new(0),
        stop: AtomicBool::new(false),
    });
    let thread_hub = Arc::clone(&hub);
    std::thread::spawn(move || {
        let mut buf = [0u8; 128];
        while !thread_hub.stop.load(Ordering::Relaxed) {
            let got = if let Ok(packet) = input.recv_from(&mut buf) {
                Some(packet)
            } else if let Some(sock) = input_v6.as_ref() {
                sock.recv_from(&mut buf).ok()
            } else {
                None
            };
            let Some((n, src)) = got else {
                poll_rtcp(&thread_hub);
                continue;
            };
            if n >= 41 && &buf[..5] == b"OFFER" {
                note_offered(super::UDP_RTP);
                if n >= 42 {
                    match buf[41] {
                        b'P' => super::choose_udp_plain(true),
                        b'G' => super::choose_udp_plain(false),
                        _ => {}
                    }
                }
                let mut player_public = [0u8; 32];
                player_public.copy_from_slice(&buf[5..37]);
                let picture_port = u16::from_le_bytes([buf[37], buf[38]]);
                let audio_port = u16::from_le_bytes([buf[39], buf[40]]);
                let picture_peer = SocketAddr::new(src.ip(), picture_port);
                let audio_peer = SocketAddr::new(src.ip(), audio_port);
                // Every offer gets a config packet. The first send can be
                // lost, and a later offer from the same socket is the retry.
                *thread_hub
                    .peer_picture
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(picture_peer);
                *thread_hub
                    .peer_audio
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(audio_peer);
                *thread_hub
                    .offer_from
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(src);
                let host = host_share();
                let key = derive_aes128(&host.shared(&player_public));
                *thread_hub.exchanged.lock().unwrap_or_else(|p| p.into_inner()) = Some(key);
                let mut payload = b"CFG".to_vec();
                payload.extend_from_slice(&host.public);
                let packet = RtpHeader {
                    marker: true,
                    payload_type: PT_PICTURE,
                    sequence: 0,
                    timestamp: 0,
                    ssrc: 1,
                }
                .encode(&payload);
                let cfg_sock = if picture_peer.is_ipv6() {
                    thread_hub.picture_v6.as_ref()
                } else {
                    Some(&thread_hub.picture)
                };
                if let Some(sock) = cfg_sock {
                    if udp_send(sock, &packet, picture_peer).is_ok() {
                        thread_hub.config_sent.store(true, Ordering::Relaxed);
                    }
                }
            } else if n >= 9 && &buf[..5] == b"RPORT" {
                let picture_port = u16::from_le_bytes([buf[5], buf[6]]);
                let audio_port = u16::from_le_bytes([buf[7], buf[8]]);
                *thread_hub
                    .peer_picture
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) =
                    Some(SocketAddr::new(src.ip(), picture_port));
                *thread_hub
                    .peer_audio
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) =
                    Some(SocketAddr::new(src.ip(), audio_port));
            } else if n >= 3 && &buf[..3] == b"KEY" {
                let seen = thread_hub.key_seen.fetch_add(1, Ordering::Relaxed) + 1;
                if seen >= 2 {
                    let reply = if src.is_ipv6() {
                        input_v6.as_ref()
                    } else {
                        Some(&input)
                    };
                    if let Some(sock) = reply {
                        let _ = udp_send(sock, b"EAKU", src);
                    }
                }
            } else if n >= 3 && (&buf[..3] == b"AVB" || &buf[..3] == b"ST2") {
                if &buf[..3] == b"AVB" {
                    ::avb::note_listener(true);
                    note_offered(super::AVB);
                } else {
                    note_offered(super::ST2110);
                }
                if &buf[..3] == b"ST2" && n >= 8 {
                    match buf[7] {
                        b'G' => super::choose_st_gcm(true),
                        b'P' => super::choose_st_gcm(false),
                        _ => {}
                    }
                }
                let picture_port = if n >= 5 {
                    u16::from_le_bytes([buf[3], buf[4]])
                } else {
                    src.port()
                };
                let audio_port = if n >= 7 {
                    u16::from_le_bytes([buf[5], buf[6]])
                } else {
                    picture_port
                };
                *thread_hub
                    .peer_picture
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) =
                    Some(SocketAddr::new(src.ip(), picture_port));
                *thread_hub
                    .peer_audio
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) =
                    Some(SocketAddr::new(src.ip(), audio_port));
            }
            poll_rtcp(&thread_hub);
        }
    });
    let mut guard = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(old) = guard.take() {
        old.stop.store(true, Ordering::Relaxed);
    }
    *guard = Some(hub);
}

fn ensure_rtp_listener(port: u16) {
    listen_once(port);
}

fn poll_rtcp(hub: &RtpHub) {
    let mut buf = [0u8; 256];
    if let Ok((n, _)) = hub.picture.recv_from(&mut buf) {
        let _ = ingest_global_rtcp(Instant::now(), &buf[..n]);
    }
}

fn send_udp(picture: bool, packet: &[u8]) {
    let guard = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner());
    let Some(hub) = guard.as_ref() else {
        return;
    };
    let peer = if picture {
        *hub.peer_picture.lock().unwrap_or_else(|p| p.into_inner())
    } else {
        *hub.peer_audio.lock().unwrap_or_else(|p| p.into_inner())
    };
    let Some(peer) = peer else {
        return;
    };
    let sock = if peer.is_ipv6() {
        if picture {
            hub.picture_v6.as_ref()
        } else {
            hub.audio_v6.as_ref()
        }
    } else if picture {
        Some(&hub.picture)
    } else {
        Some(&hub.audio)
    };
    if let Some(sock) = sock {
        let _ = udp_send(sock, packet, peer);
    }
}

pub(super) fn hub_config_sent() -> bool {
    let guard = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner());
    let Some(hub) = guard.as_ref() else {
        return false;
    };
    hub.config_sent.load(Ordering::Relaxed)
}

/// True when a UDP/AVB/ST 2110 sender has somewhere to put a packet.
/// No hub means this process is not on that path, so the caller should not wait.
pub fn rtp_media_ready() -> bool {
    let guard = RTP_HUB.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(hub) = guard.as_ref() else {
        return true;
    };
    if hub.stop.load(Ordering::Relaxed) {
        return true;
    }
    let ready = hub
        .peer_picture
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .is_some();
    ready
}

pub(super) fn hub_has_peer() -> bool {
    let guard = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner());
    let Some(hub) = guard.as_ref() else {
        return false;
    };
    let present = hub
        .peer_picture
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some();
    present
}

pub(super) fn hub_exchanged_key() -> Option<[u8; 16]> {
    let guard = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner());
    let hub = guard.as_ref()?;
    let key = *hub.exchanged.lock().unwrap_or_else(|p| p.into_inner());
    key
}

/// AVB and ST 2110 leave on the same sockets as RTP. Those sockets are bound
/// to 0.0.0.0. A socket bound to 127.0.0.1 cannot send to a player on the LAN.
pub(super) fn emit_packet(picture: bool, packet: &[u8]) {
    send_udp(picture, packet);
}

/// Player side of the live UDP socket. The offer repeats every 20 ms on the
/// input port until one config RTP packet arrives, or for 5 s. The first
/// config datagram can be lost. The key is the X25519 exchange from that
/// offer, not the host public. Keys retry until `EAKU` or 200 ms. The
/// pointer datagram is sent once.
pub fn spawn_player_rtp(
    host_input: SocketAddr,
    picture: tokio::sync::mpsc::Sender<Vec<u8>>,
    audio: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    std::thread::spawn(move || {
        let Some(picture_sock) = bind_unspecified(host_input) else {
            return;
        };
        let Some(audio_sock) = bind_unspecified(host_input) else {
            return;
        };
        let Some(input_sock) = bind_unspecified(host_input) else {
            return;
        };
        let _ = picture_sock.set_nonblocking(true);
        let _ = audio_sock.set_nonblocking(true);
        let _ = input_sock.set_nonblocking(true);
        let Ok(picture_port) = picture_sock.local_addr() else {
            return;
        };
        let Ok(audio_port) = audio_sock.local_addr() else {
            return;
        };
        let plain = super::gates().plain;
        let player = KeyShare::from_seed(fresh_seed());
        let mut offer = b"OFFER".to_vec();
        offer.extend_from_slice(&player.public);
        offer.extend(picture_port.port().to_le_bytes());
        offer.extend(audio_port.port().to_le_bytes());
        offer.push(if plain { b'P' } else { b'G' });
        let started = Instant::now();
        let mut last_offer: Option<Instant> = None;
        let mut gcm = None;
        let mut ready = false;
        let mut early_picture = Vec::new();
        let mut early_audio = Vec::new();
        let mut buf = [0u8; 2048];
        while !ready && started.elapsed() < Duration::from_secs(5) {
            let due = last_offer
                .map(|last| last.elapsed() >= Duration::from_millis(20))
                .unwrap_or(true);
            if due {
                let _ = udp_send(&input_sock, &offer, host_input);
                last_offer = Some(Instant::now());
            }
            if let Ok((n, _)) = picture_sock.recv_from(&mut buf) {
                if let Some((_, payload)) = RtpHeader::decode(&buf[..n]) {
                    if payload.starts_with(b"CFG") && payload.len() >= 35 {
                        if !plain {
                            let mut host_public = [0u8; 32];
                            host_public.copy_from_slice(&payload[3..35]);
                            let key = derive_aes128(&player.shared(&host_public));
                            gcm = Some(GcmSession::new(key));
                        }
                        ready = true;
                    } else {
                        early_picture.push(buf[..n].to_vec());
                    }
                }
            }
            if let Ok((n, _)) = audio_sock.recv_from(&mut buf) {
                early_audio.push(buf[..n].to_vec());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if !ready {
            return;
        }
        let mut gcm = gcm.unwrap_or_else(|| GcmSession::new(session_key()));
        let mut pictures = Assembler::new();
        let mut audios = Assembler::new();
        let picture_deadline = player_picture_wait();
        let batch = Instant::now();
        let mut picture_open = true;
        let mut audio_open = true;
        for packet in early_picture {
            if !accept_rtp(
                &packet,
                plain,
                &mut gcm,
                &mut pictures,
                &picture,
                picture_deadline,
                batch,
            ) {
                picture_open = false;
                break;
            }
        }
        let audio_wait = audio_deadline();
        for packet in early_audio {
            if !audio_open {
                break;
            }
            if !accept_rtp(
                &packet,
                plain,
                &mut gcm,
                &mut audios,
                &audio,
                audio_wait,
                batch,
            ) {
                audio_open = false;
                break;
            }
        }
        let key_end = Instant::now() + Duration::from_millis(200);
        let mut eaku = false;
        let mut pointed = false;
        while picture_open || audio_open {
            if picture.is_closed() && audio.is_closed() {
                break;
            }
            if !eaku && Instant::now() < key_end {
                let _ = udp_send(&input_sock, b"KEY\x04", host_input);
            }
            if !pointed {
                let _ = udp_send(&input_sock, b"PTR", host_input);
                pointed = true;
            }
            if let Ok((n, _)) = input_sock.recv_from(&mut buf) {
                if n >= 4 && &buf[..4] == b"EAKU" {
                    eaku = true;
                }
            }
            if picture_open
                && !pump_rtp(
                    &picture_sock,
                    plain,
                    &mut gcm,
                    &mut pictures,
                    &picture,
                    picture_deadline,
                )
            {
                picture_open = false;
            }
            if audio_open && !pump_rtp(&audio_sock, plain, &mut gcm, &mut audios, &audio, audio_wait)
            {
                audio_open = false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
}

fn pump_rtp(
    sock: &UdpSocket,
    clear: bool,
    gcm: &mut GcmSession,
    asm: &mut Assembler,
    tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    deadline: Duration,
) -> bool {
    let mut buf = [0u8; 2048];
    // One timestamp for the packets already queued. Decrypting them must not
    // look like a hole that outlived one frame interval.
    let now = Instant::now();
    loop {
        let n = match sock.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return true,
            Err(_) => return true,
        };
        if !accept_rtp(&buf[..n], clear, gcm, asm, tx, deadline, now) {
            return false;
        }
    }
}

fn accept_rtp(
    packet: &[u8],
    clear: bool,
    gcm: &mut GcmSession,
    asm: &mut Assembler,
    tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    deadline: Duration,
    now: Instant,
) -> bool {
    let Some((header, payload)) = RtpHeader::decode(packet) else {
        return true;
    };
    if payload.starts_with(b"CFG") {
        return true;
    }
    let plain = if clear {
        payload.to_vec()
    } else {
        match gcm.open_next(header.ssrc, header.sequence, payload) {
            Ok(plain) => plain,
            Err(_) => return true,
        }
    };
    let Some(slice) = Slice::decode(&plain) else {
        return true;
    };
    if let Some(record) = asm.push(slice, now, now + deadline) {
        if tx.blocking_send(record).is_err() {
            return false;
        }
    }
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

impl RtpHeader {
    pub fn encode(&self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + payload.len());
        out.push(0x80);
        out.push((u8::from(self.marker) << 7) | (self.payload_type & 0x7f));
        out.extend(self.sequence.to_be_bytes());
        out.extend(self.timestamp.to_be_bytes());
        out.extend(self.ssrc.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<(Self, &[u8])> {
        if bytes.len() < 12 || bytes[0] >> 6 != 2 {
            return None;
        }
        let marker = bytes[1] & 0x80 != 0;
        let payload_type = bytes[1] & 0x7f;
        let sequence = u16::from_be_bytes([bytes[2], bytes[3]]);
        let timestamp = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let ssrc = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        Some((
            Self {
                marker,
                payload_type,
                sequence,
                timestamp,
                ssrc,
            },
            &bytes[12..],
        ))
    }
}

pub struct KeyShare {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl KeyShare {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let secret = StaticSecret::from(seed);
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    pub fn shared(&self, peer: &[u8; 32]) -> [u8; 32] {
        let peer = PublicKey::from(*peer);
        self.secret.diffie_hellman(&peer).to_bytes()
    }
}

pub fn derive_aes128(shared: &[u8; 32]) -> [u8; 16] {
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut okm = [0u8; 16];
    hk.expand(b"etherquic-rtp", &mut okm)
        .expect("hkdf expand");
    okm
}

pub fn srtp_nonce(ssrc: u32, roc: u32, seq: u16) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0..4].copy_from_slice(&ssrc.to_be_bytes());
    nonce[4..8].copy_from_slice(&roc.to_be_bytes());
    nonce[8..10].copy_from_slice(&seq.to_be_bytes());
    nonce
}

#[derive(Debug)]
pub struct GcmSession {
    key: [u8; 16],
    /// Per SSRC: highest packet index (roc << 16 | seq) and a replay bitmap
    /// of the 128 indices below it. A set of every nonce grew without bound.
    seen: HashMap<u32, (u64, u128)>,
    /// Per SSRC: roll-over counter and last sequence, for `seal_next` and
    /// `open_next`.
    roc: HashMap<u32, (u32, u16)>,
    closed: bool,
}

impl GcmSession {
    pub fn new(key: [u8; 16]) -> Self {
        Self {
            key,
            seen: HashMap::new(),
            roc: HashMap::new(),
            closed: false,
        }
    }

    pub fn closed(&self) -> bool {
        self.closed
    }

    pub fn seal(&mut self, ssrc: u32, roc: u32, seq: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        self.guard(ssrc, roc, seq)?;
        let cipher = Aes128Gcm::new_from_slice(&self.key).map_err(|e| e.to_string())?;
        let nonce = Nonce::from(srtp_nonce(ssrc, roc, seq));
        cipher
            .encrypt(&nonce, payload)
            .map_err(|e| e.to_string())
    }

    pub fn open(&mut self, ssrc: u32, roc: u32, seq: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        if self.closed {
            return Err("session closed".into());
        }
        // Authenticate before the replay window moves. A plaintext or
        // corrupt packet used to occupy the sequence, so the real packet
        // looked like a repeated nonce and closed the session.
        let cipher = Aes128Gcm::new_from_slice(&self.key).map_err(|e| e.to_string())?;
        let nonce = Nonce::from(srtp_nonce(ssrc, roc, seq));
        let plain = cipher.decrypt(&nonce, payload).map_err(|e| e.to_string())?;
        self.guard(ssrc, roc, seq)?;
        Ok(plain)
    }

    /// Seal with the roll-over counter this session keeps for `ssrc`. The
    /// counter advances when the sequence wraps. With a fixed counter of 0
    /// the 65,537th packet repeated a nonce and closed the session, about
    /// ten seconds into a PyroWave stream.
    pub fn seal_next(&mut self, ssrc: u32, seq: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        let roc = match self.roc.get(&ssrc).copied() {
            Some((roc, last)) if seq < last => roc.wrapping_add(1),
            Some((roc, _)) => roc,
            None => 0,
        };
        self.roc.insert(ssrc, (roc, seq));
        self.seal(ssrc, roc, seq, payload)
    }

    /// Open with the RFC 3711 roll-over estimate for `ssrc`.
    pub fn open_next(&mut self, ssrc: u32, seq: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        let (roc, last) = self.roc.get(&ssrc).copied().unwrap_or((0, seq));
        let guess = if last < 0x8000 {
            if seq > last && seq - last > 0x8000 {
                roc.wrapping_sub(1)
            } else {
                roc
            }
        } else if last - 0x8000 > seq {
            roc.wrapping_add(1)
        } else {
            roc
        };
        let opened = self.open(ssrc, guess, seq, payload)?;
        let newer = (u64::from(guess) << 16 | u64::from(seq)) > (u64::from(roc) << 16 | u64::from(last));
        if newer || !self.roc.contains_key(&ssrc) {
            self.roc.insert(ssrc, (guess, seq));
        }
        Ok(opened)
    }

    fn guard(&mut self, ssrc: u32, roc: u32, seq: u16) -> Result<(), String> {
        if self.closed {
            return Err("session closed".into());
        }
        let index = u64::from(roc) << 16 | u64::from(seq);
        let Some((top, bits)) = self.seen.get(&ssrc).copied() else {
            self.seen.insert(ssrc, (index, 0));
            return Ok(());
        };
        if index > top {
            let shift = index - top;
            let bits = if shift >= 128 { 0 } else { (bits << shift) | (1u128 << (shift - 1)) };
            self.seen.insert(ssrc, (index, bits));
            return Ok(());
        }
        let back = top - index;
        if back == 0 || (back <= 128 && bits & (1u128 << (back - 1)) != 0) {
            self.closed = true;
            return Err("repeated nonce".into());
        }
        if back > 128 {
            // Too old to tell. Drop the packet, keep the session.
            return Err("packet older than the replay window".into());
        }
        self.seen.insert(ssrc, (top, bits | (1u128 << (back - 1))));
        Ok(())
    }
}

/// Player resends an unacked key every 2 ms and drops it at the picture deadline.
#[derive(Clone, Debug)]
pub struct KeyPump {
    pub id: u16,
    pub payload: Vec<u8>,
    sent: u32,
    acked: bool,
    last: Option<Instant>,
}

impl KeyPump {
    pub fn new(id: u16, payload: Vec<u8>) -> Self {
        Self {
            id,
            payload,
            sent: 0,
            acked: false,
            last: None,
        }
    }

    pub fn sent(&self) -> u32 {
        self.sent
    }

    /// `Some` when a datagram should leave now.
    pub fn poll(&mut self, now: Instant, picture_deadline: Instant) -> Option<Vec<u8>> {
        if self.acked || now >= picture_deadline {
            return None;
        }
        let due = self
            .last
            .map(|last| now.duration_since(last) >= Duration::from_millis(2))
            .unwrap_or(true);
        if !due {
            return None;
        }
        self.last = Some(now);
        self.sent += 1;
        let mut bytes = self.id.to_le_bytes().to_vec();
        bytes.extend_from_slice(&self.payload);
        Some(bytes)
    }

    pub fn ack(&mut self, magic: &[u8], id: u16) {
        if magic == b"EAKU" && id == self.id {
            self.acked = true;
        }
    }
}

/// Host applies keys in id order. A gap waits until the deadline, then skips.
#[derive(Debug, Default)]
pub struct KeyInbox {
    next: u16,
    waiting: BTreeMap<u16, Vec<u8>>,
    applied: Vec<u16>,
}

impl KeyInbox {
    pub fn push(&mut self, id: u16, payload: Vec<u8>, deadline_hit: bool) {
        self.waiting.entry(id).or_insert(payload);
        loop {
            if let Some(payload) = self.waiting.remove(&self.next) {
                let _ = payload;
                self.applied.push(self.next);
                self.next = self.next.wrapping_add(1);
                continue;
            }
            if deadline_hit {
                if self.waiting.keys().next().is_some() {
                    self.next = self.next.wrapping_add(1);
                    continue;
                }
            }
            break;
        }
    }

    pub fn applied(&self) -> &[u16] {
        &self.applied
    }
}

/// In-memory UDP pair: picture, audio, and input ports are offsets from `base`.
pub struct RtpSession {
    pub base: u16,
    picture: Vec<Vec<u8>>,
    audio: Vec<Vec<u8>>,
    input: Vec<Vec<u8>>,
    asm: Assembler,
    offers: u32,
    config_seen: bool,
    pointer_sends: u32,
    last_offer: Option<Instant>,
    plain: bool,
    gcm: GcmSession,
    applied: Option<[u8; 16]>,
    seq: u16,
    bound: u16,
    audio_rate: u32,
    frame: Duration,
    rtcp_loss: Option<f64>,
    last_rtcp: Option<Instant>,
    /// Hub generation captured when this session was opened. Checked per
    /// packet so an in-flight frame cannot follow a replaced hub.
    epoch: u64,
    pic_ssrc: u32,
    aud_ssrc: u32,
}

impl RtpSession {
    pub fn new(base: u16) -> Self {
        Self {
            base,
            picture: Vec::new(),
            audio: Vec::new(),
            input: Vec::new(),
            asm: Assembler::new(),
            offers: 0,
            config_seen: false,
            pointer_sends: 0,
            last_offer: None,
            plain: false,
            gcm: GcmSession::new(session_key()),
            applied: None,
            seq: 1,
            bound: 0,
            audio_rate: 48_000,
            frame: Duration::from_millis(16),
            rtcp_loss: None,
            last_rtcp: None,
            epoch: media_epoch(),
            pic_ssrc: fresh_ssrc(),
            aud_ssrc: fresh_ssrc(),
        }
    }

    pub(super) fn bind_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
    }

    pub fn set_audio_rate(&mut self, rate: u32) {
        self.audio_rate = rate.max(1);
    }

    pub fn set_frame(&mut self, frame: Duration) {
        self.frame = frame;
    }

    pub fn rtcp_loss(&self) -> Option<f64> {
        self.rtcp_loss
    }

    /// Apply one RTCP receiver report. Reports closer than one second are ignored.
    pub fn ingest_rtcp(&mut self, now: Instant, packet: &[u8]) -> Option<f64> {
        let fraction = rtcp_loss_fraction(packet)?;
        if self
            .last_rtcp
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(1))
        {
            return None;
        }
        self.last_rtcp = Some(now);
        self.rtcp_loss = Some(fraction);
        Some(fraction)
    }

    /// Bind the host picture, audio, and input sockets on `base`.
    /// A concurrent announce probe may hold the ports for a moment, so retry.
    pub fn listen(&mut self, base: u16) {
        self.bound = base;
        for _ in 0..50 {
            ensure_rtp_listener(base);
            if rtp_owns(base) {
                self.epoch = media_epoch();
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn set_plain(&mut self, plain: bool) {
        self.plain = plain;
    }

    pub fn ports_free(taken: impl Fn(u16) -> bool, base: u16) -> bool {
        !taken(base) && !taken(base.saturating_add(1)) && !taken(base.saturating_add(2))
    }

    pub fn pointer_sends(&self) -> u32 {
        self.pointer_sends
    }

    pub fn offers(&self) -> u32 {
        self.offers
    }

    /// One offer tick. Sends when 20 ms have passed since the last offer.
    /// Returns false once a second has passed with no config.
    pub fn repeat_offer(&mut self, now: Instant, started: Instant, offer: &[u8]) -> bool {
        if self.config_seen {
            return true;
        }
        if now.saturating_duration_since(started) >= Duration::from_secs(1) {
            return false;
        }
        let due = self
            .last_offer
            .map(|last| now.saturating_duration_since(last) >= Duration::from_millis(20))
            .unwrap_or(true);
        if due {
            self.offers += 1;
            self.input.push(offer.to_vec());
            self.last_offer = Some(now);
        }
        false
    }

    /// Player repeats the offer on the input port every 20 ms until a config
    /// RTP packet arrives or 1 s elapses. This in-memory host answers the first offer.
    pub fn player_until_config(&mut self, offer: &[u8]) -> bool {
        let start = Instant::now();
        let mut now = start;
        loop {
            if !self.repeat_offer(now, start, offer) && now.saturating_duration_since(start) >= Duration::from_secs(1)
            {
                return false;
            }
            if self.offers == 1 && !self.config_seen {
                self.host_config(b"eqneg 1\nvideo mjpeg\n");
            }
            if self.config_seen {
                return true;
            }
            now += Duration::from_millis(20);
        }
    }

    fn host_config(&mut self, config: &[u8]) {
        let header = RtpHeader {
            marker: true,
            payload_type: PT_PICTURE,
            sequence: 0,
            timestamp: 0,
            ssrc: 1,
        };
        self.picture.push(header.encode(config));
        self.config_seen = true;
    }

    fn ensure_key(&mut self) {
        if self.bound == 0 {
            return;
        }
        let Some(key) = hub_exchanged_key() else {
            return;
        };
        if self.applied == Some(key) {
            return;
        }
        self.gcm = GcmSession::new(key);
        self.applied = Some(key);
    }

    pub fn send_record(
        &mut self,
        picture: bool,
        record_id: u16,
        bytes: &[u8],
        drop_index: Option<u16>,
        capture: Duration,
    ) -> Option<Vec<u8>> {
        if self.bound != 0 && !rtp_owns(self.bound) {
            ensure_rtp_listener(self.bound);
        }
        self.ensure_key();
        let slices = split_record(record_id, bytes, RTP_SLICE_BUDGET);
        let pt = if picture { PT_PICTURE } else { PT_AUDIO };
        let ssrc = if picture { self.pic_ssrc } else { self.aud_ssrc };
        let now = Instant::now();
        let mut got = None;
        for slice in &slices {
            if drop_index == Some(slice.index) {
                continue;
            }
            let deadline = if picture {
                now + picture_deadline(self.frame)
            } else {
                now + audio_deadline()
            };
            if let Some(record) = self.asm.push(slice.clone(), now, deadline) {
                got = Some(record);
            }
            let seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            let plain = slice.encode();
            let payload = if self.plain {
                plain
            } else {
                match self.gcm.seal_next(ssrc, seq, &plain) {
                    Ok(sealed) => {
                        SEALED.fetch_add(1, Ordering::Relaxed);
                        sealed
                    }
                    Err(_) => continue,
                }
            };
            let timestamp = if picture {
                picture_rtp_timestamp(capture)
            } else {
                audio_rtp_timestamp(capture, self.audio_rate)
            };
            let packet = RtpHeader {
                marker: slice.index + 1 == slice.count,
                payload_type: pt,
                sequence: seq,
                timestamp,
                ssrc,
            }
            .encode(&payload);
            if !emit_if_current(self.epoch, picture, &packet) {
                return got;
            }
            if picture {
                self.picture.push(packet);
            } else {
                self.audio.push(packet);
            }
        }
        got
    }

    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        let mut out = std::mem::take(&mut self.picture);
        out.append(&mut self.audio);
        out
    }

    pub fn send_pointer_once(&mut self, bytes: &[u8]) {
        if self.pointer_sends == 0 {
            self.pointer_sends = 1;
            self.input.push(bytes.to_vec());
        }
    }

    pub fn measured_cap(measured_bps: Option<u64>) -> RateCap {
        RateCap::new(measured_bps.map(|rate| rate.saturating_mul(80) / 100).unwrap_or(DEFAULT_CAP))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcm_survives_a_sequence_wrap() {
        let _globals = crate::carrier::test_globals();
        let mut host = GcmSession::new([5u8; 16]);
        let mut player = GcmSession::new([5u8; 16]);
        let mut seq = 65_000u16;
        for i in 0..2_000u32 {
            let body = i.to_le_bytes();
            let sealed = host.seal_next(9, seq, &body).unwrap();
            if i % 7 == 3 {
                // A lost packet does not change the roll-over estimate.
                seq = seq.wrapping_add(1);
                continue;
            }
            assert_eq!(player.open_next(9, seq, &sealed).unwrap(), body);
            seq = seq.wrapping_add(1);
        }
        assert!(!player.closed());
    }

    #[test]
    fn gcm_round_trips_and_a_repeated_nonce_closes() {
        let _globals = crate::carrier::test_globals();
        let host = KeyShare::from_seed([3u8; 32]);
        let player = KeyShare::from_seed([9u8; 32]);
        let shared = host.shared(&player.public);
        assert_eq!(shared, player.shared(&host.public));
        let key = derive_aes128(&shared);
        let mut a = GcmSession::new(key);
        let mut b = GcmSession::new(key);
        let sealed = a.seal(7, 0, 1, b"picture").unwrap();
        assert_eq!(b.open(7, 0, 1, &sealed).unwrap(), b"picture");
        assert!(a.seal(7, 0, 1, b"again").is_err());
        assert!(a.closed());
    }

    #[test]
    fn gcm_ignores_a_corrupt_packet() {
        let _globals = crate::carrier::test_globals();
        let mut host = GcmSession::new([9u8; 16]);
        let mut player = GcmSession::new([9u8; 16]);
        let sealed = host.seal(11, 0, 1, b"frame").unwrap();
        let mut garbage = sealed.clone();
        garbage[0] ^= 0xff;
        assert!(player.open(11, 0, 1, &garbage).is_err());
        assert!(!player.closed(), "a failed open must not close the session");
        assert_eq!(player.open(11, 0, 1, &sealed).unwrap(), b"frame");
        assert!(player.open(11, 0, 1, &sealed).is_err());
        assert!(player.closed());
    }

    #[test]
    fn a_key_is_resent_until_eaku_and_a_pointer_goes_once() {
        let _globals = crate::carrier::test_globals();
        let mut pump = KeyPump::new(4, b"k".to_vec());
        let start = Instant::now();
        let deadline = start + Duration::from_millis(20);
        assert!(pump.poll(start, deadline).is_some());
        assert!(pump.poll(start + Duration::from_millis(1), deadline).is_none());
        assert!(pump.poll(start + Duration::from_millis(2), deadline).is_some());
        pump.ack(b"EAKU", 4);
        assert!(pump.poll(start + Duration::from_millis(4), deadline).is_none());
        assert!(pump.sent() >= 2);

        let mut rtp = RtpSession::new(4433);
        rtp.send_pointer_once(b"ptr");
        rtp.send_pointer_once(b"ptr");
        assert_eq!(rtp.pointer_sends(), 1);
        assert!(rtp.player_until_config(b"offer"));
        assert_eq!(rtp.offers(), 1);
        let start = Instant::now();
        let mut spaced = RtpSession::new(9);
        assert!(!spaced.repeat_offer(start, start, b"o"));
        assert_eq!(spaced.offers(), 1);
        assert!(!spaced.repeat_offer(start + Duration::from_millis(1), start, b"o"));
        assert_eq!(spaced.offers(), 1);
        assert!(!spaced.repeat_offer(start + Duration::from_millis(20), start, b"o"));
        assert_eq!(spaced.offers(), 2);
        assert!(!spaced.repeat_offer(start + Duration::from_secs(1), start, b"o"));
        assert_eq!(spaced.offers(), 2);
        assert!(rtp.offers() >= 1);

        let start = Instant::now();
        let mut unanswered = RtpSession::new(0);
        let mut now = start;
        while now.saturating_duration_since(start) < Duration::from_secs(1) {
            assert!(!unanswered.repeat_offer(now, start, b"offer"));
            now += Duration::from_millis(20);
        }
        assert_eq!(unanswered.offers(), 50, "one offer every 20 ms through 980 ms");
        assert!(!unanswered.repeat_offer(start + Duration::from_secs(1), start, b"offer"));
        assert_eq!(unanswered.offers(), 50, "the 1 s give-up does not send another offer");
        assert_eq!(picture_rtp_timestamp(Duration::from_secs(1)), 90_000);
        assert_eq!(audio_rtp_timestamp(Duration::from_secs(1), 48_000), 48_000);
        assert_eq!(picture_deadline(Duration::from_millis(1)), Duration::from_millis(2));
        assert_eq!(audio_deadline(), Duration::from_millis(20));
        let report = rtcp_receiver_report(9, 11, 100, 10, 50);
        let seen = Instant::now();
        assert!(ingest_global_rtcp(seen, &report).unwrap() > 0.01);
        assert!(ingest_global_rtcp(seen + Duration::from_millis(200), &report).is_none());

        let mut inbox = KeyInbox::default();
        inbox.push(1, b"b".to_vec(), false);
        assert!(inbox.applied().is_empty());
        inbox.push(0, b"a".to_vec(), false);
        assert_eq!(inbox.applied(), &[0, 1]);
        inbox.push(3, b"d".to_vec(), true);
        assert!(inbox.applied().contains(&3) || inbox.applied().windows(1).any(|w| w == [3]));
    }

    #[test]
    fn a_dropped_slice_does_not_block_the_next_record() {
        let _globals = crate::carrier::test_globals();
        let mut rtp = RtpSession::new(5000);
        assert!(RtpSession::ports_free(|_| false, 5000));
        assert!(!RtpSession::ports_free(|p| p == 5001, 5000));
        let record = vec![1u8; 100];
        assert!(rtp.send_record(true, 1, &record, Some(0), Duration::ZERO).is_none());
        assert_eq!(
            rtp.send_record(true, 2, b"next", None, Duration::ZERO).as_deref(),
            Some(b"next".as_slice())
        );
        assert_eq!(RtpSession::measured_cap(None).cap, DEFAULT_CAP);
        assert_eq!(RtpSession::measured_cap(Some(1_000_000)).cap, 800_000);
        let packet = RtpHeader {
            marker: true,
            payload_type: PT_PICTURE,
            sequence: 1,
            timestamp: 90_000,
            ssrc: 1,
        }
        .encode(b"x");
        let (header, _) = RtpHeader::decode(&packet).unwrap();
        assert!(header.marker);
        assert_eq!(header.payload_type, PT_PICTURE);
    }

    #[test]
    fn capture_time_is_the_rtp_timestamp() {
        let _globals = crate::carrier::test_globals();
        let mut rtp = RtpSession::new(0);
        rtp.set_plain(true);
        assert!(rtp
            .send_record(true, 1, b"frame", None, Duration::from_secs(1))
            .is_some());
        let packet = &rtp.drain()[0];
        let (header, _) = RtpHeader::decode(packet).unwrap();
        assert_eq!(header.timestamp, 90_000);
        assert_ne!(session_key(), derive_aes128(&[0x71; 32]));
    }

    fn lan_v4() -> Ipv4Addr {
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("probe");
        probe
            .connect("192.168.0.58:4434")
            .expect("route to the host");
        match probe.local_addr().expect("local") {
            SocketAddr::V4(addr) => {
                assert!(!addr.ip().is_loopback(), "route to the host stayed on loopback");
                *addr.ip()
            }
            other => panic!("expected ipv4, got {other}"),
        }
    }

    fn free_rtp_port() -> u16 {
        for base in 23_000..23_200u16 {
            let Ok(picture) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, base)) else {
                continue;
            };
            let Ok(audio) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, base + 1)) else {
                continue;
            };
            let Ok(input) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, base + 2)) else {
                continue;
            };
            drop((picture, audio, input));
            return base;
        }
        panic!("no free rtp ports");
    }

    #[test]
    fn a_picture_socket_holds_a_full_frame() {
        let _globals = crate::carrier::test_globals();
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind");
        widen_udp(&sock);
        let got = socket2::SockRef::from(&sock)
            .recv_buffer_size()
            .expect("recv buffer");
        assert!(
            got >= 1024 * 1024,
            "picture socket buffer is {got}, too small for one frame"
        );
    }

    #[test]
    fn emit_reaches_a_non_loopback_peer() {
        let _globals = crate::carrier::test_globals();
        let ip = lan_v4();
        release_rtp();
        let port = free_rtp_port();
        listen_once(port);
        let recv = UdpSocket::bind((ip, 0)).expect("player socket");
        recv.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("timeout");
        let recv_port = recv.local_addr().expect("player").port();
        let hello = UdpSocket::bind((ip, 0)).expect("hello socket");
        let mut body = b"AVB".to_vec();
        body.extend(recv_port.to_le_bytes());
        body.extend(recv_port.to_le_bytes());
        hello
            .send_to(&body, SocketAddr::new(std::net::IpAddr::V4(ip), port + 2))
            .expect("hello");
        let start = Instant::now();
        while !hub_has_peer() && start.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(hub_has_peer(), "AVB hello did not register a peer");
        let marker = b"lan-picture-marker";
        emit_packet(true, marker);
        let mut buf = [0u8; 64];
        let (n, src) = recv.recv_from(&mut buf).expect("peer received nothing");
        assert_eq!(&buf[..n], marker);
        let std::net::IpAddr::V4(from) = src.ip() else {
            panic!("expected ipv4 source");
        };
        assert!(!from.is_loopback(), "packet came from {from}");
        release_rtp();
    }

    fn rtp_offer(picture_port: u16, audio_port: u16, seed: u8) -> Vec<u8> {
        let player = KeyShare::from_seed([seed; 32]);
        let mut body = b"OFFER".to_vec();
        body.extend_from_slice(&player.public);
        body.extend(picture_port.to_le_bytes());
        body.extend(audio_port.to_le_bytes());
        body.push(b'P');
        body
    }

    #[test]
    fn a_later_offer_replaces_the_rtp_peer() {
        let _globals = crate::carrier::test_globals();
        let ip = lan_v4();
        let was_plain = super::super::udp_plain();
        super::super::choose_udp_plain(true);
        release_rtp();
        let port = free_rtp_port();
        listen_once(port);
        let first = UdpSocket::bind((ip, 0)).expect("first player");
        let second = UdpSocket::bind((ip, 0)).expect("second player");
        first
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("timeout");
        second
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("timeout");
        let first_port = first.local_addr().expect("first").port();
        let second_port = second.local_addr().expect("second").port();
        let input = SocketAddr::new(std::net::IpAddr::V4(ip), port + 2);
        let hello_first = UdpSocket::bind((ip, 0)).expect("first hello");
        let hello_second = UdpSocket::bind((ip, 0)).expect("second hello");
        hello_first
            .send_to(&rtp_offer(first_port, first_port, 3), input)
            .expect("first offer");
        let start = Instant::now();
        while !hub_config_sent() && start.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(hub_config_sent(), "first offer was not answered");
        hello_second
            .send_to(&rtp_offer(second_port, second_port, 9), input)
            .expect("second offer");
        let mut buf = [0u8; 128];
        let mut configured = false;
        let wait = Instant::now();
        while wait.elapsed() < Duration::from_secs(1) {
            match second.recv_from(&mut buf) {
                Ok((n, _)) if buf[..n].windows(3).any(|window| window == b"CFG") => {
                    configured = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(configured, "second player was not configured");
        let marker = b"second-player";
        emit_packet(true, marker);
        let mut saw = false;
        let wait = Instant::now();
        while wait.elapsed() < Duration::from_secs(1) {
            match second.recv_from(&mut buf) {
                Ok((n, _)) if &buf[..n] == marker => {
                    saw = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(saw, "picture stayed on the first player");
        let mut leaked = false;
        while let Ok((n, _)) = first.recv_from(&mut buf) {
            if &buf[..n] == marker {
                leaked = true;
            }
        }
        assert!(!leaked, "first player still received the picture");
        super::super::choose_udp_plain(was_plain);
        release_rtp();
    }

    #[test]
    fn a_large_picture_reaches_the_player_off_loopback() {
        let _globals = crate::carrier::test_globals();
        let ip = lan_v4();
        let was_plain = super::super::udp_plain();
        super::super::choose_udp_plain(true);
        release_rtp();
        let port = free_rtp_port();
        let mut host = RtpSession::new(port);
        host.set_plain(true);
        host.listen(port);
        assert!(rtp_owns(port), "host rtp sockets did not bind");
        let (picture_tx, mut picture_rx) = tokio::sync::mpsc::channel(2);
        let (audio_tx, _audio_rx) = tokio::sync::mpsc::channel(1);
        spawn_player_rtp(
            SocketAddr::new(std::net::IpAddr::V4(ip), port + 2),
            picture_tx,
            audio_tx,
        );
        let start = Instant::now();
        while !hub_has_peer() && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(hub_has_peer(), "player offer did not reach the host");
        let cfg = Instant::now();
        while !hub_config_sent() && cfg.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(hub_config_sent(), "host did not deliver the RTP config");
        // The offer loop reads one datagram per millisecond. Wait until it has
        // the config and is draining the picture socket, then send.
        std::thread::sleep(Duration::from_millis(200));
        let picture = vec![0xA5u8; 400_000];
        let assembled = host.send_record(true, 3, &picture, None, Duration::from_millis(4));
        assert_eq!(assembled.as_deref(), Some(picture.as_slice()));
        let packets = host.drain();
        assert!(
            packets.len() < 400,
            "picture used {} packets",
            packets.len()
        );
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(bytes) = picture_rx.blocking_recv() {
                let _ = done_tx.send(bytes);
            }
        });
        let got = done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("player did not reassemble the picture");
        assert_eq!(got, picture);
        super::super::choose_udp_plain(was_plain);
        release_rtp();
    }

    #[test]
    fn a_slow_picture_still_reassembles_inside_the_player_wait() {
        let _globals = crate::carrier::test_globals();
        let mut rtp = RtpSession::new(0);
        rtp.set_plain(true);
        let picture = vec![0x3Cu8; 80_000];
        assert!(rtp
            .send_record(true, 9, &picture, None, Duration::from_millis(8))
            .is_some());
        let packets = rtp.drain();
        assert!(packets.len() > 4, "picture was one packet");
        let mut asm = Assembler::new();
        let mut now = Instant::now();
        let mut got = None;
        for (index, packet) in packets.iter().enumerate() {
            if index == packets.len() / 2 {
                now += Duration::from_millis(50);
            }
            let (_, payload) = RtpHeader::decode(packet).expect("rtp");
            let slice = Slice::decode(payload).expect("slice");
            if let Some(record) = asm.push(slice, now, now + player_picture_wait()) {
                got = Some(record);
            }
        }
        assert_eq!(got.as_deref(), Some(picture.as_slice()));
    }
}
