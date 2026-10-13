//! Opus packet duration and the LOC mapping for one audio track.
//!
//! draft-ietf-moq-loc section 4.1: each Opus packet is one object, one object
//! per group, object id 0. Timescale is 48000. The timestamp is the Ogg
//! granule position of the first sample in the packet (decoded samples at
//! 48 kHz from the start of the stream, before pre-skip). The audio config
//! is the RFC 7845 identification header.

use etherquic_container::{Object, Property};

use crate::Error;

pub const TIMESCALE: u64 = 48_000;
const MAX_PACKET_SAMPLES: u32 = 48_000 * 120 / 1000;

/// Samples at 48 kHz for each TOC config (RFC 6716, table 2).
const FRAME_SAMPLES: [u32; 32] = [
    480, 960, 1920, 2880, // SILK NB, 10/20/40/60 ms
    480, 960, 1920, 2880, // SILK MB
    480, 960, 1920, 2880, // SILK WB
    480, 960, // Hybrid SWB, 10/20 ms
    480, 960, // Hybrid FB
    120, 240, 480, 960, // CELT NB, 2.5/5/10/20 ms
    120, 240, 480, 960, // CELT WB
    120, 240, 480, 960, // CELT SWB
    120, 240, 480, 960, // CELT FB
];

/// RFC 7845 identification header. `raw` is the LOC audio config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusId {
    pub channels: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain: i16,
    pub channel_mapping_family: u8,
    pub raw: Vec<u8>,
}

/// One Opus packet (a single TOC sequence), not an Ogg packet of several streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusPacket {
    pub data: Vec<u8>,
    /// Granule position of the first sample in this packet.
    pub start_granule: u64,
    pub samples: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusStream {
    pub id: OpusId,
    pub packets: Vec<OpusPacket>,
    /// Decoded samples dropped from the end of the last packet on remux.
    ///
    /// Ogg can crop the last page. LOC has no end-trim property, so
    /// [`from_objects`] leaves this at 0 and the remuxed file keeps the
    /// whole last packet.
    pub end_trim: u64,
}

/// Build an OpusHead identification header for a family-0 stream.
pub fn id_header(channels: u8, pre_skip: u16, input_sample_rate: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(19);
    raw.extend_from_slice(b"OpusHead");
    raw.push(1);
    raw.push(channels);
    raw.extend_from_slice(&pre_skip.to_le_bytes());
    raw.extend_from_slice(&input_sample_rate.to_le_bytes());
    raw.extend_from_slice(&0i16.to_le_bytes());
    raw.push(0);
    raw
}

pub fn parse_id(bytes: &[u8]) -> Result<OpusId, Error> {
    if bytes.len() < 19 || &bytes[..8] != b"OpusHead" {
        return Err(Error::Opus(
            "audio config is not an Opus identification header".into(),
        ));
    }
    if bytes[8] != 1 {
        return Err(Error::Opus(format!(
            "unsupported OpusHead version {}",
            bytes[8]
        )));
    }
    let channels = bytes[9];
    if channels == 0 {
        return Err(Error::Opus("OpusHead channel count is 0".into()));
    }
    Ok(OpusId {
        channels,
        pre_skip: u16::from_le_bytes([bytes[10], bytes[11]]),
        input_sample_rate: u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")),
        output_gain: i16::from_le_bytes([bytes[16], bytes[17]]),
        channel_mapping_family: bytes[18],
        raw: bytes.to_vec(),
    })
}

/// Duration of one Opus packet in samples at 48 kHz. Stereo does not double it.
pub fn packet_samples(packet: &[u8]) -> Result<u32, Error> {
    if packet.is_empty() {
        return Err(Error::Opus("empty Opus packet".into()));
    }
    let toc = packet[0];
    let config = (toc >> 3) as usize;
    let code = toc & 0x03;
    let frame = FRAME_SAMPLES[config];
    let frames = match code {
        0 => 1u32,
        1 | 2 => 2,
        _ => {
            if packet.len() < 2 {
                return Err(Error::Opus("truncated code-3 Opus packet".into()));
            }
            let count = u32::from(packet[1] & 0x3f);
            if count == 0 {
                return Err(Error::Opus("Opus code-3 frame count is 0".into()));
            }
            count
        }
    };
    let total = frames
        .checked_mul(frame)
        .ok_or_else(|| Error::Opus("Opus packet duration overflow".into()))?;
    if total == 0 || total > MAX_PACKET_SAMPLES {
        return Err(Error::Opus(format!(
            "Opus packet duration {total} samples is outside 2.5..=120 ms"
        )));
    }
    Ok(total)
}

/// Section 4.1 mapping: one group per packet, object id 0, timescale on the first object.
pub fn to_objects(stream: &OpusStream) -> Vec<Object> {
    stream
        .packets
        .iter()
        .enumerate()
        .map(|(index, packet)| {
            let mut public = Vec::new();
            if index == 0 {
                public.push(Property::Timescale(TIMESCALE));
                public.push(Property::AudioConfig(stream.id.raw.clone()));
            }
            public.push(Property::Timestamp(packet.start_granule));
            Object {
                group_id: index as u64,
                subgroup_id: 0,
                object_id: 0,
                public,
                private: Vec::new(),
                payload: packet.data.clone(),
            }
        })
        .collect()
}

struct RawPacket {
    data: Vec<u8>,
    timestamp: u64,
    samples: u32,
    timescale: Option<u64>,
}

/// Reverse of [`to_objects`].
///
/// A timescale present at or before the first packet means timestamps are
/// absolute media time in that unit (48000 matches the granule directly).
/// If no timescale is present, timestamps are wall-clock microseconds and
/// the granule is the distance from the first packet.
pub fn from_objects(objects: &[Object]) -> Result<OpusStream, Error> {
    let mut id: Option<OpusId> = None;
    let mut running_scale: Option<u64> = None;
    let mut raw = Vec::new();

    for object in objects {
        if let Some(config) = object.audio_config() {
            let parsed = parse_id(config)?;
            if let Some(previous) = &id {
                if previous.raw != parsed.raw {
                    return Err(Error::Opus("audio config changed mid-stream".into()));
                }
            } else {
                id = Some(parsed);
            }
        }
        if let Some(scale) = object.timescale() {
            if scale == 0 {
                return Err(Error::Opus("timescale is 0".into()));
            }
            running_scale = Some(scale);
        }
        if object.payload.is_empty() {
            continue;
        }
        let timestamp = object
            .timestamp()
            .ok_or_else(|| Error::Opus("audio object is missing a timestamp".into()))?;
        let samples = packet_samples(&object.payload)?;
        raw.push(RawPacket {
            data: object.payload.clone(),
            timestamp,
            samples,
            timescale: running_scale,
        });
    }

    let id = id.ok_or_else(|| Error::Opus("LOC stream has no Opus audio config".into()))?;
    if raw.is_empty() {
        return Err(Error::Opus("LOC stream has no Opus packets".into()));
    }
    let mixed = raw.iter().any(|packet| packet.timescale.is_none())
        && raw.iter().any(|packet| packet.timescale.is_some());
    if mixed {
        return Err(Error::Opus(
            "timescale must cover every packet or none of them".into(),
        ));
    }

    let wall_clock = raw[0].timescale.is_none();
    let origin = raw[0].timestamp;
    let mut packets = Vec::with_capacity(raw.len());
    let mut previous = 0u64;
    for (index, packet) in raw.into_iter().enumerate() {
        let start = if wall_clock {
            if packet.timestamp < origin {
                return Err(Error::Opus("timestamps go backwards".into()));
            }
            rescale(packet.timestamp - origin, 1_000_000, TIMESCALE)?
        } else {
            rescale(packet.timestamp, packet.timescale.unwrap(), TIMESCALE)?
        };
        if index > 0 && start < previous {
            return Err(Error::Opus("timestamps go backwards".into()));
        }
        previous = start;
        packets.push(OpusPacket {
            data: packet.data,
            start_granule: start,
            samples: packet.samples,
        });
    }

    Ok(OpusStream {
        id,
        packets,
        end_trim: 0,
    })
}

fn rescale(value: u64, from: u64, to: u64) -> Result<u64, Error> {
    let scaled = u128::from(value)
        .checked_mul(u128::from(to))
        .ok_or_else(|| Error::Opus("timestamp overflow".into()))?
        / u128::from(from);
    u64::try_from(scaled).map_err(|_| Error::Opus("timestamp overflow".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_durations() {
        assert_eq!(packet_samples(&[0x00]).unwrap(), 480);
        assert_eq!(packet_samples(&[0xf9]).unwrap(), 1920);
        // config 16 (2.5 ms), code 3, four frames -> 10 ms.
        assert_eq!(packet_samples(&[0x83, 0x04]).unwrap(), 480);
        assert!(packet_samples(&[]).is_err());
        assert!(packet_samples(&[0x03]).is_err());
        assert!(packet_samples(&[0x03, 0x00]).is_err());
    }

    #[test]
    fn loc_roundtrip_keeps_granules() {
        let stream = OpusStream {
            id: parse_id(&id_header(1, 312, 48_000)).unwrap(),
            packets: vec![
                OpusPacket {
                    data: vec![0x98],
                    start_granule: 0,
                    samples: 960,
                },
                OpusPacket {
                    data: vec![0x98, 0x11],
                    start_granule: 960,
                    samples: 960,
                },
            ],
            end_trim: 0,
        };
        let restored = from_objects(&to_objects(&stream)).unwrap();
        assert_eq!(restored, stream);
        assert_eq!(to_objects(&stream)[0].group_id, 0);
        assert_eq!(to_objects(&stream)[1].group_id, 1);
        assert_eq!(to_objects(&stream)[0].object_id, 0);
        assert_eq!(to_objects(&stream)[1].object_id, 0);
    }
}
