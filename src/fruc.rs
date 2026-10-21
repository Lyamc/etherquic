//! Client frame generation: insert a synthesised picture between arrivals.
//!
//! NVIDIA's `fruc_vulkan` filter (VK_NV_optical_flow, Ampere+) is not in the
//! FFmpeg 9.0 this tree ships, and Turing cards do not expose that extension.
//! This module does the same job on the player: estimate motion between the
//! last two decoded pictures and warp the latest one half a period forward.
//! Discrete GPUs run a Vulkan/Metal/DX12 compute path; everything else uses
//! a coarse CPU block match.

#[cfg(not(target_os = "android"))]
mod gpu;

/// Block size for motion search.
const BLOCK: i32 = 16;
/// Search radius, in pixels, around each block.
const RADIUS: i32 = 8;

/// Whether the player inserts generated pictures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameGen {
    #[default]
    Off,
    /// On when a discrete GPU is present.
    Auto,
    On,
}

impl FrameGen {
    pub const ALL: [FrameGen; 3] = [Self::Off, Self::Auto, Self::On];

    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::On => "on",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Auto => "Auto",
            Self::On => "On",
        }
    }

    pub fn enabled(self) -> bool {
        match self {
            Self::Off => false,
            Self::On => true,
            Self::Auto => crate::gpu_compute_present() || crate::nvidia_present(),
        }
    }
}

/// Start the compute device off the present path.
pub fn warm() {
    #[cfg(not(target_os = "android"))]
    gpu::warm();
}

/// Warp `curr` half a step beyond the motion from `prev` to `curr`.
///
/// `t_q8` is 8.8 fixed-point time: 256 is `curr`, 384 is halfway to the next
/// expected picture. Returns false when the buffers do not match.
pub fn interpolate(
    prev: &[u8],
    curr: &[u8],
    width: u32,
    height: u32,
    t_q8: u32,
    out: &mut [u8],
) -> bool {
    let need = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(3);
    if width == 0 || height == 0 || prev.len() < need || curr.len() < need || out.len() < need {
        return false;
    }
    if t_q8 == 0 {
        out[..need].copy_from_slice(&prev[..need]);
        return true;
    }
    if t_q8 == 256 {
        out[..need].copy_from_slice(&curr[..need]);
        return true;
    }
    if !cfg!(test) {
        #[cfg(not(target_os = "android"))]
        if gpu::try_interpolate(prev, curr, width, height, t_q8, out) {
            return true;
        }
    }
    cpu_interpolate(prev, curr, width, height, t_q8, out);
    true
}

fn cpu_interpolate(prev: &[u8], curr: &[u8], width: u32, height: u32, t_q8: u32, out: &mut [u8]) {
    let w = width as i32;
    let h = height as i32;
    let bw = (w + BLOCK - 1) / BLOCK;
    let bh = (h + BLOCK - 1) / BLOCK;
    let mut flow = vec![0i32; (bw * bh * 2) as usize];
    for by in 0..bh {
        for bx in 0..bw {
            let (mx, my) = best_motion(prev, curr, w, h, bx * BLOCK, by * BLOCK);
            let i = ((by * bw + bx) * 2) as usize;
            flow[i] = mx;
            flow[i + 1] = my;
        }
    }
    // t=1 is curr. Extra time beyond that continues the same motion.
    let extra = t_q8 as i32 - 256;
    for y in 0..h {
        let by = (y / BLOCK).min(bh - 1);
        for x in 0..w {
            let bx = (x / BLOCK).min(bw - 1);
            let i = ((by * bw + bx) * 2) as usize;
            let mx = flow[i];
            let my = flow[i + 1];
            let sx = x + ((mx * extra) >> 8);
            let sy = y + ((my * extra) >> 8);
            let pix = sample(curr, w, h, sx, sy);
            let o = ((y * w + x) * 3) as usize;
            out[o] = pix[0];
            out[o + 1] = pix[1];
            out[o + 2] = pix[2];
        }
    }
}

/// Object motion from `prev` to `curr` at this block: curr[p] ≈ prev[p - M].
fn best_motion(prev: &[u8], curr: &[u8], w: i32, h: i32, ox: i32, oy: i32) -> (i32, i32) {
    let mut best = i32::MAX;
    let mut mx = 0;
    let mut my = 0;
    for dy in -RADIUS..=RADIUS {
        for dx in -RADIUS..=RADIUS {
            let mut sad = 0i32;
            let mut y = oy;
            while y < oy + BLOCK && y < h {
                let mut x = ox;
                while x < ox + BLOCK && x < w {
                    let a = luma(curr, w, h, x, y);
                    let b = luma(prev, w, h, x - dx, y - dy);
                    sad += (i32::from(a) - i32::from(b)).abs();
                    x += 2;
                }
                y += 2;
            }
            if sad < best {
                best = sad;
                mx = dx;
                my = dy;
            }
        }
    }
    (mx, my)
}

fn luma(rgb: &[u8], w: i32, h: i32, x: i32, y: i32) -> u8 {
    let x = x.clamp(0, w - 1) as usize;
    let y = y.clamp(0, h - 1) as usize;
    let i = (y * w as usize + x) * 3;
    ((77 * u32::from(rgb[i]) + 150 * u32::from(rgb[i + 1]) + 29 * u32::from(rgb[i + 2])) >> 8) as u8
}

fn sample(rgb: &[u8], w: i32, h: i32, x: i32, y: i32) -> [u8; 3] {
    let x = x.clamp(0, w - 1) as usize;
    let y = y.clamp(0, h - 1) as usize;
    let i = (y * w as usize + x) * 3;
    [rgb[i], rgb[i + 1], rgb[i + 2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip() {
        for mode in FrameGen::ALL {
            assert_eq!(FrameGen::from_token(mode.token()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(FrameGen::from_token("dlss-fg"), None);
        assert!(!FrameGen::Off.enabled());
        assert!(FrameGen::On.enabled());
    }

    #[test]
    fn solid_colour_stays_flat() {
        let prev = vec![10u8, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30];
        let curr = prev.clone();
        let mut out = vec![0u8; 12];
        assert!(interpolate(&prev, &curr, 2, 2, 384, &mut out));
        assert_eq!(out, prev);
    }

    #[test]
    fn a_shifted_block_keeps_moving() {
        let mut prev = vec![0u8; 32 * 16 * 3];
        let mut curr = vec![0u8; 32 * 16 * 3];
        fill_block(&mut prev, 32, 4, 4, 8, 8, [200, 0, 0]);
        fill_block(&mut curr, 32, 8, 4, 8, 8, [200, 0, 0]);
        let mut out = vec![0u8; prev.len()];
        assert!(interpolate(&prev, &curr, 32, 16, 384, &mut out));
        // Halfway to the next picture: another +2 px.
        let pix = sample(&out, 32, 16, 12, 8);
        assert!(pix[0] > 100, "{pix:?}");
        let behind = sample(&out, 32, 16, 4, 8);
        assert!(behind[0] < 80, "{behind:?}");
    }

    fn fill_block(rgb: &mut [u8], w: i32, x0: i32, y0: i32, bw: i32, bh: i32, colour: [u8; 3]) {
        for y in y0..y0 + bh {
            for x in x0..x0 + bw {
                let i = ((y * w + x) * 3) as usize;
                rgb[i] = colour[0];
                rgb[i + 1] = colour[1];
                rgb[i + 2] = colour[2];
            }
        }
    }
}
