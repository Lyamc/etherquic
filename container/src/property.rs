//! LOC properties from draft-ietf-moq-loc (20 July 2026).
//!
//! Even type IDs carry a single varint and omit the length. Odd type IDs
//! carry a varint length and then that many bytes. That is the MOQT
//! key-value rule the draft registers these IDs under.

use crate::varint;
use crate::Error;

/// Registered property type IDs.
pub mod id {
    pub const TIMESCALE: u64 = 0x08;
    pub const VIDEO_FRAME_MARKING: u64 = 0x09;
    pub const AUDIO_LEVEL: u64 = 0x0c;
    pub const VIDEO_CONFIG: u64 = 0x0d;
    pub const AUDIO_CONFIG: u64 = 0x0f;
    pub const TIMESTAMP: u64 = 0x10;
}

/// RFC 6464 audio level, carried in the low 8 bits of the Audio Level varint.
///
/// `level_dbov` is the magnitude in -dBov (0 is the overload point, 127 is
/// silence). `voice` is the V bit: true means the sender believes the packet
/// contains voice activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLevel {
    pub voice: bool,
    pub level_dbov: u8,
}

impl AudioLevel {
    pub fn from_byte(byte: u8) -> Self {
        Self {
            voice: byte & 0x80 != 0,
            level_dbov: byte & 0x7f,
        }
    }

    pub fn to_byte(self) -> u8 {
        let mut byte = self.level_dbov & 0x7f;
        if self.voice {
            byte |= 0x80;
        }
        byte
    }
}

/// Value of a property this crate does not specialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    Varint(u64),
    Bytes(Vec<u8>),
}

/// One LOC property. Unknown IDs are kept so a round trip does not drop them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Property {
    /// Units of [`Property::Timestamp`] per second. 48000 is Opus media time.
    Timescale(u64),
    /// Media time in [`Property::Timescale`] units. Without a timescale on the
    /// track, the draft defines this as microseconds since the Unix epoch.
    Timestamp(u64),
    AudioLevel(AudioLevel),
    /// WebCodecs `AudioDecoderConfig.description`. For Opus that is the
    /// RFC 7845 identification header, including the `OpusHead` magic.
    AudioConfig(Vec<u8>),
    /// Codec extradata. Maps to WebCodecs `VideoDecoderConfig.description`.
    VideoConfig(Vec<u8>),
    /// RFC 9626 frame mark, 1 to 4 bytes, without the RTP extension header.
    VideoFrameMarking(Vec<u8>),
    Unknown {
        id: u64,
        value: PropertyValue,
    },
}

impl Property {
    pub fn type_id(&self) -> u64 {
        match self {
            Property::Timescale(_) => id::TIMESCALE,
            Property::Timestamp(_) => id::TIMESTAMP,
            Property::AudioLevel(_) => id::AUDIO_LEVEL,
            Property::AudioConfig(_) => id::AUDIO_CONFIG,
            Property::VideoConfig(_) => id::VIDEO_CONFIG,
            Property::VideoFrameMarking(_) => id::VIDEO_FRAME_MARKING,
            Property::Unknown { id, .. } => *id,
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let type_id = self.type_id();
        varint::encode(type_id, out)?;
        match self {
            Property::Timescale(value) | Property::Timestamp(value) => {
                varint::encode(*value, out)?;
            }
            Property::AudioLevel(level) => {
                varint::encode(u64::from(level.to_byte()), out)?;
            }
            Property::AudioConfig(bytes)
            | Property::VideoConfig(bytes)
            | Property::VideoFrameMarking(bytes) => {
                encode_bytes(bytes, out)?;
            }
            Property::Unknown { value, .. } => match (type_id % 2 == 0, value) {
                (true, PropertyValue::Varint(value)) => varint::encode(*value, out)?,
                (false, PropertyValue::Bytes(bytes)) => encode_bytes(bytes, out)?,
                _ => return Err(Error::BadProperty { id: type_id }),
            },
        }
        Ok(())
    }
}

fn encode_bytes(bytes: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    varint::encode(bytes.len() as u64, out)?;
    out.extend_from_slice(bytes);
    Ok(())
}

pub(crate) fn decode_block(bytes: &[u8]) -> Result<Vec<Property>, Error> {
    let mut cur = crate::object::Cursor::new(bytes);
    let mut props = Vec::new();
    while cur.remaining() > 0 {
        props.push(decode_one(&mut cur).map_err(|err| match err {
            Error::NeedMore => Error::BadPropertyBlock,
            other => other,
        })?);
    }
    Ok(props)
}

fn decode_one(cur: &mut crate::object::Cursor<'_>) -> Result<Property, Error> {
    let type_id = cur.varint()?;
    if type_id % 2 == 0 {
        let value = cur.varint()?;
        Ok(match type_id {
            id::TIMESCALE => Property::Timescale(value),
            id::TIMESTAMP => Property::Timestamp(value),
            id::AUDIO_LEVEL if value <= 0xff => {
                Property::AudioLevel(AudioLevel::from_byte(value as u8))
            }
            _ => Property::Unknown {
                id: type_id,
                value: PropertyValue::Varint(value),
            },
        })
    } else {
        let len = length(cur.varint()?)?;
        let bytes = cur.bytes(len)?.to_vec();
        Ok(match type_id {
            id::AUDIO_CONFIG => Property::AudioConfig(bytes),
            id::VIDEO_CONFIG => Property::VideoConfig(bytes),
            id::VIDEO_FRAME_MARKING => Property::VideoFrameMarking(bytes),
            _ => Property::Unknown {
                id: type_id,
                value: PropertyValue::Bytes(bytes),
            },
        })
    }
}

pub(crate) fn length(value: u64) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| Error::Message("length does not fit usize".into()))
}
