//! Low Overhead Media Container (LOC) objects.
//!
//! This follows [draft-ietf-moq-loc](https://github.com/moq-wg/loc) (20 July 2026).
//! Public properties use the registered type IDs. The payload of an object is
//! the elementary bitstream of one encoded chunk: an Opus packet, or one
//! video frame. See [`object`] for the byte layout used on an etherquic stream.

#![forbid(unsafe_code)]

mod error;
mod object;
mod property;
mod varint;
mod video;

pub use error::Error;
pub use object::{
    decode_object, decode_objects, encode_object, encode_objects, Object, ObjectReader,
};
pub use property::{id, AudioLevel, Property, PropertyValue};
pub use varint::{decode as decode_varint, encode as encode_varint};
pub use video::{video_frame, FrameMark};
