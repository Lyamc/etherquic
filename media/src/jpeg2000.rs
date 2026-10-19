//! Motion JPEG 2000: every picture a JPEG 2000 codestream (raw J2K).
//!
//! Quality 1–99 maps to an OpenJPEG compression ratio on the irreversible
//! 9/7 path. 100 is lossless 5/3. MJPEG stays the negotiation floor; this
//! row can be switched off.

use std::num::NonZeroUsize;

use openjp2::{
    ColorSpace, DecodeOptions, Decoder, EncodeOptions, Encoder, Format, Image, OutputFormat,
};

use crate::negotiate::clamp_quality;
use crate::Frame;

/// OpenJPEG rate: uncompressed size / coded size. Higher is smaller.
fn rate(quality: u8) -> f32 {
    let q = f32::from(clamp_quality(quality));
    (2.0 + (100.0 - q) * 0.35).clamp(1.0, 80.0)
}

fn encode_options(quality: u8) -> EncodeOptions {
    let quality = clamp_quality(quality);
    let lossless = quality >= 100;
    EncodeOptions {
        format: Format::J2k,
        threads: 0,
        irreversible: !lossless,
        use_mct: true,
        rates: if lossless {
            Vec::new()
        } else {
            vec![rate(quality)]
        },
        num_resolutions: None,
    }
}

pub fn encode(frame: &Frame, quality: u8) -> Result<Vec<u8>, String> {
    frame.check()?;
    let image =
        Image::from_interleaved_u8(frame.width, frame.height, ColorSpace::Srgb, 3, &frame.rgb)
            .map_err(|error| error.to_string())?;
    Encoder::encode(&image, &encode_options(quality)).map_err(|error| error.to_string())
}

fn sample_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

pub fn decode(bytes: &[u8]) -> Result<Frame, String> {
    let packed = Decoder::decode_to_u8(
        bytes,
        DecodeOptions {
            format: Format::Auto,
            threads: 0,
            output: OutputFormat::Interleaved8 {
                channels: NonZeroUsize::new(3).unwrap(),
            },
            ..DecodeOptions::default()
        },
    );
    if let Ok(image) = packed {
        let frame = Frame {
            width: image.width,
            height: image.height,
            rgb: image.pixels,
        };
        frame.check()?;
        return Ok(frame);
    }
    let format = Format::detect(bytes).unwrap_or(Format::J2k);
    let image = Decoder::new(format)
        .map_err(|error| error.to_string())?
        .decode_slice(bytes)
        .map_err(|error| error.to_string())?;
    let width = image.width;
    let height = image.height;
    let n = (width as usize)
        .checked_mul(height as usize)
        .ok_or("jpeg2000 picture is too large")?;
    let rgb = match image.components.as_slice() {
        [y] => {
            if y.data.len() != n {
                return Err("jpeg2000 luma size mismatch".into());
            }
            y.data
                .iter()
                .flat_map(|s| {
                    let v = sample_u8(*s);
                    [v, v, v]
                })
                .collect()
        }
        [r, g, b, ..] => {
            if r.data.len() != n || g.data.len() != n || b.data.len() != n {
                return Err("jpeg2000 rgb size mismatch".into());
            }
            let mut rgb = Vec::with_capacity(n * 3);
            for i in 0..n {
                rgb.push(sample_u8(r.data[i]));
                rgb.push(sample_u8(g.data[i]));
                rgb.push(sample_u8(b.data[i]));
            }
            rgb
        }
        _ => return Err("jpeg2000 has no components".into()),
    };
    let frame = Frame { width, height, rgb };
    frame.check()?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(width: u32, height: u32) -> Frame {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(&[(x * 7) as u8, (y * 5) as u8, ((x + y) * 3) as u8]);
            }
        }
        Frame { width, height, rgb }
    }

    #[test]
    fn lossy_round_trip_keeps_size_and_tracks_quality() {
        let frame = patch(64, 48);
        let low = encode(&frame, 7).expect("encode q7");
        let high = encode(&frame, 80).expect("encode q80");
        assert!(low.len() < high.len(), "{} vs {}", low.len(), high.len());
        let back = decode(&high).expect("decode");
        assert_eq!(back.width, 64);
        assert_eq!(back.height, 48);
        assert_eq!(back.rgb.len(), 64 * 48 * 3);
    }
}
