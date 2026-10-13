//! Opus audio over etherquic, framed as LOC objects.
//!
//! An Ogg Opus file is split into packets. Each packet becomes one
//! [LOC](https://github.com/moq-wg/loc) object: timescale 48000, the Opus
//! identification header as the audio config, and the granule position of
//! the packet as the timestamp. Those objects are what
//! `etherquic-audio-send` writes on a unidirectional QUIC stream.
//! `etherquic-audio-recv` reads it back. [`VirtualSound`] is the loopback
//! cable for a machine with no audio device.
//!
//! End-trim, the Ogg crop of the last page, is not a LOC property. A file
//! that comes back out keeps the whole last packet. Comment tags are
//! replaced with a short vendor string.

#![forbid(unsafe_code)]

mod error;
mod ogg;
mod opus;
mod sound;

pub use error::Error;
pub use ogg::{demux as demux_opus, mux as mux_opus};
pub use opus::{
    from_objects, id_header as opus_id_header, packet_samples, parse_id, to_objects, OpusId,
    OpusPacket, OpusStream,
};
pub use sound::{Capture, Playback, VirtualSound, DEFAULT_CABLE};

use etherquic_container::{decode_objects, encode_objects, Object};

/// Service id for the audio tools. Video uses 4433, so both can share a segment.
pub const DEFAULT_SERVICE_ID: u16 = 4434;

/// Read an Ogg Opus file, or a LOC object stream when the bytes are not Ogg.
pub fn load(bytes: &[u8]) -> Result<Vec<Object>, Error> {
    load_as(bytes, None)
}

/// `format` is `"opus"`, `"ogg"`, `"loc"`, or `None` to detect Ogg by the
/// `OggS` capture pattern.
pub fn load_as(bytes: &[u8], format: Option<&str>) -> Result<Vec<Object>, Error> {
    match format {
        Some("loc") => load_loc(bytes),
        Some("opus") | Some("ogg") => load_opus(bytes),
        None if bytes.starts_with(b"OggS") => load_opus(bytes),
        None => load_loc(bytes),
        Some(other) => Err(Error::Opus(format!(
            "unknown format '{other}', expected opus or loc"
        ))),
    }
}

fn load_opus(bytes: &[u8]) -> Result<Vec<Object>, Error> {
    if !bytes.starts_with(b"OggS") {
        return Err(Error::Opus("expected an Ogg Opus file".into()));
    }
    let stream = demux_opus(bytes)?;
    if stream.packets.is_empty() {
        return Err(Error::Opus("Ogg Opus file has no audio packets".into()));
    }
    Ok(to_objects(&stream))
}

fn load_loc(bytes: &[u8]) -> Result<Vec<Object>, Error> {
    if bytes.is_empty() {
        return Err(Error::Opus("file is empty".into()));
    }
    let objects = decode_objects(bytes)?;
    if objects.is_empty() {
        return Err(Error::Opus("LOC stream has no objects".into()));
    }
    Ok(objects)
}

/// Write LOC bytes, or remux the objects to Ogg Opus.
pub fn store(objects: &[Object], as_loc: bool) -> Result<Vec<u8>, Error> {
    if as_loc {
        return Ok(encode_objects(objects)?);
    }
    mux_opus(&from_objects(objects)?)
}

pub fn summary(objects: &[Object]) -> String {
    match from_objects(objects) {
        Ok(stream) => {
            let samples: u64 = stream
                .packets
                .iter()
                .map(|packet| u64::from(packet.samples))
                .sum();
            format!(
                "opus channels={} rate={} pre_skip={} packets={} samples={}",
                stream.id.channels,
                stream.id.input_sample_rate,
                stream.id.pre_skip,
                stream.packets.len(),
                samples
            )
        }
        Err(_) => format!("{} LOC object(s)", objects.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ogg_loc_ogg_keeps_packets() {
        let original = OpusStream {
            id: parse_id(&opus_id_header(1, 312, 48_000)).unwrap(),
            packets: vec![
                OpusPacket {
                    data: vec![0x98],
                    start_granule: 0,
                    samples: 960,
                },
                OpusPacket {
                    data: vec![0x98, 1],
                    start_granule: 960,
                    samples: 960,
                },
            ],
            end_trim: 40,
        };
        let objects = load(&mux_opus(&original).unwrap()).unwrap();
        let loc = store(&objects, true).unwrap();
        let again = load_as(&loc, Some("loc")).unwrap();
        let remuxed = demux_opus(&store(&again, false).unwrap()).unwrap();
        assert_eq!(remuxed.packets, original.packets);
        assert_eq!(remuxed.id.pre_skip, 312);
        assert_eq!(remuxed.end_trim, 0);
        assert!(summary(&objects).contains("packets=2"));
    }
}
