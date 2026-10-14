//! LAN announcement on the QUIC EtherType.
//!
//! The window sends a short burst of broadcasts. A host answers each one with
//! the MAC and service a player dials, the IP addresses on that adapter, and
//! the service port. Round-trip times of those answers are the quality
//! numbers: latency samples, jitter, and min / average / max / standard deviation.
//!
//! Discovery uses service [`SERVICE`], not the play service, so the QUIC
//! socket never sees these frames.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::frame::{Frame, DEFAULT_ETHER_TYPE};
use crate::pcap_link::{self, InterfaceInfo};
use crate::Mac;

/// Service id carried in the Ethernet frame. Not 4433, 4434, or 4435.
pub const SERVICE: u16 = 4440;

const MAGIC: &[u8; 4] = b"EQDS";
const VERSION: u8 = 1;
/// Offer that appends the host's codec hardware. Announces stay at [`VERSION`].
const VERSION_CODECS: u8 = 2;
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

/// Broadcast a burst and collect hosts. `generation` cancels the burst when it changes.
pub fn discover(
    nic: Option<&str>,
    generation: &AtomicU64,
    expected: u64,
) -> Result<Vec<Found>, String> {
    let info = pcap_link::interface_info(nic).map_err(|err| err.to_string())?;
    let src = info.mac.unwrap_or([0x02, 0x00, 0x00, 0x00, 0x00, 0x0d]);
    let mut link =
        pcap_link::open(Some(&info.name), DEFAULT_ETHER_TYPE).map_err(|err| err.to_string())?;
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
        let id = base.wrapping_add(index);
        let frame = announce_frame(src, id);
        link.send(&frame.encode()).map_err(|err| err.to_string())?;
        pending.insert(id, Instant::now());
        drain(&mut link, &pending, &mut acc, GAP, generation, expected);
    }
    if generation.load(Ordering::SeqCst) == expected {
        drain(&mut link, &pending, &mut acc, TAIL, generation, expected);
    }
    let mut found: Vec<Found> = acc.into_values().map(Acc::finish).collect();
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.l2_mac.cmp(&b.l2_mac)));
    Ok(found)
}

/// Answer announces until `stop` is set. One reply per announce.
pub fn serve(nic: Option<&str>, card: &HostCard, stop: &AtomicBool) -> Result<(), String> {
    let info = pcap_link::interface_info(nic).map_err(|err| err.to_string())?;
    let src = info.mac.unwrap_or(card.l2_mac);
    let mut link =
        pcap_link::open(Some(&info.name), DEFAULT_ETHER_TYPE).map_err(|err| err.to_string())?;
    while !stop.load(Ordering::SeqCst) {
        match link.recv() {
            Ok(Some(raw)) => {
                if let Some(id_dst) = announce_for_us(&raw, src) {
                    let (id, dst) = id_dst;
                    let frame = offer_frame(src, dst, id, card, &info);
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

struct Acc {
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
        }
    }
}

fn drain(
    link: &mut pcap_link::Link,
    pending: &HashMap<u32, Instant>,
    acc: &mut HashMap<Mac, Acc>,
    budget: Duration,
    generation: &AtomicU64,
    expected: u64,
) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if generation.load(Ordering::SeqCst) != expected {
            return;
        }
        match link.recv() {
            Ok(Some(raw)) => note_offer(&raw, pending, acc),
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn note_offer(raw: &[u8], pending: &HashMap<u32, Instant>, acc: &mut HashMap<Mac, Acc>) {
    let Some((id, info, mac)) = decode_offer_frame(raw) else {
        return;
    };
    let Some(sent) = pending.get(&id) else {
        return;
    };
    let ms = sent.elapsed().as_secs_f32() * 1000.0;
    let row = acc.entry(mac).or_insert_with(|| Acc {
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
    row.name = info.name;
    row.l2_service = info.l2_service;
    row.l3_port = info.l3_port;
    row.ipv4 = info.ipv4;
    row.ipv6 = info.ipv6;
    row.video = info.video;
    row.audio = info.audio;
    row.l2_backend = info.l2_backend;
    row.samples.push((id, ms));
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

fn offer_frame(src: Mac, dst: Mac, id: u32, card: &HostCard, info: &InterfaceInfo) -> Frame {
    let codecs = card.video.is_some() || card.audio.is_some();
    let version = if codecs { VERSION_CODECS } else { VERSION };
    let mut payload = encode_header(version, OFFER, id);
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
    if codecs {
        write_codecs(&mut payload, card.video.as_deref().unwrap_or(&[]));
        write_codecs(&mut payload, card.audio.as_deref().unwrap_or(&[]));
        // One byte, only when the host knows its driver. A missing byte is
        // still a version-2 offer, so an older host keeps answering.
        if card.l2_backend != 0 {
            payload.push(card.l2_backend);
        }
    }
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

fn decode_offer_frame(raw: &[u8]) -> Option<(u32, OfferBody, Mac)> {
    let frame = Frame::decode(raw).ok()?;
    if !is_discovery(&frame) {
        return None;
    }
    let (id, body) = decode_offer(&frame.payload)?;
    if body_mac_unusable(frame.payload.get(HEADER..HEADER + 6)?) {
        return None;
    }
    let mac = body_mac(&frame.payload)?;
    Some((id, body, mac))
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
    if kind != OFFER || (version != VERSION && version != VERSION_CODECS) {
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
    let (video, audio, l2_backend) = if version == VERSION {
        if !rest.is_empty() {
            return None;
        }
        (None, None, 0)
    } else {
        let video = read_codecs(&mut rest)?;
        let audio = read_codecs(&mut rest)?;
        let l2_backend = match rest.len() {
            0 => 0,
            1 => rest[0],
            _ => return None,
        };
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
        },
    ))
}

fn split_header(payload: &[u8]) -> Option<(u8, u8, u32, &[u8])> {
    if payload.len() < HEADER || &payload[..4] != MAGIC {
        return None;
    }
    let version = payload[4];
    if version != VERSION && version != VERSION_CODECS {
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
    let bytes = name.as_bytes();
    let mut end = bytes.len().min(MAX_NAME);
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
        let (id, body, mac) = decode_offer_frame(&offer.encode()).unwrap();
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
        let (id, body, _) = decode_offer_frame(&offer.encode()).unwrap();
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
        let (_, body, _) = decode_offer_frame(&offer.encode()).unwrap();
        assert_eq!(body.l2_backend, L2_NPCAP);
        assert_eq!(l2_backend_label(body.l2_backend), "npcap");
        assert_eq!(l2_backend_label(L2_LIBPCAP), "libpcap");
        assert_eq!(l2_backend_label(L2_MEMBRANE), "membrane");
        assert_eq!(l2_backend_label(0), "");

        announced.l2_backend = 0;
        let plain = offer_frame([1; 6], [2; 6], 9, &announced, &info());
        let (_, body, _) = decode_offer_frame(&plain.encode()).unwrap();
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
}
