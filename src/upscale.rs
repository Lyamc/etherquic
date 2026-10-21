//! Client spatial upscalers: FSR 1 EASU+RCAS, and DLSS/XeSS-shaped presets.
//!
//! The player asks the host for 720p when a mode is on, then this module
//! fills the window. FSR 1 is implemented here (Catmull–Rom EASU tap plus
//! RCAS). DLSS and XeSS use the same spatial path with a vendor sharpness
//! until those runtimes are present.

use crate::negotiate::Limit;

/// Widest picture the host should send while a client upscaler is on.
pub const STREAM_WIDTH: u32 = 1280;
/// Tallest picture the host should send while a client upscaler is on.
pub const STREAM_HEIGHT: u32 = 720;

/// Which upscaler the player asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Upscale {
    #[default]
    Off,
    /// FSR on AMD and unknown GPUs, DLSS on NVIDIA, XeSS on Intel.
    Auto,
    Fsr,
    Dlss,
    Xess,
}

/// Spatial kernel actually run on a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Fsr,
    Dlss,
    Xess,
}

impl Upscale {
    pub const ALL: [Upscale; 5] = [
        Self::Off,
        Self::Auto,
        Self::Fsr,
        Self::Dlss,
        Self::Xess,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Fsr => "fsr",
            Self::Dlss => "dlss",
            Self::Xess => "xess",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Auto => "Auto",
            Self::Fsr => "FSR",
            Self::Dlss => "DLSS",
            Self::Xess => "XeSS",
        }
    }

    pub fn enabled(self) -> bool {
        self != Self::Off
    }

    /// 1280×720 while on, unlimited while off.
    pub fn stream_limit(self) -> Limit {
        if self.enabled() {
            Limit {
                width: STREAM_WIDTH,
                height: STREAM_HEIGHT,
                fps: 0,
            }
        } else {
            Limit::NONE
        }
    }

    pub fn kind(self) -> Option<Kind> {
        match self {
            Self::Off => None,
            Self::Fsr => Some(Kind::Fsr),
            Self::Dlss => Some(if crate::nvidia_present() {
                Kind::Dlss
            } else {
                Kind::Fsr
            }),
            Self::Xess => Some(if crate::intel_present() {
                Kind::Xess
            } else {
                Kind::Fsr
            }),
            Self::Auto => Some(if crate::nvidia_present() {
                Kind::Dlss
            } else if crate::intel_present() {
                Kind::Xess
            } else {
                Kind::Fsr
            }),
        }
    }
}

impl Kind {
    fn rcas_stop(self) -> i32 {
        match self {
            Self::Fsr => 48,
            Self::Dlss => 72,
            Self::Xess => 36,
        }
    }
}

/// Scale packed RGB into `0x00RRGGBB` pixels for the picture window.
pub fn blit_argb(
    rgb: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    kind: Kind,
) {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }
    let need = (dst_w as usize).saturating_mul(dst_h as usize);
    if dst.len() < need {
        return;
    }
    let expect = (src_w as usize).saturating_mul(src_h as usize).saturating_mul(3);
    if rgb.len() < expect {
        return;
    }
    if src_w == dst_w && src_h == dst_h {
        pack_argb(rgb, dst, need);
        rcas_argb(dst, dst_w, dst_h, kind);
        return;
    }
    let mut scaled = vec![0u8; need * 3];
    cubic_rgb(rgb, src_w, src_h, &mut scaled, dst_w, dst_h);
    pack_argb(&scaled, dst, need);
    rcas_argb(dst, dst_w, dst_h, kind);
}

fn pack_argb(rgb: &[u8], dst: &mut [u32], pixels: usize) {
    for i in 0..pixels {
        let o = i * 3;
        dst[i] = (u32::from(rgb[o]) << 16) | (u32::from(rgb[o + 1]) << 8) | u32::from(rgb[o + 2]);
    }
}

fn cubic_rgb(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let sw = sw as usize;
    let sh = sh as usize;
    let dw = dw as usize;
    let dh = dh as usize;
    let mut mid = vec![0u8; dw * sh * 3];
    for y in 0..sh {
        for x in 0..dw {
            let fx = ((x as u64 * sw as u64) << 8) / dw as u64;
            let ix = (fx >> 8) as i32;
            let t = (fx & 255) as i32;
            let wts = cubic_w(t);
            let mut r = 0i32;
            let mut g = 0i32;
            let mut b = 0i32;
            for k in 0..4 {
                let sx = (ix + k - 1).clamp(0, sw as i32 - 1) as usize;
                let i = (y * sw + sx) * 3;
                let w = wts[k as usize];
                r += i32::from(src[i]) * w;
                g += i32::from(src[i + 1]) * w;
                b += i32::from(src[i + 2]) * w;
            }
            let o = (y * dw + x) * 3;
            mid[o] = clamp8(r);
            mid[o + 1] = clamp8(g);
            mid[o + 2] = clamp8(b);
        }
    }
    for y in 0..dh {
        let fy = ((y as u64 * sh as u64) << 8) / dh as u64;
        let iy = (fy >> 8) as i32;
        let t = (fy & 255) as i32;
        let wts = cubic_w(t);
        for x in 0..dw {
            let mut r = 0i32;
            let mut g = 0i32;
            let mut b = 0i32;
            for k in 0..4 {
                let sy = (iy + k - 1).clamp(0, sh as i32 - 1) as usize;
                let i = (sy * dw + x) * 3;
                let w = wts[k as usize];
                r += i32::from(mid[i]) * w;
                g += i32::from(mid[i + 1]) * w;
                b += i32::from(mid[i + 2]) * w;
            }
            let o = (y * dw + x) * 3;
            dst[o] = clamp8(r);
            dst[o + 1] = clamp8(g);
            dst[o + 2] = clamp8(b);
        }
    }
}

/// Catmull–Rom weights for `t` in 0..256, 8-bit signed taps that sum to 256.
fn cubic_w(t: i32) -> [i32; 4] {
    let t2 = (t * t) >> 8;
    let t3 = (t2 * t) >> 8;
    [
        (-t3 + 2 * t2 - t) >> 1,
        (3 * t3 - 5 * t2 + 512) >> 1,
        (-3 * t3 + 4 * t2 + t) >> 1,
        (t3 - t2) >> 1,
    ]
}

fn clamp8(value: i32) -> u8 {
    (value >> 8).clamp(0, 255) as u8
}

fn rcas_argb(dst: &mut [u32], width: u32, height: u32, kind: Kind) {
    let w = width as usize;
    let h = height as usize;
    if w < 3 || h < 3 {
        return;
    }
    let stop = kind.rcas_stop();
    let copy = dst.to_vec();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let e = copy[i];
            let b = copy[(y - 1) * w + x];
            let d = copy[y * w + (x - 1)];
            let f = copy[y * w + (x + 1)];
            let n = copy[(y + 1) * w + x];
            dst[i] = rcas_pixel(e, b, d, f, n, stop);
        }
    }
}

fn rcas_pixel(e: u32, b: u32, d: u32, f: u32, h: u32, stop: i32) -> u32 {
    let ch = |shift: u32| -> u8 {
        let ev = ((e >> shift) & 255) as i32;
        let bv = ((b >> shift) & 255) as i32;
        let dv = ((d >> shift) & 255) as i32;
        let fv = ((f >> shift) & 255) as i32;
        let hv = ((h >> shift) & 255) as i32;
        let mn = ev.min(bv).min(dv).min(fv).min(hv);
        let mx = ev.max(bv).max(dv).max(fv).max(hv);
        let amp = mn.min(255 - mx).min(stop);
        let out = ev * (256 + 4 * amp) - (bv + dv + fv + hv) * amp;
        (out >> 8).clamp(0, 255) as u8
    };
    (u32::from(ch(16)) << 16) | (u32::from(ch(8)) << 8) | u32::from(ch(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip() {
        for mode in Upscale::ALL {
            assert_eq!(Upscale::from_token(mode.token()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(Upscale::from_token("nis"), None);
        assert!(!Upscale::Off.enabled());
        assert!(Upscale::Fsr.enabled());
        assert_eq!(Upscale::Off.stream_limit(), Limit::NONE);
        assert_eq!(Upscale::Auto.stream_limit().width, STREAM_WIDTH);
        assert_eq!(Upscale::Auto.stream_limit().height, STREAM_HEIGHT);
        assert_eq!(Upscale::Off.kind(), None);
        assert_eq!(Upscale::Fsr.kind(), Some(Kind::Fsr));
    }

    #[test]
    fn solid_colour_stays_flat_when_doubled() {
        let src = vec![10u8, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30];
        let mut dst = vec![0u32; 16];
        blit_argb(&src, 2, 2, &mut dst, 4, 4, Kind::Fsr);
        for pixel in dst {
            assert_eq!(pixel, 0x000A141E, "{pixel:08x}");
        }
    }
}
