//! Session carriers. Capture and the offer stay put. A carrier only moves
//! finished records.
//!
//! Ids: 1 QUIC streams, 2 QUIC datagrams, 3 UDP/RTP, 4 AVB, 5 ST 2110.
//! Carriers 2–4 share [`assemble::Assembler`]. ST 2110 packetizes the raster
//! or a JPEG XS payload and does not use that header.

mod assemble;
mod avb;
mod matrix;
mod quic;
mod rtp;
mod session;
mod st2110;

pub use assemble::{
    apply_reset, class_for, pulse_on_later_config, record_class, split_record, Assembler,
    AssemblerReset, Class, Lane, Slice,
};
pub use avb::{
    pacer_stays_inside, pcp, reservation_fails_closed, AvtpFrame, Pacer, AVTP_SUBTYPE, ETHERTYPE_AVTP,
    PCP_AUDIO, PCP_INPUT, PCP_PICTURE, RATE_AUDIO, RATE_INPUT, RATE_PICTURE,
};
pub use quic::{
    apply_mode, exchange_picture, observe_udp_rtt, profile_for, reset_udp_rtt, udp_rtt_sample,
    Exchange, PathProfile, MODE1_WINDOW, MODE2_WINDOW,
};

/// Install the agreed carrier's congestion profile on this connection.
/// L2 mode 2 takes the short path immediately. UDP mode 2 takes it only
/// after this connection's own RTT is under 2 ms. Mode 1 stays at 12,000 bytes.
pub fn apply_agreed_carrier(conn: &quinn::Connection, mode: u8) {
    if crate::socket_addr_to_peer(conn.remote_address()).is_some() {
        crate::l2_endpoint::apply_agreed_mode(conn, mode);
    } else {
        crate::udp_endpoint::apply_agreed_mode(conn, mode);
    }
}
pub use rtp::{
    derive_aes128, offered_carrier, release_rtp, rtp_media_ready, rtp_sealed, spawn_player_rtp,
    srtp_nonce,
    GcmSession, KeyInbox, KeyPump, KeyShare, RtpHeader, RtpSession, PROFILE_GCM, PROFILE_PLAIN,
};
pub use session::{
    complete_session, host_push, open_carrier_player, play_on_carrier, prepare_host,
    spawn_player_avb, spawn_player_st2110, SessionReport, StAudio,
};
pub use matrix::{
    matrix_label, matrix_sessions, MatrixCell, MatrixLayer, MatrixProfile, MatrixSession,
};
pub use st2110::{
    packetize_jpeg_xs, packetize_rfc4175, XsAssembler, XsPacket, RFC4175_HEIGHT, RFC4175_WIDTH,
};

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;
use std::time::Duration;

struct Gates {
    avb: bool,
    st2110: bool,
    picture_ok: bool,
    nic_mbps: u32,
    override_speed: bool,
    l3_port: u16,
    audio_opus: bool,
    plain: bool,
    reserved: bool,
    vlan: bool,
    locked: bool,
    st_gcm: bool,
}

/// Held across a whole announce read so a test can flip the gates without
/// another test observing the intermediate set.
static ANNOUNCE: Mutex<()> = Mutex::new(());

/// Carrier tests share process globals (profile, ports, gates, the RTP peer).
/// `cargo test` runs them on parallel threads, so each takes this lock.
#[cfg(test)]
pub(crate) fn test_globals() -> std::sync::MutexGuard<'static, ()> {
    static GLOBALS: Mutex<()> = Mutex::new(());
    GLOBALS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// RTP input port this process published (`base + 2`). Zero means it did not.
static PUBLISHED: AtomicU16 = AtomicU16::new(0);

struct SavedRtp {
    armed: bool,
    previous: Option<u16>,
}

/// The `rtp=` line this process replaced, so clearing restores it.
static SAVED_RTP: Mutex<SavedRtp> = Mutex::new(SavedRtp {
    armed: false,
    previous: None,
});

static GATES: Mutex<Gates> = Mutex::new(Gates {
    avb: false,
    st2110: false,
    picture_ok: false,
    nic_mbps: 1_000,
    override_speed: false,
    l3_port: 0,
    audio_opus: false,
    plain: false,
    reserved: false,
    vlan: false,
    locked: false,
    st_gcm: false,
});

fn copy_gates(gates: &Gates) -> Gates {
    Gates {
        avb: gates.avb,
        st2110: gates.st2110,
        picture_ok: gates.picture_ok,
        nic_mbps: gates.nic_mbps,
        override_speed: gates.override_speed,
        l3_port: gates.l3_port,
        audio_opus: gates.audio_opus,
        plain: gates.plain,
        reserved: gates.reserved,
        vlan: gates.vlan,
        locked: gates.locked,
        st_gcm: gates.st_gcm,
    }
}

fn gates() -> Gates {
    match GATES.lock() {
        Ok(gates) => copy_gates(&gates),
        Err(poison) => copy_gates(&poison.into_inner()),
    }
}

/// The UDP port discovery bound. Port 0 means the RTP ports were not opened,
/// so carriers 3 and 5 stay off the announce.
pub fn announced_l3_port() -> u16 {
    gates().l3_port
}

pub fn set_l3_port(port: u16) {
    if port == 0 {
        rtp::release_rtp();
        // A loopback that starts at port 0 must not wipe a user's `rtp=` line.
        if PUBLISHED.load(Ordering::Relaxed) != 0 {
            clear_published_input();
        }
    }
    if let Ok(mut gates) = GATES.lock() {
        gates.l3_port = port;
    }
    if port != 0 {
        rtp::listen_once(port);
    }
}

/// `connections.txt` next to the UI preferences file.
fn connections_file() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("etherquic").join("connections.txt"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return Some(
                PathBuf::from(home)
                    .join(".config")
                    .join("etherquic")
                    .join("connections.txt"),
            );
        }
    }
    if let Some(dir) = std::env::var_os("APPDATA") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("etherquic").join("connections.txt"));
        }
    }
    None
}

fn exact_rtp_port(line: &str) -> Option<u16> {
    let rest = line.strip_prefix("rtp=")?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u16>().ok().filter(|port| *port != 0)
}

/// Replace exact `rtp=<digits>` lines. Other lines stay. Returns the first old port.
fn rewrite_rtp(text: &str, port: Option<u16>) -> (String, Option<u16>) {
    let mut previous = None;
    let mut out = String::new();
    let mut wrote = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if let Some(found) = exact_rtp_port(bare) {
            if previous.is_none() {
                previous = Some(found);
            }
            if !wrote {
                if let Some(port) = port {
                    out.push_str(&format!("rtp={port}\n"));
                }
                wrote = true;
            }
            continue;
        }
        out.push_str(line);
    }
    if !wrote {
        if let Some(port) = port {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("rtp={port}\n"));
        }
    }
    (out, previous)
}

fn write_connections(path: &Path, body: &str) -> Result<(), ()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| ())?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, body).map_err(|_| ())?;
    std::fs::rename(&tmp, path).map_err(|_| ())?;
    Ok(())
}

fn apply_rtp_file(port: Option<u16>) -> Option<u16> {
    let path = connections_file()?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let (body, previous) = rewrite_rtp(&text, port);
    if body != text {
        let _ = write_connections(&path, &body);
    }
    previous
}

fn read_rtp_port() -> u16 {
    let Some(path) = connections_file() else {
        return 0;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    for line in text.lines() {
        if let Some(port) = exact_rtp_port(line) {
            return port;
        }
    }
    0
}

/// Remember the RTP input port (`base + 2`) and write `rtp=<port>` for the player.
pub fn publish_input(port: u16) {
    if port == 0 {
        return;
    }
    PUBLISHED.store(port, Ordering::Relaxed);
    let previous = apply_rtp_file(Some(port));
    let mut saved = SAVED_RTP.lock().unwrap_or_else(|poison| poison.into_inner());
    if !saved.armed {
        saved.armed = true;
        saved.previous = previous;
    }
}

/// Drop the line this process wrote. A process that never published leaves the file alone.
pub fn clear_published_input() {
    let port = PUBLISHED.swap(0, Ordering::Relaxed);
    if port == 0 {
        return;
    }
    let restore = {
        let mut saved = SAVED_RTP.lock().unwrap_or_else(|poison| poison.into_inner());
        let restore = if saved.armed {
            saved.previous.filter(|old| *old != port)
        } else {
            None
        };
        saved.armed = false;
        saved.previous = None;
        restore
    };
    let _ = apply_rtp_file(restore);
}

/// RTP input socket bound in this process. Discovery reads this, not the saved line.
pub fn bound_input_port() -> u16 {
    rtp::bound_input()
}

/// RTP socket named by a scanned offer.
///
/// `rtp_port` is [`crate::discover::Found::rtp_port`]. Zero, or the host's
/// QUIC service port, is not a target. This does not read the local `rtp=` line
/// and does not substitute `l3_port` or `l3_port + 2`.
pub fn rtp_socket(ip: Option<IpAddr>, rtp_port: u16, quic_port: u16) -> Option<SocketAddr> {
    let ip = ip?;
    if rtp_port == 0 || (quic_port != 0 && rtp_port == quic_port) {
        return None;
    }
    Some(SocketAddr::new(ip, rtp_port))
}

/// Default L2 Connect for carriers 3–5.
///
/// The flag is true when the dial is not a QUIC address, so the scanned offer
/// is the only RTP target. The socket is the host IP and [`crate::discover::Found::rtp_port`].
/// A missing port stays `None` and does not fall through to the local `rtp=` line.
///
/// ST 2110 is the exception. Its essence is still that scanned UDP socket, but
/// the desktop control session stays on the L2 QUIC dial, so the flag is false
/// and the NIC is opened. UDP/RTP on L2 is not a desktop session: the Connect
/// card greys that chip out instead of dialing it.
pub fn l2_media_target(
    carrier: u8,
    udp: Option<SocketAddr>,
    host_ip: Option<IpAddr>,
    rtp_port: u16,
    quic_port: u16,
) -> (bool, Option<SocketAddr>) {
    if carrier == ST2110 && udp.is_none() {
        return (false, rtp_socket(host_ip, rtp_port, quic_port));
    }
    if carrier < UDP_RTP || udp.is_some() {
        return (false, None);
    }
    (true, rtp_socket(host_ip, rtp_port, quic_port))
}

/// Where picture and audio for carriers 3–5 are offered.
///
/// A QUIC path that Quinn minted from a MAC is not a routable IP, so discovery
/// on that address cannot name the RTP input. The scanned host socket is the
/// target. A real IPv4 or IPv6 dial still asks the host.
pub fn media_input(remote: SocketAddr, scanned: Option<SocketAddr>) -> Option<SocketAddr> {
    let scanned = scanned.filter(|addr| addr.port() != 0);
    if crate::socket_addr_to_peer(remote).is_some() {
        return scanned;
    }
    player_host(Some(remote)).or(scanned)
}

/// Where the player sends the carrier hello.
///
/// `udp` is a QUIC service address. Its port is never the RTP target; the
/// input port is read from a fresh discovery reply.
/// `l2_offer` is a default L2 Connect. `rtp` is the host IP and the RTP port
/// already parsed from that host's offer. A missing port fails the lookup.
/// The saved `rtp=` line on 127.0.0.1 is used only when this connect has no
/// UDP address and no scanned offer.
pub fn player_host_for(
    udp: Option<SocketAddr>,
    l2_offer: bool,
    rtp: Option<SocketAddr>,
) -> Option<SocketAddr> {
    if l2_offer && udp.is_none() {
        return rtp.filter(|addr| addr.port() != 0);
    }
    player_host(udp)
}

/// True only for a QUIC dial on raw Ethernet. An explicit UDP address and an
/// L2 Connect that already holds an RTP offer do not open a NIC.
pub fn connect_opens_ethernet(udp: Option<SocketAddr>, l2_offer: bool) -> bool {
    udp.is_none() && !l2_offer
}

/// Open the raw NIC only when [`connect_opens_ethernet`] is true.
/// `open` is not called for a UDP address or an `--l2-offer` connect.
pub fn connect_net<T, E>(
    udp: Option<SocketAddr>,
    l2_offer: bool,
    nic: Option<&str>,
    open: impl FnOnce(Option<&str>) -> Result<T, E>,
) -> Result<Option<T>, E> {
    if connect_opens_ethernet(udp, l2_offer) {
        open(nic).map(Some)
    } else {
        Ok(None)
    }
}

/// Address the player offers on when the dial is a QUIC address, or when
/// this connect has no scanned host.
///
/// A UDP address is the host's QUIC service address. The port on that address
/// is never the target. The input port comes from the host's discovery offer.
/// With no address, this machine's saved `rtp=` line is the target on
/// 127.0.0.1. A remote L2 Connect uses [`player_host_for`] instead.
pub fn player_host(udp: Option<SocketAddr>) -> Option<SocketAddr> {
    if let Some(udp) = udp {
        let port = crate::discover::ask_rtp_input(udp.ip())?;
        if port == 0 || port == udp.port() {
            return None;
        }
        return Some(SocketAddr::new(udp.ip(), port));
    }
    let mut port = PUBLISHED.load(Ordering::Relaxed);
    if port == 0 {
        port = read_rtp_port();
    }
    if port == 0 {
        return None;
    }
    Some(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
    ))
}

/// Default profile is `gcm`. `plain` is the explicit profile.
pub fn set_plain_profile(plain: bool) {
    if let Ok(mut gates) = GATES.lock() {
        gates.plain = plain;
    }
}

pub fn udp_plain() -> bool {
    gates().plain
}

pub fn st_uses_gcm() -> bool {
    gates().st_gcm
}

/// Connect card and `--profile`. UDP `plain` is explicit. ST 2110 `gcm` is explicit.
/// Host startup does not call this, so a choice survives `enable_on_host`.
pub fn choose_udp_plain(plain: bool) {
    set_plain_profile(plain);
}

pub fn choose_st_gcm(on: bool) {
    note_st_gcm(on);
}

pub fn choose_profile(udp_plain: bool, st_gcm: bool) {
    choose_udp_plain(udp_plain);
    choose_st_gcm(st_gcm);
}

/// An explicit Connect choice is a carrier this session will open.
///
/// AVB stays off until a session asks for it. ST 2110's 10 Gb/s and JPEG XS
/// gates describe uncompressed essence. MJPEG, PyroWave, and H.264 are already
/// packetized on that seam, including on a 1 Gb/s NIC, so an explicit ST 2110
/// request opens the gate instead of failing `agree`.
pub fn honor_explicit_carrier(requested: Option<u8>) {
    match requested {
        Some(AVB) => set_avb_enabled(true),
        Some(ST2110) => {
            let current = gates();
            set_st2110_enabled(true, true, current.nic_mbps.max(1_000), true);
        }
        _ => {}
    }
}

pub fn set_audio_opus(on: bool) {
    if let Ok(mut gates) = GATES.lock() {
        gates.audio_opus = on;
    }
}

pub fn note_reservation(on: bool) {
    if let Ok(mut gates) = GATES.lock() {
        gates.reserved = on;
    }
}

/// The player socket is the AVB listener. Connect clears this before that socket joins.
pub fn note_avb_listener(on: bool) {
    ::avb::note_listener(on);
}

pub fn note_locked(on: bool) {
    if let Ok(mut gates) = GATES.lock() {
        gates.locked = on;
    }
}

pub fn note_st_gcm(on: bool) {
    if let Ok(mut gates) = GATES.lock() {
        gates.st_gcm = on;
    }
}

/// Picture kind and audio kind from the offer, read by [`announce`].
/// Opus with no PCM suppresses ST 2110. JPEG XS marks the picture kind.
pub fn prepare_announce(picture_is_jpegxs: bool, audio_is_opus: bool) {
    if let Ok(mut gates) = GATES.lock() {
        if picture_is_jpegxs {
            gates.picture_ok = true;
        }
        gates.audio_opus = audio_is_opus;
    }
}

/// Fastest NIC this host can see. A missing or negative speed is ignored.
pub fn link_mbps() -> u32 {
    let mut best = 0u32;
    let Ok(dir) = std::fs::read_dir("/sys/class/net") else {
        return 1_000;
    };
    for entry in dir.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path().join("speed")) else {
            continue;
        };
        let Ok(speed) = text.trim().parse::<i64>() else {
            continue;
        };
        if speed > 0 {
            best = best.max(speed as u32);
        }
    }
    if best == 0 { 1_000 } else { best }
}

/// Host startup. The RTP base is not the QUIC port. Picture kind stays off
/// until the offer includes JPEG XS. The UDP and ST 2110 profiles stay as the
/// operator left them (`gcm` and `plain` until [`choose_profile`]).
pub fn enable_on_host(avoid: u16) {
    set_avb_enabled(true);
    let current = gates();
    set_st2110_enabled(
        true,
        current.picture_ok,
        link_mbps(),
        current.override_speed,
    );
    note_locked(::st2110::clock_locked(::st2110::ptp_follower_present()));
    let bound = announced_l3_port();
    if bound != 0 && rtp::rtp_owns(bound) {
        publish_input(bound.saturating_add(2));
        return;
    }
    for candidate in 20_000..20_200u16 {
        let ports = [
            candidate,
            candidate.saturating_add(1),
            candidate.saturating_add(2),
            candidate.saturating_add(4),
        ];
        if ports.into_iter().any(|port| port == avoid) {
            continue;
        }
        if ports.into_iter().any(|port| !udp_port_free(port)) {
            continue;
        }
        set_l3_port(candidate);
        if rtp::rtp_owns(candidate) {
            publish_input(candidate.saturating_add(2));
            return;
        }
    }
}

/// Drain of the packets a session already sent on its socket.
pub fn open_carrier_tap() -> tokio::sync::mpsc::UnboundedSender<Vec<u8>> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while rx.blocking_recv().is_some() {}
    });
    tx
}

/// Operator switch for AVB. Off until a host turns it on.
pub fn set_avb_enabled(on: bool) {
    if let Ok(mut gates) = GATES.lock() {
        gates.avb = on;
    }
}

/// Operator switch for ST 2110, plus the NIC speed and the picture kind.
pub fn set_st2110_enabled(on: bool, picture_ok: bool, nic_mbps: u32, override_speed: bool) {
    if let Ok(mut gates) = GATES.lock() {
        gates.st2110 = on;
        gates.picture_ok = picture_ok;
        gates.nic_mbps = nic_mbps;
        gates.override_speed = override_speed;
    }
}

pub const QUIC_STREAMS: u8 = 1;
pub const QUIC_DATAGRAMS: u8 = 2;
pub const UDP_RTP: u8 = 3;
pub const AVB: u8 = 4;
pub const ST2110: u8 = 5;
pub const TLV_CARRIERS: u8 = 4;
pub const MAX_CARRIERS: usize = 8;

/// What this host is allowed to put in `TLV_CARRIERS`.
#[derive(Clone, Debug)]
pub struct Announce {
    pub android: bool,
    pub has_l2: bool,
    pub has_udp: bool,
    pub avb_enabled: bool,
    pub st2110_enabled: bool,
    pub nic_mbps: u32,
    pub override_speed: bool,
    /// Uncompressed or JPEG XS is the picture this host would send.
    pub picture_ok_for_st2110: bool,
    pub audio_opus: bool,
    /// `l3_port`, `+1`, `+2` taken.
    pub rtp_port_taken: bool,
    /// `l3_port`, `+2`, `+4` taken.
    pub st2110_port_taken: bool,
}

impl Announce {
    pub fn this_machine(l3_port: u16) -> Self {
        let gates = gates();
        let port = if l3_port == 0 { gates.l3_port } else { l3_port };
        let rtp_ports = [port, port.saturating_add(1), port.saturating_add(2)];
        let st_ports = [port, port.saturating_add(2), port.saturating_add(4)];
        // A zero port was never bound, so those carriers are not open.
        let unbound = port == 0;
        Self {
            android: cfg!(target_os = "android"),
            has_l2: !cfg!(target_os = "android"),
            has_udp: true,
            avb_enabled: gates.avb,
            st2110_enabled: gates.st2110,
            nic_mbps: gates.nic_mbps,
            override_speed: gates.override_speed,
            picture_ok_for_st2110: gates.picture_ok,
            audio_opus: gates.audio_opus,
            rtp_port_taken: unbound || rtp_ports.into_iter().any(|port| !udp_port_free(port)),
            st2110_port_taken: unbound || st_ports.into_iter().any(|port| !udp_port_free(port)),
        }
    }
}

fn udp_port_free(port: u16) -> bool {
    if rtp::rtp_owns(port) {
        return true;
    }
    UdpSocket::bind(("0.0.0.0", port)).is_ok()
}

/// Carrier ids this machine will actually open. A missing list means carrier 1.
pub fn announce(input: &Announce) -> Vec<u8> {
    let mut ids = Vec::new();
    if input.has_l2 || input.has_udp {
        ids.push(QUIC_STREAMS);
        ids.push(QUIC_DATAGRAMS);
    }
    if input.has_udp && !input.rtp_port_taken {
        ids.push(UDP_RTP);
    }
    if input.has_l2 && !input.android && input.avb_enabled {
        ids.push(AVB);
    }
    let fast = input.nic_mbps >= 10_000 || input.override_speed;
    if input.has_udp
        && input.st2110_enabled
        && fast
        && input.picture_ok_for_st2110
        && !input.audio_opus
        && !input.st2110_port_taken
    {
        ids.push(ST2110);
    }
    ids.truncate(MAX_CARRIERS);
    ids
}

fn carriers_now(l3_port: u16) -> Vec<u8> {
    announce(&Announce::this_machine(l3_port))
}

pub fn local_carriers(l3_port: u16) -> Vec<u8> {
    let _guard = ANNOUNCE.lock().unwrap_or_else(|poison| poison.into_inner());
    carriers_now(l3_port)
}

pub fn encode_carrier_tlv(ids: &[u8]) -> Vec<u8> {
    let n = ids.len().min(MAX_CARRIERS);
    let mut out = Vec::with_capacity(1 + n);
    out.push(n as u8);
    out.extend_from_slice(&ids[..n]);
    out
}

pub fn parse_carrier_tlv(value: &[u8]) -> Option<Vec<u8>> {
    let n = *value.first()? as usize;
    if n > MAX_CARRIERS || value.len() != 1 + n {
        return None;
    }
    Some(value[1..].to_vec())
}

/// Empty (no TLV) selects carrier 1 only.
pub fn effective_carriers(parsed: &[u8]) -> Vec<u8> {
    if parsed.is_empty() {
        vec![QUIC_STREAMS]
    } else {
        parsed.to_vec()
    }
}

/// ALPN `etherquic` is always carrier 1. A v2 host echoes an announced id.
pub fn agree(alpn_v2: bool, requested: Option<u8>, announced: &[u8]) -> Result<u8, String> {
    if !alpn_v2 {
        return Ok(QUIC_STREAMS);
    }
    let requested = requested.unwrap_or(QUIC_STREAMS);
    if requested == QUIC_STREAMS {
        return Ok(QUIC_STREAMS);
    }
    let announced = effective_carriers(announced);
    if announced.contains(&requested) {
        Ok(requested)
    } else {
        Err(format!("carrier {requested} was not announced"))
    }
}

/// Close before the first picture when the echo is not the request.
pub fn config_matches(alpn_v2: bool, requested: Option<u8>, echoed: Option<u8>) -> Result<u8, String> {
    if !alpn_v2 {
        return Ok(QUIC_STREAMS);
    }
    let requested = requested.unwrap_or(QUIC_STREAMS);
    let echoed = echoed.unwrap_or(QUIC_STREAMS);
    if requested != echoed {
        Err(format!(
            "carrier {requested} does not match the host echo {echoed}"
        ))
    } else {
        Ok(echoed)
    }
}

/// Connect-row wording. Untagged AVB is not a priority class. ST 2110 says
/// when the software clock is not locked. UDP/RTP defaults to `gcm`.
pub fn connect_row(id: u8, vlan: bool, reserved: bool, locked: bool, gcm: bool) -> String {
    match id {
        AVB if reserved => "AVB (reserved), not encrypted".into(),
        AVB if vlan => "AVB (shaped), not encrypted".into(),
        AVB => avb::row_label(false, false),
        ST2110 if !locked => {
            let profile = if gcm { "gcm" } else { "plain" };
            format!("ST 2110 ({profile}), not locked")
        }
        ST2110 => {
            let profile = if gcm { "gcm" } else { "plain" };
            format!("ST 2110 ({profile})")
        }
        UDP_RTP => {
            if gcm {
                "UDP/RTP (gcm)".into()
            } else {
                "UDP/RTP (plain)".into()
            }
        }
        QUIC_DATAGRAMS => "QUIC datagrams".into(),
        _ => "QUIC streams".into(),
    }
}

/// Connect row from the gates the session last set.
pub fn connect_row_now(id: u8) -> String {
    let gates = gates();
    let gcm = if id == ST2110 {
        gates.st_gcm
    } else {
        !gates.plain
    };
    let locked = if id == ST2110 { gates.locked } else { true };
    connect_row(id, gates.vlan, gates.reserved, locked, gcm)
}

/// Encoder cap. One short loss sample does not cut. Above 1% for a full
/// second the cap drops 20% until a clean second.
#[derive(Clone, Debug)]
pub struct RateCap {
    pub nominal: u64,
    pub cap: u64,
    reduced: bool,
    bad: Duration,
    clean: Duration,
}

impl RateCap {
    pub fn new(nominal: u64) -> Self {
        Self {
            nominal,
            cap: nominal,
            reduced: false,
            bad: Duration::ZERO,
            clean: Duration::ZERO,
        }
    }

    /// Loss over `sent` packets. The host calls this with Quinn's path counters.
    pub fn reduced(&self) -> bool {
        self.reduced
    }

    pub fn apply_loss(&mut self, dt: Duration, sent: u64, lost: u64) -> u64 {
        let loss = if sent == 0 {
            0.0
        } else {
            lost as f64 / sent as f64
        };
        self.observe(dt, loss);
        self.cap
    }

    pub fn observe(&mut self, dt: Duration, loss: f64) {
        if loss > 0.01 {
            self.clean = Duration::ZERO;
            self.bad = self.bad.saturating_add(dt);
            if self.bad >= Duration::from_secs(1) && !self.reduced {
                self.cap = self.cap.saturating_mul(80) / 100;
                self.reduced = true;
                self.bad = Duration::ZERO;
            }
        } else {
            self.bad = Duration::ZERO;
            if self.reduced {
                self.clean = self.clean.saturating_add(dt);
                if self.clean >= Duration::from_secs(1) {
                    self.cap = self.nominal;
                    self.reduced = false;
                    self.clean = Duration::ZERO;
                }
            }
        }
    }
}

/// One clean picture and one clean PCM record, then an optional dropped slice
/// that must not block the next picture.
#[derive(Clone, Debug)]
pub struct LoopReport {
    pub picture: Option<Vec<u8>>,
    pub audio: Option<Vec<u8>>,
    pub next_picture: Option<Vec<u8>>,
    pub ethertype: u16,
    pub retried: bool,
    pub datagrams: usize,
    /// Bytes a listener actually received, including the Ethernet header when
    /// the carrier is a frame.
    pub wire: Vec<Vec<u8>>,
}

pub fn run_loopback(id: u8, picture: &[u8], audio: &[u8], drop_slice: bool) -> LoopReport {
    match id {
        QUIC_STREAMS => reliable_loop(picture, audio, drop_slice),
        QUIC_DATAGRAMS => sliced_loop(id, picture, audio, drop_slice, true),
        UDP_RTP | AVB | ST2110 => seam_loop(id, picture, audio, drop_slice),
        _ => LoopReport {
            picture: None,
            audio: None,
            next_picture: None,
            ethertype: 0,
            retried: false,
            datagrams: 0,
            wire: Vec::new(),
        },
    }
}

fn reliable_loop(picture: &[u8], audio: &[u8], drop_once: bool) -> LoopReport {
    // Carrier 1 is reliable and ordered. The first frame is injected with no
    // listener, so the fabric drops it. The retry is what the listener reads.
    let fabric = crate::fabric::Fabric::with_ether_type(1500, crate::ETHERTYPE_QUIC);
    let dst = [2, 0, 0, 0, 0, 9];
    let src = [2, 0, 0, 0, 0, 1];
    let mut retried = false;
    let mut wire = Vec::new();
    let mut deliver = |bytes: &[u8]| -> Vec<u8> {
        let frame = crate::frame::Frame {
            dst,
            src,
            ether_type: crate::ETHERTYPE_QUIC,
            dst_service: 1,
            src_service: 1,
            payload: bytes.to_vec(),
        };
        if drop_once && !retried {
            let before = fabric.stats.dropped_no_dst.load(std::sync::atomic::Ordering::Relaxed);
            fabric.inject(frame.clone());
            let after = fabric.stats.dropped_no_dst.load(std::sync::atomic::Ordering::Relaxed);
            retried = after > before;
        }
        let got = fabric.deliver(dst, frame).expect("carrier 1 retry");
        wire.push(crate::frame::encode_frame(&got));
        got.payload
    };
    let picture_out = deliver(picture);
    let audio_out = deliver(audio);
    let next = deliver(b"next-picture");
    LoopReport {
        picture: Some(picture_out),
        audio: Some(audio_out),
        next_picture: Some(next),
        ethertype: u16::from_be_bytes([wire[0][12], wire[0][13]]),
        retried,
        datagrams: 0,
        wire,
    }
}

/// Session send path for carriers 3, 4, and 5. The host calls the same type.
pub struct Seam {
    id: u8,
    rtp: RtpSession,
    avb_seq: u8,
    avb: Option<std::sync::Arc<crate::fabric::Fabric>>,
    picture_id: u16,
    audio_id: u16,
    packets: Vec<Vec<u8>>,
    ethertype: u16,
    picture_pacer: Pacer,
    audio_pacer: Pacer,
    frame_due: Option<Duration>,
    /// Next RTP sequence for ST 2110 picture packets. A new frame used to
    /// start again at 0. The player keeps one GCM session, and a repeated
    /// sequence is a repeated nonce, which closes that session. The config
    /// record is the first frame, so the picture that follows never opens.
    st_pic_seq: u16,
    st_pic_ts: u32,
    st_frame: u8,
    st_aud_seq: u16,
    /// Picture and audio SSRCs for this seam. A leftover packet from the
    /// previous seam must not advance this player's replay window.
    st_pic_ssrc: u32,
    st_aud_ssrc: u32,
    /// One session for every ST 2110 packet this seam seals. The roll-over
    /// counter has to survive from one frame to the next.
    st_gcm: Option<rtp::GcmSession>,
    /// [`rtp::media_epoch`] at [`Seam::open`]. A later hub belongs to the
    /// next session.
    epoch: u64,
}

impl Seam {
    pub fn open(id: u8) -> Self {
        let avb = if id == AVB {
            Some(crate::fabric::Fabric::with_ether_type(1500, ETHERTYPE_AVTP))
        } else {
            None
        };
        let mut rtp = RtpSession::new(0);
        if id == UDP_RTP {
            rtp.set_plain(gates().plain);
            let port = gates().l3_port;
            if port != 0 {
                rtp.listen(port);
            }
        }
        let epoch = rtp::media_epoch();
        rtp.bind_epoch(epoch);
        Self {
            id,
            rtp,
            avb_seq: 0,
            avb,
            picture_id: 1,
            audio_id: 1,
            packets: Vec::new(),
            ethertype: 0,
            picture_pacer: Pacer::new(RATE_PICTURE),
            audio_pacer: Pacer::new(RATE_AUDIO),
            frame_due: None,
            st_pic_seq: 0,
            st_pic_ts: 0,
            st_frame: 0,
            st_aud_seq: 0,
            st_pic_ssrc: rtp::fresh_ssrc(),
            st_aud_ssrc: rtp::fresh_ssrc(),
            st_gcm: None,
            epoch,
        }
    }

    fn current(&self) -> bool {
        self.epoch == rtp::media_epoch()
    }

    pub fn id(&self) -> u8 {
        self.id
    }

    /// Store one RTCP receiver report. A second report inside one second is ignored.
    pub fn ingest_rtcp(&mut self, packet: &[u8]) -> Option<f64> {
        self.rtp.ingest_rtcp(std::time::Instant::now(), packet)
    }

    pub fn rtcp_loss(&self) -> Option<f64> {
        self.rtp.rtcp_loss()
    }

    /// UDP/RTP uses the stored receiver-report fraction. Other carriers leave the cap.
    pub fn drive_cap(&mut self, rate: &mut RateCap, dt: Duration) -> u64 {
        if self.id == UDP_RTP {
            if let Some(loss) = self.rtp.rtcp_loss().or_else(rtp::latest_rtcp_loss) {
                rate.observe(dt, loss);
            }
        }
        rate.cap
    }

    pub fn packets(&self) -> &[Vec<u8>] {
        &self.packets
    }

    pub fn take_packets(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.packets)
    }

    pub fn ethertype(&self) -> u16 {
        self.ethertype
    }

    /// `body` is the record the QUIC reader would hand the decoder.
    /// `drop_index` omits that slice. Carriers 4 and 5 ignore a lane byte.
    pub fn send(
        &mut self,
        lane: Lane,
        body: &[u8],
        drop_index: Option<u16>,
        capture: Duration,
    ) -> Option<Vec<u8>> {
        if self.id >= UDP_RTP && !self.current() {
            return None;
        }
        match self.id {
            UDP_RTP => {
                let id = self.bump(lane);
                let got = self
                    .rtp
                    .send_record(lane == Lane::Picture, id, body, drop_index, capture);
                for packet in self.rtp.drain() {
                    crate::capture::note_rtp(crate::capture::DIR_SEND, &packet);
                    self.packets.push(packet);
                }
                got
            }
            AVB => self.send_avb(lane, body, drop_index, capture),
            ST2110 => self.send_st2110(lane, body, drop_index, capture),
            _ => None,
        }
    }

    /// Uncompressed picture. Real RFC 4175 RTP from the `st2110` crate.
    /// Rows wait for their `send_at` so the frame is not one burst.
    pub fn send_raster(&mut self, samples: &[u8], width: u32, height: u32) -> Vec<Vec<u8>> {
        if !self.current() {
            return Vec::new();
        }
        let frame = st_frame();
        if self.late(Duration::ZERO, frame) {
            return Vec::new();
        }
        self.frame_due = Some(frame);
        let packets = st2110::packetize_rfc4175(samples, width, height, ST_FRAME_TICKS);
        let origin = std::time::Instant::now();
        let mut out = Vec::with_capacity(packets.len());
        for row in packets {
            if !self.current() {
                return out;
            }
            pace_until(origin, row.send_at);
            let mut raw = row.rtp;
            stamp_ssrc(&mut raw, self.st_pic_ssrc);
            crate::capture::note_rtp(crate::capture::DIR_SEND, &raw);
            if !rtp::emit_if_current(self.epoch, true, &raw) {
                return out;
            }
            out.push(raw);
        }
        self.packets.extend(out.iter().cloned());
        out
    }

    fn late(&self, capture: Duration, frame: Duration) -> bool {
        self.frame_due
            .is_some_and(|due| ::st2110::drop_late_frame(capture.saturating_sub(due), frame))
    }

    fn bump(&mut self, lane: Lane) -> u16 {
        let slot = if lane == Lane::Audio {
            &mut self.audio_id
        } else {
            &mut self.picture_id
        };
        let id = *slot;
        *slot = slot.wrapping_add(1);
        id
    }

    fn send_avb(
        &mut self,
        lane: Lane,
        body: &[u8],
        drop_index: Option<u16>,
        capture: Duration,
    ) -> Option<Vec<u8>> {
        if ::avb::try_reserve().is_err() {
            note_reservation(false);
            return None;
        }
        let fabric = self.avb.clone().expect("avb fabric");
        let dst = [2, 0, 0, 0, 0, 4];
        let src = [2, 0, 0, 0, 0, 1];
        let id = self.bump(lane);
        let slices = split_record(id, body, 1000);
        let mut asm = Assembler::new();
        let now = std::time::Instant::now();
        let mut got = None;
        let capture = capture.max(Duration::from_millis(1));
        let timestamp = if lane == Lane::Audio {
            rtp::audio_rtp_timestamp(capture, 48_000).max(1)
        } else {
            rtp::picture_rtp_timestamp(capture).max(1)
        };
        let origin = std::time::Instant::now();
        let mut earliest = capture;
        for slice in &slices {
            if drop_index == Some(slice.index) {
                continue;
            }
            let mut avtp = AvtpFrame::new(src, lane, self.avb_seq, &slice.encode());
            avtp.timestamp = timestamp;
            self.avb_seq = self.avb_seq.wrapping_add(1);
            let frame = crate::frame::Frame {
                dst,
                src,
                ether_type: ETHERTYPE_AVTP,
                dst_service: 0,
                src_service: 0,
                payload: avtp.encode(),
            };
            let Some(received) = fabric.deliver(dst, frame) else {
                continue;
            };
            self.ethertype = received.ether_type;
            let raw = crate::frame::encode_frame(&received);
            let nbytes = raw.len();
            let at = {
                let pacer = if lane == Lane::Audio {
                    &mut self.audio_pacer
                } else {
                    &mut self.picture_pacer
                };
                pacer.schedule(earliest, nbytes)
            };
            earliest = at;
            if !self.current() {
                return got;
            }
            pace_until(origin, at);
            crate::capture::note_avtp(crate::capture::DIR_SEND, &raw);
            if !rtp::emit_if_current(self.epoch, lane != Lane::Audio, &raw) {
                return got;
            }
            self.packets.push(raw);
            let Some(decoded) = AvtpFrame::decode(&received.payload) else {
                continue;
            };
            let Some(slice) = Slice::decode(&decoded.payload) else {
                continue;
            };
            if let Some(record) = asm.push(slice, now, now + Duration::from_secs(1)) {
                got = Some(record);
            }
        }
        if got.is_some() {
            note_reservation(true);
        }
        got
    }

    fn send_st2110(
        &mut self,
        lane: Lane,
        body: &[u8],
        drop_index: Option<u16>,
        capture: Duration,
    ) -> Option<Vec<u8>> {
        let frame = st_frame();
        if self.late(capture, frame) {
            self.frame_due = Some(capture.saturating_add(frame));
            return None;
        }
        self.frame_due = Some(capture.saturating_add(frame));
        if lane == Lane::Audio {
            return self.send_pcm(body);
        }
        let packets = st2110::packetize_jpeg_xs_at(
            body,
            ST_FRAME_TICKS,
            self.st_pic_seq,
            self.st_pic_ts,
            self.st_frame,
        );
        let count = u16::try_from(packets.len()).unwrap_or(u16::MAX);
        self.st_pic_seq = self.st_pic_seq.wrapping_add(count);
        self.st_pic_ts = self.st_pic_ts.wrapping_add(ST_FRAME_TICKS);
        self.st_frame = self.st_frame.wrapping_add(1) & 0x1f;
        let mut asm = XsAssembler::new();
        let mut missing = false;
        let mut got = None;
        let origin = std::time::Instant::now();
        let picture = lane != Lane::Audio;
        for (index, packet) in packets.iter().enumerate() {
            if drop_index == Some(index as u16) {
                missing = true;
                continue;
            }
            if !self.current() {
                return None;
            }
            pace_until(origin, packet.send_at);
            let mut raw = packet.rtp.clone();
            stamp_ssrc(&mut raw, self.st_pic_ssrc);
            let rtp = seal_st(self, &raw);
            if rtp.is_empty() {
                missing = true;
                continue;
            }
            crate::capture::note_rtp(crate::capture::DIR_SEND, &rtp);
            if !rtp::emit_if_current(self.epoch, picture, &rtp) {
                return None;
            }
            self.packets.push(rtp);
            let media = packet.payload.as_slice();
            let first = index == 0 && drop_index != Some(0);
            if let Some(record) = asm.push(1, first, packet.marker, media) {
                got = Some(record);
            }
        }
        if missing {
            None
        } else {
            got
        }
    }

    /// ST 2110-30 level A. One millisecond of PCM, not a JPEG XS frame.
    fn send_pcm(&mut self, body: &[u8]) -> Option<Vec<u8>> {
        if body.is_empty() {
            return None;
        }
        // A live audio body is a `D` record: kind, rate u32le, channels u16le,
        // then i16le samples. ST 2110-30 carries only the samples, at the
        // record's channel count. Sending the header as samples and calling
        // it mono left the player with records it could not decode.
        let (mut samples, channels, mut rate) = match pcm_record(body) {
            Some(parts) => parts,
            None => (pcm_samples(body), 1, 48_000),
        };
        // Level A is 48 kHz. The player counts channels as samples per 1 ms
        // packet divided by 48, so any other rate shifts that count and the
        // interleaved samples play as noise.
        if rate != 48_000 {
            samples = resample_pcm(&samples, channels, rate, 48_000);
            rate = 48_000;
        }
        let mut packets = ::st2110::packetize_pcm(&samples, channels, rate);
        if packets.is_empty() {
            return None;
        }
        for packet in &mut packets {
            let seq = self.st_aud_seq;
            self.st_aud_seq = self.st_aud_seq.wrapping_add(1);
            if packet.rtp.len() >= 12 {
                packet.rtp[2..4].copy_from_slice(&seq.to_be_bytes());
                packet.rtp[8..12].copy_from_slice(&self.st_aud_ssrc.to_be_bytes());
            }
        }
        let origin = std::time::Instant::now();
        for packet in &packets {
            if !self.current() {
                return None;
            }
            pace_until(origin, packet.send_at);
            let rtp = seal_st(self, &packet.rtp);
            if rtp.is_empty() {
                continue;
            }
            crate::capture::note_rtp(crate::capture::DIR_SEND, &rtp);
            if !rtp::emit_if_current(self.epoch, false, &rtp) {
                return None;
            }
            self.packets.push(rtp);
        }
        Some(body.to_vec())
    }
}

/// One 60 fps frame on the 90 kHz ST 2110 clock. Not a one-second tick count.
const ST_FRAME_TICKS: u32 = 1_500;

fn st_frame() -> Duration {
    Duration::from_nanos(u64::from(ST_FRAME_TICKS) * 1_000_000_000 / 90_000)
}

/// Samples, channels, and rate of a `D` PCM record, or `None` for other bodies.
fn pcm_record(body: &[u8]) -> Option<(Vec<i16>, u16, u32)> {
    if body.len() < 7 || body[0] != crate::negotiate::RECORD_DATA || (body.len() - 7) % 2 != 0 {
        return None;
    }
    let rate = u32::from_le_bytes([body[1], body[2], body[3], body[4]]);
    let channels = u16::from_le_bytes([body[5], body[6]]);
    if rate == 0 || channels == 0 {
        return None;
    }
    let samples = body[7..]
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Some((samples, channels, rate))
}

fn resample_pcm(samples: &[i16], channels: u16, from: u32, to: u32) -> Vec<i16> {
    let ch = usize::from(channels.max(1));
    let frames = samples.len() / ch;
    if frames == 0 || from == 0 || to == 0 || from == to {
        return samples.to_vec();
    }
    let out_frames = ((u64::from(to) * frames as u64) / u64::from(from)).max(1) as usize;
    let mut out = Vec::with_capacity(out_frames * ch);
    for i in 0..out_frames {
        let src = if out_frames == 1 {
            0
        } else {
            (i as u64 * (frames as u64 - 1) / (out_frames as u64 - 1)) as usize
        };
        let src = src.min(frames - 1) * ch;
        out.extend_from_slice(&samples[src..src + ch]);
    }
    out
}

fn pcm_samples(body: &[u8]) -> Vec<i16> {
    let mut padded = body.to_vec();
    if padded.len() % 2 == 1 {
        padded.push(0);
    }
    padded
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

fn stamp_ssrc(rtp: &mut [u8], ssrc: u32) {
    if rtp.len() >= 12 {
        rtp[8..12].copy_from_slice(&ssrc.to_be_bytes());
    }
}

/// Seal the RTP payload when the operator chose ST 2110 `gcm`. The header stays clear.
///
/// ST 2110 does not run the UDP `OFFER` exchange, so the key is the
/// certificate session key on both sides. A key still stored on the RTP hub
/// belongs to the previous UDP player. An empty return means the seal failed.
/// Sending the clear payload would not open, and the player would keep the
/// nonce and then reject the real packet.
fn seal_st(seam: &mut Seam, rtp: &[u8]) -> Vec<u8> {
    if !gates().st_gcm || rtp.len() < 12 {
        return rtp.to_vec();
    }
    if seam.st_gcm.is_none() {
        seam.st_gcm = Some(rtp::GcmSession::new(rtp::session_key()));
    }
    let seq = u16::from_be_bytes([rtp[2], rtp[3]]);
    let ssrc = u32::from_be_bytes([rtp[8], rtp[9], rtp[10], rtp[11]]);
    let Some(session) = seam.st_gcm.as_mut() else {
        return Vec::new();
    };
    match session.seal_next(ssrc, seq, &rtp[12..]) {
        Ok(sealed) => {
            let mut out = rtp[..12].to_vec();
            out.extend(sealed);
            out
        }
        Err(_) => Vec::new(),
    }
}

fn pace_until(origin: std::time::Instant, send_at: Duration) {
    let target = origin.checked_add(send_at).unwrap_or(origin);
    let Some(wait) = target.checked_duration_since(std::time::Instant::now()) else {
        return;
    };
    if wait.is_zero() {
        return;
    }
    // Picture and audio sends run on the host's current-thread QUIC runtime.
    // Sleeping there freezes accept and the idle timer: the next player times
    // out, and a player that already left never shows as disconnected.
    if tokio::runtime::Handle::try_current().is_ok() {
        return;
    }
    std::thread::sleep(wait);
}

fn seam_loop(id: u8, picture: &[u8], audio: &[u8], drop_slice: bool) -> LoopReport {
    if id == AVB {
        ::avb::note_listener(true);
    }
    let mut seam = Seam::open(id);
    let drop = drop_slice.then_some(0);
    let capture = Duration::from_millis(1);
    let picture_out = seam.send(Lane::Picture, picture, drop, capture);
    let audio_out = seam.send(Lane::Audio, audio, None, capture);
    let next = seam.send(Lane::Picture, b"next-picture", None, capture);
    LoopReport {
        picture: picture_out,
        audio: audio_out,
        next_picture: next,
        ethertype: seam.ethertype(),
        retried: false,
        datagrams: seam.packets().len(),
        wire: seam.packets().to_vec(),
    }
}

fn sliced_loop(id: u8, picture: &[u8], audio: &[u8], drop_slice: bool, lane_byte: bool) -> LoopReport {
    let ethertype = if id == AVB {
        ETHERTYPE_AVTP
    } else {
        crate::ETHERTYPE_QUIC
    };
    let budget = 48usize;
    let (picture_out, datagrams) = deliver_sliced(1, picture, budget, drop_slice, lane_byte);
    let (audio_out, more) = deliver_sliced(2, audio, budget, false, lane_byte);
    let (next, _) = deliver_sliced(3, b"next-picture", budget, false, lane_byte);
    LoopReport {
        picture: picture_out,
        audio: audio_out,
        next_picture: next,
        ethertype,
        retried: false,
        datagrams: datagrams + more,
        wire: Vec::new(),
    }
}

fn deliver_sliced(
    record_id: u16,
    bytes: &[u8],
    budget: usize,
    drop_first: bool,
    lane_byte: bool,
) -> (Option<Vec<u8>>, usize) {
    let slices = split_record(record_id, bytes, budget);
    let mut asm = Assembler::new();
    let now = std::time::Instant::now();
    let deadline = now + Duration::from_millis(30);
    let mut got = None;
    let mut n = 0usize;
    for (i, slice) in slices.iter().enumerate() {
        if drop_first && i == 0 {
            continue;
        }
        let _ = lane_byte;
        n += 1;
        if let Some(record) = asm.push(slice.clone(), now, deadline) {
            got = Some(record);
        }
    }
    (got, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct GateReset;

    impl Drop for GateReset {
        fn drop(&mut self) {
            set_l3_port(0);
            set_avb_enabled(false);
            set_st2110_enabled(false, false, 1_000, false);
            set_audio_opus(false);
            set_plain_profile(false);
            note_reservation(false);
            note_locked(false);
            note_st_gcm(false);
            ::avb::note_listener(false);
        }
    }

    #[test]
    fn missing_tlv_is_carrier_1_and_unknown_bytes_still_parse() {
        let _globals = crate::carrier::test_globals();
        assert_eq!(effective_carriers(&[]), vec![QUIC_STREAMS]);
        let value = encode_carrier_tlv(&[1, 2, 9]);
        assert_eq!(parse_carrier_tlv(&value).unwrap(), vec![1, 2, 9]);
        assert!(parse_carrier_tlv(&[9, 1]).is_none());
    }

    #[test]
    fn announce_omits_what_this_machine_cannot_open() {
        let _globals = crate::carrier::test_globals();
        let mut input = Announce {
            android: true,
            has_l2: false,
            has_udp: true,
            avb_enabled: true,
            st2110_enabled: true,
            nic_mbps: 10_000,
            override_speed: false,
            picture_ok_for_st2110: true,
            audio_opus: false,
            rtp_port_taken: false,
            st2110_port_taken: false,
        };
        let ids = announce(&input);
        assert!(ids.contains(&QUIC_STREAMS));
        assert!(ids.contains(&UDP_RTP));
        assert!(!ids.contains(&AVB), "android has no AVB");
        assert!(ids.contains(&ST2110));
        input.audio_opus = true;
        assert!(!announce(&input).contains(&ST2110));
        input.audio_opus = false;
        input.picture_ok_for_st2110 = false;
        assert!(!announce(&input).contains(&ST2110));
        input.picture_ok_for_st2110 = true;
        input.nic_mbps = 1_000;
        assert!(!announce(&input).contains(&ST2110));
        input.override_speed = true;
        assert!(announce(&input).contains(&ST2110));
        input.rtp_port_taken = true;
        assert!(!announce(&input).contains(&UDP_RTP));
        input.android = false;
        input.has_l2 = true;
        input.avb_enabled = false;
        assert!(!announce(&input).contains(&AVB));
        input.avb_enabled = true;
        assert!(announce(&input).contains(&AVB));
    }

    #[test]
    fn alpn_v1_is_carrier_1_and_a_mismatch_closes() {
        let _globals = crate::carrier::test_globals();
        assert_eq!(agree(false, Some(2), &[1, 2]).unwrap(), 1);
        assert_eq!(agree(true, None, &[]).unwrap(), 1);
        assert_eq!(agree(true, Some(2), &[1, 2]).unwrap(), 2);
        assert!(agree(true, Some(4), &[1, 2]).is_err());
        assert!(config_matches(true, Some(2), Some(1)).is_err());
        assert_eq!(config_matches(true, Some(1), None).unwrap(), 1);
        assert_eq!(config_matches(false, Some(2), Some(2)).unwrap(), 1);
    }

    #[test]
    fn one_loss_sample_does_not_cut_the_cap() {
        let _globals = crate::carrier::test_globals();
        let mut cap = RateCap::new(100_000_000);
        cap.observe(Duration::from_millis(1), 1.0);
        assert_eq!(cap.cap, 100_000_000);
        cap.observe(Duration::from_secs(1), 0.02);
        assert_eq!(cap.cap, 80_000_000);
        cap.observe(Duration::from_secs(1), 0.0);
        assert_eq!(cap.cap, 100_000_000);
    }

    #[test]
    fn offer_and_config_echo_the_carrier_line() {
        let _globals = crate::carrier::test_globals();
        use crate::negotiate::{build_offer, Offer, OfferInput, StreamConfig, Video};
        let mut offer = build_offer(&OfferInput {
            want_dirty_rects: true,
            ..OfferInput::default()
        });
        assert_eq!(offer.carrier, None);
        offer.carrier = Some(QUIC_DATAGRAMS);
        let back = Offer::decode(&offer.encode()).unwrap();
        assert_eq!(back.carrier, Some(QUIC_DATAGRAMS));
        let text = String::from_utf8(offer.encode()).unwrap();
        assert!(text.contains("carrier 2"));
        let mut config = StreamConfig::video(Video::Mjpeg, "jpeg", 16, 16, 30);
        config.carrier = Some(QUIC_DATAGRAMS);
        let echoed = StreamConfig::decode(&config.encode()).unwrap();
        assert_eq!(echoed.carrier, Some(QUIC_DATAGRAMS));
        assert!(config_matches(true, Some(QUIC_DATAGRAMS), echoed.carrier).is_ok());
        assert!(config_matches(true, Some(UDP_RTP), echoed.carrier).is_err());
    }

    #[test]
    fn this_machine_leaves_out_a_carrier_it_cannot_open() {
        let _globals = crate::carrier::test_globals();
        let _announce = ANNOUNCE.lock().unwrap_or_else(|poison| poison.into_inner());
        let _restore = GateReset;
        set_l3_port(0);
        set_avb_enabled(false);
        set_st2110_enabled(false, false, 1_000, false);
        let plain = carriers_now(0);
        assert!(plain.contains(&QUIC_STREAMS) && plain.contains(&QUIC_DATAGRAMS));
        assert!(!plain.contains(&UDP_RTP));
        assert!(!plain.contains(&AVB));
        assert!(!plain.contains(&ST2110));

        let mut port = 0u16;
        for candidate in 20_000..20_200 {
            if udp_port_free(candidate)
                && udp_port_free(candidate + 1)
                && udp_port_free(candidate + 2)
                && udp_port_free(candidate + 4)
            {
                port = candidate;
                break;
            }
        }
        assert_ne!(port, 0);
        let held = [
            std::net::UdpSocket::bind(("0.0.0.0", port)).unwrap(),
            std::net::UdpSocket::bind(("0.0.0.0", port + 1)).unwrap(),
            std::net::UdpSocket::bind(("0.0.0.0", port + 2)).unwrap(),
        ];
        set_l3_port(port);
        set_avb_enabled(true);
        assert!(!carriers_now(0).contains(&UDP_RTP), "taken rtp ports");
        assert!(carriers_now(0).contains(&AVB));
        drop(held);
        set_st2110_enabled(true, true, 10_000, false);
        let open = carriers_now(0);
        assert!(open.contains(&UDP_RTP));
        assert!(open.contains(&AVB));
        assert!(open.contains(&ST2110));
        set_st2110_enabled(true, false, 10_000, false);
        assert!(!carriers_now(0).contains(&ST2110), "picture kind stays off");
        prepare_announce(true, false);
        assert!(carriers_now(0).contains(&ST2110), "jpeg xs opens the picture gate");
        prepare_announce(true, true);
        assert!(!carriers_now(0).contains(&ST2110), "opus suppresses st 2110");
        prepare_announce(true, false);
        assert!(carriers_now(0).contains(&ST2110));
    }

    #[test]
    fn the_session_packetizes_rfc4175_and_jpeg_xs() {
        let _globals = crate::carrier::test_globals();
        let mut seam = Seam::open(ST2110);
        let samples = vec![1u8; (RFC4175_WIDTH * RFC4175_HEIGHT * 3) as usize];
        let packets = seam.send_raster(&samples, RFC4175_WIDTH, RFC4175_HEIGHT);
        assert_eq!(packets.len(), RFC4175_HEIGHT as usize);
        let (last, _) = RtpHeader::decode(packets.last().unwrap()).unwrap();
        assert!(last.marker);
        let (first, _) = RtpHeader::decode(&packets[0]).unwrap();
        assert!(!first.marker);

        // The ST 2110 crate puts 1200 media bytes in each RTP packet.
        let coded = vec![9u8; 2500];
        let paced = std::time::Instant::now();
        assert_eq!(
            seam.send(Lane::Picture, &coded, None, Duration::from_millis(1))
                .as_deref(),
            Some(coded.as_slice())
        );
        let waited = paced.elapsed();
        assert!(
            waited > Duration::from_millis(8),
            "jpeg xs packets wait across the frame, waited {waited:?}"
        );
        assert!(
            waited < Duration::from_millis(80),
            "one frame is not a one-second sleep, waited {waited:?}"
        );
        let stamps: Vec<u32> = packets
            .iter()
            .map(|packet| RtpHeader::decode(packet).unwrap().0.timestamp)
            .collect();
        assert!(stamps.iter().all(|stamp| *stamp == stamps[0]));
        let xs = seam.packets();
        let xs = &xs[packets.len()..];
        assert!(xs.len() > 1);
        let (header, _) = RtpHeader::decode(xs.last().unwrap()).unwrap();
        assert!(header.marker);
        let (earlier, _) = RtpHeader::decode(&xs[0]).unwrap();
        assert!(!earlier.marker);

        let holed = vec![4u8; 2500];
        assert!(
            seam.send(Lane::Picture, &holed, Some(1), Duration::from_millis(1)).is_none(),
            "a missing JPEG XS slice must not deliver a partial frame"
        );
        assert_eq!(
            seam.send(Lane::Picture, b"next-xs", None, Duration::from_millis(1)).as_deref(),
            Some(b"next-xs".as_slice())
        );
        let audio: Vec<u8> = (0..48i16).flat_map(|sample| sample.to_le_bytes()).collect();
        assert_eq!(
            seam.send(Lane::Audio, &audio, None, Duration::from_millis(1)).as_deref(),
            Some(audio.as_slice())
        );
        assert!(seam.send(Lane::Audio, b"pcm-level-a-16b", None, Duration::from_millis(1)).is_none());
        assert!(seam.packets().iter().any(|packet| {
            packet.len() > 12 && packet[1] & 0x7f == 97 && packet[1] & 0x80 == 0
        }));
        assert!(
            seam.send(Lane::Picture, b"behind", None, Duration::from_millis(80)).is_none(),
            "a send more than one frame behind drops the frame in hand"
        );
    }

    #[test]
    fn a_failed_reservation_does_not_send_and_a_large_picture_is_paced() {
        let _globals = crate::carrier::test_globals();
        ::avb::note_listener(false);
        note_reservation(false);
        let mut blocked = Seam::open(AVB);
        assert!(blocked
            .send(Lane::Picture, b"no-listener", None, Duration::from_millis(1))
            .is_none());
        assert!(connect_row_now(AVB).contains("shaped"));
        assert!(!connect_row_now(AVB).contains("reserved"));

        ::avb::note_listener(true);
        let picture = vec![3u8; 100_000];
        let mut seam = Seam::open(AVB);
        let started = std::time::Instant::now();
        let got = seam.send(Lane::Picture, &picture, None, Duration::from_millis(1));
        let elapsed = started.elapsed();
        assert_eq!(got.as_deref(), Some(picture.as_slice()));
        assert!(connect_row_now(AVB).contains("reserved"));
        let wire: usize = seam.packets().iter().map(|packet| packet.len()).sum();
        let min_ns = (wire as u64).saturating_mul(8).saturating_mul(1_000_000_000) / RATE_PICTURE;
        assert!(
            elapsed + Duration::from_millis(5) >= Duration::from_nanos(min_ns),
            "100 kB was not paced inside 200 Mbit/s ({elapsed:?}, {wire} bytes)"
        );
        ::avb::note_listener(false);
        note_reservation(false);
    }

    #[test]
    fn the_operator_can_choose_udp_plain_and_st2110_gcm() {
        let _globals = crate::carrier::test_globals();
        choose_profile(true, false);
        let mut rtp = Seam::open(UDP_RTP);
        let body = b"clear-rtp-body-0123456789abcd";
        assert_eq!(
            rtp.send(Lane::Picture, body, None, Duration::from_millis(1))
                .as_deref(),
            Some(body.as_slice())
        );
        let (_, payload) = RtpHeader::decode(&rtp.packets()[0]).unwrap();
        let clear = Slice {
            record_id: 1,
            index: 0,
            count: 1,
            bytes: body.to_vec(),
        }
        .encode();
        assert_eq!(payload, clear.as_slice());
        assert!(connect_row_now(UDP_RTP).contains("plain"));

        choose_profile(false, true);
        let mut st = Seam::open(ST2110);
        let coded = b"xs-gcm-body";
        assert_eq!(
            st.send(Lane::Picture, coded, None, Duration::from_millis(1))
                .as_deref(),
            Some(coded.as_slice())
        );
        let (_, payload) = RtpHeader::decode(st.packets().last().unwrap()).unwrap();
        assert_ne!(&payload[..4], ::st2110::xs_header(true, 0, 0).as_slice());
        assert!(connect_row_now(ST2110).contains("gcm"));
        choose_profile(false, false);
    }

    /// The config record and the picture are separate sends. Both have to open
    /// on the one session the player keeps.
    #[test]
    fn st2110_gcm_nonces_continue_across_frames() {
        let _globals = crate::carrier::test_globals();
        choose_profile(false, true);
        let mut st = Seam::open(ST2110);
        let first = b"frame-one-0123456789";
        let second = b"frame-two-0123456789";
        assert_eq!(
            st.send(Lane::Picture, first, None, Duration::from_millis(1))
                .as_deref(),
            Some(first.as_slice())
        );
        // The second capture has to stay inside one frame of the first due
        // time. A 40 ms gap is late on the 16 ms ST 2110 clock and sends nothing.
        assert_eq!(
            st.send(Lane::Picture, second, None, Duration::from_millis(10))
                .as_deref(),
            Some(second.as_slice())
        );
        let mut player = GcmSession::new(rtp::session_key());
        let mut last_seq = None;
        assert!(st.packets().len() >= 2, "each frame leaves at least one packet");
        for packet in st.packets() {
            let (header, sealed) = RtpHeader::decode(packet).unwrap();
            if let Some(prev) = last_seq {
                assert_ne!(header.sequence, prev, "a later frame reused sequence {prev}");
            }
            last_seq = Some(header.sequence);
            let plain = player
                .open_next(header.ssrc, header.sequence, sealed)
                .expect("player session stays open");
            assert!(::st2110::xs_fields(&plain).is_some());
        }
        assert!(!player.closed());
        choose_profile(false, false);
    }

    /// A leftover packet shares the certificate key. Its SSRC has to be
    /// different, or the high sequence makes the next seam's packets too old.
    #[test]
    fn st2110_gcm_ssrcs_stay_on_their_own_replay_window() {
        let _globals = crate::carrier::test_globals();
        choose_profile(false, true);
        let mut older = Seam::open(ST2110);
        let mut newer = Seam::open(ST2110);
        let body = b"frame-one-0123456789";
        assert!(older
            .send(Lane::Picture, body, None, Duration::from_millis(1))
            .is_some());
        assert!(newer
            .send(Lane::Picture, body, None, Duration::from_millis(1))
            .is_some());
        let (old_header, old_payload) = RtpHeader::decode(&older.packets()[0]).unwrap();
        let (new_header, new_payload) = RtpHeader::decode(&newer.packets()[0]).unwrap();
        assert_ne!(old_header.ssrc, new_header.ssrc);
        assert_ne!(old_header.ssrc, 0);
        assert_ne!(new_header.ssrc, 5, "picture ssrc is no longer the fixed value");
        let mut player = GcmSession::new(rtp::session_key());
        player
            .open_next(old_header.ssrc, old_header.sequence, old_payload)
            .expect("old seam opens");
        player
            .open_next(new_header.ssrc, new_header.sequence, new_payload)
            .expect("new ssrc has its own window");
        assert!(!player.closed());
        choose_profile(false, false);
    }

    #[test]
    fn a_replaced_hub_drops_a_st2110_gcm_packet() {
        let _globals = crate::carrier::test_globals();
        choose_profile(false, true);
        let mut st = Seam::open(ST2110);
        release_rtp();
        assert!(st
            .send(Lane::Picture, b"frame-one-0123456789", None, Duration::from_millis(1))
            .is_none());
        assert!(st.packets().is_empty());
        choose_profile(false, false);
    }

    #[test]
    fn udp_gcm_picture_ssrcs_differ_per_seam() {
        let _globals = crate::carrier::test_globals();
        let mut older = Seam::open(UDP_RTP);
        let mut newer = Seam::open(UDP_RTP);
        let body = b"jpeg-picture-body-0123456789";
        assert!(older
            .send(Lane::Picture, body, None, Duration::from_millis(1))
            .is_some());
        assert!(newer
            .send(Lane::Picture, body, None, Duration::from_millis(1))
            .is_some());
        let (old_header, _) = RtpHeader::decode(&older.packets()[0]).unwrap();
        let (new_header, _) = RtpHeader::decode(&newer.packets()[0]).unwrap();
        assert_ne!(old_header.ssrc, new_header.ssrc);
        assert_ne!(old_header.ssrc, 11);
        assert_ne!(new_header.ssrc, 11);
    }

    #[test]
    fn carrier_3_default_profile_seals_the_rtp_payload() {
        let _globals = crate::carrier::test_globals();
        let before = rtp_sealed();
        let mut seam = Seam::open(UDP_RTP);
        let body = b"jpeg-picture-body-0123456789";
        assert_eq!(
            seam.send(Lane::Picture, body, None, Duration::from_secs(1))
                .as_deref(),
            Some(body.as_slice())
        );
        let packets = seam.packets();
        assert!(!packets.is_empty());
        let (header, payload) = RtpHeader::decode(&packets[0]).unwrap();
        let clear = Slice {
            record_id: 1,
            index: 0,
            count: 1,
            bytes: body.to_vec(),
        }
        .encode();
        assert_ne!(payload, clear.as_slice(), "default profile is gcm, not clear RTP");
        assert!(rtp_sealed() > before, "send_record must call GcmSession::seal");
        let mut gcm = GcmSession::new(rtp::session_key());
        let plain = gcm
            .open(header.ssrc, 0, header.sequence, payload)
            .expect("player opens with the same session key");
        assert_eq!(plain, clear);
    }

    #[test]
    fn an_rtcp_report_cuts_the_cap_and_a_second_report_inside_one_second_is_ignored() {
        let _globals = crate::carrier::test_globals();
        let mut seam = Seam::open(UDP_RTP);
        let packet = rtp::rtcp_receiver_report(9, 11, 100, 10, 50);
        let loss = seam.ingest_rtcp(&packet).expect("receiver report");
        assert!(loss > 0.01);
        assert!(seam.ingest_rtcp(&packet).is_none());
        let mut rate = RateCap::new(1_000_000);
        let cap = seam.drive_cap(&mut rate, Duration::from_secs(1));
        assert!(cap < 1_000_000, "loss fraction from the report cut the encoder cap");
    }

    #[test]
    fn connect_row_names_the_profile_and_an_untagged_frame() {
        let _globals = crate::carrier::test_globals();
        let _announce = ANNOUNCE.lock().unwrap_or_else(|poison| poison.into_inner());
        assert!(connect_row(AVB, false, false, false, true).contains("not a priority class"));
        assert!(connect_row(AVB, true, false, false, true).contains("shaped"));
        let st = connect_row(ST2110, false, false, false, false);
        assert!(st.contains("plain"));
        assert!(st.contains("not locked"));
        assert!(connect_row(UDP_RTP, false, false, false, true).contains("gcm"));
        note_reservation(false);
        assert!(connect_row_now(AVB).contains("shaped"));
        note_reservation(true);
        assert!(connect_row_now(AVB).contains("reserved"));
        note_reservation(false);
        set_plain_profile(true);
        assert!(connect_row_now(UDP_RTP).contains("plain"));
        set_plain_profile(false);
        assert!(connect_row_now(UDP_RTP).contains("gcm"));
        note_st_gcm(false);
        assert!(connect_row_now(ST2110).contains("plain"));
        note_st_gcm(true);
        assert!(connect_row_now(ST2110).contains("gcm"));
        note_st_gcm(false);
    }

    #[test]
    fn each_carrier_loopback_delivers_or_drops_without_blocking() {
        let _globals = crate::carrier::test_globals();
        let picture = vec![7u8; 200];
        let audio = b"pcm-fixed-record".to_vec();
        // ST 2110-30 Level A needs 1 ms of 48 kHz PCM. Opaque bytes that are
        // shorter than that never leave send_pcm.
        let st_audio: Vec<u8> = (0..48i16)
            .flat_map(|sample| (1000 - sample).to_le_bytes())
            .collect();
        for id in 1..=5 {
            let audio_in = if id == ST2110 { &st_audio } else { &audio };
            let clean = run_loopback(id, &picture, audio_in, false);
            assert_eq!(clean.picture.as_deref(), Some(picture.as_slice()), "id {id}");
            assert_eq!(clean.audio.as_deref(), Some(audio_in.as_slice()), "id {id}");
            assert_eq!(clean.next_picture.as_deref(), Some(b"next-picture".as_slice()));
            if id == 1 {
                assert_eq!(clean.ethertype, crate::ETHERTYPE_QUIC);
                assert_eq!(clean.datagrams, 0);
                let dropped = run_loopback(id, &picture, audio_in, true);
                assert_eq!(dropped.picture.as_deref(), Some(picture.as_slice()));
                assert!(dropped.retried);
            } else {
                let dropped = run_loopback(id, &picture, audio_in, true);
                assert!(dropped.picture.is_none(), "partial id {id}");
                assert_eq!(dropped.next_picture.as_deref(), Some(b"next-picture".as_slice()));
            }
            if id == AVB {
                assert_eq!(clean.ethertype, ETHERTYPE_AVTP);
                assert!(!clean.wire.is_empty());
                assert_eq!(
                    &clean.wire[0][12..14],
                    &ETHERTYPE_AVTP.to_be_bytes(),
                    "AVB frame ethertype"
                );
            }
            if id == ST2110 {
                let (header, _) = RtpHeader::decode(clean.wire.last().unwrap()).unwrap();
                assert!(header.marker);
            }
            if id == QUIC_STREAMS {
                assert_eq!(
                    &clean.wire[0][12..14],
                    &crate::ETHERTYPE_QUIC.to_be_bytes()
                );
            }
            if id == QUIC_DATAGRAMS {
                assert!(clean.datagrams > 1, "sliced datagrams {}", clean.datagrams);
            }
        }
    }

    #[test]
    fn an_l2_offer_uses_the_scanned_rtp_port() {
        let _globals = crate::carrier::test_globals();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3));
        let rtp = SocketAddr::new(ip, 21_002);
        assert_eq!(rtp_socket(Some(ip), 21_002, 4433), Some(rtp));
        assert_eq!(rtp_socket(Some(ip), 0, 4433), None);
        assert_eq!(rtp_socket(Some(ip), 4433, 4433), None);
        assert_eq!(rtp_socket(None, 21_002, 4433), None);
        assert_eq!(player_host_for(None, true, Some(rtp)), Some(rtp));
        assert_eq!(player_host_for(None, true, None), None);
        assert_eq!(
            player_host_for(None, true, Some(SocketAddr::new(ip, 0))),
            None
        );
        let (offer, socket) = l2_media_target(UDP_RTP, None, Some(ip), 21_002, 4433);
        assert!(offer);
        assert_eq!(socket, Some(rtp));
        let (offer, socket) = l2_media_target(QUIC_STREAMS, None, Some(ip), 21_002, 4433);
        assert!(!offer);
        assert!(socket.is_none());
        let quic = SocketAddr::new(ip, 4433);
        let (offer, socket) = l2_media_target(UDP_RTP, Some(quic), Some(ip), 21_002, 4433);
        assert!(!offer);
        assert!(socket.is_none());
        let (offer, socket) = l2_media_target(ST2110, None, Some(ip), 21_002, 4433);
        assert!(!offer, "ST 2110 keeps the L2 QUIC dial");
        assert_eq!(socket, Some(rtp));
        let (offer, socket) = l2_media_target(ST2110, None, None, 21_002, 4433);
        assert!(!offer);
        assert!(socket.is_none());
        let minted = crate::peer_to_socket_addr([0x02, 0, 0, 0, 0, 1], 4433);
        assert!(crate::socket_addr_to_peer(minted).is_some());
        assert_eq!(media_input(minted, Some(rtp)), Some(rtp));
        assert_eq!(media_input(minted, None), None);
        assert!(connect_opens_ethernet(None, false));
        assert!(!connect_opens_ethernet(None, true));
        assert!(!connect_opens_ethernet(Some(quic), false));
        let mut opened = false;
        let net: Result<Option<()>, &str> = connect_net(None, true, None, |_| {
            opened = true;
            Err("raw nic")
        });
        assert!(!opened);
        assert!(net.unwrap().is_none());
        let mut opened = false;
        let net: Result<Option<&str>, &str> = connect_net(None, false, None, |_| {
            opened = true;
            Ok("nic")
        });
        assert!(opened);
        assert_eq!(net.unwrap(), Some("nic"));
    }

    #[test]
    fn pacing_does_not_block_the_quic_runtime() {
        let _globals = crate::carrier::test_globals();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let origin = std::time::Instant::now();
            let started = std::time::Instant::now();
            pace_until(origin, Duration::from_millis(400));
            assert!(
                started.elapsed() < Duration::from_millis(100),
                "pace slept on the runtime"
            );
        });
    }
}
