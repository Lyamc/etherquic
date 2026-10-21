//! AVB carrier. Frames are built by the `avb` crate (`open1722` CVF and AAF).
//! Subtype `0x7F` is not a wire format.

use std::time::Duration;

use super::Lane;

pub use avb::{
    pacer_stays_inside, reservation_fails_closed, row_label, Pacer, Reservation, ETHERTYPE_AVTP,
    PCP_AUDIO, PCP_INPUT, PCP_PICTURE, RATE_AUDIO, RATE_INPUT, RATE_PICTURE, SUBTYPE_AAF,
    SUBTYPE_CVF,
};

/// Picture subtype on the wire. CVF, not the experimental value.
pub const AVTP_SUBTYPE: u8 = SUBTYPE_CVF;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvtpFrame {
    pub subtype: u8,
    pub sequence: u8,
    pub stream_id: [u8; 8],
    pub timestamp: u32,
    pub payload: Vec<u8>,
}

impl AvtpFrame {
    pub fn new(mac: [u8; 6], lane: Lane, sequence: u8, payload: &[u8]) -> Self {
        Self {
            subtype: if lane == Lane::Audio {
                SUBTYPE_AAF
            } else {
                SUBTYPE_CVF
            },
            sequence,
            stream_id: avb::stream_id_bytes(mac, lane as u8),
            timestamp: 0,
            payload: payload.to_vec(),
        }
    }

    /// IEEE 1722 PDU. The Ethernet header is added by [`Self::ethernet`].
    pub fn encode(&self) -> Vec<u8> {
        avb::payload_of(&self.ethernet(None))
            .expect("avtp pdu")
            .to_vec()
    }

    pub fn ethernet(&self, vlan: Option<u16>) -> Vec<u8> {
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&self.stream_id[..6]);
        let lane = self.stream_id[7];
        let dst = [2, 0, 0, 0, 0, 4];
        let built = if lane == Lane::Audio as u8 {
            avb::audio_frame(dst, mac, lane, self.sequence, self.timestamp, &self.payload, vlan)
        } else {
            avb::picture_frame(
                dst,
                mac,
                lane,
                self.sequence,
                self.timestamp,
                &self.payload,
                vlan,
                true,
            )
        }
        .expect("open1722 frame");
        assert_ne!(built.subtype, 0x7F);
        built.ethernet
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let frame = avb::decode_pdu(bytes)?;
        Some(Self {
            subtype: frame.subtype,
            sequence: frame.sequence,
            stream_id: frame.stream_id,
            timestamp: frame.timestamp,
            payload: frame.payload,
        })
    }
}

pub fn pcp(lane: Lane, vlan: Option<u16>) -> Option<u8> {
    avb::pcp_for(lane as u8, vlan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_22f0_not_88b6_and_the_pacer_holds_the_rate() {
        let frame = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Picture, 1, b"slice");
        let raw = frame.encode();
        assert_eq!(raw[0], SUBTYPE_CVF);
        assert_ne!(raw[0], 0x7F);
        assert_eq!(AvtpFrame::decode(&raw).unwrap().stream_id[6], 0);
        assert_eq!(
            AvtpFrame::decode(&raw).unwrap().stream_id[7],
            Lane::Picture as u8
        );
        let ether = frame.ethernet(None);
        assert_eq!(avb::ethertype_of(&ether), Some(ETHERTYPE_AVTP));
        assert_ne!(ETHERTYPE_AVTP, crate::ETHERTYPE_QUIC);
        let audio = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Audio, 1, b"pcm");
        assert_eq!(audio.encode()[0], SUBTYPE_AAF);
        assert!(pacer_stays_inside(RATE_PICTURE, Duration::from_millis(100), 1500));
        assert_eq!(RATE_AUDIO, 2_000_000);
        assert_eq!(RATE_INPUT, 1_000_000);
        assert_eq!(pcp(Lane::Picture, Some(2)), Some(PCP_PICTURE));
        assert_eq!(pcp(Lane::Audio, Some(2)), Some(PCP_AUDIO));
        assert_eq!(pcp(Lane::Input, Some(2)), Some(PCP_INPUT));
        assert_eq!(pcp(Lane::Picture, None), None);
        assert!(reservation_fails_closed(Reservation::Failed("no listener")).is_err());
        assert!(avb::advertise(false, true).is_err());
        assert!(avb::follow_gptp(true, false).grandmaster);
    }
}
