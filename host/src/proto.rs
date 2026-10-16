//! Play-session bytes shared by the service and the desktop helper.
//!
//! Picture and audio use the same records as `etherquic-play`: JPEG on an
//! `EVID` stream, and interleaved `i16` PCM on an `EAUD` stream. Input is an
//! EQIN document. The helper and the service exchange those payloads on a
//! named pipe, one length-prefixed message at a time.

#![forbid(unsafe_code)]

use jpeg_encoder::{ColorType, Encoder};

pub const VIDEO_MAGIC: &[u8; 4] = b"EVID";
pub const AUDIO_MAGIC: &[u8; 4] = b"EAUD";
pub const INPUT_MAGIC: &[u8; 4] = b"EINP";

/// Desk helper capture rate. The service encodes; at 30 fps a 1280-wide
/// frame is about 83 MB/s on the pipe.
pub const CAPTURE_FPS: u32 = 30;

pub const KIND_JPEG: u8 = 1;
pub const KIND_PCM: u8 = 2;
pub const KIND_NOTE: u8 = 3;
pub const KIND_EQIN: u8 = 4;
pub const KIND_ACK: u8 = 5;
pub const KIND_WAKE: u8 = 6;
pub const KIND_RELEASE: u8 = 7;
/// Helper to service: one raw RGB frame, `u32le` width and height first.
pub const KIND_RAW: u8 = 8;
/// Helper to service: the cursor record body.
pub const KIND_CURSOR: u8 = 9;

/// Same 15 bytes as `etherquic-play`'s `EACK` record.
pub const ACK_KEY: u8 = 1;
pub const ACK_POINTER: u8 = 2;
pub const ACK_OTHER: u8 = 3;
pub const ACK_APPLIED: u8 = 0;
pub const ACK_IGNORED: u8 = 1;
pub const ACK_FAILED: u8 = 2;

/// Matches the play session's record cap.
pub const MAX_PAYLOAD: usize = 12 * 1024 * 1024;

/// Packed RGB. `rgb` has `width * height` pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Rgb {
    /// The [`Message::Raw`] payload.
    pub fn raw_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.rgb.len());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.rgb);
        out
    }

    pub fn from_raw(payload: &[u8]) -> Result<Rgb, String> {
        if payload.len() < 8 {
            return Err("raw frame is short".into());
        }
        let width = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let height = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
        let rgb = payload[8..].to_vec();
        if width == 0 || height == 0 || rgb.len() != width as usize * height as usize * 3 {
            return Err(format!(
                "raw frame {width}x{height} has {} bytes",
                rgb.len()
            ));
        }
        Ok(Rgb { width, height, rgb })
    }
}

/// Interleaved little-endian PCM. Same layout as `etherquic-play`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioChunk {
    pub sample_rate: u32,
    pub channels: u16,
    pub pcm: Vec<i16>,
}

impl AudioChunk {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.channels == 0 {
            return Err("audio has no channels".into());
        }
        if self.pcm.len() % self.channels as usize != 0 {
            return Err("audio length is not a whole number of frames".into());
        }
        let mut out = Vec::with_capacity(6 + self.pcm.len() * 2);
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        for sample in &self.pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 6 {
            return Err("audio record is short".into());
        }
        let sample_rate = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let channels = u16::from_le_bytes([bytes[4], bytes[5]]);
        if channels == 0 {
            return Err("audio has no channels".into());
        }
        let pcm_bytes = &bytes[6..];
        if pcm_bytes.len() % 2 != 0 {
            return Err("audio pcm is not i16".into());
        }
        let pcm: Vec<i16> = pcm_bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        if pcm.len() % channels as usize != 0 {
            return Err("audio length is not a whole number of frames".into());
        }
        Ok(Self {
            sample_rate,
            channels,
            pcm,
        })
    }
}

/// What crosses the desktop pipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Jpeg(Vec<u8>),
    Pcm(Vec<u8>),
    Note(String),
    Eqin(Vec<u8>),
    /// Fifteen-byte input confirmation, desk helper to service.
    Ack(Vec<u8>),
    /// Service to helper: a player is attached. Follow the input desktop,
    /// wake the display, and keep it on until `Release`.
    Wake,
    /// Service to helper: the player left. Let the display idle again.
    Release,
    /// Helper to service: width u32le, height u32le, packed RGB. The service
    /// encodes it with the codec the player negotiated.
    Raw(Vec<u8>),
    /// Helper to service: the cursor record body.
    Cursor(Vec<u8>),
}

impl Message {
    pub fn parts(&self) -> (u8, &[u8]) {
        match self {
            Self::Jpeg(bytes) => (KIND_JPEG, bytes),
            Self::Pcm(bytes) => (KIND_PCM, bytes),
            Self::Note(text) => (KIND_NOTE, text.as_bytes()),
            Self::Eqin(bytes) => (KIND_EQIN, bytes),
            Self::Ack(bytes) => (KIND_ACK, bytes),
            Self::Wake => (KIND_WAKE, &[][..]),
            Self::Release => (KIND_RELEASE, &[][..]),
            Self::Raw(bytes) => (KIND_RAW, bytes),
            Self::Cursor(bytes) => (KIND_CURSOR, bytes),
        }
    }

    pub fn decode(kind: u8, payload: &[u8]) -> Result<Self, String> {
        if payload.len() > MAX_PAYLOAD {
            return Err(format!("desk message is {} bytes", payload.len()));
        }
        Ok(match kind {
            KIND_JPEG => Self::Jpeg(payload.to_vec()),
            KIND_PCM => Self::Pcm(payload.to_vec()),
            KIND_NOTE => Self::Note(String::from_utf8_lossy(payload).into_owned()),
            KIND_EQIN => Self::Eqin(payload.to_vec()),
            KIND_ACK => Self::Ack(payload.to_vec()),
            KIND_WAKE => Self::Wake,
            KIND_RELEASE => Self::Release,
            KIND_RAW => Self::Raw(payload.to_vec()),
            KIND_CURSOR => Self::Cursor(payload.to_vec()),
            other => return Err(format!("unknown desk message {other}")),
        })
    }
}

/// Five-byte header plus payload. The pipe writer sends this in one direction.
pub fn frame(kind: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() > MAX_PAYLOAD {
        return Err(format!("desk message is {} bytes", payload.len()));
    }
    let len = payload.len() as u32;
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Scale `rgba` so the width is at most `max_width`, and both sides are even.
pub fn fit_rgba(rgba: &[u8], width: u32, height: u32, max_width: u32) -> Result<Rgb, String> {
    if width == 0 || height == 0 {
        return Err("empty picture".into());
    }
    let expect = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "picture is too large".to_string())?;
    if rgba.len() < expect {
        return Err("rgba buffer is short".into());
    }
    let max_width = max_width.max(2);
    let mut tw = width.min(max_width);
    tw -= tw % 2;
    let mut th = ((u64::from(height) * u64::from(tw)) / u64::from(width)).max(2) as u32;
    th -= th % 2;
    if th == 0 {
        th = 2;
    }
    if tw == width && th == height {
        let mut rgb = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for px in rgba[..expect].chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        return Ok(Rgb { width, height, rgb });
    }
    let mut rgb = vec![0u8; (tw as usize) * (th as usize) * 3];
    for y in 0..th {
        let sy = (u64::from(y) * u64::from(height)) / u64::from(th);
        for x in 0..tw {
            let sx = (u64::from(x) * u64::from(width)) / u64::from(tw);
            let si = ((sy * u64::from(width) + sx) * 4) as usize;
            let di = ((y * tw + x) * 3) as usize;
            rgb[di..di + 3].copy_from_slice(&rgba[si..si + 3]);
        }
    }
    Ok(Rgb {
        width: tw,
        height: th,
        rgb,
    })
}

pub fn encode_jpeg(picture: &Rgb, quality: u8) -> Result<Vec<u8>, String> {
    let width = u16::try_from(picture.width).map_err(|_| "width does not fit".to_string())?;
    let height = u16::try_from(picture.height).map_err(|_| "height does not fit".to_string())?;
    let expect = (picture.width as usize) * (picture.height as usize) * 3;
    if picture.rgb.len() < expect {
        return Err("rgb buffer is short".into());
    }
    let mut out = Vec::new();
    Encoder::new(&mut out, quality.clamp(1, 100))
        .encode(&picture.rgb[..expect], width, height, ColorType::Rgb)
        .map_err(|error| error.to_string())?;
    Ok(out)
}

/// EQIN `0..=65535` onto a screen. The center lands on the center pixel.
pub fn place_pointer(x: u16, y: u16, width: u32, height: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    (axis(x, width), axis(y, height))
}

/// Stamp, kind, code, result, detail. The player matches this to the event it sent.
pub fn encode_input_ack(time_us: u64, kind: u8, code: u32, result: u8, detail: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(15);
    out.extend_from_slice(&time_us.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(&code.to_le_bytes());
    out.push(result);
    out.push(detail);
    out
}

fn axis(pos: u16, extent: u32) -> u32 {
    let scaled = (u64::from(pos) * u64::from(extent) + 32_767) / 65_535;
    u32::try_from(scaled).unwrap_or(extent - 1).min(extent - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_chunk_round_trips() {
        let chunk = AudioChunk {
            sample_rate: 48_000,
            channels: 2,
            pcm: vec![1, -1, 2, -2],
        };
        let back = AudioChunk::decode(&chunk.encode().unwrap()).unwrap();
        assert_eq!(back, chunk);
    }

    #[test]
    fn note_frame_keeps_kind_and_text() {
        let bytes = frame(KIND_NOTE, b"applied pointer 1 2").unwrap();
        assert_eq!(bytes[0], KIND_NOTE);
        assert_eq!(u32::from_le_bytes(bytes[1..5].try_into().unwrap()), 19);
        let msg = Message::decode(bytes[0], &bytes[5..]).unwrap();
        assert_eq!(msg, Message::Note("applied pointer 1 2".into()));
    }

    #[test]
    fn input_ack_is_fifteen_bytes() {
        let bytes = encode_input_ack(7, ACK_KEY, 0xffeb, ACK_APPLIED, 1);
        assert_eq!(bytes.len(), 15);
        assert_eq!(u64::from_le_bytes(bytes[..8].try_into().unwrap()), 7);
        assert_eq!(bytes[8], ACK_KEY);
        let msg = Message::decode(KIND_ACK, &bytes).unwrap();
        assert_eq!(msg, Message::Ack(bytes));
    }

    #[test]
    fn wake_and_release_are_empty_frames() {
        let rgb = Rgb {
            width: 2,
            height: 2,
            rgb: (0..12).collect(),
        };
        assert_eq!(Rgb::from_raw(&rgb.raw_payload()).unwrap(), rgb);
        assert!(Rgb::from_raw(&[2, 0, 0, 0, 2, 0, 0, 0, 1]).is_err());
        for (message, kind) in [(Message::Wake, KIND_WAKE), (Message::Release, KIND_RELEASE)] {
            let (got, payload) = message.parts();
            assert_eq!(got, kind);
            assert!(payload.is_empty());
            let bytes = frame(got, payload).unwrap();
            assert_eq!(bytes, vec![kind, 0, 0, 0, 0]);
            assert_eq!(Message::decode(bytes[0], &bytes[5..]).unwrap(), message);
        }
    }

    #[test]
    fn cursor_message_keeps_its_bytes() {
        let message = Message::Cursor(vec![1, 2, 3, 4]);
        let (kind, payload) = message.parts();
        assert_eq!(kind, KIND_CURSOR);
        let bytes = frame(kind, payload).unwrap();
        assert_eq!(Message::decode(bytes[0], &bytes[5..]).unwrap(), message);
    }

    fn pointer_center_is_the_middle_pixel() {
        assert_eq!(place_pointer(32_767, 32_768, 1920, 1080), (960, 540));
    }

    #[test]
    fn jpeg_of_a_solid_frame_is_non_empty() {
        let rgba = [
            255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        let picture = fit_rgba(&rgba, 2, 2, 1280).unwrap();
        assert_eq!((picture.width, picture.height), (2, 2));
        let jpeg = encode_jpeg(&picture, 40).unwrap();
        assert!(jpeg.len() > 2);
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }
}
