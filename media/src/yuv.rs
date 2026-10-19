//! RGB to planar YUV and back, BT.601 full range. Both ends use this pair,
//! so the colours survive a PyroWave round trip unchanged in matrix.
//!
//! Chroma is saturation-weighted so a coloured UI pixel on a light desk keeps
//! more of its chroma than a box-average of RGB would. 4:2:2 and 4:2:0 average
//! along the subsampled axes; 4:4:4 keeps every chroma sample.

use crate::negotiate::{YuvChroma, YuvDepth, YuvFormat};
use crate::Frame;

/// Y, U, V planes of a [`Frame`] with even sides.
pub struct I420 {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl I420 {
    pub fn new(width: usize, height: usize) -> I420 {
        I420 {
            width,
            height,
            y: vec![0; width * height],
            u: vec![128; (width / 2) * (height / 2)],
            v: vec![128; (width / 2) * (height / 2)],
        }
    }
}

fn clamp(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn luma(r: i32, g: i32, b: i32) -> i32 {
    (77 * r + 150 * g + 29 * b + 128) >> 8
}

fn chroma_u(r: i32, g: i32, b: i32) -> i32 {
    ((-43 * r - 85 * g + 128 * b + 128) >> 8) + 128
}

fn chroma_v(r: i32, g: i32, b: i32) -> i32 {
    ((128 * r - 107 * g - 21 * b + 128) >> 8) + 128
}

fn chroma_mid(depth: YuvDepth) -> u16 {
    match depth {
        YuvDepth::Depth8 => 128,
        YuvDepth::Depth10 => 512,
    }
}

fn scale_sample(value8: i32, depth: YuvDepth) -> u16 {
    match depth {
        YuvDepth::Depth8 => value8.clamp(0, 255) as u16,
        YuvDepth::Depth10 => ((value8.clamp(0, 255) * 1023 + 127) / 255) as u16,
    }
}

fn sample8(value: u16, depth: YuvDepth) -> i32 {
    match depth {
        YuvDepth::Depth8 => value as i32,
        YuvDepth::Depth10 => ((value as i32 * 255 + 511) / 1023).clamp(0, 255),
    }
}

/// Width and height of a chroma plane for `chroma`.
pub fn chroma_size(width: usize, height: usize, chroma: YuvChroma) -> (usize, usize) {
    match chroma {
        YuvChroma::Yuv420 => (width / 2, height / 2),
        YuvChroma::Yuv422 => (width / 2, height),
        YuvChroma::Yuv444 => (width, height),
    }
}

/// Planar Y, U, V. 8-bit samples sit in the low 8 bits; 10-bit in the low 10.
pub struct YuvPlanes {
    pub width: usize,
    pub height: usize,
    pub format: YuvFormat,
    pub y: Vec<u16>,
    pub u: Vec<u16>,
    pub v: Vec<u16>,
}

impl YuvPlanes {
    pub fn new(width: usize, height: usize, format: YuvFormat) -> YuvPlanes {
        let (cw, ch) = chroma_size(width, height, format.chroma);
        let mid = chroma_mid(format.depth);
        YuvPlanes {
            width,
            height,
            format,
            y: vec![0; width * height],
            u: vec![mid; cw * ch],
            v: vec![mid; cw * ch],
        }
    }

    pub fn chroma_size(&self) -> (usize, usize) {
        chroma_size(self.width, self.height, self.format.chroma)
    }

    /// 4:2:2 becomes 4:4:4 by repeating each chroma sample horizontally. Other
    /// layouts are cloned.
    pub fn for_bitstream(&self) -> YuvPlanes {
        let bitstream = self.format.bitstream();
        if bitstream.chroma == self.format.chroma {
            return self.clone();
        }
        let mut out = YuvPlanes::new(self.width, self.height, bitstream);
        out.y.copy_from_slice(&self.y);
        let (cw, ch) = self.chroma_size();
        for row in 0..ch {
            for cx in 0..cw {
                let src = self.u[row * cw + cx];
                let dst = row * self.width + cx * 2;
                out.u[dst] = src;
                out.u[dst + 1] = src;
                let src = self.v[row * cw + cx];
                out.v[dst] = src;
                out.v[dst + 1] = src;
            }
        }
        out
    }
}

impl Clone for YuvPlanes {
    fn clone(&self) -> Self {
        YuvPlanes {
            width: self.width,
            height: self.height,
            format: self.format,
            y: self.y.clone(),
            u: self.u.clone(),
            v: self.v.clone(),
        }
    }
}

fn weighted_chroma(
    rgb: &[u8],
    w: usize,
    xs: &[usize],
    ys: &[usize],
    depth: YuvDepth,
) -> (u16, u16) {
    let mut u_acc = 0;
    let mut v_acc = 0;
    let mut wt_acc = 0;
    for &y in ys {
        for &x in xs {
            let i = (y * w + x) * 3;
            let r = rgb[i] as i32;
            let g = rgb[i + 1] as i32;
            let b = rgb[i + 2] as i32;
            let u = chroma_u(r, g, b);
            let v = chroma_v(r, g, b);
            let wt = 1 + (u - 128).abs() + (v - 128).abs();
            u_acc += u * wt;
            v_acc += v * wt;
            wt_acc += wt;
        }
    }
    (
        scale_sample((u_acc + wt_acc / 2) / wt_acc, depth),
        scale_sample((v_acc + wt_acc / 2) / wt_acc, depth),
    )
}

pub fn rgb_to_yuv(frame: &Frame, out: &mut YuvPlanes) {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if out.width != w || out.height != h {
        *out = YuvPlanes::new(w, h, out.format);
    }
    let depth = out.format.depth;
    let rgb = &frame.rgb;
    for row in 0..h {
        for x in 0..w {
            let i = (row * w + x) * 3;
            out.y[row * w + x] = scale_sample(
                luma(rgb[i] as i32, rgb[i + 1] as i32, rgb[i + 2] as i32),
                depth,
            );
        }
    }
    let (cw, ch) = out.chroma_size();
    match out.format.chroma {
        YuvChroma::Yuv444 => {
            for row in 0..h {
                for x in 0..w {
                    let i = (row * w + x) * 3;
                    let r = rgb[i] as i32;
                    let g = rgb[i + 1] as i32;
                    let b = rgb[i + 2] as i32;
                    out.u[row * w + x] = scale_sample(chroma_u(r, g, b), depth);
                    out.v[row * w + x] = scale_sample(chroma_v(r, g, b), depth);
                }
            }
        }
        YuvChroma::Yuv422 => {
            for row in 0..ch {
                for cx in 0..cw {
                    let (u, v) = weighted_chroma(rgb, w, &[cx * 2, cx * 2 + 1], &[row], depth);
                    out.u[row * cw + cx] = u;
                    out.v[row * cw + cx] = v;
                }
            }
        }
        YuvChroma::Yuv420 => {
            for cy in 0..ch {
                for cx in 0..cw {
                    let (u, v) = weighted_chroma(
                        rgb,
                        w,
                        &[cx * 2, cx * 2 + 1],
                        &[cy * 2, cy * 2 + 1],
                        depth,
                    );
                    out.u[cy * cw + cx] = u;
                    out.v[cy * cw + cx] = v;
                }
            }
        }
    }
}

pub fn yuv_to_rgb(planes: &YuvPlanes) -> Frame {
    let w = planes.width;
    let h = planes.height;
    let (cw, _) = planes.chroma_size();
    let depth = planes.format.depth;
    let mut rgb = vec![0u8; w * h * 3];
    for row in 0..h {
        for x in 0..w {
            let y = sample8(planes.y[row * w + x], depth);
            let (cx, cy) = match planes.format.chroma {
                YuvChroma::Yuv444 => (x, row),
                YuvChroma::Yuv422 => (x / 2, row),
                YuvChroma::Yuv420 => (x / 2, row / 2),
            };
            let u = sample8(planes.u[cy * cw + cx], depth) - 128;
            let v = sample8(planes.v[cy * cw + cx], depth) - 128;
            let i = (row * w + x) * 3;
            rgb[i] = clamp(y + ((359 * v + 128) >> 8));
            rgb[i + 1] = clamp(y - ((88 * u + 183 * v + 128) >> 8));
            rgb[i + 2] = clamp(y + ((454 * u + 128) >> 8));
        }
    }
    Frame {
        width: w as u32,
        height: h as u32,
        rgb,
    }
}

pub fn rgb_to_i420(frame: &Frame, out: &mut I420) {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if out.width != w || out.height != h {
        *out = I420::new(w, h);
    }
    let rgb = &frame.rgb;
    for row in 0..h {
        let line = &rgb[row * w * 3..(row + 1) * w * 3];
        let yline = &mut out.y[row * w..(row + 1) * w];
        for (x, px) in line.chunks_exact(3).enumerate() {
            yline[x] = clamp(luma(px[0] as i32, px[1] as i32, px[2] as i32));
        }
    }
    let cw = w / 2;
    for cy in 0..h / 2 {
        for cx in 0..cw {
            let mut u_acc = 0;
            let mut v_acc = 0;
            let mut wt_acc = 0;
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let i = ((cy * 2 + dy) * w + cx * 2 + dx) * 3;
                let r = rgb[i] as i32;
                let g = rgb[i + 1] as i32;
                let b = rgb[i + 2] as i32;
                let u = chroma_u(r, g, b);
                let v = chroma_v(r, g, b);
                let wt = 1 + (u - 128).abs() + (v - 128).abs();
                u_acc += u * wt;
                v_acc += v * wt;
                wt_acc += wt;
            }
            out.u[cy * cw + cx] = clamp((u_acc + wt_acc / 2) / wt_acc);
            out.v[cy * cw + cx] = clamp((v_acc + wt_acc / 2) / wt_acc);
        }
    }
}

pub fn i420_to_rgb(planes: &I420) -> Frame {
    let w = planes.width;
    let h = planes.height;
    let cw = w / 2;
    let mut rgb = vec![0u8; w * h * 3];
    for row in 0..h {
        for x in 0..w {
            let y = planes.y[row * w + x] as i32;
            let c = (row / 2) * cw + x / 2;
            let u = planes.u[c] as i32 - 128;
            let v = planes.v[c] as i32 - 128;
            let i = (row * w + x) * 3;
            rgb[i] = clamp(y + ((359 * v + 128) >> 8));
            rgb[i + 1] = clamp(y - ((88 * u + 183 * v + 128) >> 8));
            rgb[i + 2] = clamp(y + ((454 * u + 128) >> 8));
        }
    }
    Frame {
        width: w as u32,
        height: h as u32,
        rgb,
    }
}

/// Nearest-neighbour shrink so the width is at most `max_width` (and the
/// height at most `max_height` when that is set), sides even. A frame that
/// already fits with even sides is returned as it is.
pub fn fit(frame: &Frame, max_width: u32, max_height: u32) -> Frame {
    let mut w = frame.width;
    let mut h = frame.height;
    if max_width > 0 && w > max_width {
        h = ((h as u64 * max_width as u64) / w as u64) as u32;
        w = max_width;
    }
    if max_height > 0 && h > max_height {
        w = ((w as u64 * max_height as u64) / h as u64) as u32;
        h = max_height;
    }
    w = (w & !1).max(2);
    h = (h & !1).max(2);
    if w == frame.width && h == frame.height {
        return frame.clone();
    }
    let sw = frame.width as usize;
    let sh = frame.height as usize;
    let mut rgb = vec![0u8; w as usize * h as usize * 3];
    for y in 0..h as usize {
        let sy = (y * sh / h as usize).min(sh - 1);
        for x in 0..w as usize {
            let sx = (x * sw / w as usize).min(sw - 1);
            let s = (sy * sw + sx) * 3;
            let d = (y * w as usize + x) * 3;
            rgb[d..d + 3].copy_from_slice(&frame.rgb[s..s + 3]);
        }
    }
    Frame {
        width: w,
        height: h,
        rgb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_and_colours_round_trip_closely() {
        let mut rgb = Vec::new();
        for (r, g, b) in [(128u8, 128u8, 128u8), (255, 0, 0), (0, 255, 0), (0, 0, 255)] {
            for _ in 0..4 {
                rgb.extend_from_slice(&[r, g, b]);
            }
        }
        // 4 solid 2x2 blocks laid out as a 8x2 picture would mix colours, so
        // test each block as its own 2x2 frame.
        for block in rgb.chunks_exact(12) {
            let frame = Frame {
                width: 2,
                height: 2,
                rgb: block.to_vec(),
            };
            let mut planes = I420::new(2, 2);
            rgb_to_i420(&frame, &mut planes);
            let back = i420_to_rgb(&planes);
            for (a, b) in frame.rgb.iter().zip(back.rgb.iter()) {
                assert!(
                    (*a as i32 - *b as i32).abs() <= 3,
                    "{:?} -> {:?}",
                    frame.rgb,
                    back.rgb
                );
            }
        }
    }

    #[test]
    fn chroma_keeps_a_saturated_pixel_in_a_mixed_block() {
        // Three white, one red — coloured UI on a light desk.
        let frame = Frame {
            width: 2,
            height: 2,
            rgb: vec![255, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        };
        let mut planes = I420::new(2, 2);
        rgb_to_i420(&frame, &mut planes);
        // Solid red is V=255. A box-average of RGB lands V around 160.
        assert!(
            planes.v[0] > 200,
            "mixed-block V should stay close to red, got {}",
            planes.v[0]
        );
    }

    fn solid(r: u8, g: u8, b: u8) -> Frame {
        Frame {
            width: 4,
            height: 2,
            rgb: vec![r, g, b].repeat(8),
        }
    }

    #[test]
    fn every_yuv_layout_round_trips_a_solid_colour() {
        let frame = solid(200, 40, 16);
        for format in YuvFormat::ALL {
            let mut planes = YuvPlanes::new(4, 2, format);
            rgb_to_yuv(&frame, &mut planes);
            assert_eq!(planes.y.len(), 8);
            let (cw, ch) = planes.chroma_size();
            assert_eq!(planes.u.len(), cw * ch);
            let back = yuv_to_rgb(&planes);
            for (a, b) in frame.rgb.iter().zip(back.rgb.iter()) {
                assert!(
                    (*a as i32 - *b as i32).abs() <= 4,
                    "{format:?} {:?} -> {:?}",
                    frame.rgb,
                    back.rgb
                );
            }
            let bitstream = planes.for_bitstream();
            assert_eq!(bitstream.format.chroma, format.chroma.bitstream());
            if format.chroma == YuvChroma::Yuv422 {
                assert_eq!(bitstream.format.chroma, YuvChroma::Yuv444);
                assert_eq!(bitstream.u.len(), 4 * 2);
            }
        }
    }

    #[test]
    fn fit_keeps_even_sides() {
        let frame = Frame {
            width: 1921,
            height: 1081,
            rgb: vec![7; 1921 * 1081 * 3],
        };
        let out = fit(&frame, 1280, 0);
        assert_eq!((out.width, out.height), (1280, 720));
        let same = fit(
            &Frame {
                width: 4,
                height: 2,
                rgb: vec![0; 24],
            },
            1280,
            0,
        );
        assert_eq!((same.width, same.height), (4, 2));
    }
}
