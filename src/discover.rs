//! LAN announcement on the QUIC EtherType.
//!
//! The window sends a short burst of broadcasts. A host answers each one with
//! the MAC and service a player dials, the IP addresses on that adapter, and
//! the service port. Round-trip times of those answers are the quality
//! numbers: latency samples, jitter, and min / average / max / standard deviation.
//!
//! Discovery uses service [`SERVICE`], not the play service, so the QUIC
//! socket never sees these frames.
//!
//! The same announce and offer bytes also travel as UDP datagrams on port
//! [`UDP_PORT`] (IPv4 broadcast, IPv6 all-nodes multicast), so a player that
//! cannot open a raw socket still finds hosts.
//!
//! ## Extended offers (version 3)
//!
//! A player that understands more sets [`EXT_MARK`] in the top byte of the
//! announce id. The announce itself is unchanged, so an older host still
//! answers it with a version-1 or version-2 offer. A newer host answers a
//! marked announce with a version-3 offer: the version-2 body, the L2 driver
//! byte (always present, `0` when unknown), then type-length-value records:
//!
//! ```text
//! type u8 | length u16 BE | value
//! ```
//!
//! [`TLV_CAPS`] carries [`Capabilities`] and [`TLV_THUMBNAIL`] a JPEG XL
//! desktop thumbnail sized to the room left in the frame. [`TLV_RELEASE`]
//! carries this build's etherquic version and the operating system's name
//! and version. [`TLV_RTP`] carries the host's RTP input port.
//! Unknown types are skipped, so later records do not break
//! this reader. An unmarked announce (an older player) always gets the old
//! offer, which that player accepts.
//!
//! Every offer fits one frame: [`L2_PAYLOAD_LIMIT`] on Ethernet, and
//! [`UDP4_PAYLOAD_LIMIT`] or [`UDP6_PAYLOAD_LIMIT`] for UDP.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::frame::{Frame, DEFAULT_ETHER_TYPE, SERVICE_HEADER_LEN};
use crate::pcap_link::{self, InterfaceInfo};
use crate::Mac;

/// Service id carried in the Ethernet frame. Not 4433, 4434, or 4435.
pub const SERVICE: u16 = 4440;

const MAGIC: &[u8; 4] = b"EQDS";
const VERSION: u8 = 1;
/// Offer that appends the host's codec hardware. Announces stay at [`VERSION`].
const VERSION_CODECS: u8 = 2;
/// Offer that adds TLV records after the version-2 body. Sent only to a marked announce.
const VERSION_EXT: u8 = 3;
/// Top byte of an announce id from a player that reads version-3 offers.
pub const EXT_MARK: u32 = 0xE3 << 24;
const MARK_MASK: u32 = 0xFF << 24;

/// UDP port for discovery datagrams. Same number as the L2 service.
pub const UDP_PORT: u16 = SERVICE;
/// Ethernet MTU the offer must fit, not counting the 14-byte Ethernet header.
pub const MTU: usize = 1500;
/// Discovery payload room in one L2 frame: the MTU minus the service header.
pub const L2_PAYLOAD_LIMIT: usize = MTU - SERVICE_HEADER_LEN;
/// UDP payload room in one unfragmented IPv4 packet (20-byte IP, 8-byte UDP header).
pub const UDP4_PAYLOAD_LIMIT: usize = MTU - 20 - 8;
/// UDP payload room in one IPv6 packet (40-byte IP, 8-byte UDP header).
pub const UDP6_PAYLOAD_LIMIT: usize = MTU - 40 - 8;
/// Bytes before each TLV value.
pub const TLV_HEADER: usize = 3;
/// Capabilities record.
pub const TLV_CAPS: u8 = 1;
/// JPEG XL thumbnail record (a bare codestream).
pub const TLV_THUMBNAIL: u8 = 2;
/// Etherquic version and operating system. Older players skip this type.
pub const TLV_RELEASE: u8 = 3;
/// Carrier ids this host can open. At most eight. Older players skip this type.
pub const TLV_CARRIERS: u8 = 4;
/// RTP input port, u16 big-endian. Older players skip this type.
pub const TLV_RTP: u8 = 5;
/// Below this a thumbnail is not worth sending.
pub const MIN_THUMBNAIL: usize = 64;
const ANNOUNCE: u8 = 1;
const OFFER: u8 = 2;
const HEADER: usize = 10;
const MAX_NAME: usize = 32;
const MAX_ADDR: usize = 4;
const MAX_CODECS: usize = 8;
const PROBES: u32 = 12;
const GAP: Duration = Duration::from_millis(40);
const TAIL: Duration = Duration::from_millis(200);

const FLAG_ENCODE: u8 = 0x01;
const FLAG_DECODE: u8 = 0x02;
const FLAG_CPU: u8 = 0x04;
const FLAG_GPU: u8 = 0x08;
/// Set on an audio row when that tool was found. Older readers ignore these bits.
const FLAG_LIBOPUS: u8 = 0x40;
const FLAG_FFMPEG: u8 = 0x10;
const FLAG_GSTREAMER: u8 = 0x20;

/// Video codec ids on a version-2 offer.
pub const VIDEO_PYROWAVE: u8 = 1;
pub const VIDEO_MJPEG: u8 = 2;
pub const VIDEO_H264: u8 = 3;
pub const VIDEO_H265: u8 = 4;
pub const VIDEO_AV1: u8 = 5;
pub const VIDEO_MJ2K: u8 = 6;
pub const VIDEO_JPEGXS: u8 = 7;
pub const VIDEO_APV: u8 = 8;
/// Audio codec ids on a version-2 offer.
pub const AUDIO_PCM: u8 = 1;
pub const AUDIO_OPUS: u8 = 2;

/// L2 driver on a version-2 offer, one byte after the codec lists.
/// Older offers omit the byte. `0` is not written.
pub const L2_LIBPCAP: u8 = 1;
pub const L2_NPCAP: u8 = 2;
pub const L2_MEMBRANE: u8 = 3;

/// Name for an L2 driver byte. Empty when the offer did not carry one.
pub fn l2_backend_label(id: u8) -> &'static str {
    match id {
        L2_LIBPCAP => "libpcap",
        L2_NPCAP => "npcap",
        L2_MEMBRANE => "membrane",
        _ => "",
    }
}

const BROADCAST: Mac = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];

/// One codec on a discovery offer. Flags are the hardware this host has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnouncedCodec {
    pub id: u8,
    pub hardware_encode: bool,
    pub hardware_decode: bool,
    pub cpu: bool,
    pub gpu: bool,
    /// libopus was found on this host. Meaningful on the Opus row.
    pub libopus: bool,
    /// ffmpeg was found on this host. Meaningful on the Opus row.
    pub ffmpeg: bool,
    /// GStreamer was found on this host. Meaningful on the Opus row.
    pub gstreamer: bool,
}

/// What a player needs in order to dial this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCard {
    pub name: String,
    /// MAC passed to connect. This is the host identity, not always the NIC's burned-in address.
    pub l2_mac: Mac,
    pub l2_service: u16,
    /// Service port published with the IP addresses.
    pub l3_port: u16,
    /// `None` keeps a version-1 offer, which older players already accept.
    pub video: Option<Vec<AnnouncedCodec>>,
    /// `None` with [`Self::video`] `None` keeps a version-1 offer.
    pub audio: Option<Vec<AnnouncedCodec>>,
    /// [`L2_LIBPCAP`], [`L2_NPCAP`], or [`L2_MEMBRANE`]. `0` omits the byte.
    pub l2_backend: u8,
}

/// One host that answered, plus the round trips gathered in this burst.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub l2_mac: Mac,
    pub l2_service: u16,
    pub l3_port: u16,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    /// `None` when the host sent a version-1 offer and did not announce codecs.
    pub video: Option<Vec<AnnouncedCodec>>,
    pub audio: Option<Vec<AnnouncedCodec>>,
    /// `0` when the offer had no driver byte.
    pub l2_backend: u8,
    /// Milliseconds, in probe order.
    pub samples_ms: Vec<f32>,
    pub stats: Stats,
    /// Tells two hosts apart. Every host announces the same identity
    /// [`Self::l2_mac`], so this is the answering adapter's own MAC when known.
    pub key: Mac,
    /// From a version-3 offer. `None` for an older host.
    pub caps: Option<Capabilities>,
    /// JPEG XL codestream from a version-3 offer.
    pub thumbnail: Option<Vec<u8>>,
    /// Etherquic version and operating system. `None` from an older host.
    pub release: Option<Release>,
    /// Carrier ids from [`TLV_CARRIERS`]. Empty when that TLV was absent,
    /// which means carrier 1 only.
    pub carriers: Vec<u8>,
    /// RTP input port from [`TLV_RTP`]. `0` when the offer did not carry one.
    pub rtp_port: u16,
    /// Answered over raw Ethernet.
    pub via_l2: bool,
    /// Answered over UDP.
    pub via_udp: bool,
}

/// Monitor on a version-3 offer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorInfo {
    pub width: u16,
    pub height: u16,
    /// Hundredths of a hertz. `0` when unknown.
    pub refresh_centihz: u16,
    pub x: i16,
    pub y: i16,
    pub primary: bool,
}

impl MonitorInfo {
    pub fn refresh_hz(&self) -> f32 {
        self.refresh_centihz as f32 / 100.0
    }
}

/// Game controller on a version-3 offer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PadInfo {
    /// [`PAD_API_XINPUT`], [`PAD_API_DINPUT`], [`PAD_API_WGI`], [`PAD_API_EVDEV`], or `0`.
    pub api: u8,
    /// [`PAD_KIND_GAMEPAD`], [`PAD_KIND_WHEEL`], [`PAD_KIND_STICK`], or `0`.
    pub kind: u8,
    pub name: String,
}

pub const OS_WINDOWS: u8 = 1;
pub const OS_LINUX: u8 = 2;
pub const OS_MACOS: u8 = 3;
pub const OS_ANDROID: u8 = 4;

pub const INPUT_KEYBOARD: u16 = 1 << 0;
pub const INPUT_MOUSE: u16 = 1 << 1;
pub const INPUT_TOUCH: u16 = 1 << 2;
pub const INPUT_PEN: u16 = 1 << 3;

/// The host can create a virtual gamepad for the player.
pub const VPAD_NONE: u8 = 0;
pub const VPAD_VIGEM: u8 = 1;
pub const VPAD_UINPUT: u8 = 2;

pub const PAD_API_XINPUT: u8 = 1;
pub const PAD_API_DINPUT: u8 = 2;
pub const PAD_API_WGI: u8 = 3;
pub const PAD_API_EVDEV: u8 = 4;
pub const PAD_KIND_GAMEPAD: u8 = 1;
pub const PAD_KIND_WHEEL: u8 = 2;
pub const PAD_KIND_STICK: u8 = 3;

/// Version of the capabilities record. Fields are only ever appended; a
/// reader keeps what it knows and skips the rest. A layout that cannot be
/// read that way gets a new TLV type instead.
pub const CAPS_VERSION: u8 = 1;
/// Longest codec text the capability block carries.
pub const MAX_CODEC_TEXT: usize = 255;
const MAX_OS_LABEL: usize = 32;
const MAX_HOSTNAME: usize = 63;
const MAX_MONITORS: usize = 16;
const MAX_PADS: usize = 8;
const MAX_PAD_NAME: usize = 24;
const MAX_AUDIO_DEVICES: usize = 4;
const MAX_AUDIO_NAME: usize = 32;

/// What the host has: displays, inputs, and who it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub os: u8,
    pub os_label: String,
    /// Full host name. The offer's own name is cut at 32 bytes.
    pub hostname: String,
    /// Burned-in MAC of the adapter that answered, so two hosts stay apart.
    pub nic_mac: Option<Mac>,
    pub monitors: Vec<MonitorInfo>,
    /// `INPUT_*` bits.
    pub inputs: u16,
    /// `VPAD_*`.
    pub virtual_pad: u8,
    pub pads: Vec<PadInfo>,
    /// Sound devices, default first. Trailing fields: an older host leaves
    /// them out and an older player stops reading before them.
    pub audio_outputs: Vec<String>,
    pub audio_inputs: Vec<String>,
    /// The host's encoders as `negotiate::HostCaps::to_text` writes them.
    /// Empty from a host that does not negotiate codecs. Trailing and optional.
    pub codecs: String,
}

impl Capabilities {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![CAPS_VERSION, self.os];
        push_str(&mut out, &self.os_label, MAX_OS_LABEL);
        push_str(&mut out, &self.hostname, MAX_HOSTNAME);
        match self.nic_mac {
            Some(mac) => {
                out.push(1);
                out.extend(mac);
            }
            None => out.push(0),
        }
        let monitors = &self.monitors[..self.monitors.len().min(MAX_MONITORS)];
        out.push(monitors.len() as u8);
        for monitor in monitors {
            out.extend(monitor.width.to_be_bytes());
            out.extend(monitor.height.to_be_bytes());
            out.extend(monitor.refresh_centihz.to_be_bytes());
            out.extend(monitor.x.to_be_bytes());
            out.extend(monitor.y.to_be_bytes());
            out.push(u8::from(monitor.primary));
        }
        out.extend(self.inputs.to_be_bytes());
        out.push(self.virtual_pad);
        let pads = &self.pads[..self.pads.len().min(MAX_PADS)];
        out.push(pads.len() as u8);
        for pad in pads {
            out.push(pad.api);
            out.push(pad.kind);
            push_str(&mut out, &pad.name, MAX_PAD_NAME);
        }
        for list in [&self.audio_outputs, &self.audio_inputs] {
            let list = &list[..list.len().min(MAX_AUDIO_DEVICES)];
            out.push(list.len() as u8);
            for name in list {
                push_str(&mut out, name, MAX_AUDIO_NAME);
            }
        }
        if !self.codecs.is_empty() {
            push_str(&mut out, &self.codecs, MAX_CODEC_TEXT);
        }
        out
    }

    /// `None` when a known field is cut short. Bytes after the known fields are ignored.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let mut rest = bytes;
        let version = take_u8(&mut rest)?;
        if version == 0 {
            return None;
        }
        let os = take_u8(&mut rest)?;
        let os_label = take_str(&mut rest)?;
        let hostname = take_str(&mut rest)?;
        let nic_mac = match take_u8(&mut rest)? {
            0 => None,
            _ => Some(take(&mut rest, 6)?.try_into().ok()?),
        };
        let count = take_u8(&mut rest)? as usize;
        if count > MAX_MONITORS {
            return None;
        }
        let mut monitors = Vec::with_capacity(count);
        for _ in 0..count {
            let raw = take(&mut rest, 11)?;
            monitors.push(MonitorInfo {
                width: u16::from_be_bytes([raw[0], raw[1]]),
                height: u16::from_be_bytes([raw[2], raw[3]]),
                refresh_centihz: u16::from_be_bytes([raw[4], raw[5]]),
                x: i16::from_be_bytes([raw[6], raw[7]]),
                y: i16::from_be_bytes([raw[8], raw[9]]),
                primary: raw[10] & 1 != 0,
            });
        }
        let inputs = u16::from_be_bytes(take(&mut rest, 2)?.try_into().ok()?);
        let virtual_pad = take_u8(&mut rest)?;
        let count = take_u8(&mut rest)? as usize;
        if count > MAX_PADS {
            return None;
        }
        let mut pads = Vec::with_capacity(count);
        for _ in 0..count {
            let api = take_u8(&mut rest)?;
            let kind = take_u8(&mut rest)?;
            let name = take_str(&mut rest)?;
            pads.push(PadInfo { api, kind, name });
        }
        // Optional: absent from an older host. A cut list is dropped, not half read.
        let mut audio = [Vec::new(), Vec::new()];
        for list in &mut audio {
            let Some(count) = take_u8(&mut rest) else {
                break;
            };
            let mut names = Vec::new();
            for _ in 0..usize::from(count).min(MAX_AUDIO_DEVICES) {
                match take_str(&mut rest) {
                    Some(name) => names.push(name),
                    None => break,
                }
            }
            if names.len() != usize::from(count).min(MAX_AUDIO_DEVICES) {
                break;
            }
            *list = names;
        }
        let [audio_outputs, audio_inputs] = audio;
        let codecs = take_str(&mut rest).unwrap_or_default();
        Some(Self {
            os,
            os_label,
            hostname,
            nic_mac,
            monitors,
            inputs,
            virtual_pad,
            pads,
            audio_outputs,
            audio_inputs,
            codecs,
        })
    }
}

const MAX_PRODUCT: usize = 16;
const MAX_OS_NAME: usize = 24;
const MAX_OS_VER: usize = 16;

/// This build and the operating system, carried in [`TLV_RELEASE`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// [`crate::RELEASE`], the Cargo package version, such as `0.1.6`.
    pub version: String,
    pub os: u8,
    /// `Windows`, `macOS`, `Android`, or a Linux distribution name.
    pub os_name: String,
    /// Short mark: `11`, `8.1`, `XP`, `15.1`, `14`.
    pub os_version: String,
}

impl Release {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![1];
        push_str(&mut out, &self.version, MAX_PRODUCT);
        out.push(self.os);
        push_str(&mut out, &self.os_name, MAX_OS_NAME);
        push_str(&mut out, &self.os_version, MAX_OS_VER);
        out
    }

    /// `None` when a known field is cut short. Bytes after the known fields are ignored.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let mut rest = bytes;
        let record = take_u8(&mut rest)?;
        if record == 0 {
            return None;
        }
        let version = take_str(&mut rest)?;
        let os = take_u8(&mut rest)?;
        let os_name = take_str(&mut rest)?;
        let os_version = take_str(&mut rest)?;
        Some(Self {
            version,
            os,
            os_name,
            os_version,
        })
    }

    /// `Windows 11`, or the name alone when the version is already in it.
    pub fn os_label(&self) -> String {
        let name = self.os_name.trim();
        let ver = self.os_version.trim();
        if ver.is_empty()
            || name
                .to_ascii_lowercase()
                .contains(&ver.to_ascii_lowercase())
        {
            name.to_string()
        } else {
            format!("{name} {ver}")
        }
    }

    /// `Windows 11 · version 0.1.0`.
    pub fn list_line(&self) -> String {
        let os = self.os_label();
        let ver = self.version.trim();
        match (os.is_empty(), ver.is_empty()) {
            (false, false) => format!("{os} · version {ver}"),
            (false, true) => os,
            (true, false) => format!("version {ver}"),
            (true, true) => String::new(),
        }
    }
}

/// Which grey mark to draw. The version number is painted on top of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsFace {
    WindowsXp,
    WindowsVista,
    Windows7,
    Windows8,
    Windows81,
    Windows10,
    Windows11,
    Windows,
    Mac,
    Android,
    Other,
}

impl OsFace {
    pub fn from_os(os: u8, name: &str, version: &str) -> Self {
        let (face, _) = if version.trim().is_empty() {
            face_from_label(os, name)
        } else {
            face_from_label(os, &format!("{name} {version}"))
        };
        if face != OsFace::Other {
            return face;
        }
        match os {
            OS_WINDOWS => OsFace::Windows,
            OS_MACOS => OsFace::Mac,
            OS_ANDROID => OsFace::Android,
            _ => OsFace::Other,
        }
    }
}

/// Icon and the short number painted on its corner. `label` is what an older
/// host put in one string, such as `Windows 11` or `macOS 15.1`.
pub fn face_from_label(os: u8, label: &str) -> (OsFace, String) {
    let lower = label.to_ascii_lowercase();
    if os == OS_ANDROID || lower.contains("android") {
        return (OsFace::Android, first_number(label));
    }
    if os == OS_MACOS || lower.contains("mac") {
        return (OsFace::Mac, first_number(label));
    }
    if os == OS_WINDOWS || lower.contains("windows") {
        if let Some((face, sub)) = windows_token(&lower) {
            return (face, sub);
        }
        return (OsFace::Windows, String::new());
    }
    (OsFace::Other, first_number(label))
}

fn windows_token(lower: &str) -> Option<(OsFace, String)> {
    const RULES: &[(&str, &str, OsFace)] = &[
        ("8.1", "8.1", OsFace::Windows81),
        ("vista", "Vista", OsFace::WindowsVista),
        ("xp", "XP", OsFace::WindowsXp),
        ("11", "11", OsFace::Windows11),
        ("10", "10", OsFace::Windows10),
        ("8", "8", OsFace::Windows8),
        ("7", "7", OsFace::Windows7),
    ];
    for (needle, sub, face) in RULES {
        if contains_token(lower, needle) {
            return Some((*face, (*sub).to_string()));
        }
    }
    None
}

fn contains_token(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(at, _)| {
        let before_ok = hay[..at]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '.');
        let after_ok = hay[at + needle.len()..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '.');
        before_ok && after_ok
    })
}

fn first_number(text: &str) -> String {
    for token in text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.') {
        if token.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
            return token.chars().take(8).collect();
        }
    }
    String::new()
}

/// This process. Read once: a discovery reply must not wait on the registry.
pub fn local_release() -> Release {
    static CACHED: OnceLock<Release> = OnceLock::new();
    CACHED.get_or_init(read_local_release).clone()
}

fn read_local_release() -> Release {
    let (os, os_name, os_version) = platform_os();
    Release {
        version: crate::RELEASE.to_string(),
        os,
        os_name,
        os_version,
    }
}

/// `XP`, `Vista`, `7`, `8`, `8.1`, `10`, or `11`. Empty when the numbers are
/// not one of those.
pub fn windows_mark(major: u32, minor: u32, build: u32) -> &'static str {
    match (major, minor) {
        (5, 1) | (5, 2) => "XP",
        (6, 0) => "Vista",
        (6, 1) => "7",
        (6, 2) => "8",
        (6, 3) => "8.1",
        (10, _) if build >= 22000 => "11",
        (10, _) if build > 0 => "10",
        _ => "",
    }
}

/// `CurrentMajorVersionNumber`, `CurrentMinorVersionNumber`, and
/// `CurrentBuildNumber` from `reg query` text. `CurrentVersion` (`6.1`) fills
/// in when the major and minor values are absent, as they are before Windows 10.
pub fn parse_windows_reg(text: &str) -> Option<(u32, u32, u32)> {
    let mut major = None;
    let mut minor = None;
    let mut build = None;
    let mut current = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(_kind) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        match key {
            "CurrentMajorVersionNumber" => major = parse_reg_int(value),
            "CurrentMinorVersionNumber" => minor = parse_reg_int(value),
            "CurrentBuildNumber" => build = value.parse().ok(),
            "CurrentVersion" => current = Some(value.to_string()),
            _ => {}
        }
    }
    if major.is_none() {
        if let Some(cur) = current.as_deref() {
            let mut bits = cur.split('.');
            major = bits.next().and_then(|part| part.parse().ok());
            minor = bits.next().and_then(|part| part.parse().ok());
        }
    }
    Some((major?, minor.unwrap_or(0), build.unwrap_or(0)))
}

fn parse_reg_int(value: &str) -> Option<u32> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}

/// `NAME` and `VERSION_ID` from `/etc/os-release`.
pub fn os_release_pair(text: &str) -> (String, String) {
    let mut name = String::new();
    let mut version = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim().to_string();
        match key {
            "NAME" if name.is_empty() => name = value,
            "VERSION_ID" => version = value,
            _ => {}
        }
    }
    (name, version)
}

/// The `<string>` after `<key>name</key>` in a macOS SystemVersion plist.
pub fn plist_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("<key>{key}</key>");
    let rest = text.split_once(&needle)?.1;
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    let value = rest[start..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// `ro.build.version.release` from Android `build.prop`.
pub fn android_release(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(value) = line.trim().strip_prefix("ro.build.version.release=") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(windows)]
fn platform_os() -> (u8, String, String) {
    let text = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        ])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let (major, minor, build) = parse_windows_reg(&text).unwrap_or((0, 0, 0));
    let mark = windows_mark(major, minor, build);
    let version = if mark.is_empty() {
        if major == 0 {
            String::new()
        } else {
            format!("{major}.{minor}")
        }
    } else {
        mark.to_string()
    };
    (OS_WINDOWS, "Windows".into(), version)
}

#[cfg(target_os = "macos")]
fn platform_os() -> (u8, String, String) {
    let text = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
        .unwrap_or_default();
    let version = plist_string(&text, "ProductVersion").unwrap_or_default();
    (OS_MACOS, "macOS".into(), version)
}

#[cfg(target_os = "android")]
fn platform_os() -> (u8, String, String) {
    let text = std::fs::read_to_string("/system/build.prop").unwrap_or_default();
    let version = android_release(&text).unwrap_or_default();
    (OS_ANDROID, "Android".into(), version)
}

#[cfg(all(target_os = "linux", not(target_os = "android")))]
fn platform_os() -> (u8, String, String) {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let (name, version) = os_release_pair(&text);
    let name = if name.is_empty() {
        "Linux".into()
    } else {
        name
    };
    (OS_LINUX, name, version)
}

#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "android",
    target_os = "linux"
)))]
fn platform_os() -> (u8, String, String) {
    (0, std::env::consts::OS.into(), String::new())
}

fn push_str(out: &mut Vec<u8>, text: &str, max: usize) {
    let bytes = clip_to(text, max);
    out.push(bytes.len() as u8);
    out.extend(bytes);
}

fn take_u8(rest: &mut &[u8]) -> Option<u8> {
    Some(take(rest, 1)?[0])
}

fn take_str(rest: &mut &[u8]) -> Option<String> {
    let len = take_u8(rest)? as usize;
    let bytes = take(rest, len)?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// What a host adds to a version-3 offer. Called for each reply, so both
/// must answer from a cache.
pub trait OfferExtras: Send + Sync {
    fn capabilities(&self) -> Option<Capabilities>;
    /// A codestream of at most `budget` bytes, or `None`.
    fn thumbnail(&self, budget: usize) -> Option<Vec<u8>>;
    /// This build and the operating system. The default is this process.
    fn release(&self) -> Release {
        local_release()
    }
    /// Carrier ids for [`TLV_CARRIERS`]. Empty omits the TLV (carrier 1 only).
    fn carriers(&self) -> Vec<u8> {
        crate::carrier::local_carriers(0)
    }
}

/// No capabilities and no picture. A version-3 offer still carries the driver byte.
pub struct NoExtras;

impl OfferExtras for NoExtras {
    fn capabilities(&self) -> Option<Capabilities> {
        None
    }
    fn thumbnail(&self, _budget: usize) -> Option<Vec<u8>> {
        None
    }
}

/// Room for the thumbnail value once `used` payload bytes and its TLV header
/// are counted. `None` when less than [`MIN_THUMBNAIL`] is left.
pub fn thumbnail_budget(limit: usize, used: usize) -> Option<usize> {
    limit
        .checked_sub(used)?
        .checked_sub(TLV_HEADER)
        .filter(|room| *room >= MIN_THUMBNAIL)
        .map(|room| room.min(u16::MAX as usize))
}

/// Latency summary for one burst. Standard deviation is over the samples (divide by n).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub min_ms: f32,
    pub avg_ms: f32,
    pub max_ms: f32,
    pub std_ms: f32,
    /// Mean absolute difference between consecutive samples.
    pub jitter_ms: f32,
}

impl Stats {
    pub fn from_samples(samples: &[f32]) -> Self {
        if samples.is_empty() {
            return Self {
                min_ms: 0.0,
                avg_ms: 0.0,
                max_ms: 0.0,
                std_ms: 0.0,
                jitter_ms: 0.0,
            };
        }
        let mut min_ms = f32::MAX;
        let mut max_ms = 0.0_f32;
        let mut sum = 0.0_f32;
        for sample in samples {
            min_ms = min_ms.min(*sample);
            max_ms = max_ms.max(*sample);
            sum += *sample;
        }
        let n = samples.len() as f32;
        let avg_ms = sum / n;
        let var = samples
            .iter()
            .map(|sample| {
                let d = *sample - avg_ms;
                d * d
            })
            .sum::<f32>()
            / n;
        let jitter_ms = if samples.len() < 2 {
            0.0
        } else {
            let mut acc = 0.0_f32;
            for pair in samples.windows(2) {
                acc += (pair[1] - pair[0]).abs();
            }
            acc / (samples.len() - 1) as f32
        };
        Self {
            min_ms,
            avg_ms,
            max_ms,
            std_ms: var.sqrt(),
            jitter_ms,
        }
    }
}

/// True when this frame is a discovery announce or offer, so QUIC must not take it.
pub fn is_discovery(frame: &Frame) -> bool {
    frame.dst_service == SERVICE || frame.src_service == SERVICE
}

/// One discovery burst over every transport that opened.
#[derive(Clone, Debug, Default)]
pub struct Burst {
    pub found: Vec<Found>,
    /// Raw Ethernet could not be used, for example without `CAP_NET_RAW`.
    pub l2_error: Option<String>,
    pub udp_error: Option<String>,
}

/// Broadcast a burst and collect hosts. `generation` cancels the burst when it changes.
/// Fails only when no host answered and raw Ethernet could not be opened.
pub fn discover(
    nic: Option<&str>,
    generation: &AtomicU64,
    expected: u64,
) -> Result<Vec<Found>, String> {
    let burst = discover_burst(nic, generation, expected);
    match burst.l2_error {
        Some(err) if burst.found.is_empty() => Err(err),
        _ => Ok(burst.found),
    }
}

/// Probe over raw Ethernet and UDP at once. Each transport that fails is reported, not fatal.
pub fn discover_burst(nic: Option<&str>, generation: &AtomicU64, expected: u64) -> Burst {
    let mut burst = Burst::default();
    let info = pcap_link::interface_info(nic).map_err(|err| err.to_string());
    let mut l2 = match &info {
        Ok(info) => {
            let src = info.mac.unwrap_or([0x02, 0x00, 0x00, 0x00, 0x00, 0x0d]);
            match pcap_link::open(Some(&info.name), DEFAULT_ETHER_TYPE) {
                Ok(link) => Some((link, src)),
                Err(err) => {
                    burst.l2_error = Some(err.to_string());
                    None
                }
            }
        }
        Err(err) => {
            burst.l2_error = Some(err.clone());
            None
        }
    };
    let scope = nic.map(ipv6_scope).unwrap_or(0);
    let udp = UdpProbe::open(scope);
    if udp.is_none() {
        burst.udp_error = Some("no UDP socket for discovery".into());
    }
    let base = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(1);
    let mut pending: HashMap<u32, Instant> = HashMap::new();
    let mut acc: HashMap<Mac, Acc> = HashMap::new();
    for index in 0..PROBES {
        if generation.load(Ordering::SeqCst) != expected {
            break;
        }
        let id = EXT_MARK | (base.wrapping_add(index) & !MARK_MASK);
        if let Some((link, src)) = l2.as_mut() {
            if let Err(err) = link.send(&announce_frame(*src, id).encode()) {
                burst.l2_error = Some(err.to_string());
                l2 = None;
            }
        }
        if let Some(udp) = &udp {
            udp.send(&encode_header(VERSION, ANNOUNCE, id));
        }
        pending.insert(id, Instant::now());
        drain(&mut l2, &udp, &pending, &mut acc, GAP, generation, expected);
    }
    if generation.load(Ordering::SeqCst) == expected {
        drain(
            &mut l2, &udp, &pending, &mut acc, TAIL, generation, expected,
        );
    }
    let mut found: Vec<Found> = acc.into_values().map(Acc::finish).collect();
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.key.cmp(&b.key)));
    burst.found = found;
    burst
}

/// Answer announces until `stop` is set. One reply per announce.
pub fn serve(nic: Option<&str>, card: &HostCard, stop: &AtomicBool) -> Result<(), String> {
    serve_with(nic, card, &NoExtras, stop)
}

/// [`serve`], with capabilities and a thumbnail for a player that asks for them.
pub fn serve_with(
    nic: Option<&str>,
    card: &HostCard,
    extras: &dyn OfferExtras,
    stop: &AtomicBool,
) -> Result<(), String> {
    let info = pcap_link::interface_info(nic).map_err(|err| err.to_string())?;
    let src = info.mac.unwrap_or(card.l2_mac);
    let mut link =
        pcap_link::open(Some(&info.name), DEFAULT_ETHER_TYPE).map_err(|err| err.to_string())?;
    while !stop.load(Ordering::SeqCst) {
        match link.recv() {
            Ok(Some(raw)) => {
                if let Some(id_dst) = announce_for_us(&raw, src) {
                    let (id, dst) = id_dst;
                    let payload = offer_payload(id, card, &info, extras, L2_PAYLOAD_LIMIT);
                    let frame = offer_frame_with(src, dst, payload);
                    if let Err(err) = link.send(&frame.encode()) {
                        eprintln!("discovery reply: {err}");
                    }
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(err) => {
                let text = err.to_string();
                if text.to_ascii_lowercase().contains("permission") {
                    return Err(text);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(())
}

/// Answer UDP announces on [`UDP_PORT`] until `stop` is set. Needs no raw socket.
pub fn serve_udp(
    nic: Option<&str>,
    card: &HostCard,
    extras: &dyn OfferExtras,
    stop: &AtomicBool,
) -> Result<(), String> {
    let info = pcap_link::interface_info(nic).unwrap_or_else(|_| InterfaceInfo {
        name: nic.unwrap_or_default().to_string(),
        mac: None,
        ipv4: Vec::new(),
        ipv6: Vec::new(),
    });
    // IPv4 and IPv6 are separate sockets. The IPv6 one is IPv6-only.
    // A dual-stack `[::]` socket does not receive IPv4 broadcasts, and a
    // broadcast is how another computer finds this one.
    let mut sockets = Vec::new();
    let mut v6_at = None;
    let mut last_err = String::new();
    for addr in [
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, UDP_PORT)),
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, UDP_PORT)),
    ] {
        match bind_discovery_socket(addr) {
            Ok(socket) => {
                if addr.is_ipv6() {
                    v6_at = Some(sockets.len());
                }
                sockets.push(socket);
            }
            Err(err) => last_err = err,
        }
    }
    if sockets.is_empty() {
        return Err(last_err);
    }
    if let Some(index) = v6_at {
        let scope = ipv6_scope(&info.name);
        let _ = sockets[index].join_multicast_v6(&ALL_NODES, scope);
    }
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::SeqCst) {
        for socket in &sockets {
            let Ok((len, from)) = socket.recv_from(&mut buf) else {
                continue;
            };
            let Some(id) = decode_announce(&buf[..len]) else {
                continue;
            };
            let limit = udp_limit(&from);
            let payload = offer_payload(id, card, &info, extras, limit);
            if let Err(err) = socket.send_to(&payload, from) {
                eprintln!("discovery reply over UDP to {from}: {err}");
            }
        }
    }
    Ok(())
}

const ALL_NODES: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);

/// One discovery socket. IPv6 is IPv6-only so the same port can also be IPv4.
fn bind_discovery_socket(addr: SocketAddr) -> Result<UdpSocket, String> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|err| format!("udp {addr}: {err}"))?;
    if addr.is_ipv6() {
        socket
            .set_only_v6(true)
            .map_err(|err| format!("udp {addr}: {err}"))?;
    }
    socket
        .bind(&addr.into())
        .map_err(|err| format!("bind {addr}: {err}"))?;
    let socket: UdpSocket = socket.into();
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .map_err(|err| format!("udp {addr}: {err}"))?;
    Ok(socket)
}

/// Payload room for a reply to `peer`. An IPv4-mapped peer is IPv4 on the wire.
pub fn udp_limit(peer: &SocketAddr) -> usize {
    match peer.ip() {
        IpAddr::V4(_) => UDP4_PAYLOAD_LIMIT,
        IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some() => UDP4_PAYLOAD_LIMIT,
        IpAddr::V6(_) => UDP6_PAYLOAD_LIMIT,
    }
}

/// Interface index for IPv6 multicast. `0` lets the system choose.
fn ipv6_scope(name: &str) -> u32 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        0
    }
}

/// Player side UDP sockets: IPv4 broadcast and IPv6 all-nodes multicast.
struct UdpProbe {
    v4: Option<UdpSocket>,
    v6: Option<UdpSocket>,
    scope: u32,
}

impl UdpProbe {
    fn open(scope: u32) -> Option<Self> {
        let v4 = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .ok()
            .and_then(|socket| {
                socket.set_broadcast(true).ok()?;
                socket.set_nonblocking(true).ok()?;
                Some(socket)
            });
        let v6 = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0))
            .ok()
            .and_then(|socket| {
                socket.set_nonblocking(true).ok()?;
                Some(socket)
            });
        if v4.is_none() && v6.is_none() {
            return None;
        }
        Some(Self { v4, v6, scope })
    }

    fn send(&self, payload: &[u8]) {
        if let Some(socket) = &self.v4 {
            let _ = socket.send_to(payload, (Ipv4Addr::BROADCAST, UDP_PORT));
        }
        if let Some(socket) = &self.v6 {
            let to = SocketAddrV6::new(ALL_NODES, UDP_PORT, 0, self.scope);
            let _ = socket.send_to(payload, to);
        }
    }

    fn recv(&self, buf: &mut [u8]) -> Option<(usize, SocketAddr)> {
        for socket in [&self.v4, &self.v6].into_iter().flatten() {
            if let Ok(got) = socket.recv_from(buf) {
                return Some(got);
            }
        }
        None
    }
}

struct Acc {
    key: Mac,
    caps: Option<Capabilities>,
    thumbnail: Option<Vec<u8>>,
    release: Option<Release>,
    carriers: Vec<u8>,
    rtp_port: u16,
    via_l2: bool,
    via_udp: bool,
    name: String,
    l2_mac: Mac,
    l2_service: u16,
    l3_port: u16,
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
    video: Option<Vec<AnnouncedCodec>>,
    audio: Option<Vec<AnnouncedCodec>>,
    l2_backend: u8,
    samples: Vec<(u32, f32)>,
}

impl Acc {
    fn finish(mut self) -> Found {
        self.samples.sort_by_key(|(id, _)| *id);
        let samples_ms: Vec<f32> = self.samples.into_iter().map(|(_, ms)| ms).collect();
        let stats = Stats::from_samples(&samples_ms);
        Found {
            name: self.name,
            l2_mac: self.l2_mac,
            l2_service: self.l2_service,
            l3_port: self.l3_port,
            ipv4: self.ipv4,
            ipv6: self.ipv6,
            video: self.video,
            audio: self.audio,
            l2_backend: self.l2_backend,
            samples_ms,
            stats,
            key: self.key,
            caps: self.caps,
            thumbnail: self.thumbnail,
            release: self.release,
            carriers: self.carriers,
            rtp_port: self.rtp_port,
            via_l2: self.via_l2,
            via_udp: self.via_udp,
        }
    }
}

fn drain(
    l2: &mut Option<(pcap_link::Link, Mac)>,
    udp: &Option<UdpProbe>,
    pending: &HashMap<u32, Instant>,
    acc: &mut HashMap<Mac, Acc>,
    budget: Duration,
    generation: &AtomicU64,
    expected: u64,
) {
    let deadline = Instant::now() + budget;
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        if generation.load(Ordering::SeqCst) != expected {
            return;
        }
        let mut idle = true;
        if let Some((link, _)) = l2.as_mut() {
            if let Ok(Some(raw)) = link.recv() {
                idle = false;
                if let Some((id, body, mac, src)) = decode_offer_frame(&raw) {
                    note_offer(id, body, mac, Heard::L2(src), pending, acc);
                }
            }
        }
        if let Some(udp) = udp {
            while let Some((len, from)) = udp.recv(&mut buf) {
                idle = false;
                if let Some((id, body)) = decode_offer(&buf[..len]) {
                    if let Some(mac) = body_mac(&buf[..len]) {
                        note_offer(id, body, mac, Heard::Udp(from), pending, acc);
                    }
                }
            }
        }
        if idle {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

enum Heard {
    L2(Mac),
    Udp(SocketAddr),
}

/// Which host this is. A version-3 offer names its adapter; otherwise the
/// Ethernet source tells hosts apart, and over UDP the address does.
fn host_key(body: &OfferBody, heard: &Heard, mac: Mac) -> Mac {
    if let Some(nic) = body.caps.as_ref().and_then(|caps| caps.nic_mac) {
        return nic;
    }
    match heard {
        Heard::L2(src) => *src,
        Heard::Udp(from) => {
            let mut key = [0x06, 0, 0, 0, 0, 0];
            let octets = match from.ip() {
                IpAddr::V4(ip) => ip.octets().to_vec(),
                IpAddr::V6(ip) => ip.octets().to_vec(),
            };
            for (index, byte) in octets.iter().enumerate() {
                key[1 + index % 5] ^= byte;
            }
            if key == [0x06, 0, 0, 0, 0, 0] {
                mac
            } else {
                key
            }
        }
    }
}

fn note_offer(
    id: u32,
    info: OfferBody,
    mac: Mac,
    heard: Heard,
    pending: &HashMap<u32, Instant>,
    acc: &mut HashMap<Mac, Acc>,
) {
    let Some(sent) = pending.get(&id) else {
        return;
    };
    let ms = sent.elapsed().as_secs_f32() * 1000.0;
    let key = host_key(&info, &heard, mac);
    let row = acc.entry(key).or_insert_with(|| Acc {
        key,
        caps: None,
        thumbnail: None,
        release: None,
        carriers: Vec::new(),
        rtp_port: 0,
        via_l2: false,
        via_udp: false,
        name: info.name.clone(),
        l2_mac: mac,
        l2_service: info.l2_service,
        l3_port: info.l3_port,
        ipv4: info.ipv4.clone(),
        ipv6: info.ipv6.clone(),
        video: info.video.clone(),
        audio: info.audio.clone(),
        l2_backend: info.l2_backend,
        samples: Vec::new(),
    });
    match heard {
        Heard::L2(_) => row.via_l2 = true,
        Heard::Udp(_) => row.via_udp = true,
    }
    row.name = info.name;
    row.l2_mac = mac;
    row.l2_service = info.l2_service;
    row.l3_port = info.l3_port;
    row.ipv4 = info.ipv4;
    row.ipv6 = info.ipv6;
    row.video = info.video;
    row.audio = info.audio;
    if info.l2_backend != 0 || row.l2_backend == 0 {
        row.l2_backend = info.l2_backend;
    }
    if info.caps.is_some() {
        row.caps = info.caps;
    }
    if info.release.is_some() {
        row.release = info.release;
    }
    if !info.carriers.is_empty() {
        row.carriers = info.carriers;
    }
    if info.rtp_port != 0 {
        row.rtp_port = info.rtp_port;
    }
    // Keep the larger picture: the L2 and UDP replies may differ in room.
    if let Some(thumb) = info.thumbnail {
        if row
            .thumbnail
            .as_ref()
            .is_none_or(|old| old.len() < thumb.len())
        {
            row.thumbnail = Some(thumb);
        }
    }
    // The same probe id answered over two transports is one sample.
    if !row.samples.iter().any(|(seen, _)| *seen == id) {
        row.samples.push((id, ms));
    }
}

struct OfferBody {
    name: String,
    l2_service: u16,
    l3_port: u16,
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
    video: Option<Vec<AnnouncedCodec>>,
    audio: Option<Vec<AnnouncedCodec>>,
    l2_backend: u8,
    caps: Option<Capabilities>,
    thumbnail: Option<Vec<u8>>,
    release: Option<Release>,
    carriers: Vec<u8>,
    rtp_port: u16,
}

fn announce_frame(src: Mac, id: u32) -> Frame {
    Frame {
        dst: BROADCAST,
        src,
        ether_type: DEFAULT_ETHER_TYPE,
        dst_service: SERVICE,
        src_service: SERVICE,
        payload: encode_header(VERSION, ANNOUNCE, id),
    }
}

/// The offer an unmarked (older) announce gets, byte for byte what older hosts sent.
fn legacy_payload(id: u32, card: &HostCard, info: &InterfaceInfo) -> Vec<u8> {
    let codecs = card.video.is_some() || card.audio.is_some();
    let version = if codecs { VERSION_CODECS } else { VERSION };
    let mut payload = encode_header(version, OFFER, id);
    write_body(&mut payload, card, info);
    if codecs {
        write_codecs(&mut payload, card.video.as_deref().unwrap_or(&[]));
        write_codecs(&mut payload, card.audio.as_deref().unwrap_or(&[]));
        // One byte, only when the host knows its driver. A missing byte is
        // still a version-2 offer, so an older host keeps answering.
        if card.l2_backend != 0 {
            payload.push(card.l2_backend);
        }
    }
    payload
}

fn write_body(payload: &mut Vec<u8>, card: &HostCard, info: &InterfaceInfo) {
    payload.extend(card.l2_mac);
    payload.extend(card.l2_service.to_be_bytes());
    payload.extend(card.l3_port.to_be_bytes());
    let name = clip_name(&card.name);
    payload.push(name.len() as u8);
    payload.extend(name);
    let v4: Vec<_> = info.ipv4.iter().take(MAX_ADDR).collect();
    payload.push(v4.len() as u8);
    for ip in v4 {
        payload.extend(ip.octets());
    }
    let v6: Vec<_> = info.ipv6.iter().take(MAX_ADDR).collect();
    payload.push(v6.len() as u8);
    for ip in v6 {
        payload.extend(ip.octets());
    }
}

/// The reply to announce `id`, never longer than `limit`. A marked announce
/// gets a version-3 offer: capabilities when they fit, then a thumbnail in
/// whatever room is left, or none.
pub fn offer_payload(
    id: u32,
    card: &HostCard,
    info: &InterfaceInfo,
    extras: &dyn OfferExtras,
    limit: usize,
) -> Vec<u8> {
    if id & MARK_MASK != EXT_MARK {
        return legacy_payload(id, card, info);
    }
    let mut payload = encode_header(VERSION_EXT, OFFER, id);
    write_body(&mut payload, card, info);
    write_codecs(&mut payload, card.video.as_deref().unwrap_or(&[]));
    write_codecs(&mut payload, card.audio.as_deref().unwrap_or(&[]));
    payload.push(card.l2_backend);
    if payload.len() > limit {
        // Cannot happen with the field caps above; an older offer still fits.
        return legacy_payload(id, card, info);
    }
    if let Some(caps) = extras.capabilities() {
        let mut caps = caps;
        if caps.nic_mac.is_none() {
            caps.nic_mac = info.mac;
        }
        let record = caps.encode();
        if payload.len() + TLV_HEADER + record.len() <= limit {
            push_tlv(&mut payload, TLV_CAPS, &record);
        }
    }
    let release = extras.release().encode();
    if payload.len() + TLV_HEADER + release.len() <= limit {
        push_tlv(&mut payload, TLV_RELEASE, &release);
    }
    let carriers = crate::carrier::encode_carrier_tlv(&extras.carriers());
    if carriers.len() > 1 && payload.len() + TLV_HEADER + carriers.len() <= limit {
        push_tlv(&mut payload, TLV_CARRIERS, &carriers);
    }
    let rtp = crate::carrier::bound_input_port();
    if rtp != 0 && payload.len() + TLV_HEADER + 2 <= limit {
        push_tlv(&mut payload, TLV_RTP, &rtp.to_be_bytes());
    }
    if let Some(budget) = thumbnail_budget(limit, payload.len()) {
        if let Some(thumb) = extras.thumbnail(budget) {
            if !thumb.is_empty() && thumb.len() <= budget {
                push_tlv(&mut payload, TLV_THUMBNAIL, &thumb);
            }
        }
    }
    debug_assert!(payload.len() <= limit);
    payload
}

/// Carrier ids on a version-3 offer. `Some(empty)` is a parsed offer with no
/// [`TLV_CARRIERS`] (carrier 1 only). `None` is not an offer. An unknown TLV
/// does not fail the parse.
pub fn offered_carriers(payload: &[u8]) -> Option<Vec<u8>> {
    decode_offer(payload).map(|(_, body)| body.carriers)
}

fn push_tlv(payload: &mut Vec<u8>, kind: u8, value: &[u8]) {
    payload.push(kind);
    payload.extend((value.len() as u16).to_be_bytes());
    payload.extend(value);
}

#[cfg(test)]
fn offer_frame(src: Mac, dst: Mac, id: u32, card: &HostCard, info: &InterfaceInfo) -> Frame {
    offer_frame_with(src, dst, legacy_payload(id, card, info))
}

fn offer_frame_with(src: Mac, dst: Mac, payload: Vec<u8>) -> Frame {
    Frame {
        dst,
        src,
        ether_type: DEFAULT_ETHER_TYPE,
        dst_service: SERVICE,
        src_service: SERVICE,
        payload,
    }
}

fn announce_for_us(raw: &[u8], ether_src: Mac) -> Option<(u32, Mac)> {
    let frame = Frame::decode(raw).ok()?;
    if !is_discovery(&frame) || frame.src == ether_src || frame.src == BROADCAST {
        return None;
    }
    if frame.dst != BROADCAST && frame.dst != ether_src {
        return None;
    }
    let id = decode_announce(&frame.payload)?;
    Some((id, frame.src))
}

fn decode_offer_frame(raw: &[u8]) -> Option<(u32, OfferBody, Mac, Mac)> {
    let frame = Frame::decode(raw).ok()?;
    if !is_discovery(&frame) {
        return None;
    }
    let (id, body) = decode_offer(&frame.payload)?;
    if body_mac_unusable(frame.payload.get(HEADER..HEADER + 6)?) {
        return None;
    }
    let mac = body_mac(&frame.payload)?;
    Some((id, body, mac, frame.src))
}

fn body_mac(payload: &[u8]) -> Option<Mac> {
    payload.get(HEADER..HEADER + 6)?.try_into().ok()
}

fn body_mac_unusable(mac: &[u8]) -> bool {
    mac.iter().all(|b| *b == 0) || mac.iter().all(|b| *b == 0xff)
}

fn encode_header(version: u8, kind: u8, id: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER);
    out.extend(MAGIC);
    out.push(version);
    out.push(kind);
    out.extend(id.to_be_bytes());
    out
}

fn write_codecs(payload: &mut Vec<u8>, codecs: &[AnnouncedCodec]) {
    let codecs = &codecs[..codecs.len().min(MAX_CODECS)];
    payload.push(codecs.len() as u8);
    for codec in codecs {
        payload.push(codec.id);
        let mut flags = 0;
        if codec.hardware_encode {
            flags |= FLAG_ENCODE;
        }
        if codec.hardware_decode {
            flags |= FLAG_DECODE;
        }
        if codec.cpu {
            flags |= FLAG_CPU;
        }
        if codec.gpu {
            flags |= FLAG_GPU;
        }
        if codec.libopus {
            flags |= FLAG_LIBOPUS;
        }
        if codec.ffmpeg {
            flags |= FLAG_FFMPEG;
        }
        if codec.gstreamer {
            flags |= FLAG_GSTREAMER;
        }
        payload.push(flags);
    }
}

fn read_codecs(rest: &mut &[u8]) -> Option<Vec<AnnouncedCodec>> {
    let count = *rest.first()? as usize;
    *rest = &rest[1..];
    if count > MAX_CODECS {
        return None;
    }
    let mut codecs = Vec::with_capacity(count);
    for _ in 0..count {
        let id = *rest.first()?;
        *rest = &rest[1..];
        let flags = *rest.first()?;
        *rest = &rest[1..];
        codecs.push(AnnouncedCodec {
            id,
            hardware_encode: flags & FLAG_ENCODE != 0,
            hardware_decode: flags & FLAG_DECODE != 0,
            cpu: flags & FLAG_CPU != 0,
            gpu: flags & FLAG_GPU != 0,
            libopus: flags & FLAG_LIBOPUS != 0,
            ffmpeg: flags & FLAG_FFMPEG != 0,
            gstreamer: flags & FLAG_GSTREAMER != 0,
        });
    }
    Some(codecs)
}

fn decode_announce(payload: &[u8]) -> Option<u32> {
    let (version, kind, id, rest) = split_header(payload)?;
    if version != VERSION || kind != ANNOUNCE || !rest.is_empty() {
        return None;
    }
    Some(id)
}

fn decode_offer(payload: &[u8]) -> Option<(u32, OfferBody)> {
    let (version, kind, id, mut rest) = split_header(payload)?;
    if kind != OFFER || !matches!(version, VERSION | VERSION_CODECS | VERSION_EXT) {
        return None;
    }
    let mac_bytes = take(&mut rest, 6)?;
    if body_mac_unusable(mac_bytes) {
        return None;
    }
    let l2_service = u16::from_be_bytes(take(&mut rest, 2)?.try_into().ok()?);
    let l3_port = u16::from_be_bytes(take(&mut rest, 2)?.try_into().ok()?);
    let name_len = *rest.first()? as usize;
    rest = &rest[1..];
    if name_len > MAX_NAME {
        return None;
    }
    let name_bytes = take(&mut rest, name_len)?;
    let name = String::from_utf8(name_bytes.to_vec()).ok()?;
    let n4 = *rest.first()? as usize;
    rest = &rest[1..];
    if n4 > MAX_ADDR {
        return None;
    }
    let mut ipv4 = Vec::with_capacity(n4);
    for _ in 0..n4 {
        let octets: [u8; 4] = take(&mut rest, 4)?.try_into().ok()?;
        ipv4.push(Ipv4Addr::from(octets));
    }
    let n6 = *rest.first()? as usize;
    rest = &rest[1..];
    if n6 > MAX_ADDR || rest.len() < n6 * 16 {
        return None;
    }
    let mut ipv6 = Vec::with_capacity(n6);
    for _ in 0..n6 {
        let octets: [u8; 16] = take(&mut rest, 16)?.try_into().ok()?;
        ipv6.push(Ipv6Addr::from(octets));
    }
    let mut caps = None;
    let mut thumbnail = None;
    let mut release = None;
    let mut carriers = Vec::new();
    let mut rtp_port = 0u16;
    let (video, audio, l2_backend) = if version == VERSION {
        if !rest.is_empty() {
            return None;
        }
        (None, None, 0)
    } else if version == VERSION_CODECS {
        let video = read_codecs(&mut rest)?;
        let audio = read_codecs(&mut rest)?;
        let l2_backend = match rest.len() {
            0 => 0,
            1 => rest[0],
            _ => return None,
        };
        (Some(video), Some(audio), l2_backend)
    } else {
        let video = read_codecs(&mut rest)?;
        let audio = read_codecs(&mut rest)?;
        let l2_backend = take_u8(&mut rest)?;
        // A record cut short ends the list; what came before still counts.
        while rest.len() >= TLV_HEADER {
            let kind = rest[0];
            let len = u16::from_be_bytes([rest[1], rest[2]]) as usize;
            rest = &rest[TLV_HEADER..];
            let Some(value) = take(&mut rest, len) else {
                break;
            };
            match kind {
                TLV_CAPS => caps = Capabilities::parse(value),
                TLV_THUMBNAIL => thumbnail = Some(value.to_vec()),
                TLV_RELEASE => release = Release::parse(value),
                TLV_CARRIERS => {
                    if let Some(ids) = crate::carrier::parse_carrier_tlv(value) {
                        carriers = ids;
                    }
                }
                TLV_RTP => {
                    if value.len() == 2 {
                        let port = u16::from_be_bytes([value[0], value[1]]);
                        if port != 0 {
                            rtp_port = port;
                        }
                    }
                }
                _ => {}
            }
        }
        (Some(video), Some(audio), l2_backend)
    };
    Some((
        id,
        OfferBody {
            name,
            l2_service,
            l3_port,
            ipv4,
            ipv6,
            video,
            audio,
            l2_backend,
            caps,
            thumbnail,
            release,
            carriers,
            rtp_port,
        },
    ))
}

/// RTP input port carried by one offer. `None` when the offer has no [`TLV_RTP`].
pub fn rtp_input_port(payload: &[u8]) -> Option<u16> {
    let (_, body) = decode_offer(payload)?;
    (body.rtp_port != 0).then_some(body.rtp_port)
}

/// One offer from `ip` on the discovery port that carries [`TLV_RTP`].
pub fn fetch_offer(ip: IpAddr) -> Option<Vec<u8>> {
    let bind = if ip.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok()?;
    let announce = encode_header(VERSION, ANNOUNCE, EXT_MARK | 7);
    let dest = SocketAddr::new(ip, UDP_PORT);
    let mut buf = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        let _ = socket.send_to(&announce, dest);
        match socket.recv_from(&mut buf) {
            Ok((n, from)) if from.ip() == ip => {
                let payload = buf[..n].to_vec();
                if rtp_input_port(&payload).is_some() {
                    return Some(payload);
                }
            }
            _ => {}
        }
    }
    None
}

/// Ask `ip` on the discovery port for its RTP input. The QUIC service port is not used.
pub fn ask_rtp_input(ip: IpAddr) -> Option<u16> {
    rtp_input_port(&fetch_offer(ip)?)
}

/// Host IP and RTP input port from one offer.
///
/// The IP is the first announced address. The port is [`TLV_RTP`]. The QUIC
/// service port is not a substitute.
pub fn offer_rtp_socket(payload: &[u8]) -> Option<SocketAddr> {
    let (_, body) = decode_offer(payload)?;
    let ip = body
        .ipv4
        .first()
        .copied()
        .map(IpAddr::V4)
        .or_else(|| body.ipv6.first().copied().map(IpAddr::V6))?;
    crate::carrier::rtp_socket(Some(ip), body.rtp_port, body.l3_port)
}

fn split_header(payload: &[u8]) -> Option<(u8, u8, u32, &[u8])> {
    if payload.len() < HEADER || &payload[..4] != MAGIC {
        return None;
    }
    let version = payload[4];
    if !matches!(version, VERSION | VERSION_CODECS | VERSION_EXT) {
        return None;
    }
    let id = u32::from_be_bytes(payload[6..10].try_into().ok()?);
    Some((version, payload[5], id, &payload[HEADER..]))
}

fn take<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if rest.len() < n {
        return None;
    }
    let (head, tail) = rest.split_at(n);
    *rest = tail;
    Some(head)
}

fn clip_name(name: &str) -> &[u8] {
    clip_to(name, MAX_NAME)
}

fn clip_to(name: &str, max: usize) -> &[u8] {
    let bytes = name.as_bytes();
    let mut end = bytes.len().min(max);
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    &bytes[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> HostCard {
        HostCard {
            name: "etherquic".into(),
            l2_mac: [0xf2, 0x94, 0xbf, 0x4c, 0xa7, 0x1b],
            l2_service: 4433,
            l3_port: 4433,
            video: None,
            audio: None,
            l2_backend: 0,
        }
    }

    fn info() -> InterfaceInfo {
        InterfaceInfo {
            name: "enp4s0".into(),
            mac: Some([0x04, 0x7c, 0x16, 0xe4, 0x72, 0x24]),
            ipv4: vec![Ipv4Addr::new(192, 168, 0, 58)],
            ipv6: vec!["fe80::1".parse().unwrap()],
        }
    }

    #[test]
    fn announce_and_offer_round_trip() {
        let announce = announce_frame([0xd8, 0x5e, 0xd3, 0x88, 0x5e, 0x0e], 0x11);
        let bytes = announce.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert!(is_discovery(&decoded));
        assert_eq!(decode_announce(&decoded.payload), Some(0x11));
        assert!(announce_for_us(&bytes, [0x04, 0x7c, 0x16, 0xe4, 0x72, 0x24]).is_some());
        assert!(announce_for_us(&bytes, [0xd8, 0x5e, 0xd3, 0x88, 0x5e, 0x0e]).is_none());

        let offer = offer_frame(
            [0x04, 0x7c, 0x16, 0xe4, 0x72, 0x24],
            [0xd8, 0x5e, 0xd3, 0x88, 0x5e, 0x0e],
            0x11,
            &card(),
            &info(),
        );
        let (id, body, mac, _) = decode_offer_frame(&offer.encode()).unwrap();
        assert_eq!(id, 0x11);
        assert_eq!(mac, card().l2_mac);
        assert_eq!(body.name, "etherquic");
        assert_eq!(body.l2_service, 4433);
        assert_eq!(body.l3_port, 4433);
        assert_eq!(body.ipv4, info().ipv4);
        assert_eq!(body.ipv6, info().ipv6);
        assert_eq!(body.video, None);
        assert_eq!(body.audio, None);
        assert_eq!(body.l2_backend, 0);
    }

    #[test]
    fn offer_carries_h265_hardware_and_a_legacy_offer_does_not() {
        let mut announced = card();
        announced.video = Some(vec![AnnouncedCodec {
            id: VIDEO_H265,
            hardware_encode: true,
            hardware_decode: false,
            cpu: false,
            gpu: false,
            libopus: false,
            ffmpeg: false,
            gstreamer: false,
        }]);
        announced.audio = Some(vec![AnnouncedCodec {
            id: AUDIO_OPUS,
            hardware_encode: false,
            hardware_decode: false,
            cpu: true,
            gpu: false,
            libopus: false,
            ffmpeg: true,
            gstreamer: false,
        }]);
        let offer = offer_frame(
            [0x04, 0x7c, 0x16, 0xe4, 0x72, 0x24],
            [0xd8, 0x5e, 0xd3, 0x88, 0x5e, 0x0e],
            7,
            &announced,
            &info(),
        );
        let (id, body, _, _) = decode_offer_frame(&offer.encode()).unwrap();
        assert_eq!(id, 7);
        let video = body.video.unwrap();
        assert_eq!(video.len(), 1);
        assert_eq!(video[0].id, VIDEO_H265);
        assert!(video[0].hardware_encode);
        assert!(!video[0].hardware_decode);
        let audio = body.audio.unwrap();
        assert_eq!(audio[0].id, AUDIO_OPUS);
        assert!(audio[0].cpu);
        assert!(!audio[0].libopus);
        assert!(audio[0].ffmpeg);
        assert!(!audio[0].gstreamer);
        assert!(!audio[0].hardware_decode);
        assert_eq!(body.l2_backend, 0);
    }

    #[test]
    fn offer_carries_one_l2_driver_byte() {
        let mut announced = card();
        announced.video = Some(vec![]);
        announced.audio = Some(vec![]);
        announced.l2_backend = L2_NPCAP;
        let offer = offer_frame([1; 6], [2; 6], 9, &announced, &info());
        let (_, body, _, _) = decode_offer_frame(&offer.encode()).unwrap();
        assert_eq!(body.l2_backend, L2_NPCAP);
        assert_eq!(l2_backend_label(body.l2_backend), "npcap");
        assert_eq!(l2_backend_label(L2_LIBPCAP), "libpcap");
        assert_eq!(l2_backend_label(L2_MEMBRANE), "membrane");
        assert_eq!(l2_backend_label(0), "");

        announced.l2_backend = 0;
        let plain = offer_frame([1; 6], [2; 6], 9, &announced, &info());
        let (_, body, _, _) = decode_offer_frame(&plain.encode()).unwrap();
        assert_eq!(body.l2_backend, 0);

        let mut extra = offer.payload;
        extra.push(L2_LIBPCAP);
        assert!(decode_offer(&extra).is_none());
    }

    #[test]
    fn rejects_a_short_or_foreign_payload() {
        assert!(decode_announce(b"nope").is_none());
        assert!(decode_announce(&encode_header(VERSION, OFFER, 1)).is_none());
        let mut offer = offer_frame([1; 6], [2; 6], 3, &card(), &info()).payload;
        offer.pop();
        assert!(decode_offer(&offer).is_none());
    }

    fn full_caps() -> Capabilities {
        Capabilities {
            os: OS_WINDOWS,
            os_label: "Windows 11 Pro 24H2 build 26100.4652 extra".into(),
            hostname: "a-very-long-host-name-that-goes-on-and-on-and-on-past-the-limit-xyz".into(),
            nic_mac: Some([0x04, 0x7c, 0x16, 0xe4, 0x72, 0x24]),
            monitors: (0..20)
                .map(|i| MonitorInfo {
                    width: 3840,
                    height: 2160,
                    refresh_centihz: 14400,
                    x: (i * 3840) as i16,
                    y: -1080,
                    primary: i == 0,
                })
                .collect(),
            inputs: INPUT_KEYBOARD | INPUT_MOUSE | INPUT_TOUCH | INPUT_PEN,
            virtual_pad: VPAD_VIGEM,
            pads: (0..12)
                .map(|_| PadInfo {
                    api: PAD_API_XINPUT,
                    kind: PAD_KIND_GAMEPAD,
                    name: "Controller (Xbox One For Windows) with a long name".into(),
                })
                .collect(),
            audio_outputs: (0..6)
                .map(|i| format!("Speakers {i} (High Definition Audio Device)"))
                .collect(),
            audio_inputs: vec!["Microphone".into()],
            codecs: String::new(),
        }
    }

    struct Fixed {
        caps: Option<Capabilities>,
        /// Thumbnail length to hand back: `budget` minus this.
        short_by: Option<isize>,
    }

    impl OfferExtras for Fixed {
        fn capabilities(&self) -> Option<Capabilities> {
            self.caps.clone()
        }
        fn thumbnail(&self, budget: usize) -> Option<Vec<u8>> {
            let len = budget as isize - self.short_by?;
            (len > 0).then(|| vec![0xff; len as usize])
        }
    }

    fn big_info() -> InterfaceInfo {
        InterfaceInfo {
            name: "enp4s0".into(),
            mac: Some([0x04, 0x7c, 0x16, 0xe4, 0x72, 0x24]),
            ipv4: (1..=9).map(|n| Ipv4Addr::new(10, 0, 0, n)).collect(),
            ipv6: (1..=9u16)
                .map(|n| Ipv6Addr::new(0x2604, 0x3d09, 0, 0, 0, 0, 0, n))
                .collect(),
        }
    }

    fn big_card() -> HostCard {
        let codec = AnnouncedCodec {
            id: VIDEO_AV1,
            hardware_encode: true,
            hardware_decode: true,
            cpu: true,
            gpu: true,
            libopus: true,
            ffmpeg: true,
            gstreamer: true,
        };
        HostCard {
            name: "x".repeat(200),
            video: Some(vec![codec; 20]),
            audio: Some(vec![codec; 20]),
            l2_backend: L2_NPCAP,
            ..card()
        }
    }

    #[test]
    fn capabilities_round_trip_and_clip() {
        let caps = full_caps();
        let parsed = Capabilities::parse(&caps.encode()).unwrap();
        assert_eq!(parsed.os, OS_WINDOWS);
        assert_eq!(parsed.os_label.len(), MAX_OS_LABEL);
        assert_eq!(parsed.hostname.len(), MAX_HOSTNAME);
        assert_eq!(parsed.nic_mac, caps.nic_mac);
        assert_eq!(parsed.monitors.len(), MAX_MONITORS);
        assert_eq!(parsed.monitors[0], caps.monitors[0]);
        assert_eq!(parsed.monitors[1].x, 3840);
        assert_eq!(parsed.monitors[1].y, -1080);
        assert_eq!(parsed.inputs, caps.inputs);
        assert_eq!(parsed.virtual_pad, VPAD_VIGEM);
        assert_eq!(parsed.pads.len(), MAX_PADS);
        assert_eq!(parsed.pads[0].name.len(), MAX_PAD_NAME);

        let small = Capabilities {
            hostname: "hyte".into(),
            monitors: vec![MonitorInfo {
                width: 2560,
                height: 1440,
                refresh_centihz: 16500,
                x: 0,
                y: 0,
                primary: true,
            }],
            ..Capabilities::default()
        };
        assert_eq!(Capabilities::parse(&small.encode()), Some(small.clone()));
        assert_eq!(small.monitors[0].refresh_hz(), 165.0);
    }

    #[test]
    fn audio_devices_are_optional_trailing_fields() {
        let caps = full_caps();
        let bytes = caps.encode();
        let parsed = Capabilities::parse(&bytes).unwrap();
        assert_eq!(parsed.audio_outputs.len(), MAX_AUDIO_DEVICES);
        assert_eq!(parsed.audio_outputs[0].len(), MAX_AUDIO_NAME);
        assert_eq!(parsed.audio_inputs, vec!["Microphone".to_string()]);
        // An older host stops after the controllers: no devices, still parsed.
        let older = Capabilities {
            audio_outputs: Vec::new(),
            audio_inputs: Vec::new(),
            codecs: String::new(),
            ..caps.clone()
        }
        .encode();
        let older = &older[..older.len() - 2];
        let parsed = Capabilities::parse(older).unwrap();
        assert!(parsed.audio_outputs.is_empty() && parsed.audio_inputs.is_empty());
        assert_eq!(parsed.pads.len(), MAX_PADS);
        // Cut inside the device names: the controllers survive, the cut list does not.
        let cut = &bytes[..bytes.len() - 5];
        let parsed = Capabilities::parse(cut).unwrap();
        assert_eq!(parsed.pads.len(), MAX_PADS);
        assert!(parsed.audio_inputs.is_empty());
    }

    #[test]
    fn capabilities_from_a_newer_host_keep_known_fields() {
        let caps = full_caps();
        let mut newer = caps.encode();
        newer[0] = CAPS_VERSION + 4;
        newer.extend([0xde, 0xad, 0xbe, 0xef]);
        let parsed = Capabilities::parse(&newer).unwrap();
        assert_eq!(
            parsed.hostname,
            Capabilities::parse(&caps.encode()).unwrap().hostname
        );
        assert_eq!(parsed.pads.len(), MAX_PADS);
        // A cut inside a required field is rejected, not half read.
        let core = Capabilities {
            audio_outputs: Vec::new(),
            audio_inputs: Vec::new(),
            codecs: String::new(),
            ..caps.clone()
        }
        .encode();
        let core = &core[..core.len() - 2];
        for cut in [0, 1, 5, core.len() / 2, core.len() - 1] {
            assert!(Capabilities::parse(&core[..cut]).is_none(), "cut {cut}");
        }
        assert!(Capabilities::parse(&[0]).is_none());
    }

    #[test]
    fn an_older_player_gets_the_older_offer() {
        let extras = Fixed {
            caps: Some(full_caps()),
            short_by: Some(0),
        };
        let mut announced = card();
        announced.video = Some(vec![]);
        announced.audio = Some(vec![]);
        announced.l2_backend = L2_LIBPCAP;
        // Ids from an older player never carry the mark.
        let payload = offer_payload(0x0000_03e7, &announced, &info(), &extras, L2_PAYLOAD_LIMIT);
        assert_eq!(payload[4], VERSION_CODECS);
        // Byte for byte what an older host sends: header, body, codecs, driver byte.
        let mut expected = encode_header(VERSION_CODECS, OFFER, 0x3e7);
        expected.extend(announced.l2_mac);
        expected.extend(4433u16.to_be_bytes());
        expected.extend(4433u16.to_be_bytes());
        expected.push(9);
        expected.extend(b"etherquic");
        expected.extend([1, 192, 168, 0, 58, 1]);
        expected.extend("fe80::1".parse::<Ipv6Addr>().unwrap().octets());
        expected.extend([0, 0, L2_LIBPCAP]);
        assert_eq!(payload, expected);
        // A version-1 card still gets version 1.
        let v1 = offer_payload(7, &card(), &info(), &extras, L2_PAYLOAD_LIMIT);
        assert_eq!(v1[4], VERSION);
        assert_eq!(v1, legacy_payload(7, &card(), &info()));
    }

    /// What an older player does with an offer: versions 1 and 2 only, and
    /// nothing after the driver byte.
    fn older_player_accepts(payload: &[u8]) -> bool {
        if payload.len() < HEADER || &payload[..4] != MAGIC {
            return false;
        }
        matches!(payload[4], VERSION | VERSION_CODECS) && decode_offer(payload).is_some()
    }

    #[test]
    fn version_three_is_ignored_by_older_players_and_read_by_newer_ones() {
        let extras = Fixed {
            caps: Some(full_caps()),
            short_by: Some(0),
        };
        let id = EXT_MARK | 42;
        let payload = offer_payload(id, &card(), &info(), &extras, L2_PAYLOAD_LIMIT);
        assert_eq!(payload[4], VERSION_EXT);
        assert!(!older_player_accepts(&payload));
        let (got, body) = decode_offer(&payload).unwrap();
        assert_eq!(got, id);
        assert_eq!(body.name, "etherquic");
        assert_eq!(body.video, Some(vec![]));
        let caps = body.caps.unwrap();
        assert_eq!(caps.nic_mac, full_caps().nic_mac);
        assert_eq!(caps.monitors.len(), MAX_MONITORS);
        assert!(body.thumbnail.is_some());
        // A newer player still reads an older host's offer, without extras.
        let old = legacy_payload(5, &card(), &info());
        let (_, body) = decode_offer(&old).unwrap();
        assert!(body.caps.is_none() && body.thumbnail.is_none());
        // The adapter MAC is filled in when the host did not say.
        let anon = Fixed {
            caps: Some(Capabilities::default()),
            short_by: None,
        };
        let payload = offer_payload(id, &card(), &info(), &anon, L2_PAYLOAD_LIMIT);
        let (_, body) = decode_offer(&payload).unwrap();
        assert_eq!(body.caps.unwrap().nic_mac, info().mac);
    }

    #[test]
    fn codec_text_is_trailing_and_optional() {
        let caps = Capabilities {
            hostname: "box".into(),
            codecs: "host-video pyrowave=pyrowave:0x0@0 mjpeg=jpeg:0x0@0\nhost-audio opus pcm\n"
                .into(),
            ..Capabilities::default()
        };
        let bytes = caps.encode();
        assert_eq!(Capabilities::parse(&bytes).unwrap().codecs, caps.codecs);
        // An older host stops after the audio lists.
        let older = Capabilities {
            codecs: String::new(),
            ..caps.clone()
        }
        .encode();
        assert!(bytes.starts_with(&older));
        assert!(Capabilities::parse(&older).unwrap().codecs.is_empty());
    }

    #[test]
    fn unknown_and_cut_records_are_skipped() {
        let mut payload = encode_header(VERSION_EXT, OFFER, EXT_MARK | 1);
        write_body(&mut payload, &card(), &info());
        payload.extend([0, 0, L2_NPCAP]);
        push_tlv(&mut payload, 0x7f, b"from the future");
        let caps = Capabilities {
            hostname: "box".into(),
            ..Capabilities::default()
        };
        push_tlv(&mut payload, TLV_CAPS, &caps.encode());
        // A thumbnail record that claims more than arrived.
        payload.extend([TLV_THUMBNAIL, 0x05, 0x00, 1, 2, 3]);
        let (_, body) = decode_offer(&payload).unwrap();
        assert_eq!(body.l2_backend, L2_NPCAP);
        assert_eq!(body.caps.unwrap().hostname, "box");
        assert!(body.thumbnail.is_none());
        // A version-3 offer without its driver byte is malformed.
        let mut short = encode_header(VERSION_EXT, OFFER, 1);
        write_body(&mut short, &card(), &info());
        short.extend([0, 0]);
        assert!(decode_offer(&short).is_none());
    }

    #[test]
    fn budget_math() {
        assert_eq!(L2_PAYLOAD_LIMIT, 1494);
        assert_eq!(UDP4_PAYLOAD_LIMIT, 1472);
        assert_eq!(UDP6_PAYLOAD_LIMIT, 1452);
        assert_eq!(thumbnail_budget(1494, 200), Some(1291));
        assert_eq!(thumbnail_budget(1494, 1494), None);
        assert_eq!(
            thumbnail_budget(1494, 1494 - TLV_HEADER - MIN_THUMBNAIL),
            Some(MIN_THUMBNAIL)
        );
        assert_eq!(
            thumbnail_budget(1494, 1494 - TLV_HEADER - MIN_THUMBNAIL + 1),
            None
        );
        assert_eq!(thumbnail_budget(100, 5000), None);
        assert_eq!(
            udp_limit(&"10.0.0.1:4440".parse().unwrap()),
            UDP4_PAYLOAD_LIMIT
        );
        assert_eq!(
            udp_limit(&"[::ffff:10.0.0.1]:4440".parse().unwrap()),
            UDP4_PAYLOAD_LIMIT
        );
        assert_eq!(
            udp_limit(&"[fe80::1]:4440".parse().unwrap()),
            UDP6_PAYLOAD_LIMIT
        );
    }

    #[test]
    fn offers_never_exceed_the_frame() {
        let limits = [
            L2_PAYLOAD_LIMIT,
            UDP4_PAYLOAD_LIMIT,
            UDP6_PAYLOAD_LIMIT,
            600,
            200,
        ];
        let shapes = [(card(), info()), (big_card(), big_info())];
        // A thumbnail exactly as big as asked, one byte over, and none.
        for short_by in [Some(0), Some(-1), Some(10), None] {
            for caps in [None, Some(full_caps())] {
                let extras = Fixed {
                    caps: caps.clone(),
                    short_by,
                };
                for (card, info) in &shapes {
                    for limit in limits {
                        let payload = offer_payload(EXT_MARK | 9, card, info, &extras, limit);
                        assert!(payload.len() <= limit, "{} > {limit}", payload.len());
                        let frame = offer_frame_with([1; 6], [2; 6], payload.clone()).encode();
                        assert!(frame.len() <= crate::frame::ETHER_HEADER_LEN + MTU);
                        let (_, body) = decode_offer(&payload).expect("still decodes");
                        if short_by == Some(0) && limit == L2_PAYLOAD_LIMIT {
                            // The picture fills the frame exactly.
                            assert_eq!(payload.len(), limit);
                            assert!(body.thumbnail.is_some());
                        }
                        if short_by == Some(-1) {
                            assert!(body.thumbnail.is_none(), "oversized picture sent");
                        }
                    }
                }
            }
        }
        // With no room left, the offer still goes out with no picture.
        let extras = Fixed {
            caps: Some(full_caps()),
            short_by: Some(0),
        };
        let payload = offer_payload(EXT_MARK, &big_card(), &big_info(), &extras, 200);
        let (_, body) = decode_offer(&payload).unwrap();
        assert!(body.thumbnail.is_none());
        assert!(body.caps.is_none(), "caps did not fit in 200 bytes");
    }

    #[test]
    fn marked_announce_is_still_an_announce() {
        let announce = announce_frame([0xd8, 0x5e, 0xd3, 0x88, 0x5e, 0x0e], EXT_MARK | 5);
        let bytes = announce.encode();
        // An older host reads the id and answers it like any other.
        assert_eq!(
            decode_announce(&Frame::decode(&bytes).unwrap().payload),
            Some(EXT_MARK | 5)
        );
    }

    #[test]
    fn stats_of_a_known_burst() {
        let stats = Stats::from_samples(&[10.0, 12.0, 11.0, 15.0]);
        assert_eq!(stats.min_ms, 10.0);
        assert_eq!(stats.max_ms, 15.0);
        assert_eq!(stats.avg_ms, 12.0);
        assert!((stats.std_ms - 3.5_f32.sqrt()).abs() < 0.001);
        assert!((stats.jitter_ms - (7.0 / 3.0)).abs() < 0.001);
        let one = Stats::from_samples(&[4.0]);
        assert_eq!(one.std_ms, 0.0);
        assert_eq!(one.jitter_ms, 0.0);
        assert_eq!(Stats::from_samples(&[]).avg_ms, 0.0);
    }

    #[test]
    fn release_names_the_build_and_the_operating_system() {
        let rel = Release {
            version: "0.1.0".into(),
            os: OS_WINDOWS,
            os_name: "Windows".into(),
            os_version: "11".into(),
        };
        assert_eq!(Release::parse(&rel.encode()).unwrap(), rel);
        assert_eq!(rel.os_label(), "Windows 11");
        assert_eq!(rel.list_line(), "Windows 11 · version 0.1.0");
        assert_eq!(
            OsFace::from_os(OS_WINDOWS, "Windows", "11"),
            OsFace::Windows11
        );
        assert_eq!(
            OsFace::from_os(OS_WINDOWS, "Windows", "XP"),
            OsFace::WindowsXp
        );
        assert_eq!(
            OsFace::from_os(OS_WINDOWS, "Windows", "Vista"),
            OsFace::WindowsVista
        );
        assert_eq!(
            OsFace::from_os(OS_WINDOWS, "Windows", "7"),
            OsFace::Windows7
        );
        assert_eq!(
            OsFace::from_os(OS_WINDOWS, "Windows", "8"),
            OsFace::Windows8
        );
        assert_eq!(
            OsFace::from_os(OS_WINDOWS, "Windows", "8.1"),
            OsFace::Windows81
        );
        assert_eq!(
            OsFace::from_os(OS_WINDOWS, "Windows", "10"),
            OsFace::Windows10
        );
        assert_eq!(OsFace::from_os(OS_MACOS, "macOS", "15.1"), OsFace::Mac);
        assert_eq!(
            OsFace::from_os(OS_ANDROID, "Android", "14"),
            OsFace::Android
        );
        assert_eq!(
            face_from_label(OS_WINDOWS, "Windows 8.1 Pro").0,
            OsFace::Windows81
        );
        assert_eq!(face_from_label(OS_MACOS, "macOS 14.6.1").1, "14.6.1");
        assert_eq!(windows_mark(10, 0, 26100), "11");
        assert_eq!(windows_mark(10, 0, 19045), "10");
        assert_eq!(windows_mark(6, 1, 7601), "7");
        assert_eq!(windows_mark(6, 3, 9600), "8.1");
        assert_eq!(windows_mark(5, 1, 2600), "XP");
        assert_eq!(windows_mark(6, 0, 6002), "Vista");
        assert_eq!(windows_mark(6, 2, 9200), "8");
        assert_eq!(windows_mark(10, 0, 0), "");
        let reg = "\
CurrentMajorVersionNumber    REG_DWORD    0xa
CurrentMinorVersionNumber    REG_DWORD    0x0
CurrentBuildNumber    REG_SZ    26100
";
        assert_eq!(parse_windows_reg(reg), Some((10, 0, 26100)));
        let xp = "\
CurrentVersion    REG_SZ    5.1
CurrentBuildNumber    REG_SZ    2600
";
        assert_eq!(parse_windows_reg(xp), Some((5, 1, 2600)));
        assert_eq!(
            os_release_pair("NAME=\"NixOS\"\nVERSION_ID=\"24.11\"\n"),
            ("NixOS".into(), "24.11".into())
        );
        assert_eq!(
            plist_string(
                "<key>ProductVersion</key>\n<string>15.1</string>\n",
                "ProductVersion"
            ),
            Some("15.1".into())
        );
        assert_eq!(
            android_release("ro.build.version.release=14\n"),
            Some("14".into())
        );

        let extras = Fixed {
            caps: None,
            short_by: None,
        };
        let payload = offer_payload(EXT_MARK | 3, &card(), &info(), &extras, L2_PAYLOAD_LIMIT);
        let (_, body) = decode_offer(&payload).unwrap();
        let sent = body.release.expect("offer carries the release");
        assert_eq!(sent.version, crate::RELEASE);
        // An older reader skips the record. A hand-built one still decodes.
        let mut marked = encode_header(VERSION_EXT, OFFER, EXT_MARK | 1);
        write_body(&mut marked, &card(), &info());
        marked.extend([0, 0, L2_NPCAP]);
        push_tlv(&mut marked, TLV_RELEASE, &rel.encode());
        push_tlv(&mut marked, 0x7f, b"later");
        let (_, body) = decode_offer(&marked).unwrap();
        assert_eq!(body.release.unwrap(), rel);
    }

    #[test]
    fn carrier_tlv_parses_and_an_unknown_tlv_does_not_drop_the_offer() {
        let extras = Fixed {
            caps: None,
            short_by: None,
        };
        let payload = offer_payload(EXT_MARK | 11, &card(), &info(), &extras, L2_PAYLOAD_LIMIT);
        assert_eq!(payload[4], VERSION_EXT);
        let ids = offered_carriers(&payload).expect("offer parses");
        assert_eq!(ids, extras.carriers());
        assert!(!ids.is_empty());
        assert!(ids.len() <= crate::carrier::MAX_CARRIERS);
        let mut with_unknown = payload.clone();
        push_tlv(&mut with_unknown, 9, b"mystery");
        assert_eq!(offered_carriers(&with_unknown).unwrap(), ids);
        let old = legacy_payload(5, &card(), &info());
        assert_eq!(offered_carriers(&old).unwrap(), Vec::<u8>::new());
        assert_eq!(
            crate::carrier::effective_carriers(&offered_carriers(&old).unwrap()),
            vec![crate::carrier::QUIC_STREAMS]
        );
    }

    #[test]
    fn ipv4_announces_arrive_while_ipv6_is_bound() {
        let mut pair = None;
        for port in 40_000..40_200 {
            let v4 = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
            let Ok(v4) = bind_discovery_socket(v4) else {
                continue;
            };
            let v6 = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));
            let v6 = bind_discovery_socket(v6).expect("IPv6-only socket shares the discovery port");
            pair = Some((v4, v6, port));
            break;
        }
        let (v4, _v6, port) = pair.expect("a free UDP port");
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let announce = b"EQDS\x01\x01\x00\x00\x00\x2a";
        sender
            .send_to(announce, (Ipv4Addr::LOCALHOST, port))
            .unwrap();
        let mut buf = [0u8; 64];
        let (len, _) = v4.recv_from(&mut buf).expect("IPv4 announce");
        assert_eq!(&buf[..len], announce);
    }
}
