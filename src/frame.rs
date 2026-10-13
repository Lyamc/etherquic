//! Experimental QUIC-over-Ethernet frame codec (borderless-inspired L2).
//!
//! Layout:
//! ```text
//! dst MAC (6) | src MAC (6) | EtherType (2)
//! | dst_service (2) | src_service (2) | payload_len (2) | QUIC datagram
//! ```
//!
//! `payload_len` is required because NICs pad frames to the 60-byte Ethernet
//! minimum; without it, short QUIC ACKs absorb padding and fail AEAD.

use crate::Mac;

/// Locally administered experimental EtherType (borderless uses `0x88B5`).
pub const DEFAULT_ETHER_TYPE: u16 = 0x88B6;

pub const ETHER_HEADER_LEN: usize = 14;
pub const SERVICE_HEADER_LEN: usize = 6; // dst_svc + src_svc + payload_len
pub const ADAPTER_OVERHEAD: usize = ETHER_HEADER_LEN + SERVICE_HEADER_LEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub dst: Mac,
    pub src: Mac,
    pub ether_type: u16,
    pub dst_service: u16,
    pub src_service: u16,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("frame shorter than Ethernet + service header")]
    Truncated,
    #[error("unexpected EtherType {found:#06x}, expected {expected:#06x}")]
    WrongEtherType { expected: u16, found: u16 },
    #[error("payload_len {declared} exceeds remaining {remaining}")]
    BadPayloadLen { declared: usize, remaining: usize },
}

fn mac_from(bytes: &[u8]) -> Option<(Mac, &[u8])> {
    let (head, rest) = bytes.split_at_checked(6)?;
    Some((head.try_into().ok()?, rest))
}

fn u16_from(bytes: &[u8]) -> Option<(u16, &[u8])> {
    let (head, rest) = bytes.split_at_checked(2)?;
    Some((u16::from_be_bytes(head.try_into().ok()?), rest))
}

/// Pure encode: header fields then payload, no mutation of `frame`.
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let len = u16::try_from(frame.payload.len()).unwrap_or(u16::MAX);
    frame
        .dst
        .into_iter()
        .chain(frame.src)
        .chain(frame.ether_type.to_be_bytes())
        .chain(frame.dst_service.to_be_bytes())
        .chain(frame.src_service.to_be_bytes())
        .chain(len.to_be_bytes())
        .chain(frame.payload.iter().copied().take(len as usize))
        .collect()
}

/// Pure decode. Ethernet padding past `payload_len` is ignored.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame, FrameError> {
    let (dst, rest) = mac_from(bytes).ok_or(FrameError::Truncated)?;
    let (src, rest) = mac_from(rest).ok_or(FrameError::Truncated)?;
    let (ether_type, rest) = u16_from(rest).ok_or(FrameError::Truncated)?;
    let (dst_service, rest) = u16_from(rest).ok_or(FrameError::Truncated)?;
    let (src_service, rest) = u16_from(rest).ok_or(FrameError::Truncated)?;
    let (payload_len, rest) = u16_from(rest).ok_or(FrameError::Truncated)?;
    let payload_len = payload_len as usize;
    if payload_len > rest.len() {
        return Err(FrameError::BadPayloadLen {
            declared: payload_len,
            remaining: rest.len(),
        });
    }
    Ok(Frame {
        dst,
        src,
        ether_type,
        dst_service,
        src_service,
        payload: rest[..payload_len].to_vec(),
    })
}

pub fn decode_frame_filtered(bytes: &[u8], expected_ether_type: u16) -> Result<Frame, FrameError> {
    decode_frame(bytes).and_then(|frame| {
        if frame.ether_type == expected_ether_type {
            Ok(frame)
        } else {
            Err(FrameError::WrongEtherType {
                expected: expected_ether_type,
                found: frame.ether_type,
            })
        }
    })
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        encode_frame(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        decode_frame(bytes)
    }

    pub fn decode_filtered(bytes: &[u8], expected_ether_type: u16) -> Result<Self, FrameError> {
        decode_frame_filtered(bytes, expected_ether_type)
    }
}

/// Maximum QUIC datagram size for an interface MTU (Ethernet payload capacity).
pub fn max_quic_datagram(interface_mtu: usize) -> usize {
    interface_mtu.saturating_sub(SERVICE_HEADER_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let frame = Frame {
            dst: [0x02, 0, 0, 0, 0, 2],
            src: [0x02, 0, 0, 0, 0, 1],
            ether_type: DEFAULT_ETHER_TYPE,
            dst_service: 4433,
            src_service: 1234,
            payload: b"quic-bytes".to_vec(),
        };
        let encoded = frame.encode();
        assert_eq!(encoded.len(), ADAPTER_OVERHEAD + 10);
        assert_eq!(Frame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn strips_ethernet_padding() {
        let frame = Frame {
            dst: [0x02, 0, 0, 0, 0, 2],
            src: [0x02, 0, 0, 0, 0, 1],
            ether_type: DEFAULT_ETHER_TYPE,
            dst_service: 4433,
            src_service: 4433,
            payload: vec![0xAA, 0xBB, 0xCC],
        };
        let mut encoded = frame.encode();
        while encoded.len() < 60 {
            encoded.push(0);
        }
        let decoded = Frame::decode(&encoded).unwrap();
        assert_eq!(decoded.payload, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn mtu_math() {
        assert_eq!(max_quic_datagram(1500), 1494);
    }
}
