//! RGB to planar 4:2:0 and back, BT.601 full range. Both ends use this pair,
//! so the colours survive a PyroWave round trip unchanged in matrix.

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
            let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
            yline[x] = clamp((77 * r + 150 * g + 29 * b + 128) >> 8);
        }
    }
    let cw = w / 2;
    for cy in 0..h / 2 {
        for cx in 0..cw {
            let mut r = 0;
            let mut g = 0;
            let mut b = 0;
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let i = ((cy * 2 + dy) * w + cx * 2 + dx) * 3;
                r += rgb[i] as i32;
                g += rgb[i + 1] as i32;
                b += rgb[i + 2] as i32;
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            out.u[cy * cw + cx] = clamp(((-43 * r - 85 * g + 128 * b + 128) >> 8) + 128);
            out.v[cy * cw + cx] = clamp(((128 * r - 107 * g - 21 * b + 128) >> 8) + 128);
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
    Frame { width: w as u32, height: h as u32, rgb }
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
    Frame { width: w, height: h, rgb }
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
            let frame = Frame { width: 2, height: 2, rgb: block.to_vec() };
            let mut planes = I420::new(2, 2);
            rgb_to_i420(&frame, &mut planes);
            let back = i420_to_rgb(&planes);
            for (a, b) in frame.rgb.iter().zip(back.rgb.iter()) {
                assert!((*a as i32 - *b as i32).abs() <= 3, "{:?} -> {:?}", frame.rgb, back.rgb);
            }
        }
    }

    #[test]
    fn fit_keeps_even_sides() {
        let frame = Frame { width: 1921, height: 1081, rgb: vec![7; 1921 * 1081 * 3] };
        let out = fit(&frame, 1280, 0);
        assert_eq!((out.width, out.height), (1280, 720));
        let same = fit(&Frame { width: 4, height: 2, rgb: vec![0; 24] }, 1280, 0);
        assert_eq!((same.width, same.height), (4, 2));
    }
}
