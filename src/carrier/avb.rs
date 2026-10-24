//! AVB carrier. Frames are built by the `avtp` crate through `avb`.
//! CVF and AAF are used only when the payload is that media.

use std::time::Duration;

use super::assemble::Slice;
use super::Lane;

pub use avb::{
    pacer_stays_inside, reservation_fails_closed, row_label, Pacer, Reservation, ETHERTYPE_AVTP,
    PCP_AUDIO, PCP_INPUT, PCP_PICTURE, RATE_AUDIO, RATE_INPUT, RATE_PICTURE, SUBTYPE_CVF,
    SUBTYPE_EXPERIMENTAL,
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
            subtype: if lane != Lane::Audio && avb::cvf_codec(payload).is_some() {
                SUBTYPE_CVF
            } else {
                SUBTYPE_EXPERIMENTAL
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
        let built = if lane != Lane::Audio as u8 && avb::cvf_codec(&self.payload).is_some() {
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
        } else if Slice::decode(&self.payload).is_some() {
            avb::record_frame(
                dst,
                mac,
                lane,
                self.sequence,
                self.timestamp,
                &self.payload,
                vlan,
            )
        } else if lane == Lane::Audio as u8 {
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
        .expect("avtp frame");
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
        assert_eq!(raw[0], SUBTYPE_EXPERIMENTAL);
        assert_ne!(raw[0], SUBTYPE_CVF);
        assert_eq!(AvtpFrame::decode(&raw).unwrap().payload, b"slice");
        assert_eq!(AvtpFrame::decode(&raw).unwrap().stream_id[6], 0);
        assert_eq!(
            AvtpFrame::decode(&raw).unwrap().stream_id[7],
            Lane::Picture as u8
        );
        let ether = frame.ethernet(None);
        assert_eq!(avb::ethertype_of(&ether), Some(ETHERTYPE_AVTP));
        assert_ne!(ETHERTYPE_AVTP, crate::ETHERTYPE_QUIC);
        let jpeg = [0xFF, 0xD8, 0xFF, 0xD9];
        let picture = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Picture, 1, &jpeg);
        assert_eq!(picture.encode()[0], SUBTYPE_CVF);
        assert_eq!(AvtpFrame::decode(&picture.encode()).unwrap().payload, jpeg);
        let audio = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Audio, 1, b"pcm");
        assert_eq!(audio.encode()[0], SUBTYPE_EXPERIMENTAL);
        assert_eq!(AvtpFrame::decode(&audio.encode()).unwrap().payload, b"pcm");
        let slice = Slice {
            record_id: 0,
            index: 1,
            count: 2,
            bytes: b"picture".to_vec(),
        }
        .encode();
        assert!(slice.starts_with(&[0x00, 0x00, 0x01]));
        let carried = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Picture, 9, &slice);
        let carried_pdu = carried.encode();
        assert_eq!(carried_pdu[0], SUBTYPE_EXPERIMENTAL);
        assert_ne!(carried_pdu[0], SUBTYPE_CVF);
        assert_eq!(AvtpFrame::decode(&carried_pdu).unwrap().payload, slice);
        let h264 = [0x00, 0x00, 0x00, 0x01, 0x65];
        let video = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Picture, 2, &h264);
        assert_eq!(video.encode()[0], SUBTYPE_CVF);
        assert_eq!(AvtpFrame::decode(&video.encode()).unwrap().payload, h264);
        let h264_long = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88];
        assert!(Slice::decode(&h264_long).is_some());
        let long = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Picture, 4, &h264_long);
        let long_pdu = long.encode();
        assert_eq!(long_pdu[0], SUBTYPE_CVF);
        assert_ne!(long_pdu[0], SUBTYPE_EXPERIMENTAL);
        assert_eq!(&long_pdu[24..28], &[0, 0, 0, 0]);
        assert_eq!(&long_pdu[28..], &h264_long);
        assert_eq!(
            u16::from_be_bytes([long_pdu[20], long_pdu[21]]) as usize,
            4 + h264_long.len()
        );
        assert_eq!(AvtpFrame::decode(&long_pdu).unwrap().payload, h264_long);
        let j2k = [0xFF, 0x4F, 0xFF, 0x51, 0x00];
        let j2k_frame = AvtpFrame::new([1, 2, 3, 4, 5, 6], Lane::Picture, 3, &j2k);
        assert_eq!(j2k_frame.encode()[0], SUBTYPE_CVF);
        assert_eq!(AvtpFrame::decode(&j2k_frame.encode()).unwrap().payload, j2k);
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
