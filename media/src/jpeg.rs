//! MJPEG: every picture a JPEG. The legacy `EVID` stream and the floor of
//! every negotiation.

use jpeg_encoder::{ColorType, Encoder};
use zune_core::colorspace::ColorSpace;
use zune_jpeg::JpegDecoder;

use crate::Frame;

pub const QUALITY: u8 = 40;

pub fn encode(frame: &Frame, quality: u8) -> Result<Vec<u8>, String> {
    frame.check()?;
    let width = u16::try_from(frame.width).map_err(|_| "picture too wide for JPEG")?;
    let height = u16::try_from(frame.height).map_err(|_| "picture too tall for JPEG")?;
    let mut out = Vec::new();
    Encoder::new(&mut out, quality)
        .encode(&frame.rgb, width, height, ColorType::Rgb)
        .map_err(|error| error.to_string())?;
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Frame, String> {
    let mut decoder = JpegDecoder::new(bytes);
    let pixels = decoder.decode().map_err(|error| error.to_string())?;
    let info = decoder.info().ok_or("jpeg has no header")?;
    let width = info.width as u32;
    let height = info.height as u32;
    let rgb = match decoder.get_output_colorspace() {
        Some(ColorSpace::RGB) => pixels,
        Some(ColorSpace::Luma) => pixels.into_iter().flat_map(|y| [y, y, y]).collect(),
        other => return Err(format!("unsupported jpeg colorspace {other:?}")),
    };
    let frame = Frame { width, height, rgb };
    frame.check()?;
    Ok(frame)
}
