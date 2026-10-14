//! Pictures small enough to share a link with audio and input.
//!
//! [`encode_picture`] and [`decode_picture`] select the codec by identity.
//! MJPEG is the desktop session's codec. Intra is a second identity on that
//! same path. A bare JPEG payload (no prefix) is MJPEG, which is what the
//! current desktop session sends.

use jpeg_encoder::{ColorType, Encoder};
use zune_core::colorspace::ColorSpace;
use zune_jpeg::JpegDecoder;

use crate::Error;

/// Which encoder produced a picture payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureCodec {
    /// JPEG payload, no prefix. This is the desktop session.
    Mjpeg,
    /// Uncompressed intra frame with an `EINT` prefix. A second identity
    /// on the same encode and decode path as MJPEG.
    Intra,
}

const INTRA_MAGIC: &[u8; 4] = b"EINT";

/// Decoded picture. `rgb` is packed `R,G,B` with `width * height` pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Scale `rgba` so the width is at most `max_width`, and both sides are even.
pub fn fit_rgba(rgba: &[u8], width: u32, height: u32, max_width: u32) -> Result<Picture, Error> {
    if width == 0 || height == 0 {
        return Err(Error::Picture("empty picture".into()));
    }
    let expect = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| Error::Picture("picture is too large".into()))?;
    if rgba.len() < expect {
        return Err(Error::Picture("rgba buffer is short".into()));
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
        return Ok(Picture { width, height, rgb });
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
    Ok(Picture {
        width: tw,
        height: th,
        rgb,
    })
}

/// Encode `picture` with `codec`. `quality` applies to MJPEG.
pub fn encode_picture(
    codec: PictureCodec,
    picture: &Picture,
    quality: u8,
) -> Result<Vec<u8>, Error> {
    match codec {
        PictureCodec::Mjpeg => encode_jpeg(picture, quality),
        PictureCodec::Intra => encode_intra(picture),
    }
}

/// Decode a payload by the codec identity it carries.
///
/// An `EINT` prefix is [`PictureCodec::Intra`]. A JPEG start-of-image is
/// [`PictureCodec::Mjpeg`]. Anything else is refused.
pub fn decode_picture(bytes: &[u8]) -> Result<Picture, Error> {
    match picture_codec(bytes)? {
        PictureCodec::Mjpeg => decode_jpeg(bytes),
        PictureCodec::Intra => decode_intra(bytes),
    }
}

fn picture_codec(bytes: &[u8]) -> Result<PictureCodec, Error> {
    if bytes.starts_with(INTRA_MAGIC) {
        return Ok(PictureCodec::Intra);
    }
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        return Ok(PictureCodec::Mjpeg);
    }
    Err(Error::Picture("unknown picture codec".into()))
}

fn encode_intra(picture: &Picture) -> Result<Vec<u8>, Error> {
    if picture.width == 0 || picture.height == 0 {
        return Err(Error::Picture("empty picture".into()));
    }
    let expect = (picture.width as usize)
        .checked_mul(picture.height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(|| Error::Picture("picture is too large".into()))?;
    if picture.rgb.len() < expect {
        return Err(Error::Picture("rgb buffer is short".into()));
    }
    let mut out = Vec::with_capacity(12 + expect);
    out.extend_from_slice(INTRA_MAGIC);
    out.extend_from_slice(&picture.width.to_le_bytes());
    out.extend_from_slice(&picture.height.to_le_bytes());
    out.extend_from_slice(&picture.rgb[..expect]);
    Ok(out)
}

fn decode_intra(bytes: &[u8]) -> Result<Picture, Error> {
    if bytes.len() < 12 || &bytes[..4] != INTRA_MAGIC {
        return Err(Error::Picture("intra frame is short".into()));
    }
    let mut width_buf = [0u8; 4];
    let mut height_buf = [0u8; 4];
    width_buf.copy_from_slice(&bytes[4..8]);
    height_buf.copy_from_slice(&bytes[8..12]);
    let width = u32::from_le_bytes(width_buf);
    let height = u32::from_le_bytes(height_buf);
    if width == 0 || height == 0 {
        return Err(Error::Picture("empty picture".into()));
    }
    let expect = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(|| Error::Picture("picture is too large".into()))?;
    let rgb = bytes
        .get(12..12 + expect)
        .ok_or_else(|| Error::Picture("intra frame is short".into()))?;
    Ok(Picture {
        width,
        height,
        rgb: rgb.to_vec(),
    })
}

pub fn encode_jpeg(picture: &Picture, quality: u8) -> Result<Vec<u8>, Error> {
    let width =
        u16::try_from(picture.width).map_err(|_| Error::Picture("width does not fit".into()))?;
    let height =
        u16::try_from(picture.height).map_err(|_| Error::Picture("height does not fit".into()))?;
    let expect = (picture.width as usize) * (picture.height as usize) * 3;
    if picture.rgb.len() < expect {
        return Err(Error::Picture("rgb buffer is short".into()));
    }
    let mut out = Vec::new();
    Encoder::new(&mut out, quality.clamp(1, 100))
        .encode(&picture.rgb[..expect], width, height, ColorType::Rgb)
        .map_err(|error| Error::Picture(error.to_string()))?;
    Ok(out)
}

pub fn decode_jpeg(bytes: &[u8]) -> Result<Picture, Error> {
    let mut decoder = JpegDecoder::new(bytes);
    let pixels = decoder
        .decode()
        .map_err(|error| Error::Picture(error.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::Picture("jpeg has no header".into()))?;
    let width = info.width as u32;
    let height = info.height as u32;
    let rgb = match decoder.get_output_colorspace() {
        Some(ColorSpace::RGB) => pixels,
        Some(ColorSpace::Luma) => pixels.into_iter().flat_map(|y| [y, y, y]).collect(),
        other => {
            return Err(Error::Picture(format!(
                "unsupported jpeg colorspace {other:?}"
            )));
        }
    };
    let expect = (width as usize) * (height as usize) * 3;
    if rgb.len() < expect {
        return Err(Error::Picture("decoded jpeg is short".into()));
    }
    Ok(Picture { width, height, rgb })
}

/// Nearest-neighbor blit into `0x00RRGGBB` pixels. `dst` is `dst_w * dst_h`.
pub fn blit_rgb(picture: &Picture, dst: &mut [u32], dst_w: u32, dst_h: u32) {
    if picture.width == 0 || picture.height == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }
    let need = (dst_w as usize).saturating_mul(dst_h as usize);
    if dst.len() < need {
        return;
    }
    for y in 0..dst_h {
        let sy = (u64::from(y) * u64::from(picture.height)) / u64::from(dst_h);
        for x in 0..dst_w {
            let sx = (u64::from(x) * u64::from(picture.width)) / u64::from(dst_w);
            let si = ((sy * u64::from(picture.width) + sx) * 3) as usize;
            if si + 2 >= picture.rgb.len() {
                return;
            }
            let rgb = (u32::from(picture.rgb[si]) << 16)
                | (u32::from(picture.rgb[si + 1]) << 8)
                | u32::from(picture.rgb[si + 2]);
            dst[(y * dst_w + x) as usize] = rgb;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_round_trip_keeps_the_size_and_a_red_pixel() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        rgba[0] = 220;
        rgba[1] = 10;
        rgba[2] = 10;
        rgba[3] = 255;
        let fitted = fit_rgba(&rgba, 4, 4, 1280).unwrap();
        let jpeg = encode_jpeg(&fitted, 90).unwrap();
        let back = decode_jpeg(&jpeg).unwrap();
        assert_eq!((back.width, back.height), (4, 4));
        assert!(back.rgb[0] > 180, "red channel {}", back.rgb[0]);
        assert!(back.rgb[1] < 40, "green channel {}", back.rgb[1]);
    }

    #[test]
    fn fit_halves_an_overwide_picture() {
        let rgba = vec![0u8; 8 * 4 * 4];
        let picture = fit_rgba(&rgba, 8, 4, 4).unwrap();
        assert_eq!((picture.width, picture.height), (4, 2));
        assert_eq!(picture.rgb.len(), 4 * 2 * 3);
    }

    #[test]
    fn mjpeg_and_intra_round_trip_through_the_codec_dispatch() {
        let picture = Picture {
            width: 4,
            height: 2,
            rgb: vec![12; 4 * 2 * 3],
        };
        for codec in [PictureCodec::Mjpeg, PictureCodec::Intra] {
            let encoded = encode_picture(codec, &picture, 80).expect("encode");
            let back = decode_picture(&encoded).expect("decode");
            println!(
                "roundtrip {codec:?} bytes {} picture {}x{} rgb {}",
                encoded.len(),
                back.width,
                back.height,
                back.rgb.len()
            );
            assert!(!encoded.is_empty());
            assert!(!back.rgb.is_empty());
            assert_eq!((back.width, back.height), (picture.width, picture.height));
        }
    }

    #[test]
    fn blit_paints_the_top_left_pixel() {
        let picture = Picture {
            width: 1,
            height: 1,
            rgb: vec![1, 2, 3],
        };
        let mut dst = [0u32; 4];
        blit_rgb(&picture, &mut dst, 2, 2);
        assert_eq!(dst[0], (1 << 16) | (2 << 8) | 3);
    }
}
