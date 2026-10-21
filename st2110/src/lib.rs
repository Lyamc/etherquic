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

/// `samples` is packed 8-bit RGB, `width * height * 3` bytes.
/// Each packet carries one row. Timestamps and send times span the frame.
pub fn packetize_rfc4175(
    samples: &[u8],
    width: u32,
    height: u32,
    frame_ticks: u32,
) -> Vec<Rfc4175Packet> {
    let width = width.max(1);
    let height = height.max(1);
    let row_bytes = width as usize * 3;
    let frame_ticks = frame_ticks.max(1);
    let frame_ns = u64::from(frame_ticks) * 1_000_000_000 / u64::from(CLOCK_HZ);
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
        let timestamp = (u64::from(row) * u64::from(frame_ticks) / u64::from(height)) as u32;
        let send_at = Duration::from_nanos(
            u64::from(row) * frame_ns / u64::from(height),
        );
        let marker = row + 1 == height;
        let rtp = rtp_packet(
            marker,
            PT_PICTURE,
            row as u16,
            timestamp,
            1,
            &rfc4175_payload(row as u16, row as u16, &slice),
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
pub fn rfc4175_payload(ext_seq: u16, row: u16, samples: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + samples.len());
    out.extend(ext_seq.to_be_bytes());
    out.extend((samples.len() as u16).to_be_bytes());
    out.extend(row.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend_from_slice(samples);
    out
}

/// Recover the sample bytes from an RFC 4175 RTP packet. `None` when the
/// header is not one row at offset 0.
pub fn rfc4175_samples(rtp: &[u8]) -> Option<(u16, Vec<u8>, bool, u32)> {
    if rtp.len() < 20 || rtp[0] >> 6 != 2 {
        return None;
    }
    let marker = rtp[1] & 0x80 != 0;
    let timestamp = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
    let payload = &rtp[12..];
    if payload.len() < 8 {
        return None;
    }
    let length = u16::from_be_bytes([payload[2], payload[3]]) as usize;
    let row = u16::from_be_bytes([payload[4], payload[5]]) & 0x7fff;
    let offset = u16::from_be_bytes([payload[6], payload[7]]) & 0x7fff;
    if offset != 0 || payload.len() < 8 + length {
        return None;
    }
    Some((row, payload[8..8 + length].to_vec(), marker, timestamp))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XsPacket {
    pub payload: Vec<u8>,
    pub rtp: Vec<u8>,
    pub marker: bool,
    pub timestamp: u32,
    pub send_at: Duration,
}

/// ST 2110-22. The RTP marker is set only on the last packet. The payload
/// starts with the 4-byte JPEG XS header (`K` and `L` set on that last packet).
pub fn packetize_jpeg_xs(frame: &[u8], frame_ticks: u32) -> Vec<XsPacket> {
    let chunks: Vec<&[u8]> = if frame.is_empty() {
        vec![b""]
    } else {
        frame.chunks(1200).collect()
    };
    let count = chunks.len().max(1);
    let frame_ticks = frame_ticks.max(1);
    let frame_ns = u64::from(frame_ticks) * 1_000_000_000 / u64::from(CLOCK_HZ);
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let marker = index + 1 == count;
            let timestamp = (u64::from(index as u32) * u64::from(frame_ticks) / count as u64) as u32;
            let send_at = Duration::from_nanos(index as u64 * frame_ns / count as u64);
            let mut payload = xs_header(marker, index as u16).to_vec();
            payload.extend_from_slice(chunk);
            let rtp = rtp_packet(marker, PT_PICTURE, index as u16, timestamp, 5, &payload);
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

/// ST 2110-22 payload header. `K` and `L` are set on the last packet of the frame.
pub fn xs_header(last: bool, index: u16) -> [u8; 4] {
    let k = u8::from(last);
    let l = u8::from(last);
    let mut hdr = [0u8; 4];
    hdr[0] = (k << 6) | (l << 5);
    hdr[1] = (index >> 8) as u8;
    hdr[2] = index as u8;
    hdr
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcmPacket {
    pub rtp: Vec<u8>,
    pub frames: usize,
    pub timestamp: u32,
    pub send_at: Duration,
}

/// ST 2110-30 level A. Each packet is 1 ms of interleaved big-endian L16.
pub fn packetize_pcm(samples: &[i16], channels: u16, rate: u32) -> Vec<PcmPacket> {
    let channels = usize::from(channels.max(1));
    let per_ms = (rate as usize / 1000).max(1);
    let stride = per_ms * channels;
    if samples.is_empty() {
        return Vec::new();
    }
    let chunks: Vec<&[i16]> = samples.chunks(stride).collect();
    let count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let mut body = Vec::with_capacity(chunk.len() * 2);
            for sample in chunk {
                body.extend(sample.to_be_bytes());
            }
            let timestamp = (index as u32).saturating_mul(per_ms as u32);
            let marker = index + 1 == count;
            let rtp = rtp_packet(marker, PT_AUDIO, index as u16, timestamp, 30, &body);
            PcmPacket {
                rtp,
                frames: chunk.len() / channels,
                timestamp,
                send_at: Duration::from_millis(index as u64),
            }
        })
        .collect()
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
        assert!(packets.last().unwrap().timestamp > packets[0].timestamp);
        assert!(times.windows(2).all(|pair| pair[1] > pair[0]));
        assert_ne!(times[0], times[63]);
    }

    #[test]
    fn jpeg_xs_marks_the_last_packet_and_pcm_is_one_millisecond() {
        let frame = vec![9u8; 3000];
        let packets = packetize_jpeg_xs(&frame, 1500);
        assert!(packets.len() > 1);
        assert!(packets.last().unwrap().marker);
        assert!(packets[..packets.len() - 1].iter().all(|packet| !packet.marker));
        assert_eq!(packets.last().unwrap().payload[0] & 0x60, 0x60);
        assert_ne!(packets[0].send_at, packets.last().unwrap().send_at);
        let pcm = packetize_pcm(&[1i16, 2, 3, 4], 2, 48_000);
        assert_eq!(pcm.len(), 1);
        assert_eq!(pcm[0].frames, 2);
        assert!(!quic_overlaps(&[9], 5000));
        assert!(quic_overlaps(&[5004], 5000));
        assert!(drop_late_frame(Duration::from_millis(40), Duration::from_millis(16)));
        assert!(!drop_late_frame(Duration::from_millis(10), Duration::from_millis(16)));
        assert_eq!(default_profile(), Profile::Plain);
        assert!(!picture_allowed("opus"));
        assert!(picture_allowed("jpeg-xs"));
        assert!(row_label(false, Profile::Plain).contains("not locked"));
    }
}
