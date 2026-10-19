//! 128×128 start-menu crop used to preview picture quality.
//!
//! The source is a bundled drawing of default desktop chrome (Explorer,
//! Firefox, Settings), not a capture of this computer. Encode/decode at the
//! current quality and colour, then estimate bitrate at seven common
//! resolutions. Other sizes interpolate those anchors by pixel area.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use etherquic::negotiate::{scale_budget, YuvFormat};
use etherquic_play::etherquic_media::{ffmpeg, jpeg, jpeg2000, pyrowave, yuv, Frame};
use etherquic_play::{CodecId, PicturePref};

pub const WIDTH: u32 = 128;
pub const HEIGHT: u32 = 128;

/// Common sizes used as bitrate anchors. Interpolation for other sizes
/// (800×600, 1600×900, …) is linear in pixel area between these, sorted by area.
pub const ANCHORS: [(u32, u32); 7] = [
    (640, 480),
    (1024, 768),
    (1600, 1200),
    (1280, 720),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub pref: PicturePref,
    pub quality: u8,
    pub yuv: YuvFormat,
    pub fps: u32,
}

pub struct Sample {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 1280×720 estimate, kept for tests and a single-number summary.
    #[allow(dead_code)]
    pub kbps: u32,
    pub line: String,
    /// Anchor bitrates, sorted by pixel area: (width, height, kbps).
    pub rates: [(u32, u32, u32); 7],
}

struct PyroPair {
    format: YuvFormat,
    encoder: pyrowave::Encoder,
    decoder: pyrowave::Decoder,
}

static PYRO: Mutex<Option<PyroPair>> = Mutex::new(None);
static PYRO_FAIL: AtomicBool = AtomicBool::new(false);

pub fn source_frame() -> Frame {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/preview-128.rgb"));
    assert!(bytes.len() >= 8, "quality preview is empty");
    let width = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let height = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let rgb = bytes[8..].to_vec();
    let frame = Frame {
        width,
        height,
        rgb,
    };
    frame.check().expect("quality preview rgb");
    frame
}

pub fn simulate(key: Key) -> Sample {
    let quality = key.quality.clamp(1, 100);
    let fps = key.fps.max(1);
    let source = source_frame();
    let codec = key.pref.codec();
    let (decoded, coded) = match roundtrip(&source, codec, quality, key.yuv) {
        Ok(pair) => pair,
        Err(_) => yuv_then_jpeg(&source, quality, key.yuv),
    };
    let rates = anchor_rates(codec, quality, fps, coded);
    let kbps = interpolate_kbps(&rates, 1280, 720);
    Sample {
        rgb: decoded.rgb,
        width: decoded.width,
        height: decoded.height,
        kbps,
        line: format!("{} · {fps} fps", key.pref.name()),
        rates,
    }
}

/// Estimates at the seven anchors, sorted by pixel area.
pub fn anchor_rates(codec: CodecId, quality: u8, fps: u32, coded: usize) -> [(u32, u32, u32); 7] {
    let mut rates = ANCHORS.map(|(w, h)| (w, h, estimated_kbps(codec, quality, fps, coded, w, h)));
    rates.sort_by_key(|(w, h, _)| u64::from(*w) * u64::from(*h));
    rates
}

/// Bitrate for an exact anchor, or a linear interpolation in pixel area.
pub fn interpolate_kbps(rates: &[(u32, u32, u32)], width: u32, height: u32) -> u32 {
    kbps_at(rates, width, height)
}

fn kbps_at(rates: &[(u32, u32, u32)], width: u32, height: u32) -> u32 {
    let area = u64::from(width) * u64::from(height);
    if rates.is_empty() {
        return 1;
    }
    if let Some((_, _, kbps)) = rates.iter().find(|(w, h, _)| *w == width && *h == height) {
        return *kbps;
    }
    let mut ordered: Vec<(u64, u32)> = rates
        .iter()
        .map(|(w, h, kbps)| (u64::from(*w) * u64::from(*h), *kbps))
        .collect();
    ordered.sort_by_key(|(a, _)| *a);
    if area <= ordered[0].0 {
        return ordered[0].1;
    }
    let last = *ordered.last().unwrap();
    if area >= last.0 {
        return last.1;
    }
    for pair in ordered.windows(2) {
        let (a0, k0) = pair[0];
        let (a1, k1) = pair[1];
        if area >= a0 && area <= a1 {
            if a1 == a0 {
                return k0;
            }
            let t = (area - a0) as f64 / (a1 - a0) as f64;
            let kbps = k0 as f64 + t * (k1 as f64 - k0 as f64);
            return kbps.round().clamp(1.0, 400_000.0) as u32;
        }
    }
    last.1
}

pub fn format_kbps(kbps: u32) -> String {
    if kbps >= 1000 {
        let mbits = kbps as f64 / 1000.0;
        if mbits >= 100.0 {
            format!("{mbits:.0} Mbit/s")
        } else {
            format!("{mbits:.1} Mbit/s")
        }
    } else {
        format!("{kbps} kbit/s")
    }
}

fn estimated_kbps(codec: CodecId, quality: u8, fps: u32, coded: usize, width: u32, height: u32) -> u32 {
    match codec {
        CodecId::Mjpeg | CodecId::Mj2k => scaled_sample_kbps(coded, fps, width, height),
        CodecId::Pyrowave => {
            let bytes = scale_budget(
                pyrowave::default_frame_bytes(width, height) as u32,
                quality,
            ) as usize;
            ((bytes.max(16 * 1024) as u64 * 8 * u64::from(fps)) / 1000) as u32
        }
        other => scale_budget(
            ffmpeg::default_kbps(negotiate_codec(other), width, height, fps),
            quality,
        ),
    }
}

fn scaled_sample_kbps(coded: usize, fps: u32, width: u32, height: u32) -> u32 {
    let area = u64::from(width) * u64::from(height);
    let sample = u64::from(WIDTH) * u64::from(HEIGHT);
    let bits = (coded as u64) * 8 * u64::from(fps) * area / sample.max(1);
    (bits / 1000).clamp(1, 400_000) as u32
}

fn negotiate_codec(codec: CodecId) -> etherquic::negotiate::VideoCodec {
    match codec {
        CodecId::Av1 => etherquic::negotiate::VideoCodec::Av1,
        CodecId::H265 => etherquic::negotiate::VideoCodec::H265,
        CodecId::H264 => etherquic::negotiate::VideoCodec::H264,
        CodecId::Pyrowave => etherquic::negotiate::VideoCodec::Pyrowave,
        CodecId::JpegXs => etherquic::negotiate::VideoCodec::JpegXs,
        CodecId::OpenApv => etherquic::negotiate::VideoCodec::OpenApv,
        CodecId::Mj2k => etherquic::negotiate::VideoCodec::Mj2k,
        CodecId::Mjpeg => etherquic::negotiate::VideoCodec::Mjpeg,
    }
}

fn roundtrip(
    source: &Frame,
    codec: CodecId,
    quality: u8,
    yuv: YuvFormat,
) -> Result<(Frame, usize), String> {
    match codec {
        CodecId::Mjpeg => {
            let bytes = jpeg::encode(source, quality)?;
            let frame = jpeg::decode(&bytes)?;
            Ok((frame, bytes.len()))
        }
        CodecId::Mj2k => {
            let bytes = jpeg2000::encode(source, quality)?;
            let frame = jpeg2000::decode(&bytes)?;
            Ok((frame, bytes.len()))
        }
        CodecId::Pyrowave => pyro_roundtrip(source, quality, yuv),
        _ => Ok(yuv_then_jpeg(source, quality, yuv)),
    }
}

fn yuv_then_jpeg(source: &Frame, quality: u8, format: YuvFormat) -> (Frame, usize) {
    let mut planes = yuv::YuvPlanes::new(source.width as usize, source.height as usize, format);
    yuv::rgb_to_yuv(source, &mut planes);
    let converted = yuv::yuv_to_rgb(&planes);
    match jpeg::encode(&converted, quality) {
        Ok(bytes) => match jpeg::decode(&bytes) {
            Ok(frame) => (frame, bytes.len()),
            Err(_) => (converted, bytes.len()),
        },
        Err(_) => (converted, 0),
    }
}

fn pyro_roundtrip(source: &Frame, quality: u8, format: YuvFormat) -> Result<(Frame, usize), String> {
    if PYRO_FAIL.load(Ordering::Relaxed) {
        return Err("pyrowave preview unavailable".into());
    }
    let budget = scale_budget(
        pyrowave::default_frame_bytes(source.width, source.height) as u32,
        quality,
    ) as usize;
    let mut slot = PYRO.lock().unwrap_or_else(|err| err.into_inner());
    let pair = match slot.as_mut() {
        Some(pair) if pair.format == format => pair,
        _ => {
            let encoder = match pyrowave::Encoder::new(source.width, source.height, budget, format) {
                Ok(encoder) => encoder,
                Err(err) => {
                    PYRO_FAIL.store(true, Ordering::Relaxed);
                    return Err(err);
                }
            };
            let decoder = match pyrowave::Decoder::new(source.width, source.height, format) {
                Ok(decoder) => decoder,
                Err(err) => {
                    PYRO_FAIL.store(true, Ordering::Relaxed);
                    return Err(err);
                }
            };
            *slot = Some(PyroPair {
                format,
                encoder,
                decoder,
            });
            slot.as_mut().unwrap()
        }
    };
    pair.encoder.set_frame_bytes(budget);
    let bytes = pair.encoder.encode(source)?;
    let frame = pair.decoder.decode(&bytes)?;
    Ok((frame, bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic::negotiate::{YuvChroma, YuvDepth};

    fn key(pref: PicturePref, quality: u8) -> Key {
        Key {
            pref,
            quality,
            yuv: YuvFormat {
                chroma: YuvChroma::Yuv444,
                depth: YuvDepth::Depth8,
            },
            fps: 30,
        }
    }

    #[test]
    fn source_is_one_twenty_eight() {
        let frame = source_frame();
        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
        assert_eq!(
            frame.rgb.len(),
            WIDTH as usize * HEIGHT as usize * 3
        );
    }

    #[test]
    fn mjpeg_bitrate_rises_with_quality() {
        let low = simulate(key(PicturePref::Mjpeg, 10));
        let high = simulate(key(PicturePref::Mjpeg, 90));
        assert!(
            low.kbps < high.kbps,
            "low {} high {}",
            low.kbps,
            high.kbps
        );
        assert_eq!((low.width, low.height), (WIDTH, HEIGHT));
    }

    #[test]
    fn pyrowave_budget_scales_from_forty() {
        let at_default = estimated_kbps(CodecId::Pyrowave, 40, 30, 0, 1280, 720);
        let at_double = estimated_kbps(CodecId::Pyrowave, 80, 30, 0, 1280, 720);
        assert_eq!(at_double, at_default * 2);
    }

    #[test]
    fn bitrate_rises_with_area() {
        let rates = anchor_rates(CodecId::Pyrowave, 40, 30, 0);
        let vga = kbps_at(&rates, 640, 480);
        let hd = kbps_at(&rates, 1280, 720);
        let uhd = kbps_at(&rates, 3840, 2160);
        assert!(vga < hd, "vga {vga} hd {hd}");
        assert!(hd < uhd, "hd {hd} uhd {uhd}");
        assert_eq!(rates.len(), 7);
        let areas: Vec<u64> = rates
            .iter()
            .map(|(w, h, _)| u64::from(*w) * u64::from(*h))
            .collect();
        let mut sorted = areas.clone();
        sorted.sort();
        assert_eq!(areas, sorted);
    }

    #[test]
    fn in_between_sizes_interpolate_by_area() {
        let rates = anchor_rates(CodecId::H264, 40, 30, 0);
        let exact = interpolate_kbps(&rates, 1920, 1080);
        assert_eq!(exact, kbps_at(&rates, 1920, 1080));

        let hd = interpolate_kbps(&rates, 1280, 720);
        let uxga = interpolate_kbps(&rates, 1600, 1200);
        let wide = interpolate_kbps(&rates, 1600, 900);
        let area_hd = 1280u64 * 720;
        let area_uxga = 1600u64 * 1200;
        let area_wide = 1600u64 * 900;
        let t = (area_wide - area_hd) as f64 / (area_uxga - area_hd) as f64;
        let expect = (hd as f64 + t * (uxga as f64 - hd as f64)).round() as u32;
        assert_eq!(wide, expect);
        assert!(
            wide > hd && wide < uxga,
            "hd {hd} 1600×900 {wide} 1600×1200 {uxga}"
        );

        let vga = interpolate_kbps(&rates, 640, 480);
        let xga = interpolate_kbps(&rates, 1024, 768);
        let svga = interpolate_kbps(&rates, 800, 600);
        assert!(svga > vga && svga < xga, "vga {vga} 800×600 {svga} xga {xga}");
    }

    #[test]
    fn chroma_subsample_changes_the_crop() {
        let source = source_frame();
        let mut planes = yuv::YuvPlanes::new(
            source.width as usize,
            source.height as usize,
            YuvFormat {
                chroma: YuvChroma::Yuv420,
                depth: YuvDepth::Depth8,
            },
        );
        yuv::rgb_to_yuv(&source, &mut planes);
        let back = yuv::yuv_to_rgb(&planes);
        assert_ne!(back.rgb, source.rgb);
    }
}
