//! ST 2110 essence packets.
//!
//! Uncompressed pictures are RFC 4175, at most one sample row per packet,
//! paced linearly across the frame. JPEG XS is an ST 2110-22 RTP payload
//! with the marker on the last packet. Audio is ST 2110-30 level A: 1 ms of
//! L16 PCM. Opus is not a picture or audio kind this crate announces.

use std::path::Path;
use std::time::Duration;

pub const RFC4175_WIDTH: u32 = 64;
pub const RFC4175_HEIGHT: u32 = 64;
pub const CLOCK_HZ: u32 = 90_000;
pub const PT_PICTURE: u8 = 96;
pub const PT_AUDIO: u8 = 97;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Plain,
    Gcm,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Plain => "plain",
            Profile::Gcm => "gcm",
        }
    }
}

/// `plain` unless the operator asked for the UDP/RTP exchange.
pub fn default_profile() -> Profile {
    Profile::Plain
}

/// Picture, audio, and input. RTCP is muxed on the essence port.
pub fn ports(l3_port: u16) -> (u16, u16, u16) {
    (
        l3_port,
        l3_port.saturating_add(2),
        l3_port.saturating_add(4),
    )
}

/// True when `bound` includes an essence port. A QUIC socket must not.
pub fn quic_overlaps(bound: &[u16], l3_port: u16) -> bool {
    let (picture, audio, input) = ports(l3_port);
    bound.contains(&picture) || bound.contains(&audio) || bound.contains(&input)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rfc4175Packet {
    pub row: u16,
    pub samples: Vec<u8>,
    /// RTP packet: 12-byte header plus one RFC 4175 sample-row header.
    pub rtp: Vec<u8>,
    pub marker: bool,
    pub timestamp: u32,
    pub send_at: Duration,
}

/// One decoded RFC 4175 sample row. 8-bit RGB uses a 3-octet pgroup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rfc4175Row {
    pub version: u8,
    pub sequence: u16,
    pub extended_sequence: u16,
    pub timestamp: u32,
    pub marker: bool,
    pub length: u16,
    pub field: bool,
    pub row: u16,
    pub continuation: bool,
    pub offset: u16,
    pub samples: Vec<u8>,
}

/// `samples` is packed 8-bit RGB, `width * height * 3` bytes.
/// Each packet carries one row. Every packet of the frame shares `timestamp`
/// when built by [`packetize_rfc4175_at`]. Send times still advance across
/// the frame. This entry starts at sequence 0 and timestamp 0.
pub fn packetize_rfc4175(
    samples: &[u8],
    width: u32,
    height: u32,
    frame_ticks: u32,
) -> Vec<Rfc4175Packet> {
    packetize_rfc4175_at(samples, width, height, frame_ticks, 0, 0)
}

/// Same as [`packetize_rfc4175`], continuing a 32-bit sequence and a 90 kHz
/// timestamp. The extended sequence number is the high 16 bits. The next
/// frame passes `sequence + height` and a later timestamp.
pub fn packetize_rfc4175_at(
    samples: &[u8],
    width: u32,
    height: u32,
    frame_ticks: u32,
    sequence: u32,
    timestamp: u32,
) -> Vec<Rfc4175Packet> {
    let width = width.max(1);
    let height = height.max(1);
    let row_bytes = width as usize * 3;
    let pace = frame_ticks.max(1);
    let frame_ns = u64::from(pace) * 1_000_000_000 / u64::from(CLOCK_HZ);
    let mut packets = Vec::with_capacity(height as usize);
    for row in 0..height {
        let start = row as usize * row_bytes;
        let slice = if start < samples.len() {
            let end = (start + row_bytes).min(samples.len());
            let mut row_samples = samples[start..end].to_vec();
            if row_samples.len() < row_bytes {
                row_samples.resize(row_bytes, 0);
            }
            row_samples
        } else {
            vec![0; row_bytes]
        };
        let send_at = Duration::from_nanos(u64::from(row) * frame_ns / u64::from(height));
        let marker = row + 1 == height;
        let seq = sequence.wrapping_add(row);
        let rtp = rtp_packet(
            marker,
            PT_PICTURE,
            seq as u16,
            timestamp,
            1,
            &rfc4175_payload((seq >> 16) as u16, false, row as u16, false, 0, &slice),
        );
        packets.push(Rfc4175Packet {
            row: row as u16,
            samples: slice,
            rtp,
            marker,
            timestamp,
            send_at,
        });
    }
    packets
}

/// RFC 4175 payload: extended sequence, then one sample-row header, then pixels.
/// `length` is the sample octets and is a multiple of the 3-octet pgroup.
pub fn rfc4175_payload(
    ext_seq: u16,
    field: bool,
    row: u16,
    continuation: bool,
    offset: u16,
    samples: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + samples.len());
    out.extend(ext_seq.to_be_bytes());
    out.extend((samples.len() as u16).to_be_bytes());
    let line = (u16::from(field) << 15) | (row & 0x7fff);
    let off = (u16::from(continuation) << 15) | (offset & 0x7fff);
    out.extend(line.to_be_bytes());
    out.extend(off.to_be_bytes());
    out.extend_from_slice(samples);
    out
}

/// Recover one sample row. `None` when the packet is not a valid 8-bit RGB row.
pub fn rfc4175_samples(rtp: &[u8]) -> Option<(u16, Vec<u8>, bool, u32)> {
    let row = decode_rfc4175(rtp).ok()?;
    Some((row.row, row.samples, row.marker, row.timestamp))
}

/// Parse an RFC 4175 packet that carries one 8-bit RGB sample row.
pub fn decode_rfc4175(rtp: &[u8]) -> Result<Rfc4175Row, &'static str> {
    if rtp.len() < 12 {
        return Err("truncated RTP header");
    }
    let version = rtp[0] >> 6;
    if version != 2 {
        return Err("RTP version is not 2");
    }
    if rtp.len() < 20 {
        return Err("truncated RFC 4175 header");
    }
    let marker = rtp[1] & 0x80 != 0;
    let sequence = u16::from_be_bytes([rtp[2], rtp[3]]);
    let timestamp = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
    let payload = &rtp[12..];
    let extended_sequence = u16::from_be_bytes([payload[0], payload[1]]);
    let length = u16::from_be_bytes([payload[2], payload[3]]);
    if payload.len() < 8 + usize::from(length) {
        return Err("sample-row length past the buffer");
    }
    if usize::from(length) % 3 != 0 {
        return Err("sample-row length is not a multiple of the pgroup");
    }
    let line = u16::from_be_bytes([payload[4], payload[5]]);
    let off = u16::from_be_bytes([payload[6], payload[7]]);
    Ok(Rfc4175Row {
        version,
        sequence,
        extended_sequence,
        timestamp,
        marker,
        length,
        field: line & 0x8000 != 0,
        row: line & 0x7fff,
        continuation: off & 0x8000 != 0,
        offset: off & 0x7fff,
        samples: payload[8..8 + usize::from(length)].to_vec(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XsPacket {
    pub payload: Vec<u8>,
    pub rtp: Vec<u8>,
    pub marker: bool,
    pub timestamp: u32,
    pub send_at: Duration,
}

/// RFC 9134 payload header. Codestream mode, progressive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XsFields {
    /// Transmission mode. 1 means packets are sent in order.
    pub t: bool,
    /// Packetization mode. 0 is codestream, 1 is slice.
    pub k: bool,
    /// Last packet of the packetization unit. Equals the RTP marker in codestream mode.
    pub l: bool,
    /// Interlace information. 0 is progressive.
    pub interlace: u8,
    /// Frame counter modulo 32.
    pub frame: u8,
    /// Slice and extended packet counter.
    pub sep: u16,
    /// Packet counter modulo 2048 within the frame.
    pub packet: u16,
}

impl XsFields {
    /// Index of this packet within the frame. SEP extends the packet counter.
    pub fn index(self) -> u32 {
        u32::from(self.sep) * 2048 + u32::from(self.packet)
    }
}

/// ST 2110-22 codestream mode, starting at sequence 0, timestamp 0, frame 0.
/// The RTP marker is set only on the last packet. Every packet of the frame
/// shares one 90 kHz timestamp.
pub fn packetize_jpeg_xs(frame: &[u8], frame_ticks: u32) -> Vec<XsPacket> {
    packetize_jpeg_xs_at(frame, frame_ticks, 0, 0, 0)
}

/// Same as [`packetize_jpeg_xs`], continuing the RTP sequence, the 90 kHz
/// timestamp, and the 5-bit frame counter. The next frame uses the next
/// sequence, a later timestamp, and `frame_counter + 1`.
pub fn packetize_jpeg_xs_at(
    frame: &[u8],
    frame_ticks: u32,
    sequence: u16,
    timestamp: u32,
    frame_counter: u8,
) -> Vec<XsPacket> {
    let chunks: Vec<&[u8]> = if frame.is_empty() {
        vec![b""]
    } else {
        frame.chunks(1200).collect()
    };
    let count = chunks.len().max(1);
    let pace = frame_ticks.max(1);
    let frame_ns = u64::from(pace) * 1_000_000_000 / u64::from(CLOCK_HZ);
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let marker = index + 1 == count;
            let send_at = Duration::from_nanos(index as u64 * frame_ns / count as u64);
            let mut payload = xs_header(marker, index as u32, frame_counter).to_vec();
            payload.extend_from_slice(chunk);
            let rtp = rtp_packet(
                marker,
                PT_PICTURE,
                sequence.wrapping_add(index as u16),
                timestamp,
                5,
                &payload,
            );
            XsPacket {
                payload,
                rtp,
                marker,
                timestamp,
                send_at,
            }
        })
        .collect()
}

/// RFC 9134 4-byte payload header for codestream mode.
///
/// `T` is 1, `K` is 0, `I` is 00. `L` is set on the last packet. The frame
/// counter is modulo 32. The packet counter is `packet_index` modulo 2048,
/// and the SEP counter counts how many times that counter has overrun.
pub fn xs_header(last: bool, packet_index: u32, frame: u8) -> [u8; 4] {
    let packet = packet_index % 2048;
    let sep = (packet_index / 2048) & 0x7ff;
    let word = (1u32 << 31)
        | (u32::from(last) << 29)
        | (u32::from(frame & 0x1f) << 22)
        | (sep << 11)
        | packet;
    word.to_be_bytes()
}

/// Read an RFC 9134 payload header. `None` when fewer than 4 bytes are present.
pub fn xs_fields(header: &[u8]) -> Option<XsFields> {
    if header.len() < 4 {
        return None;
    }
    let word = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    Some(XsFields {
        t: word >> 31 & 1 == 1,
        k: word >> 30 & 1 == 1,
        l: word >> 29 & 1 == 1,
        interlace: ((word >> 27) & 0x3) as u8,
        frame: ((word >> 22) & 0x1f) as u8,
        sep: ((word >> 11) & 0x7ff) as u16,
        packet: (word & 0x7ff) as u16,
    })
}

/// Join one JPEG XS frame from RTP payloads that each start with the 4-byte header.
///
/// `T=0` together with `K=0` is rejected. A `K` bit that is not the same on
/// every packet is rejected. The media bytes are returned in packet-counter order.
pub fn reassemble_jpeg_xs(payloads: &[&[u8]]) -> Result<Vec<u8>, &'static str> {
    if payloads.is_empty() {
        return Err("truncated JPEG XS header");
    }
    let mut k_seen = None;
    let mut frame = None;
    let mut parts: Vec<(u32, &[u8])> = Vec::with_capacity(payloads.len());
    for payload in payloads {
        let fields = xs_fields(payload).ok_or("truncated JPEG XS header")?;
        if !fields.t && !fields.k {
            return Err("JPEG XS T=0 and K=0");
        }
        if let Some(k) = k_seen {
            if k != fields.k {
                return Err("JPEG XS K bit changes inside the stream");
            }
        } else {
            k_seen = Some(fields.k);
        }
        if let Some(open) = frame {
            if open != fields.frame {
                return Err("JPEG XS frame counter changes inside the frame");
            }
        } else {
            frame = Some(fields.frame);
        }
        parts.push((fields.index(), &payload[4..]));
    }
    parts.sort_by_key(|(index, _)| *index);
    for (n, (index, _)) in parts.iter().enumerate() {
        if *index != n as u32 {
            return Err("JPEG XS packet counter is not the index within the frame");
        }
    }
    let mut out = Vec::new();
    for (_, bytes) in parts {
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcmPacket {
    pub rtp: Vec<u8>,
    pub frames: usize,
    pub timestamp: u32,
    pub send_at: Duration,
    pub marker: bool,
}

/// ST 2110-30 level A. 48 kHz, 1 to 8 channels, 1 ms packet time, interleaved
/// big-endian L16. The marker bit is 0. The 48 kHz timestamp increases by 48.
///
/// A rate other than 48 kHz, a channel count outside 1..=8, or a buffer that
/// is not a whole number of 1 ms packets produces no packets.
pub fn packetize_pcm(samples: &[i16], channels: u16, rate: u32) -> Vec<PcmPacket> {
    if rate != 48_000 || !(1..=8).contains(&channels) {
        return Vec::new();
    }
    let channels = usize::from(channels);
    let stride = 48 * channels;
    if samples.is_empty() || samples.len() % stride != 0 {
        return Vec::new();
    }
    samples
        .chunks(stride)
        .enumerate()
        .map(|(index, chunk)| {
            let mut body = Vec::with_capacity(chunk.len() * 2);
            for sample in chunk {
                body.extend(sample.to_be_bytes());
            }
            let timestamp = (index as u32).saturating_mul(48);
            let rtp = rtp_packet(false, PT_AUDIO, index as u16, timestamp, 30, &body);
            PcmPacket {
                rtp,
                frames: chunk.len() / channels,
                timestamp,
                send_at: Duration::from_millis(index as u64),
                marker: false,
            }
        })
        .collect()
}

/// Samples from one level-A packet. The marker bit must be 0, and the payload
/// must be exactly `channels × 48 × 2` octets for a channel count in 1..=8.
pub fn decode_pcm(rtp: &[u8]) -> Result<(u16, Vec<i16>, u32), &'static str> {
    if rtp.len() < 12 {
        return Err("truncated RTP header");
    }
    if rtp[0] >> 6 != 2 {
        return Err("RTP version is not 2");
    }
    if rtp[1] & 0x80 != 0 {
        return Err("level A marker is not 0");
    }
    let timestamp = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
    let payload = &rtp[12..];
    if payload.len() % 2 != 0 {
        return Err("level A payload is not a whole number of samples");
    }
    let samples = payload.len() / 2;
    if samples < 48 || samples % 48 != 0 {
        return Err("level A payload is not one millisecond");
    }
    let channels = samples / 48;
    if !(1..=8).contains(&channels) {
        return Err("level A channel count is outside 1..=8");
    }
    let pcm = payload
        .chunks_exact(2)
        .map(|pair| i16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    Ok((channels as u16, pcm, timestamp))
}

pub fn rtp_packet(marker: bool, pt: u8, sequence: u16, timestamp: u32, ssrc: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    out.push(0x80);
    out.push((u8::from(marker) << 7) | (pt & 0x7f));
    out.extend(sequence.to_be_bytes());
    out.extend(timestamp.to_be_bytes());
    out.extend(ssrc.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// A PTP device node means the NIC or the OS has a clock to follow.
pub fn ptp_follower_present() -> bool {
    Path::new("/dev/ptp0").exists() || Path::new("/dev/ptp1").exists()
}

/// Software clock when no PTP node is present. The UI then says the stream
/// is not locked.
pub fn clock_locked(ptp_present: bool) -> bool {
    ptp_present
}

/// Drop the frame in hand when the send is more than one frame behind.
pub fn drop_late_frame(behind: Duration, frame: Duration) -> bool {
    behind > frame
}

pub fn row_label(locked: bool, profile: Profile) -> String {
    if locked {
        format!("ST 2110 ({})", profile.name())
    } else {
        format!("ST 2110 ({}), not locked", profile.name())
    }
}

/// Opus is not this carrier. Uncompressed and JPEG XS are.
pub fn picture_allowed(kind: &str) -> bool {
    matches!(kind, "uncompressed" | "rfc4175" | "jpeg-xs" | "jpegxs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_64x64_raster_is_real_rfc4175_and_not_one_burst() {
        let mut samples = vec![0u8; 64 * 64 * 3];
        for (i, byte) in samples.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let packets = packetize_rfc4175(&samples, 64, 64, 1500);
        assert_eq!(packets.len(), 64);
        let mut back = vec![0u8; samples.len()];
        let mut times = Vec::new();
        for packet in &packets {
            let (row, row_samples, marker, timestamp) = rfc4175_samples(&packet.rtp).unwrap();
            assert_eq!(row, packet.row);
            assert_eq!(row_samples, packet.samples);
            assert_eq!(marker, packet.marker);
            assert_eq!(timestamp, packet.timestamp);
            assert_eq!(row_samples.len(), 64 * 3);
            let start = row as usize * 64 * 3;
            back[start..start + row_samples.len()].copy_from_slice(&row_samples);
            times.push(packet.send_at);
        }
        assert_eq!(back, samples);
        assert!(packets.last().unwrap().marker);
        assert!(packets.iter().all(|packet| packet.timestamp == packets[0].timestamp));
        assert!(times.windows(2).all(|pair| pair[1] > pair[0]));
        assert_ne!(times[0], times[63]);
        let view = decode_rfc4175(&packets[0].rtp).unwrap();
        assert_eq!(view.version, 2);
        assert_eq!(view.sequence, 0);
        assert_eq!(view.extended_sequence, 0);
        assert!(!view.field);
        assert!(!view.continuation);
        assert_eq!(view.offset, 0);
        assert_eq!(view.length as usize, 64 * 3);
        assert_eq!(usize::from(view.length) % 3, 0);

        let again = packetize_rfc4175_at(&samples, 64, 64, 1500, 64, 1500);
        assert!(again.iter().all(|packet| packet.timestamp == 1500));
        assert_eq!(decode_rfc4175(&again[0].rtp).unwrap().sequence, 64);
        assert!(again[0].timestamp > packets[0].timestamp);
    }

    #[test]
    fn rfc4175_sequence_wraps_and_bad_packets_are_rejected() {
        let samples = vec![4u8; 2 * 3];
        let packets = packetize_rfc4175_at(&samples, 1, 2, 1500, 65_535, 9_000);
        assert_eq!(packets.len(), 2);
        let first = decode_rfc4175(&packets[0].rtp).unwrap();
        let second = decode_rfc4175(&packets[1].rtp).unwrap();
        assert_eq!(first.sequence, 65_535);
        assert_eq!(first.extended_sequence, 0);
        assert_eq!(second.sequence, 0);
        assert_eq!(second.extended_sequence, 1);
        assert_eq!(first.timestamp, second.timestamp);
        assert!(!first.marker);
        assert!(second.marker);
        assert_eq!(first.samples, samples[..3]);
        assert_eq!(second.samples, samples[3..]);
        println!(
            "rfc4175 version={} seq0={} ext0={} seq1={} ext1={} timestamp={} marker_last={} length={}",
            first.version,
            first.sequence,
            first.extended_sequence,
            second.sequence,
            second.extended_sequence,
            first.timestamp,
            u8::from(second.marker),
            first.length
        );

        let mut version = packets[0].rtp.clone();
        version[0] = (version[0] & 0x3f) | (1 << 6);
        assert_eq!(decode_rfc4175(&version), Err("RTP version is not 2"));
        assert_eq!(decode_rfc4175(&packets[0].rtp[..11]), Err("truncated RTP header"));
        assert_eq!(decode_rfc4175(&packets[0].rtp[..16]), Err("truncated RFC 4175 header"));
        let mut past = packets[0].rtp.clone();
        past[14] = 0xff;
        past[15] = 0xff;
        assert_eq!(decode_rfc4175(&past), Err("sample-row length past the buffer"));
        let mut pgroup = packets[0].rtp.clone();
        pgroup[14] = 0;
        pgroup[15] = 2;
        assert_eq!(
            decode_rfc4175(&pgroup),
            Err("sample-row length is not a multiple of the pgroup")
        );
    }

    #[test]
    fn jpeg_xs_codestream_mode_and_level_a_follow_the_specs() {
        let frame = vec![9u8; 3000];
        let packets = packetize_jpeg_xs_at(&frame, 1500, 7, 90_000, 31);
        assert!(packets.len() > 1);
        assert!(packets.iter().all(|packet| packet.timestamp == 90_000));
        assert!(packets.last().unwrap().marker);
        assert!(packets[..packets.len() - 1].iter().all(|packet| !packet.marker));
        assert_ne!(packets[0].send_at, packets.last().unwrap().send_at);
        for (index, packet) in packets.iter().enumerate() {
            let fields = xs_fields(&packet.payload).unwrap();
            assert!(fields.t);
            assert!(!fields.k);
            assert_eq!(fields.l, packet.marker);
            assert_eq!(fields.interlace, 0);
            assert_eq!(fields.frame, 31);
            assert_eq!(fields.index(), index as u32);
            assert_eq!(fields.sep, 0);
            assert_eq!(packet.rtp[0] >> 6, 2);
            assert_eq!(packet.rtp[1] & 0x80 != 0, packet.marker);
        }
        let payloads: Vec<&[u8]> = packets.iter().map(|packet| packet.payload.as_slice()).collect();
        assert_eq!(reassemble_jpeg_xs(&payloads).unwrap(), frame);

        let next = packetize_jpeg_xs_at(&frame, 1500, packets.len() as u16, 90_000 + 1500, 32);
        assert!(next.iter().all(|packet| packet.timestamp == 91_500));
        assert!(next[0].timestamp > packets[0].timestamp);
        assert_eq!(xs_fields(&next[0].payload).unwrap().frame, 0);
        assert_eq!(u16::from_be_bytes([next[0].rtp[2], next[0].rtp[3]]), packets.len() as u16);

        let overrun = xs_fields(&xs_header(false, 2048, 1)).unwrap();
        assert_eq!((overrun.sep, overrun.packet, overrun.frame), (1, 0, 1));
        assert!(overrun.t && !overrun.k && !overrun.l);

        let mut unordered = packets[0].payload.clone();
        unordered[0] &= !0xc0;
        assert_eq!(
            reassemble_jpeg_xs(&[unordered.as_slice()]),
            Err("JPEG XS T=0 and K=0")
        );
        let mut slice_mode = packets[0].payload.clone();
        slice_mode[0] |= 0x40;
        assert_eq!(
            reassemble_jpeg_xs(&[packets[0].payload.as_slice(), slice_mode.as_slice()]),
            Err("JPEG XS K bit changes inside the stream")
        );

        let stereo = vec![7i16; 48 * 2];
        let pcm = packetize_pcm(&stereo, 2, 48_000);
        assert_eq!(pcm.len(), 1);
        assert_eq!(pcm[0].frames, 48);
        assert!(!pcm[0].marker);
        assert_eq!(pcm[0].rtp.len() - 12, 2 * 48 * 2);
        assert_eq!(pcm[0].rtp[1] & 0x80, 0);
        let (channels, samples, timestamp) = decode_pcm(&pcm[0].rtp).unwrap();
        assert_eq!((channels, timestamp), (2, 0));
        assert_eq!(samples, stereo);

        let two_ms = vec![1i16; 48 * 2 * 2];
        let pcm = packetize_pcm(&two_ms, 2, 48_000);
        assert_eq!(pcm.len(), 2);
        let ts_step = pcm[1].timestamp - pcm[0].timestamp;
        assert_eq!(ts_step, 48);
        assert!(pcm.iter().all(|packet| !packet.marker));
        let level_a_marker = pcm[0].marker;
        let mono = vec![3i16; 48];
        let pcm = packetize_pcm(&mono, 1, 48_000);
        assert_eq!(pcm.len(), 1);
        assert_eq!(decode_pcm(&pcm[0].rtp).unwrap().0, 1);
        let wide = vec![4i16; 48 * 8];
        let pcm = packetize_pcm(&wide, 8, 48_000);
        assert_eq!(pcm.len(), 1);
        assert_eq!(pcm[0].rtp.len() - 12, 8 * 48 * 2);
        assert_eq!(decode_pcm(&pcm[0].rtp).unwrap().1, wide);

        assert!(packetize_pcm(&stereo, 2, 44_100).is_empty());
        assert!(packetize_pcm(&stereo, 0, 48_000).is_empty());
        assert!(packetize_pcm(&stereo, 9, 48_000).is_empty());
        assert!(packetize_pcm(&[1, 2, 3, 4], 2, 48_000).is_empty());

        assert!(!quic_overlaps(&[9], 5000));
        assert!(quic_overlaps(&[5004], 5000));
        assert!(drop_late_frame(Duration::from_millis(40), Duration::from_millis(16)));
        assert!(!drop_late_frame(Duration::from_millis(10), Duration::from_millis(16)));
        assert_eq!(default_profile(), Profile::Plain);
        assert!(!picture_allowed("opus"));
        assert!(picture_allowed("jpeg-xs"));
        assert!(row_label(false, Profile::Plain).contains("not locked"));
        let open = xs_fields(&packets[0].payload).unwrap();
        let last = xs_fields(packets.last().unwrap().payload.as_slice()).unwrap();
        println!(
            "jpegxs t={} k={} l_open={} l_last={} frame={} next_frame={} timestamp={} next_timestamp={} level_a_octets={} marker={} ts_step={}",
            u8::from(open.t),
            u8::from(open.k),
            u8::from(open.l),
            u8::from(last.l),
            open.frame,
            xs_fields(&next[0].payload).unwrap().frame,
            packets[0].timestamp,
            next[0].timestamp,
            stereo.len() * 2,
            u8::from(level_a_marker),
            ts_step
        );
    }
}
