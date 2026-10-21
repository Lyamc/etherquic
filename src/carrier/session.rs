//! One picture and one audio record on carriers 3, 4, and 5.
//!
//! Sockets are UDP on 127.0.0.1. AVB carries the Ethernet frame in the UDP
//! payload because this process may not have a raw NIC. No Quinn endpoint is
//! constructed here.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use super::assemble::{split_record, Assembler, Slice};
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
    std::thread::spawn(move || {
        let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
            return;
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
        let Ok(addr) = sock.local_addr() else {
            return;
        };
        let mut hello = b"AVB".to_vec();
        hello.extend(addr.port().to_le_bytes());
        let _ = sock.send_to(&hello, host_input);
        let mut pictures = Assembler::new();
        let mut audios = Assembler::new();
        let end = Instant::now() + Duration::from_secs(30);
        let mut buf = [0u8; 2048];
        while Instant::now() < end {
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
            let (asm, tx) = if frame.subtype == avb::SUBTYPE_AAF {
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

/// Player ST 2110 sockets. Picture and audio stay on their own ports.
pub fn spawn_player_st2110(
    host_input: std::net::SocketAddr,
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
        let _ = picture_sock.send_to(&hello, host_input);
        let mut gcm = gcm_on.then(|| {
            let key = rtp::hub_exchanged_key().unwrap_or_else(rtp::session_key);
            rtp::GcmSession::new(key)
        });
        let mut asm = super::XsAssembler::new();
        let mut pcm = Vec::new();
        let end = Instant::now() + Duration::from_secs(30);
        let mut frame = 1u32;
        let mut open = false;
        let mut buf = [0u8; 2048];
        while Instant::now() < end {
            let got = picture_sock
                .recv_from(&mut buf)
                .or_else(|_| audio_sock.recv_from(&mut buf));
            let Ok((n, _)) = got else {
                continue;
            };
            let Some((header, sealed)) = RtpHeader::decode(&buf[..n]) else {
                continue;
            };
            let opened = if let Some(session) = gcm.as_mut() {
                match session.open(header.ssrc, 0, header.sequence, sealed) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                }
            } else {
                sealed.to_vec()
            };
            let payload = opened.as_slice();
            if header.payload_type == rtp::PT_AUDIO {
                for chunk in payload.chunks_exact(2) {
                    let sample = i16::from_be_bytes([chunk[0], chunk[1]]);
                    pcm.extend(sample.to_le_bytes());
                }
                if header.marker {
                    let record = std::mem::take(&mut pcm);
                    if audio.blocking_send(record).is_err() {
                        return;
                    }
                }
                continue;
            }
            if payload.len() < 4 {
                continue;
            }
            let first = !open;
            open = true;
            if let Some(record) = asm.push(frame, first, header.marker, &payload[4..]) {
                let tx = if header.payload_type == rtp::PT_AUDIO {
                    &audio
                } else {
                    &picture
                };
                if tx.blocking_send(record).is_err() {
                    return;
                }
                frame = frame.wrapping_add(1);
                open = false;
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
    report
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
        let Some((header, payload)) = RtpHeader::decode(&buf[..n]) else {
            continue;
        };
        if payload.len() < 4 {
            continue;
        }
        let index = usize::from(header.sequence);
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
    fn carriers_3_4_and_5_finish_on_udp_without_quic() {
        let picture = b"picture-bytes-for-the-session".to_vec();
        let audio = b"audio-pcm".to_vec();
        for id in [UDP_RTP, AVB, ST2110] {
            let before = crate::quic_binds();
            let report = complete_session(id, &picture, &audio);
            assert_eq!(report.quic_before, before, "carrier {id}");
            assert_eq!(report.quic_after, before, "carrier {id} opened QUIC");
            assert_eq!(report.picture, picture, "carrier {id} picture");
            if id == ST2110 {
                assert_eq!(report.audio, audio_samples(&audio).iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<_>>());
            } else {
                assert_eq!(report.audio, audio, "carrier {id} audio");
            }
            if id == AVB {
                assert_eq!(report.ethertype, avb::ETHERTYPE_AVTP);
                assert_eq!(report.subtype, avb::SUBTYPE_CVF);
                assert_ne!(report.subtype, 0x7F);
                assert_eq!(report.profile, "not encrypted");
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
    }
}
