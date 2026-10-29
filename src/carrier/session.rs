//! One picture and one audio record on carriers 3, 4, and 5.
//!
//! Sockets are UDP on 127.0.0.1. AVB carries the Ethernet frame in the UDP
//! payload because this process may not have a raw NIC. No Quinn endpoint is
//! constructed here.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use super::assemble::{split_record, Assembler, Lane, Slice};
use super::rtp::{
    self, audio_deadline, derive_aes128, picture_deadline, rtcp_loss_fraction,
    rtcp_receiver_report, GcmSession, KeyShare, RtpHeader, PROFILE_GCM,
};
use super::{RateCap, AVB, ST2110, UDP_RTP};

#[derive(Clone, Debug)]
pub struct SessionReport {
    pub carrier: u8,
    pub quic_before: u64,
    pub quic_after: u64,
    pub picture: Vec<u8>,
    pub audio: Vec<u8>,
    pub ethertype: u16,
    pub subtype: u8,
    pub offers: u32,
    pub config_packets: u32,
    pub profile: String,
    pub keys_sent: u32,
    pub eaku: bool,
    pub pointer_sends: u32,
    pub loss: f64,
    pub cap: u64,
}

/// Host and player finish `picture` and `audio` without opening a QUIC socket.
pub fn complete_session(id: u8, picture: &[u8], audio: &[u8]) -> SessionReport {
    let quic_before = crate::quic_binds();
    crate::capture::set_capture_carrier(id);
    let mut report = match id {
        UDP_RTP => udp_session(picture, audio),
        AVB => avb_session(picture, audio),
        ST2110 => st2110_session(picture, audio),
        _ => empty(id, picture, audio),
    };
    report.quic_before = quic_before;
    report.quic_after = crate::quic_binds();
    report.carrier = id;
    report
}

fn empty(id: u8, picture: &[u8], audio: &[u8]) -> SessionReport {
    SessionReport {
        carrier: id,
        quic_before: 0,
        quic_after: 0,
        picture: picture.to_vec(),
        audio: audio.to_vec(),
        ethertype: 0,
        subtype: 0,
        offers: 0,
        config_packets: 0,
        profile: String::new(),
        keys_sent: 0,
        eaku: false,
        pointer_sends: 0,
        loss: 0.0,
        cap: 0,
    }
}

fn bind() -> UdpSocket {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("udp bind");
    socket
        .set_read_timeout(Some(Duration::from_millis(40)))
        .expect("timeout");
    socket
}

fn udp_session(picture: &[u8], audio: &[u8]) -> SessionReport {
    let host_in = bind();
    let host_pic = bind();
    let host_aud = bind();
    let play_pic = bind();
    let play_aud = bind();
    let play_in = bind();
    let host_in_addr = host_in.local_addr().expect("input");
    let play_pic_addr = play_pic.local_addr().expect("picture");
    let play_aud_addr = play_aud.local_addr().expect("audio");

    let player = KeyShare::from_seed([0x21; 32]);
    let host = rtp::host_share();
    let mut offer = b"OFFER".to_vec();
    offer.extend_from_slice(&player.public);
    offer.extend(play_pic_addr.port().to_le_bytes());
    offer.extend(play_aud_addr.port().to_le_bytes());

    let mut offers = 0u32;
    let mut config_packets = 0u32;
    let mut player_public = [0u8; 32];
    let started = Instant::now();
    let mut got_config = false;
    while started.elapsed() < Duration::from_secs(1) && !got_config {
        let due = offers == 0 || started.elapsed() >= Duration::from_millis(20 * u64::from(offers));
        if !due {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        offers += 1;
        let _ = play_in.send_to(&offer, host_in_addr);
        let mut buf = [0u8; 2048];
        if let Ok((n, _src)) = host_in.recv_from(&mut buf) {
            if n >= 37 && &buf[..5] == b"OFFER" && config_packets == 0 {
                player_public.copy_from_slice(&buf[5..37]);
                let mut payload = b"CFG".to_vec();
                payload.extend_from_slice(&host.public);
                let packet = RtpHeader {
                    marker: true,
                    payload_type: rtp::PT_PICTURE,
                    sequence: 0,
                    timestamp: 0,
                    ssrc: 1,
                }
                .encode(&payload);
                let _ = host_pic.send_to(&packet, play_pic_addr);
                config_packets = 1;
            }
        }
        if let Ok((n, _)) = play_pic.recv_from(&mut buf) {
            if let Some((_, payload)) = RtpHeader::decode(&buf[..n]) {
                if payload.starts_with(b"CFG") && payload.len() >= 35 {
                    got_config = true;
                }
            }
        }
    }

    let shared = player.shared(&host.public);
    let host_shared = host.shared(&player_public);
    let key = derive_aes128(&shared);
    let host_key = derive_aes128(&host_shared);
    let mut host_gcm = GcmSession::new(host_key);
    let mut play_gcm = GcmSession::new(key);

    let mut seq = 1u16;
    send_sealed(
        &host_pic,
        play_pic_addr,
        &mut host_gcm,
        true,
        1,
        picture,
        &mut seq,
    );
    send_sealed(
        &host_aud,
        play_aud_addr,
        &mut host_gcm,
        false,
        2,
        audio,
        &mut seq,
    );

    let picture_out = recv_sealed(&play_pic, &mut play_gcm, true, picture.len());
    let audio_out = recv_sealed(&play_aud, &mut play_gcm, false, audio.len());

    let (keys_sent, eaku) = retry_key(&play_in, &host_in, host_in_addr);
    let pointer_sends = send_pointer_once(&play_in, &host_in, host_in_addr);

    let report_pkt = rtcp_receiver_report(9, 11, 100, 10, 50);
    let _ = play_pic.send_to(&report_pkt, host_pic.local_addr().expect("host picture"));
    let mut buf = [0u8; 256];
    let mut loss = 0.0;
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        if let Ok((n, _)) = host_pic.recv_from(&mut buf) {
            if let Some(fraction) = rtcp_loss_fraction(&buf[..n]) {
                loss = fraction;
                break;
            }
        }
    }
    let mut cap = RateCap::new(1_000_000);
    cap.observe(Duration::from_secs(1), loss);

    let _ = (host_aud, play_aud);
    SessionReport {
        carrier: UDP_RTP,
        quic_before: 0,
        quic_after: 0,
        picture: picture_out,
        audio: audio_out,
        ethertype: 0,
        subtype: 0,
        offers,
        config_packets,
        profile: PROFILE_GCM.into(),
        keys_sent,
        eaku,
        pointer_sends,
        loss,
        cap: cap.cap,
    }
}

fn send_sealed(
    sock: &UdpSocket,
    peer: SocketAddr,
    gcm: &mut GcmSession,
    picture: bool,
    record_id: u16,
    bytes: &[u8],
    seq: &mut u16,
) {
    if bytes.is_empty() {
        return;
    }
    let ssrc = if picture { 11 } else { 22 };
    let pt = if picture { rtp::PT_PICTURE } else { rtp::PT_AUDIO };
    let slices = split_record(record_id, bytes, 48);
    for slice in &slices {
        let plain = slice.encode();
        let sealed = gcm.seal(ssrc, 0, *seq, &plain).expect("seal");
        let capture = Duration::from_millis(u64::from(*seq));
        let timestamp = if picture {
            rtp::picture_rtp_timestamp(capture)
        } else {
            rtp::audio_rtp_timestamp(capture, 48_000)
        };
        let packet = RtpHeader {
            marker: slice.index + 1 == slice.count,
            payload_type: pt,
            sequence: *seq,
            timestamp,
            ssrc,
        }
        .encode(&sealed);
        crate::capture::note_rtp(crate::capture::DIR_SEND, &packet);
        let _ = sock.send_to(&packet, peer);
        *seq = seq.wrapping_add(1);
    }
}

fn recv_sealed(sock: &UdpSocket, gcm: &mut GcmSession, picture: bool, expect: usize) -> Vec<u8> {
    if expect == 0 {
        return Vec::new();
    }
    let mut asm = Assembler::new();
    let deadline = if picture {
        picture_deadline(Duration::from_millis(16))
    } else {
        audio_deadline()
    };
    let end = Instant::now() + Duration::from_millis(400);
    let mut buf = [0u8; 2048];
    while Instant::now() < end {
        let Ok((n, _)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let Some((header, payload)) = RtpHeader::decode(&buf[..n]) else {
            continue;
        };
        if payload.starts_with(b"CFG") || payload.len() >= 32 && header.payload_type == 201 {
            continue;
        }
        let Ok(plain) = gcm.open(header.ssrc, 0, header.sequence, payload) else {
            continue;
        };
        let Some(slice) = Slice::decode(&plain) else {
            continue;
        };
        let now = Instant::now();
        if let Some(record) = asm.push(slice, now, now + deadline) {
            return record;
        }
    }
    Vec::new()
}

fn retry_key(player: &UdpSocket, host: &UdpSocket, host_addr: SocketAddr) -> (u32, bool) {
    let mut sent = 0u32;
    let mut seen = 0u32;
    let mut acked = false;
    let end = Instant::now() + Duration::from_millis(200);
    let mut buf = [0u8; 64];
    while Instant::now() < end && !acked {
        sent += 1;
        let _ = player.send_to(b"KEY\x04", host_addr);
        if let Ok((n, src)) = host.recv_from(&mut buf) {
            if n >= 4 && &buf[..3] == b"KEY" {
                seen += 1;
                if seen >= 2 {
                    let _ = host.send_to(b"EAKU", src);
                }
            }
        }
        if let Ok((n, _)) = player.recv_from(&mut buf) {
            if &buf[..n] == b"EAKU" {
                acked = true;
            }
        }
    }
    let _ = seen;
    (sent, acked)
}

fn send_pointer_once(player: &UdpSocket, host: &UdpSocket, host_addr: SocketAddr) -> u32 {
    let mut sent = 0u32;
    for _ in 0..2 {
        if sent == 0 {
            let _ = player.send_to(b"PTR", host_addr);
            sent = 1;
        }
    }
    let mut buf = [0u8; 16];
    let mut got = 0u32;
    let end = Instant::now() + Duration::from_millis(120);
    while Instant::now() < end && got == 0 {
        if let Ok((n, _)) = host.recv_from(&mut buf) {
            if &buf[..n] == b"PTR" {
                got += 1;
            }
        }
    }
    got
}

fn avb_session(picture: &[u8], audio: &[u8]) -> SessionReport {
    let host = bind();
    let player = bind();
    avb::note_listener(true);
    if avb::try_reserve().is_err() {
        avb::note_listener(false);
        return empty(AVB, &[], &[]);
    }
    let player_addr = player.local_addr().expect("player");
    let dst = [2, 0, 0, 0, 0, 4];
    let src = [2, 0, 0, 0, 0, 1];
    let picture_frame = avb::picture_frame(dst, src, 1, 1, 90_000, picture, None, true)
        .expect("cvf frame");
    let audio_frame =
        avb::audio_frame(dst, src, 2, 1, 48_000, audio, None).expect("aaf frame");
    crate::capture::note_avtp(crate::capture::DIR_SEND, &picture_frame.ethernet);
    let _ = host.send_to(&picture_frame.ethernet, player_addr);
    let _ = host.send_to(&audio_frame.ethernet, player_addr);
    let got_picture = recv_frame(&player);
    let got_audio = recv_frame(&player);
    let ethertype = avb::ethertype_of(&got_picture).unwrap_or(0);
    let subtype = avb::payload_of(&got_picture)
        .and_then(|pdu| pdu.first().copied())
        .unwrap_or(0);
    let picture_out = avb::payload_of(&got_picture)
        .and_then(avb::decode_pdu)
        .map(|frame| frame.payload)
        .unwrap_or_default();
    let audio_out = avb::payload_of(&got_audio)
        .and_then(avb::decode_pdu)
        .map(|frame| frame.payload)
        .unwrap_or_default();
    if !picture_out.is_empty() {
        super::note_reservation(true);
    }
    let mut report = empty(AVB, &picture_out, &audio_out);
    report.ethertype = ethertype;
    report.subtype = subtype;
    report.profile = "not encrypted".into();
    report
}

/// Player AVB socket. One hello on the host input port, then assembled records.
pub fn spawn_player_avb(
    host_input: std::net::SocketAddr,
    picture: tokio::sync::mpsc::Sender<Vec<u8>>,
    audio: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    // The socket is the listener. Note it before the hello so the first
    // picture is not dropped while `try_reserve` still sees no listener.
    ::avb::note_listener(true);
    std::thread::spawn(move || {
        let Some(sock) = rtp::bind_unspecified(host_input) else {
            return;
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
        let Ok(addr) = sock.local_addr() else {
            return;
        };
        let mut hello = b"AVB".to_vec();
        hello.extend(addr.port().to_le_bytes());
        let _ = rtp::udp_send(&sock, &hello, host_input);
        let mut pictures = Assembler::new();
        let mut audios = Assembler::new();
        let mut buf = [0u8; 2048];
        while !picture.is_closed() && !audio.is_closed() {
            let Ok((n, _)) = sock.recv_from(&mut buf) else {
                continue;
            };
            let raw = &buf[..n];
            // The seam sends an etherquic frame. The PDU starts after that header.
            let pdu = crate::frame::decode_frame(raw)
                .map(|frame| frame.payload)
                .unwrap_or_else(|_| avb::payload_of(raw).unwrap_or(raw).to_vec());
            let Some(frame) = super::AvtpFrame::decode(&pdu) else {
                continue;
            };
            let Some(slice) = Slice::decode(&frame.payload) else {
                continue;
            };
            let now = Instant::now();
            let is_audio = frame.subtype == avb::SUBTYPE_AAF
                || (frame.subtype != avb::SUBTYPE_CVF && frame.stream_id[7] == Lane::Audio as u8);
            let (asm, tx) = if is_audio {
                (&mut audios, &audio)
            } else {
                (&mut pictures, &picture)
            };
            if let Some(record) = asm.push(slice, now, now + Duration::from_secs(1)) {
                if tx.blocking_send(record).is_err() {
                    return;
                }
            }
        }
    });
}

const ST_AUDIO_RATE: u32 = 48_000;
const ST_AUDIO_FRAMES_PER_MS: usize = 48;

/// ST 2110-30 packets gathered into the PCM record `read_audio` decodes.
///
/// Level A is 48 kHz in 1 ms packets. The first packet of a record holds 48
/// frames per channel and that length is the channel count. A shorter first
/// packet is a capture fragment: treating its length as one channel plays the
/// following stereo samples as mono. Samples on the wire are big-endian; the
/// record is little-endian, with rate and channels in front, because that is
/// what the PCM decoder reads.
#[derive(Default)]
pub struct StAudio {
    pcm: Vec<u8>,
    channels: u16,
}

impl StAudio {
    pub fn push(&mut self, marker: bool, payload: &[u8]) -> Option<Vec<u8>> {
        let _ = marker;
        let samples = payload.len() / 2;
        // One millisecond, and an integer number of those groups. Level A
        // leaves the marker clear, so a full packet closes the record itself.
        if payload.len() % 2 != 0
            || samples < ST_AUDIO_FRAMES_PER_MS
            || samples % ST_AUDIO_FRAMES_PER_MS != 0
        {
            return None;
        }
        let channels = samples / ST_AUDIO_FRAMES_PER_MS;
        if !(1..=8).contains(&channels) {
            return None;
        }
        self.channels = u16::try_from(channels).unwrap_or(2);
        self.pcm.clear();
        for chunk in payload.chunks_exact(2) {
            let sample = i16::from_be_bytes([chunk[0], chunk[1]]);
            self.pcm.extend(sample.to_le_bytes());
        }
        let mut record = Vec::with_capacity(7 + self.pcm.len());
        record.push(crate::negotiate::RECORD_DATA);
        record.extend(ST_AUDIO_RATE.to_le_bytes());
        record.extend(self.channels.to_le_bytes());
        record.append(&mut self.pcm);
        Some(record)
    }
}

/// RTP header check and optional `gcm` open. Returns the marker and payload.
fn st_open(gcm: &mut Option<rtp::GcmSession>, packet: &[u8]) -> Option<(bool, Vec<u8>)> {
    let (header, sealed) = RtpHeader::decode(packet)?;
    let payload = match gcm.as_mut() {
        Some(session) => session.open_next(header.ssrc, header.sequence, sealed).ok()?,
        None => sealed.to_vec(),
    };
    Some((header.marker, payload))
}

/// Player ST 2110 sockets. Picture and audio stay on their own ports.
pub fn spawn_player_st2110(
    host_input: std::net::SocketAddr,
    picture: tokio::sync::mpsc::Sender<Vec<u8>>,
    audio: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    std::thread::spawn(move || {
        let Some(picture_sock) = rtp::bind_unspecified(host_input) else {
            return;
        };
        let Some(audio_sock) = rtp::bind_unspecified(host_input) else {
            return;
        };
        let _ = picture_sock.set_read_timeout(Some(Duration::from_millis(50)));
        let _ = audio_sock.set_read_timeout(Some(Duration::from_millis(50)));
        let Ok(picture_addr) = picture_sock.local_addr() else {
            return;
        };
        let Ok(audio_addr) = audio_sock.local_addr() else {
            return;
        };
        let mut hello = b"ST2".to_vec();
        hello.extend(picture_addr.port().to_le_bytes());
        hello.extend(audio_addr.port().to_le_bytes());
        let gcm_on = super::gates().st_gcm;
        hello.push(if gcm_on { b'G' } else { b'P' });
        let mut last_hello: Option<Instant> = None;
        let mut heard = false;
        // Same key `seal_st` uses. The RTP hub key is the UDP OFFER exchange
        // and is not this carrier's key.
        let key = gcm_on.then(rtp::session_key);
        // Audio has its own thread. One loop that only read the audio socket
        // after a 50 ms picture timeout never read it while pictures flowed.
        if let Ok(audio_sock) = audio_sock.try_clone() {
            let picture_alive = picture.clone();
            std::thread::spawn(move || {
                let mut gcm = key.map(rtp::GcmSession::new);
                let mut acc = StAudio::default();
                let mut buf = [0u8; 2048];
                while !audio.is_closed() && !picture_alive.is_closed() {
                    let Ok((n, _)) = audio_sock.recv_from(&mut buf) else {
                        continue;
                    };
                    let Some((marker, payload)) = st_open(&mut gcm, &buf[..n]) else {
                        continue;
                    };
                    let Some(record) = acc.push(marker, &payload) else {
                        continue;
                    };
                    if audio.blocking_send(record).is_err() {
                        return;
                    }
                }
            });
        }
        let mut gcm = key.map(rtp::GcmSession::new);
        // Frames follow the RFC 9134 header: T, K, the frame counter, and the
        // packet counter. K stays 0 in codestream mode. L, not K, ends the frame.
        let mut parts = Vec::new();
        let mut next_index: Option<u32> = None;
        let mut k_bit: Option<bool> = None;
        let mut frame_counter: Option<u8> = None;
        let mut buf = [0u8; 2048];
        while !picture.is_closed() {
            // Keep saying hello until a picture arrives. One second of
            // hellos used to end before the host had a peer, and every
            // later packet was dropped on the floor.
            let due = !heard
                && last_hello
                    .map(|sent| sent.elapsed() >= Duration::from_millis(20))
                    .unwrap_or(true);
            if due {
                let _ = rtp::udp_send(&picture_sock, &hello, host_input);
                last_hello = Some(Instant::now());
            }
            let Ok((n, _)) = picture_sock.recv_from(&mut buf) else {
                continue;
            };
            let Some((last, payload)) = st_open(&mut gcm, &buf[..n]) else {
                continue;
            };
            let Some(fields) = st2110::xs_fields(&payload) else {
                continue;
            };
            heard = true;
            if !fields.t && !fields.k {
                continue;
            }
            if let Some(k) = k_bit {
                if k != fields.k {
                    k_bit = None;
                    next_index = None;
                    frame_counter = None;
                    parts.clear();
                    continue;
                }
            } else {
                k_bit = Some(fields.k);
            }
            if frame_counter != Some(fields.frame) || fields.index() == 0 {
                parts.clear();
                next_index = Some(0);
                frame_counter = Some(fields.frame);
            }
            let index = fields.index();
            if next_index != Some(index) {
                next_index = None;
                frame_counter = None;
                parts.clear();
                continue;
            }
            parts.extend_from_slice(&payload[4..]);
            next_index = Some(index.wrapping_add(1));
            if fields.l || last {
                next_index = None;
                frame_counter = None;
                let record = std::mem::take(&mut parts);
                if picture.blocking_send(record).is_err() {
                    return;
                }
            }
        }
    });
}

fn st2110_session(picture: &[u8], audio: &[u8]) -> SessionReport {
    let host_pic = bind();
    let host_aud = bind();
    let play_pic = bind();
    let play_aud = bind();
    let picture_addr = play_pic.local_addr().expect("picture");
    let audio_addr = play_aud.local_addr().expect("audio");
    let xs = st2110::packetize_jpeg_xs(picture, 90_000);
    for packet in &xs {
        crate::capture::note_rtp(crate::capture::DIR_SEND, &packet.rtp);
        let _ = host_pic.send_to(&packet.rtp, picture_addr);
    }
    let samples = audio_samples(audio);
    let pcm = st2110::packetize_pcm(&samples, 1, 48_000);
    for packet in &pcm {
        crate::capture::note_rtp(crate::capture::DIR_SEND, &packet.rtp);
        let _ = host_aud.send_to(&packet.rtp, audio_addr);
    }
    let picture_out = recv_xs(&play_pic, xs.len());
    let audio_out = recv_pcm(&play_aud, pcm.len(), &samples);
    let ports = [
        host_pic.local_addr().expect("p").port(),
        host_aud.local_addr().expect("a").port(),
        0,
    ];
    let _ = st2110::quic_overlaps(&ports, ports[0]);
    let mut report = empty(ST2110, &picture_out, &audio_out);
    report.profile = st2110::default_profile().name().into();
    if let (Some(first), Some(last), Some(audio_pkt)) = (xs.first(), xs.last(), pcm.first()) {
        let fields = st2110::xs_fields(&first.payload);
        let shared = xs.iter().all(|packet| packet.timestamp == first.timestamp);
        let marker_only_last = last.marker && xs[..xs.len() - 1].iter().all(|packet| !packet.marker);
        let audio_octets = audio_pkt.rtp.len().saturating_sub(12);
        println!(
            "st2110 picture_identical={} audio_identical={} t={} k={} timestamp_shared={} marker_only_last={} audio_octets={} audio_marker={} audio_ts={}",
            picture_out == picture,
            audio_out == audio_le(&samples),
            fields.map(|fields| u8::from(fields.t)).unwrap_or(0),
            fields.map(|fields| u8::from(fields.k)).unwrap_or(1),
            shared,
            marker_only_last,
            audio_octets,
            audio_pkt.rtp.get(1).map(|byte| byte & 0x80).unwrap_or(0x80),
            audio_pkt.timestamp
        );
    }
    report
}

fn audio_le(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|sample| sample.to_le_bytes()).collect()
}

fn audio_samples(audio: &[u8]) -> Vec<i16> {
    if audio.is_empty() {
        return vec![0];
    }
    audio
        .chunks(2)
        .map(|chunk| {
            if chunk.len() == 2 {
                i16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                i16::from(chunk[0])
            }
        })
        .collect()
}

fn recv_frame(sock: &UdpSocket) -> Vec<u8> {
    let mut buf = [0u8; 2048];
    let end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < end {
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            return buf[..n].to_vec();
        }
    }
    Vec::new()
}

fn recv_xs(sock: &UdpSocket, count: usize) -> Vec<u8> {
    let mut parts = vec![None; count.max(1)];
    let mut buf = [0u8; 2048];
    let end = Instant::now() + Duration::from_millis(400);
    let mut got = 0usize;
    while Instant::now() < end && got < count.max(1) {
        let Ok((n, _)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let Some((_, payload)) = RtpHeader::decode(&buf[..n]) else {
            continue;
        };
        let Some(fields) = st2110::xs_fields(payload) else {
            continue;
        };
        if !fields.t && !fields.k {
            continue;
        }
        let index = fields.index() as usize;
        if index < parts.len() && parts[index].is_none() {
            parts[index] = Some(payload[4..].to_vec());
            got += 1;
        }
    }
    let mut out = Vec::new();
    for part in parts.into_iter().flatten() {
        out.extend(part);
    }
    out
}

fn recv_pcm(sock: &UdpSocket, count: usize, samples: &[i16]) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 2048];
    let end = Instant::now() + Duration::from_millis(400);
    let mut n = 0usize;
    while Instant::now() < end && n < count.max(1) {
        let Ok((len, _)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let Some((_, payload)) = RtpHeader::decode(&buf[..len]) else {
            continue;
        };
        for chunk in payload.chunks_exact(2) {
            got.push(i16::from_be_bytes([chunk[0], chunk[1]]));
        }
        n += 1;
    }
    if got != samples {
        return Vec::new();
    }
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

/// Bind the host input socket the player offers on. Port 0 means nothing opened.
pub fn prepare_host() -> std::net::SocketAddr {
    for candidate in 21_000..21_200u16 {
        let ports = [
            candidate,
            candidate.saturating_add(1),
            candidate.saturating_add(2),
            candidate.saturating_add(4),
        ];
        if ports.iter().any(|port| !super::udp_port_free(*port)) {
            continue;
        }
        super::set_l3_port(candidate);
        if rtp::rtp_owns(candidate) {
            let input = candidate.saturating_add(2);
            super::publish_input(input);
            return std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, input));
        }
    }
    std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

/// Host send on the seam, after the player hello has registered a peer.
pub fn host_push(id: u8, picture: &[u8], audio: &[u8]) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) && !rtp::hub_has_peer() {
        std::thread::sleep(Duration::from_millis(2));
    }
    if !rtp::hub_has_peer() {
        return false;
    }
    let mut seam = super::Seam::open(id);
    let capture = Duration::from_millis(1);
    let picture_ok = seam
        .send(super::Lane::Picture, picture, None, capture)
        .is_some();
    let audio_ok = seam
        .send(super::Lane::Audio, audio, None, capture)
        .is_some();
    picture_ok && audio_ok
}

/// Carrier 3–5 connect. No Quinn endpoint. `l2_offer` uses the scanned RTP socket.
pub fn open_carrier_player(
    id: u8,
    udp: Option<std::net::SocketAddr>,
    l2_offer: bool,
    rtp: Option<std::net::SocketAddr>,
) -> (Vec<u8>, Vec<u8>) {
    let Some(host) = super::player_host_for(udp, l2_offer, rtp) else {
        return (Vec::new(), Vec::new());
    };
    play_on_carrier(id, host)
}

/// Player sockets for carriers 3–5. Returns the picture and audio the host sent.
pub fn play_on_carrier(id: u8, host_input: std::net::SocketAddr) -> (Vec<u8>, Vec<u8>) {
    let (picture_tx, mut picture_rx) = tokio::sync::mpsc::channel(4);
    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel(4);
    match id {
        super::UDP_RTP => rtp::spawn_player_rtp(host_input, picture_tx, audio_tx),
        super::AVB => spawn_player_avb(host_input, picture_tx, audio_tx),
        super::ST2110 => spawn_player_st2110(host_input, picture_tx, audio_tx),
        _ => return (Vec::new(), Vec::new()),
    }
    let picture = recv_player(&mut picture_rx, Duration::from_millis(1500));
    let audio = recv_player(&mut audio_rx, Duration::from_millis(1500));
    (picture, audio)
}

fn recv_player(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>, wait: Duration) -> Vec<u8> {
    let start = Instant::now();
    while start.elapsed() < wait {
        match rx.try_recv() {
            Ok(bytes) if !bytes.is_empty() => return bytes,
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return Vec::new(),
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn st_audio_record_is_the_big_endian_samples() {
        let samples: Vec<i16> = (0..96)
            .map(|i| if i % 2 == 0 { 1000 } else { -1000 })
            .collect();
        let packets = st2110::packetize_pcm(&samples, 2, 48_000);
        assert_eq!(packets.len(), 1, "1 ms of stereo is one level-A packet");
        let mut acc = StAudio::default();
        let mut record = None;
        for packet in &packets {
            let (header, payload) = crate::carrier::RtpHeader::decode(&packet.rtp).unwrap();
            assert!(!header.marker);
            record = acc.push(header.marker, payload);
        }
        let record = record.expect("a full millisecond closes the record");
        assert_eq!(record[0], crate::negotiate::RECORD_DATA);
        let rate = u32::from_le_bytes(record[1..5].try_into().unwrap());
        let channels = u16::from_le_bytes([record[5], record[6]]);
        assert_eq!((rate, channels), (48_000, 2));
        let got: Vec<i16> = record[7..]
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(got, samples);
    }

    #[test]
    fn a_short_st2110_packet_does_not_become_one_channel() {
        // Fewer than 1 ms is not a level-A packet.
        let click = [596i16, 0, 0];
        let packets = st2110::packetize_pcm(&click, 2, 48_000);
        assert!(packets.is_empty());
        let mut acc = StAudio::default();
        assert!(acc.push(false, &click.iter().flat_map(|sample| sample.to_be_bytes()).collect::<Vec<_>>()).is_none());

        let samples = vec![0i16; 96];
        let packets = st2110::packetize_pcm(&samples, 2, 48_000);
        let (header, payload) = crate::carrier::RtpHeader::decode(&packets[0].rtp).unwrap();
        assert!(!header.marker);
        let record = acc
            .push(header.marker, payload)
            .expect("a full millisecond still closes");
        let channels = u16::from_le_bytes([record[5], record[6]]);
        assert_eq!(channels, 2);
        assert_eq!(record.len(), 7 + samples.len() * 2);
    }

    #[test]
    fn carriers_3_4_and_5_finish_on_udp_without_quic() {
        let _globals = crate::carrier::test_globals();
        let picture = b"picture-bytes-for-the-session".to_vec();
        let audio = b"audio-pcm".to_vec();
        let level_a: Vec<u8> = (0..48i16).flat_map(|sample| (1000 - sample).to_le_bytes()).collect();
        for id in [UDP_RTP, AVB, ST2110] {
            let before = crate::quic_binds();
            let sent_audio = if id == ST2110 { level_a.clone() } else { audio.clone() };
            let report = complete_session(id, &picture, &sent_audio);
            assert_eq!(report.quic_before, before, "carrier {id}");
            assert_eq!(report.quic_after, before, "carrier {id} opened QUIC");
            assert_eq!(report.picture, picture, "carrier {id} picture");
            if id == ST2110 {
                assert_eq!(report.audio, level_a);
                println!(
                    "st2110 run=1 quic_before={} quic_after={} picture_identical={} audio_identical={}",
                    report.quic_before,
                    report.quic_after,
                    report.picture == picture,
                    report.audio == level_a
                );
            } else {
                assert_eq!(report.audio, audio, "carrier {id} audio");
            }
            if id == AVB {
                assert_eq!(report.ethertype, avb::ETHERTYPE_AVTP);
                assert_eq!(report.subtype, avb::SUBTYPE_EXPERIMENTAL);
                assert_ne!(report.subtype, avb::SUBTYPE_CVF);
                assert_eq!(report.profile, "not encrypted");
                println!(
                    "avb ethertype={:#06x} subtype={:#x} picture_identical={} audio_identical={} quic_before={} quic_after={}",
                    report.ethertype,
                    report.subtype,
                    report.picture == picture,
                    report.audio == audio,
                    report.quic_before,
                    report.quic_after
                );
            }
            if id == UDP_RTP {
                assert_eq!(report.offers, 1, "host answered the first offer");
                assert_eq!(report.config_packets, 1);
                assert_eq!(report.profile, "gcm");
                assert!(report.eaku, "input socket returned EAKU");
                assert!(report.keys_sent >= 2, "key was retried");
                assert_eq!(report.pointer_sends, 1);
                assert!(report.loss > 0.01);
                assert!(report.cap < 1_000_000, "rtcp loss cut the cap");
                assert_ne!(rtp::session_key(), derive_aes128(&[0x71; 32]));
            }
            if id == ST2110 {
                assert_eq!(report.profile, "plain");
            }
        }
        let before = crate::quic_binds();
        let again = complete_session(ST2110, &picture, &level_a);
        assert_eq!(again.quic_before, before);
        assert_eq!(again.quic_after, before);
        assert_eq!(again.picture, picture);
        assert_eq!(again.audio, level_a);
        println!(
            "st2110 run=2 quic_before={} quic_after={} picture_identical={} audio_identical={}",
            again.quic_before,
            again.quic_after,
            again.picture == picture,
            again.audio == level_a
        );
    }
}
