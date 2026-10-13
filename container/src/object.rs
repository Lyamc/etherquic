//! Self-delimiting LOC objects.
//!
//! draft-ietf-moq-loc puts public properties in the MOQT object header and
//! puts private properties followed by the elementary payload in the MOQT
//! object payload. etherquic has no MOQT layer, so an object on a QUIC
//! stream is:
//!
//! ```text
//! LOC Object {
//!   Group ID (i)
//!   Subgroup ID (i)
//!   Object ID (i)
//!   Public Properties Length (i)
//!   Public Properties (..)
//!   Private Properties Length (i)
//!   Private Properties (..)
//!   Payload Length (i)
//!   Payload (..)
//! }
//! ```
//!
//! `(i)` is a QUIC variable-length integer. The payload is the WebCodecs
//! "internal data" of one encoded audio or video chunk: one Opus packet,
//! or one video frame, with no extra codec encapsulation.

use crate::property::{self, Property};
use crate::varint;
use crate::Error;

/// One LOC object. Field order is the wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub group_id: u64,
    pub subgroup_id: u64,
    pub object_id: u64,
    pub public: Vec<Property>,
    pub private: Vec<Property>,
    pub payload: Vec<u8>,
}

impl Object {
    pub fn properties(&self) -> impl Iterator<Item = &Property> {
        self.public.iter().chain(self.private.iter())
    }

    pub fn timestamp(&self) -> Option<u64> {
        self.properties().find_map(|property| match property {
            Property::Timestamp(value) => Some(*value),
            _ => None,
        })
    }

    pub fn timescale(&self) -> Option<u64> {
        self.properties().find_map(|property| match property {
            Property::Timescale(value) => Some(*value),
            _ => None,
        })
    }

    pub fn audio_config(&self) -> Option<&[u8]> {
        self.properties().find_map(|property| match property {
            Property::AudioConfig(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
    }

    pub fn audio_level(&self) -> Option<crate::AudioLevel> {
        self.properties().find_map(|property| match property {
            Property::AudioLevel(level) => Some(*level),
            _ => None,
        })
    }

    pub fn video_config(&self) -> Option<&[u8]> {
        self.properties().find_map(|property| match property {
            Property::VideoConfig(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
    }

    pub fn video_frame_marking(&self) -> Option<&[u8]> {
        self.properties().find_map(|property| match property {
            Property::VideoFrameMarking(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
    }
}

pub(crate) struct Cursor<'a> {
    buf: &'a [u8],
    index: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, index: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.index)
    }

    pub(crate) fn varint(&mut self) -> Result<u64, Error> {
        let (value, len) = varint::decode(&self.buf[self.index..])?;
        self.index += len;
        Ok(value)
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Result<&'a [u8], Error> {
        if self.remaining() < len {
            return Err(Error::NeedMore);
        }
        let start = self.index;
        self.index += len;
        Ok(&self.buf[start..self.index])
    }
}

/// Encode one object. The result is concatenated with other objects on a stream.
pub fn encode_object(object: &Object, out: &mut Vec<u8>) -> Result<(), Error> {
    varint::encode(object.group_id, out)?;
    varint::encode(object.subgroup_id, out)?;
    varint::encode(object.object_id, out)?;
    encode_properties(&object.public, out)?;
    encode_properties(&object.private, out)?;
    varint::encode(object.payload.len() as u64, out)?;
    out.extend_from_slice(&object.payload);
    Ok(())
}

fn encode_properties(properties: &[Property], out: &mut Vec<u8>) -> Result<(), Error> {
    let mut body = Vec::new();
    for property in properties {
        property.encode(&mut body)?;
    }
    varint::encode(body.len() as u64, out)?;
    out.extend_from_slice(&body);
    Ok(())
}

/// Decode one object from the front of `input`.
///
/// [`Error::NeedMore`] means `input` ends inside the object. Any other error
/// means the bytes that are present are not a valid object.
pub fn decode_object(input: &[u8]) -> Result<(Object, usize), Error> {
    let mut cur = Cursor::new(input);
    let group_id = cur.varint()?;
    let subgroup_id = cur.varint()?;
    let object_id = cur.varint()?;
    let public_len = property::length(cur.varint()?)?;
    let public = property::decode_block(cur.bytes(public_len)?)?;
    let private_len = property::length(cur.varint()?)?;
    let private = property::decode_block(cur.bytes(private_len)?)?;
    let payload_len = property::length(cur.varint()?)?;
    let payload = cur.bytes(payload_len)?.to_vec();
    Ok((
        Object {
            group_id,
            subgroup_id,
            object_id,
            public,
            private,
            payload,
        },
        cur.index,
    ))
}

/// Encode a sequence of objects.
pub fn encode_objects(objects: &[Object]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    for object in objects {
        encode_object(object, &mut out)?;
    }
    Ok(out)
}

/// Decode a buffer that contains only whole objects.
///
/// An empty buffer is an empty sequence. A buffer that ends inside an object
/// is [`Error::Truncated`].
pub fn decode_objects(input: &[u8]) -> Result<Vec<Object>, Error> {
    let mut objects = Vec::new();
    let mut rest = input;
    while !rest.is_empty() {
        match decode_object(rest) {
            Ok((object, len)) => {
                if len == 0 {
                    return Err(Error::Truncated);
                }
                rest = &rest[len..];
                objects.push(object);
            }
            Err(Error::NeedMore) => return Err(Error::Truncated),
            Err(err) => return Err(err),
        }
    }
    Ok(objects)
}

/// Incremental decoder for a QUIC stream of LOC objects.
#[derive(Debug, Default)]
pub struct ObjectReader {
    buf: Vec<u8>,
    pos: usize,
}

impl ObjectReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes and return every object that is now complete.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Object>, Error> {
        self.compact();
        if !data.is_empty() {
            self.buf.extend_from_slice(data);
        }
        let mut objects = Vec::new();
        loop {
            if self.pos >= self.buf.len() {
                break;
            }
            match decode_object(&self.buf[self.pos..]) {
                Ok((object, len)) => {
                    if len == 0 {
                        return Err(Error::Truncated);
                    }
                    self.pos += len;
                    objects.push(object);
                }
                Err(Error::NeedMore) => break,
                Err(err) => return Err(err),
            }
        }
        Ok(objects)
    }

    /// Fail if bytes remain that do not form a complete object.
    pub fn finish(&self) -> Result<(), Error> {
        if self.pos < self.buf.len() {
            Err(Error::Truncated)
        } else {
            Ok(())
        }
    }

    fn compact(&mut self) {
        if self.pos > 0 && (self.pos == self.buf.len() || self.pos > 64 * 1024) {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioLevel, FrameMark, PropertyValue};

    fn sample() -> Object {
        Object {
            group_id: 4,
            subgroup_id: 1,
            object_id: 2,
            public: vec![
                Property::Timescale(48_000),
                Property::Timestamp(960),
                Property::AudioConfig(b"OpusHead-test".to_vec()),
            ],
            private: vec![Property::AudioLevel(AudioLevel {
                voice: true,
                level_dbov: 20,
            })],
            payload: vec![0x98, 1, 2, 3],
        }
    }

    #[test]
    fn object_roundtrip() {
        let object = sample();
        let bytes = encode_objects(&[object.clone()]).unwrap();
        let decoded = decode_objects(&bytes).unwrap();
        assert_eq!(decoded, vec![object]);
    }

    #[test]
    fn unknown_properties_roundtrip() {
        let object = Object {
            group_id: 0,
            subgroup_id: 0,
            object_id: 0,
            public: vec![Property::Unknown {
                id: 0x22,
                value: PropertyValue::Varint(99),
            }],
            private: vec![Property::Unknown {
                id: 0x33,
                value: PropertyValue::Bytes(vec![7, 8]),
            }],
            payload: vec![],
        };
        let bytes = encode_objects(std::slice::from_ref(&object)).unwrap();
        assert_eq!(decode_objects(&bytes).unwrap(), vec![object]);
    }

    #[test]
    fn private_audio_level_is_visible() {
        let object = sample();
        assert_eq!(
            object.audio_level(),
            Some(AudioLevel {
                voice: true,
                level_dbov: 20
            })
        );
        assert_eq!(object.timestamp(), Some(960));
        assert_eq!(object.timescale(), Some(48_000));
    }

    #[test]
    fn truncated_and_bad_property_block() {
        let bytes = encode_objects(&[sample()]).unwrap();
        assert_eq!(
            decode_objects(&bytes[..bytes.len() - 1]),
            Err(Error::Truncated)
        );
        assert_eq!(decode_objects(&[]).unwrap(), Vec::<Object>::new());

        let mut bad = Vec::new();
        crate::varint::encode(0, &mut bad).unwrap();
        crate::varint::encode(0, &mut bad).unwrap();
        crate::varint::encode(0, &mut bad).unwrap();
        crate::varint::encode(1, &mut bad).unwrap();
        bad.push(0xff);
        assert_eq!(decode_objects(&bad), Err(Error::BadPropertyBlock));
    }

    #[test]
    fn reader_accepts_every_split() {
        let mut objects = vec![sample(), sample()];
        objects[1].object_id = 3;
        objects[1].public.push(Property::VideoFrameMarking(
            FrameMark {
                start: true,
                end: true,
                independent: true,
                discardable: false,
                base_layer_sync: false,
                temporal_id: 0,
                spatial_id: None,
                tl0_pic_idx: None,
                extra: None,
            }
            .to_bytes(),
        ));
        let bytes = encode_objects(&objects).unwrap();
        for split in 0..=bytes.len() {
            let mut reader = ObjectReader::new();
            let first = reader.push(&bytes[..split]).unwrap();
            let second = reader.push(&bytes[split..]).unwrap();
            reader.finish().unwrap();
            let mut got = first;
            got.extend(second);
            assert_eq!(got, objects, "split {split}");
        }
    }

    #[test]
    fn odd_even_mismatch_is_rejected() {
        let object = Object {
            group_id: 0,
            subgroup_id: 0,
            object_id: 0,
            public: vec![Property::Unknown {
                id: 0x02,
                value: PropertyValue::Bytes(vec![1]),
            }],
            private: Vec::new(),
            payload: Vec::new(),
        };
        let mut buf = Vec::new();
        assert_eq!(
            encode_object(&object, &mut buf),
            Err(Error::BadProperty { id: 0x02 })
        );
    }
}
