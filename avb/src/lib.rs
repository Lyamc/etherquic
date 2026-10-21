//! IEEE 1722 talker frames through `open1722`, plus the in-process pieces of
//! 802.1BA this process can run without a switch: a credit-style software
//! pacer, an 802.1AS follower, and an 802.1Qat advertise that fails closed.
//!
//! Picture frames are CVF (subtype 3). PCM frames are AAF (subtype 2).
//! The experimental subtype `0x7F` is not used.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use open1722::aaf::{Format, Pcm, SampleRate, HEADER_LEN as AAF_HEADER};
use open1722::cvf::{Codec, Cvf, HEADER_LEN as CVF_HEADER};

pub const ETHERTYPE_AVTP: u16 = 0x22F0;
/// CVF, IEEE 1722-2016 compressed video. Not the experimental subtype.
pub const SUBTYPE_CVF: u8 = 3;
/// AAF, IEEE 1722-2016 PCM.
pub const SUBTYPE_AAF: u8 = 2;
pub const RATE_PICTURE: u64 = 200_000_000;
pub const RATE_AUDIO: u64 = 2_000_000;
pub const RATE_INPUT: u64 = 1_000_000;
pub const PCP_PICTURE: u8 = 5;
pub const PCP_AUDIO: u8 = 3;
pub const PCP_INPUT: u8 = 6;

/// Picture and audio are class B. Input is class A only when that reservation
/// succeeds.
pub const CLASS_B_US: u32 = 250;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub ethertype: u16,
    pub subtype: u8,
    pub sequence: u8,
    pub stream_id: [u8; 8],
    pub timestamp: u32,
    pub payload: Vec<u8>,
    /// Full Ethernet frame, VLAN-tagged only when a VLAN id was set.
    pub ethernet: Vec<u8>,
}

/// One CVF picture PDU inside an Ethernet frame of EtherType `0x22F0`.
pub fn picture_frame(
    dst: [u8; 6],
    src: [u8; 6],
    lane: u8,
    sequence: u8,
    timestamp: u32,
    payload: &[u8],
    vlan: Option<u16>,
    marker: bool,
) -> Result<Frame, String> {
    let stream_id = stream_id_bytes(src, lane);
    let pdu = cvf_pdu(stream_id, sequence, timestamp, payload, marker)?;
    finish(dst, src, lane, sequence, stream_id, timestamp, payload, vlan, pdu, SUBTYPE_CVF)
}

/// One AAF PCM PDU inside an Ethernet frame of EtherType `0x22F0`.
pub fn audio_frame(
    dst: [u8; 6],
    src: [u8; 6],
    lane: u8,
    sequence: u8,
    timestamp: u32,
    payload: &[u8],
    vlan: Option<u16>,
) -> Result<Frame, String> {
    let stream_id = stream_id_bytes(src, lane);
    let pdu = aaf_pdu(stream_id, sequence, timestamp, payload)?;
    finish(dst, src, lane, sequence, stream_id, timestamp, payload, vlan, pdu, SUBTYPE_AAF)
}

fn finish(
    dst: [u8; 6],
    src: [u8; 6],
    lane: u8,
    sequence: u8,
    stream_id: [u8; 8],
    timestamp: u32,
    payload: &[u8],
    vlan: Option<u16>,
    pdu: Vec<u8>,
    subtype: u8,
) -> Result<Frame, String> {
    if pdu.first().copied() != Some(subtype) {
        return Err("open1722 wrote an unexpected subtype".into());
    }
    if subtype == 0x7F {
        return Err("experimental subtype is not an AVB frame".into());
    }
    let pcp = pcp_for(lane, vlan);
    let ethernet = ethernet_frame(dst, src, vlan, pcp, &pdu);
    Ok(Frame {
        ethertype: ETHERTYPE_AVTP,
        subtype,
        sequence,
        stream_id,
        timestamp,
        payload: payload.to_vec(),
        ethernet,
    })
}

fn cvf_pdu(
    stream_id: [u8; 8],
    sequence: u8,
    timestamp: u32,
    payload: &[u8],
    marker: bool,
) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; CVF_HEADER + payload.len()];
    {
        let mut pdu = Cvf::initialized(&mut buf).map_err(|err| err.to_string())?;
        pdu.set_sequence_num(sequence);
        pdu.set_stream_id(u64::from_be_bytes(stream_id));
        pdu.set_avtp_timestamp(timestamp);
        pdu.set_stream_data_length(payload.len() as u16);
        pdu.set_codec(Codec::Mjpeg);
        pdu.set_stream_id_valid(true);
        pdu.set_timestamp_valid(true);
        pdu.set_presentation_timestamp_valid(true);
        pdu.set_marker(marker);
        if pdu.subtype() != SUBTYPE_CVF {
            return Err(format!("CVF subtype {:#x}", pdu.subtype()));
        }
    }
    buf[CVF_HEADER..].copy_from_slice(payload);
    Ok(buf)
}

fn aaf_pdu(
    stream_id: [u8; 8],
    sequence: u8,
    timestamp: u32,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; AAF_HEADER + payload.len()];
    {
        let mut pdu = Pcm::initialized(&mut buf).map_err(|err| err.to_string())?;
        pdu.set_sequence_num(sequence);
        pdu.set_stream_id(u64::from_be_bytes(stream_id));
        pdu.set_avtp_timestamp(timestamp);
        pdu.set_format(Format::Int16);
        pdu.set_sample_rate(SampleRate::Hz48000);
        pdu.set_channels_per_frame(2);
        pdu.set_bit_depth(16);
        pdu.set_stream_data_length(payload.len() as u16);
        pdu.set_stream_id_valid(true);
        pdu.set_timestamp_valid(true);
        if pdu.subtype() != SUBTYPE_AAF {
            return Err(format!("AAF subtype {:#x}", pdu.subtype()));
        }
    }
    buf[AAF_HEADER..].copy_from_slice(payload);
    Ok(buf)
}

/// Parse a CVF or AAF PDU. The experimental subtype is rejected.
pub fn decode_pdu(bytes: &[u8]) -> Option<Frame> {
    let subtype = *bytes.first()?;
    let (header, payload_len, sequence, stream_id, timestamp) = if subtype == SUBTYPE_CVF {
        let pdu = Cvf::new(bytes).ok()?;
        if !pdu.is_valid() && pdu.stream_data_length() as usize + CVF_HEADER > bytes.len() {
            return None;
        }
        (
            CVF_HEADER,
            pdu.stream_data_length() as usize,
            pdu.sequence_num(),
            pdu.stream_id().to_be_bytes(),
            pdu.avtp_timestamp(),
        )
    } else if subtype == SUBTYPE_AAF {
        let pdu = Pcm::new(bytes).ok()?;
        (
            AAF_HEADER,
            pdu.stream_data_length() as usize,
            pdu.sequence_num(),
            pdu.stream_id().to_be_bytes(),
            pdu.avtp_timestamp(),
        )
    } else {
        return None;
    };
    let end = header.checked_add(payload_len)?;
    if end > bytes.len() {
        return None;
    }
    Some(Frame {
        ethertype: ETHERTYPE_AVTP,
        subtype,
        sequence,
        stream_id,
        timestamp,
        payload: bytes[header..end].to_vec(),
        ethernet: Vec::new(),
    })
}

pub fn stream_id_bytes(mac: [u8; 6], lane: u8) -> [u8; 8] {
    let mut stream_id = [0u8; 8];
    stream_id[..6].copy_from_slice(&mac);
    stream_id[6] = 0;
    stream_id[7] = lane;
    stream_id
}

/// VLAN PCP for a lane. `None` when the frame is untagged.
pub fn pcp_for(lane: u8, vlan: Option<u16>) -> Option<u8> {
    vlan.map(|_| match lane {
        2 => PCP_AUDIO,
        3 => PCP_INPUT,
        _ => PCP_PICTURE,
    })
}

pub fn ethernet_frame(
    dst: [u8; 6],
    src: [u8; 6],
    vlan: Option<u16>,
    pcp: Option<u8>,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(18 + payload.len());
    out.extend_from_slice(&dst);
    out.extend_from_slice(&src);
    if let Some(vid) = vlan {
        let pcp = pcp.unwrap_or(0) & 0x7;
        let tci = (u16::from(pcp) << 13) | (vid & 0x0fff);
        out.extend_from_slice(&0x8100u16.to_be_bytes());
        out.extend_from_slice(&tci.to_be_bytes());
    }
    out.extend_from_slice(&ETHERTYPE_AVTP.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn ethertype_of(frame: &[u8]) -> Option<u16> {
    if frame.len() >= 16 && u16::from_be_bytes([frame[12], frame[13]]) == 0x8100 {
        return Some(u16::from_be_bytes([frame[16], frame[17]]));
    }
    if frame.len() >= 14 {
        return Some(u16::from_be_bytes([frame[12], frame[13]]));
    }
    None
}

pub fn payload_of(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() >= 18 && u16::from_be_bytes([frame[12], frame[13]]) == 0x8100 {
        return Some(&frame[18..]);
    }
    if frame.len() >= 14 {
        return Some(&frame[14..]);
    }
    None
}

/// Software pacer. `now` is time since the reservation started.
#[derive(Clone, Debug)]
pub struct Pacer {
    pub rate_bps: u64,
    sent: u64,
}

impl Pacer {
    pub fn new(rate_bps: u64) -> Self {
        Self { rate_bps, sent: 0 }
    }

    /// True when `n` bytes still fit in the reserved rate at `now`.
    pub fn take(&mut self, now: Duration, n: usize) -> bool {
        let elapsed_ns = now.as_nanos() as u64;
        let budget = self.rate_bps.saturating_mul(elapsed_ns) / 8 / 1_000_000_000;
        let next = self.sent.saturating_add(n as u64);
        if next > budget {
            return false;
        }
        self.sent = next;
        true
    }

    /// Send `n` bytes no earlier than `earliest`, at the reserved rate.
    /// The returned time is when those bytes fit. Later calls see them as sent.
    pub fn schedule(&mut self, earliest: Duration, n: usize) -> Duration {
        let bytes = self.sent.saturating_add(n as u64);
        let ns = bytes.saturating_mul(8).saturating_mul(1_000_000_000) / self.rate_bps.max(1);
        let at = earliest.max(Duration::from_nanos(ns));
        self.sent = bytes;
        at
    }

    pub fn sent(&self) -> u64 {
        self.sent
    }
}

/// Over `window`, bytes sent at `rate_bps` stay inside that rate.
pub fn pacer_stays_inside(rate_bps: u64, window: Duration, mtu: usize) -> bool {
    let mut pacer = Pacer::new(rate_bps);
    let step = Duration::from_micros(250);
    let mut now = Duration::ZERO;
    while now < window {
        now += step;
        while pacer.take(now, mtu) {}
    }
    let bits = pacer.sent().saturating_mul(8);
    let secs = window.as_secs_f64().max(0.001);
    (bits as f64) / secs <= rate_bps as f64 * 1.01
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reservation {
    Ok,
    Failed(&'static str),
}

/// A failed MSRP attribute fails the session. There is no LAN flood fallback.
pub fn reservation_fails_closed(result: Reservation) -> Result<(), &'static str> {
    match result {
        Reservation::Ok => Ok(()),
        Reservation::Failed(reason) => Err(reason),
    }
}

static LISTENER: AtomicBool = AtomicBool::new(false);

/// A player hello is the in-process listener. No hello means the attribute failed.
pub fn note_listener(on: bool) {
    LISTENER.store(on, Ordering::Relaxed);
}

pub fn listener_present() -> bool {
    LISTENER.load(Ordering::Relaxed)
}

/// Advertise, then fail the session when no listener has joined.
pub fn try_reserve() -> Result<(), &'static str> {
    let joined = listener_present();
    let result = if joined {
        Reservation::Ok
    } else {
        Reservation::Failed("no listener")
    };
    reservation_fails_closed(result)?;
    advertise(joined, joined)?;
    Ok(())
}

/// 802.1AS follower. Hardware timestamps are used when the NIC has them.
/// This host is grandmaster only when the LAN has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GptpFollower {
    pub hardware_timestamps: bool,
    pub grandmaster: bool,
}

pub fn follow_gptp(nic_has_hardware_timestamps: bool, lan_has_grandmaster: bool) -> GptpFollower {
    GptpFollower {
        hardware_timestamps: nic_has_hardware_timestamps,
        grandmaster: !lan_has_grandmaster,
    }
}

/// 802.1Qat talker advertise. Picture and audio are class B. Input is class A
/// only when that listener accepted the attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TalkerAdvertise {
    pub picture_class_b: bool,
    pub audio_class_b: bool,
    pub input_class_a: bool,
}

pub fn advertise(listener_ok: bool, input_class_a_ok: bool) -> Result<TalkerAdvertise, &'static str> {
    if !listener_ok {
        return Err("reservation failed");
    }
    Ok(TalkerAdvertise {
        picture_class_b: true,
        audio_class_b: true,
        input_class_a: input_class_a_ok,
    })
}

/// Connect-row text. Untagged frames are not a priority class. The stream is
/// not encrypted. "shaped" until a reservation succeeds, then "reserved".
pub fn row_label(vlan: bool, reserved: bool) -> String {
    let state = if reserved {
        "AVB (reserved)"
    } else {
        "AVB (shaped)"
    };
    if vlan {
        format!("{state}, not encrypted")
    } else {
        format!("{state}, untagged frames are not a priority class, not encrypted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_is_cvf_on_22f0_and_audio_is_aaf() {
        let picture = picture_frame(
            [2, 0, 0, 0, 0, 4],
            [2, 0, 0, 0, 0, 1],
            1,
            1,
            90_000,
            b"slice",
            None,
            true,
        )
        .unwrap();
        assert_eq!(picture.ethertype, 0x22F0);
        assert_eq!(picture.subtype, SUBTYPE_CVF);
        assert_ne!(picture.subtype, 0x7F);
        assert_eq!(ethertype_of(&picture.ethernet), Some(0x22F0));
        assert_eq!(picture.ethernet[12], 0x22);
        let pdu = payload_of(&picture.ethernet).unwrap();
        assert_eq!(pdu[0], SUBTYPE_CVF);
        assert_eq!(decode_pdu(pdu).unwrap().payload, b"slice");

        let audio = audio_frame(
            [2, 0, 0, 0, 0, 4],
            [2, 0, 0, 0, 0, 1],
            2,
            1,
            48_000,
            b"pcm",
            Some(2),
        )
        .unwrap();
        assert_eq!(audio.subtype, SUBTYPE_AAF);
        assert_eq!(audio.ethernet[12], 0x81);
        assert_eq!(ethertype_of(&audio.ethernet), Some(0x22F0));
        let tci = u16::from_be_bytes([audio.ethernet[14], audio.ethernet[15]]);
        assert_eq!(tci >> 13, u16::from(PCP_AUDIO));
        assert!(pacer_stays_inside(RATE_PICTURE, Duration::from_millis(100), 1500));
        assert!(reservation_fails_closed(Reservation::Failed("no listener")).is_err());
        note_listener(false);
        assert!(try_reserve().is_err());
        note_listener(true);
        assert!(try_reserve().is_ok());
        note_listener(false);
        assert!(advertise(false, true).is_err());
        let ok = advertise(true, true).unwrap();
        assert!(ok.picture_class_b && ok.audio_class_b && ok.input_class_a);
        let follow = follow_gptp(true, false);
        assert!(follow.hardware_timestamps && follow.grandmaster);
        assert!(!follow_gptp(false, true).grandmaster);
        assert!(row_label(false, false).contains("not encrypted"));
        assert!(row_label(true, true).contains("reserved"));
    }
}
