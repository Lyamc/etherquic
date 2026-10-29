//! ST 2110 picture pacing. Uncompressed video is RFC 4175, one row per packet.
//! JPEG XS sets the marker on the last packet of the frame. A packet that
//! arrives after the next frame has started is dropped.

use std::time::Duration;

pub const RFC4175_WIDTH: u32 = 64;
pub const RFC4175_HEIGHT: u32 = 64;

#[derive(Clone, Debug)]
pub struct Rfc4175Packet {
    pub row: u16,
    pub samples: Vec<u8>,
    /// RTP packet built by the `st2110` crate.
    pub rtp: Vec<u8>,
    pub marker: bool,
    pub timestamp: u32,
    pub send_at: Duration,
}

/// `samples` is packed RGB, `width * height * 3` bytes. Packets are RFC 4175
/// from the `st2110` crate: one row each, one timestamp for the frame, and
/// send times that are not one burst.
pub fn packetize_rfc4175(samples: &[u8], width: u32, height: u32, frame_ticks: u32) -> Vec<Rfc4175Packet> {
    st2110::packetize_rfc4175(samples, width, height, frame_ticks)
        .into_iter()
        .map(|packet| Rfc4175Packet {
            row: packet.row,
            samples: packet.samples,
            rtp: packet.rtp,
            marker: packet.marker,
            timestamp: packet.timestamp,
            send_at: packet.send_at,
        })
        .collect()
}

/// One JPEG XS RTP payload. The marker is set only on the last packet of the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XsPacket {
    /// Media bytes, without the 4-byte JPEG XS header.
    pub payload: Vec<u8>,
    pub rtp: Vec<u8>,
    pub marker: bool,
    pub timestamp: u32,
    pub send_at: Duration,
}

/// ST 2110-22 from the `st2110` crate. `payload` is the media bytes. The RTP
/// packet the crate builds carries the JPEG XS header and the marker.
pub fn packetize_jpeg_xs(frame: &[u8], frame_ticks: u32) -> Vec<XsPacket> {
    packetize_jpeg_xs_at(frame, frame_ticks, 0, 0, 0)
}

/// Same packets as [`packetize_jpeg_xs`], continuing the RTP sequence, the
/// 90 kHz timestamp, and the frame counter. GCM nonces include that sequence.
pub fn packetize_jpeg_xs_at(
    frame: &[u8],
    frame_ticks: u32,
    sequence: u16,
    timestamp: u32,
    frame_counter: u8,
) -> Vec<XsPacket> {
    st2110::packetize_jpeg_xs_at(frame, frame_ticks, sequence, timestamp, frame_counter)
        .into_iter()
        .map(|packet| XsPacket {
            payload: packet.payload.get(4..).unwrap_or_default().to_vec(),
            rtp: packet.rtp,
            marker: packet.marker,
            timestamp: packet.timestamp,
            send_at: packet.send_at,
        })
        .collect()
}

/// JPEG XS frames. `frame` is the frame index. The first packet of the next
/// frame closes the previous one: anything older is dropped.
#[derive(Debug, Default)]
pub struct XsAssembler {
    open_frame: Option<u32>,
    parts: Vec<u8>,
    saw_next: bool,
}

impl XsAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, frame: u32, first: bool, last: bool, bytes: &[u8]) -> Option<Vec<u8>> {
        if let Some(open) = self.open_frame {
            if frame != open {
                if first && frame == open.wrapping_add(1) {
                    self.saw_next = true;
                    self.open_frame = Some(frame);
                    self.parts = bytes.to_vec();
                } else if self.saw_next || frame != open {
                    return None;
                }
            } else if self.saw_next && frame != open {
                return None;
            } else {
                self.parts.extend_from_slice(bytes);
            }
        } else if first {
            self.open_frame = Some(frame);
            self.parts = bytes.to_vec();
        } else {
            return None;
        }
        if last && self.open_frame == Some(frame) {
            self.saw_next = false;
            let done = std::mem::take(&mut self.parts);
            return Some(done);
        }
        None
    }

    /// A late packet from `frame` after `next_started` is dropped.
    pub fn accept_late(frame: u32, newest_started: u32) -> bool {
        frame == newest_started
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_64x64_raster_covers_every_sample_once_and_is_not_one_burst() {
        let mut samples = vec![0u8; 64 * 64 * 3];
        for (i, byte) in samples.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let packets = packetize_rfc4175(&samples, RFC4175_WIDTH, RFC4175_HEIGHT, 90_000);
        assert_eq!(packets.len(), RFC4175_HEIGHT as usize);
        assert!(packets.last().unwrap().marker);
        assert!(!packets[..63].iter().any(|packet| packet.marker));
        let mut back = vec![0u8; samples.len()];
        for packet in &packets {
            let start = packet.row as usize * 64 * 3;
            back[start..start + packet.samples.len()].copy_from_slice(&packet.samples);
        }
        assert_eq!(back, samples);
        let first = packets.first().unwrap().timestamp;
        let last = packets.last().unwrap().timestamp;
        assert_eq!(last, first);
        assert_ne!(packets[0].send_at, packets[63].send_at);
        let ticks: Vec<u32> = packets.iter().map(|packet| packet.timestamp).collect();
        assert!(ticks.windows(2).all(|pair| pair[1] == pair[0]));
    }

    #[test]
    fn jpeg_xs_sets_the_marker_on_the_last_packet_only() {
        let frame = vec![3u8; 3000];
        let packets = packetize_jpeg_xs(&frame, 90_000);
        assert!(packets.len() > 1);
        assert!(packets.last().unwrap().marker);
        assert!(!packets[..packets.len() - 1].iter().any(|packet| packet.marker));
        assert_ne!(packets[0].send_at, packets.last().unwrap().send_at);
        let mut back = Vec::new();
        for packet in &packets {
            back.extend_from_slice(&packet.payload);
        }
        assert_eq!(back, frame);
    }

    #[test]
    fn a_packet_after_the_next_frame_starts_is_dropped() {
        let mut asm = XsAssembler::new();
        assert!(asm.push(0, true, false, b"aa").is_none());
        assert_eq!(asm.push(0, false, true, b"bb").as_deref(), Some(b"aabb".as_slice()));
        assert!(asm.push(1, true, false, b"cc").is_none());
        assert!(asm.push(0, false, true, b"late").is_none());
        assert!(!XsAssembler::accept_late(0, 1));
        assert!(XsAssembler::accept_late(1, 1));
    }
}
