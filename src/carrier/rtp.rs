//! UDP/RTP carrier. No Quinn connection. Input is a sibling datagram.

use std::collections::{BTreeMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
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
    let mut seed = [0u8; 32];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if file.read_exact(&mut seed).is_ok() {
            return seed;
        }
    }
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(1);
    seed[..16].copy_from_slice(&tick.to_le_bytes());
    seed[16..].copy_from_slice(b"player-ephemeral!!");
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
    peer_picture: Mutex<Option<SocketAddr>>,
    peer_audio: Mutex<Option<SocketAddr>>,
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

pub fn release_rtp() {
    OFFERED.store(0, Ordering::Relaxed);
    if let Some(hub) = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner()).take() {
        hub.stop.store(true, Ordering::Relaxed);
    }
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
    let _ = input.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = picture.set_nonblocking(true);
    let hub = Arc::new(RtpHub {
        base: port,
        picture,
        audio,
        peer_picture: Mutex::new(None),
        peer_audio: Mutex::new(None),
        exchanged: Mutex::new(None),
        config_sent: AtomicBool::new(false),
        key_seen: AtomicU32::new(0),
        stop: AtomicBool::new(false),
    });
    let thread_hub = Arc::clone(&hub);
    std::thread::spawn(move || {
        let mut buf = [0u8; 128];
        while !thread_hub.stop.load(Ordering::Relaxed) {
            let Ok((n, src)) = input.recv_from(&mut buf) else {
                poll_rtcp(&thread_hub);
                continue;
            };
            if n >= 41 && &buf[..5] == b"OFFER" && !thread_hub.config_sent.load(Ordering::Relaxed) {
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
                let peer = SocketAddr::new(src.ip(), picture_port);
                if thread_hub.picture.send_to(&packet, peer).is_ok() {
                    thread_hub.config_sent.store(true, Ordering::Relaxed);
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
                    let _ = input.send_to(b"EAKU", src);
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
    let sock = if picture { &hub.picture } else { &hub.audio };
    let _ = sock.send_to(packet, peer);
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

fn emit_socket() -> &'static UdpSocket {
    static SOCK: std::sync::OnceLock<UdpSocket> = std::sync::OnceLock::new();
    SOCK.get_or_init(|| {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("carrier emit socket");
        let _ = sock.set_nonblocking(true);
        sock
    })
}

/// AVB and ST 2110 leave on a UDP socket. RTP stays on [`send_udp`].
pub(super) fn emit_packet(picture: bool, packet: &[u8]) {
    let sock = emit_socket();
    let guard = RTP_HUB.lock().unwrap_or_else(|p| p.into_inner());
    let peer = guard.as_ref().and_then(|hub| {
        let slot = if picture {
            &hub.peer_picture
        } else {
            &hub.peer_audio
        };
        *slot.lock().unwrap_or_else(|p| p.into_inner())
    });
    drop(guard);
    if let Some(peer) = peer {
        let _ = sock.send_to(packet, peer);
        return;
    }
    if let Ok(addr) = sock.local_addr() {
        let _ = sock.send_to(packet, addr);
    }
}

/// Player side of the live UDP socket. The offer repeats every 20 ms on the
/// input port until one config RTP packet arrives, or for 1 s. The key is the
/// X25519 exchange from that offer, not the host public. Keys retry until
/// `EAKU` or 200 ms. The pointer datagram is sent once.
pub fn spawn_player_rtp(
    host_input: SocketAddr,
    picture: tokio::sync::mpsc::Sender<Vec<u8>>,
    audio: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    std::thread::spawn(move || {
        let Ok(picture_sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
            return;
        };
        let Ok(audio_sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
            return;
        };
        let Ok(input_sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
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
        while !ready && started.elapsed() < Duration::from_secs(1) {
            let due = last_offer
                .map(|last| last.elapsed() >= Duration::from_millis(20))
                .unwrap_or(true);
            if due {
                let _ = input_sock.send_to(&offer, host_input);
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
        let picture_deadline = picture_deadline(Duration::from_millis(16));
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
        let media_end = Instant::now() + Duration::from_secs(30);
        let key_end = Instant::now() + Duration::from_millis(200);
        let mut eaku = false;
        let mut pointed = false;
        while Instant::now() < media_end && (picture_open || audio_open) {
            if !eaku && Instant::now() < key_end {
                let _ = input_sock.send_to(b"KEY\x04", host_input);
            }
            if !pointed {
                let _ = input_sock.send_to(b"PTR", host_input);
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
        match gcm.open(header.ssrc, 0, header.sequence, payload) {
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
    seen: HashSet<(u32, u32, u16)>,
    closed: bool,
}

impl GcmSession {
    pub fn new(key: [u8; 16]) -> Self {
        Self {
            key,
            seen: HashSet::new(),
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
        self.guard(ssrc, roc, seq)?;
        let cipher = Aes128Gcm::new_from_slice(&self.key).map_err(|e| e.to_string())?;
        let nonce = Nonce::from(srtp_nonce(ssrc, roc, seq));
        cipher.decrypt(&nonce, payload).map_err(|e| e.to_string())
    }

    fn guard(&mut self, ssrc: u32, roc: u32, seq: u16) -> Result<(), String> {
        if self.closed {
            return Err("session closed".into());
        }
        if !self.seen.insert((ssrc, roc, seq)) {
            self.closed = true;
            return Err("repeated nonce".into());
        }
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
        }
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
        let slices = split_record(record_id, bytes, 32);
        let pt = if picture { PT_PICTURE } else { PT_AUDIO };
        let ssrc = if picture { 11 } else { 22 };
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
                match self.gcm.seal(ssrc, 0, seq, &plain) {
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
            send_udp(picture, &packet);
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
    fn gcm_round_trips_and_a_repeated_nonce_closes() {
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
    fn a_key_is_resent_until_eaku_and_a_pointer_goes_once() {
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
}
