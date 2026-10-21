//! Picture looks the host paints before it encodes.
//!
//! The frame keeps its width and height. Pointer coordinates are fitted-frame
//! pixels, so a smaller grid would move every click. Each look fills solid
//! blocks instead: flatter blocks compress better, and the picture stays the
//! same size.
//!
//! Palettes, ordered screens, and error-diffusion kernels follow Image-to-Pixel
//! (<https://github.com/Tezumie/Image-to-Pixel>, MIT). Ordered dither is
//! stable across video frames. Error diffusion is included for the patterns;
//! it can flicker between frames.
//!
//! Ordered, poster, and block looks run as a GPU compute shader when a
//! discrete adapter is up. A 64³ palette LUT and reused CPU buffers keep the
//! fallback under a millisecond at 1080p.

use std::cell::RefCell;

mod dither;
#[cfg(not(target_os = "android"))]
mod gpu;
mod lut;
mod palettes;

pub use dither::Dither;

/// What the host does to the picture before sending it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// The captured picture, unchanged.
    #[default]
    Off,
    /// Downsample only. Colours stay, in solid blocks.
    Blocks,
    /// DMG green.
    GameBoy,
    /// PICO-8.
    Pico8,
    /// CGA's 16 colours.
    Cga,
    /// Black and an off-white.
    OneBit,
    /// Four greys, like a coarse halftone.
    Newsprint,
    /// Endesga 32.
    Endesga,
    /// Sweetie 16.
    Sweetie,
    Apollo,
    Batpalette,
    Beached,
    Cc29,
    Chaotic,
    Fantasy,
    Hidden,
    Intacto14,
    Journey,
    Lospec500,
    Nyx8,
    Oil6,
    Resurrect,
    Slso8,
    SweetCanyon,
    Toxic,
    Vinik24,
    /// Fewer steps per channel, full resolution. Flatter for the codec.
    Poster,
}

impl Look {
    pub const ALL: [Look; 27] = [
        Self::Off,
        Self::Blocks,
        Self::Poster,
        Self::OneBit,
        Self::GameBoy,
        Self::Newsprint,
        Self::Pico8,
        Self::Cga,
        Self::Sweetie,
        Self::Endesga,
        Self::Apollo,
        Self::Batpalette,
        Self::Beached,
        Self::Cc29,
        Self::Chaotic,
        Self::Fantasy,
        Self::Hidden,
        Self::Intacto14,
        Self::Journey,
        Self::Lospec500,
        Self::Nyx8,
        Self::Oil6,
        Self::Resurrect,
        Self::Slso8,
        Self::SweetCanyon,
        Self::Toxic,
        Self::Vinik24,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Blocks => "blocks",
            Self::GameBoy => "gameboy",
            Self::Pico8 => "pico8",
            Self::Cga => "cga",
            Self::OneBit => "onebit",
            Self::Newsprint => "newsprint",
            Self::Endesga => "endesga",
            Self::Sweetie => "sweetie",
            Self::Apollo => "apollo",
            Self::Batpalette => "batpalette",
            Self::Beached => "beached",
            Self::Cc29 => "cc-29",
            Self::Chaotic => "chaotic",
            Self::Fantasy => "fantasy",
            Self::Hidden => "hidden",
            Self::Intacto14 => "intacto14",
            Self::Journey => "journey",
            Self::Lospec500 => "lospec500",
            Self::Nyx8 => "nyx8",
            Self::Oil6 => "oil-6",
            Self::Resurrect => "resurrect",
            Self::Slso8 => "slso8",
            Self::SweetCanyon => "sweet-canyon",
            Self::Toxic => "toxic",
            Self::Vinik24 => "vinik24",
            Self::Poster => "poster",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|look| look.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Blocks => "Blocks",
            Self::GameBoy => "Game Boy",
            Self::Pico8 => "PICO-8",
            Self::Cga => "CGA",
            Self::OneBit => "1-bit",
            Self::Newsprint => "Newsprint",
            Self::Endesga => "Endesga",
            Self::Sweetie => "Sweetie",
            Self::Apollo => "Apollo",
            Self::Batpalette => "Batpalette",
            Self::Beached => "Beached",
            Self::Cc29 => "CC-29",
            Self::Chaotic => "Chaotic",
            Self::Fantasy => "Fantasy",
            Self::Hidden => "Hidden Library",
            Self::Intacto14 => "Intacto14",
            Self::Journey => "Journey",
            Self::Lospec500 => "Lospec500",
            Self::Nyx8 => "Nyx8",
            Self::Oil6 => "Oil 6",
            Self::Resurrect => "Resurrect 64",
            Self::Slso8 => "SLSO8",
            Self::SweetCanyon => "Sweet Canyon",
            Self::Toxic => "Toxic Haze",
            Self::Vinik24 => "Vinik24",
            Self::Poster => "Poster",
        }
    }

    /// Block size used when an older offer has no `pixel-size` line.
    pub fn default_pixel_size(self) -> u8 {
        match self {
            Self::Off | Self::Poster => 1,
            Self::GameBoy | Self::Cga => 4,
            _ => 2,
        }
    }
}

/// Palette, dither pattern, block size, and strength the host paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub look: Look,
    pub dither: Dither,
    pub pixel_size: u8,
    /// 0–100. Image-to-Pixel's default is 60.
    pub strength: u8,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            look: Look::Off,
            dither: Dither::Bayer4,
            pixel_size: 2,
            strength: 60,
        }
    }
}

impl Style {
    pub fn from_look(look: Look) -> Self {
        Self {
            look,
            dither: Dither::Bayer4,
            pixel_size: look.default_pixel_size(),
            strength: 60,
        }
    }

    pub fn pixel_size(self) -> u32 {
        u32::from(self.pixel_size.clamp(1, 32))
    }

    pub fn strength01(self) -> f32 {
        f32::from(self.strength.min(100)) / 100.0
    }
}

pub(crate) const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// Paint `look` with its default Bayer 4×4 and block size.
pub fn apply(rgb: &mut [u8], width: u32, height: u32, look: Look) {
    apply_style(rgb, width, height, Style::from_look(look));
}

/// Build palette tables and start the look compute device off the encode path.
pub fn warm() {
    lut::warm();
    #[cfg(not(target_os = "android"))]
    gpu::warm();
}

/// Paint `style` into packed RGB. `Off` does not write. A short buffer is left
/// alone. Width and height stay the caller's.
pub fn apply_style(rgb: &mut [u8], width: u32, height: u32, style: Style) {
    if style.look == Look::Off || width == 0 || height == 0 {
        return;
    }
    let Some(need) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(3))
    else {
        return;
    };
    if rgb.len() < need {
        return;
    }
    let rgb = &mut rgb[..need];
    #[cfg(not(target_os = "android"))]
    if gpu::try_apply(rgb, width, height, style) {
        return;
    }
    apply_cpu(rgb, width as usize, height as usize, style);
}

#[derive(Default)]
struct Scratch {
    cells: Vec<u8>,
    grid: Vec<f32>,
    acc: Vec<f32>,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default());
}

fn apply_cpu(rgb: &mut [u8], width: usize, height: usize, style: Style) {
    let block = style.pixel_size() as usize;
    let gw = width.div_ceil(block);
    let gh = height.div_ceil(block);
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        if style.dither.is_diffusion() && !style.look.palette().is_empty() {
            scratch.grid.resize(gw * gh * 3, 0.0);
            downsample(rgb, width, height, block, gw, gh, &mut scratch.grid);
            scratch.acc.clear();
            scratch.acc.resize(gw * gh * 3, 0.0);
            let Scratch {
                ref mut grid,
                ref mut acc,
                ..
            } = &mut *scratch;
            diffuse(grid, gw, gh, style.look.palette(), style, acc);
            upsample(rgb, width, height, block, gw, gh, grid);
            return;
        }
        scratch.cells.resize(gw * gh * 3, 0);
        downsample_u8(rgb, width, height, block, gw, gh, &mut scratch.cells);
        match style.look {
            Look::Off => {}
            Look::Blocks => {}
            Look::Poster => posterize_u8(&mut scratch.cells),
            _ => ordered_u8(&mut scratch.cells, gw, gh, style),
        }
        upsample_u8(rgb, width, height, block, gw, gh, &scratch.cells);
    });
}

fn downsample_u8(
    rgb: &[u8],
    width: usize,
    height: usize,
    block: usize,
    gw: usize,
    gh: usize,
    grid: &mut [u8],
) {
    for gy in 0..gh {
        let y0 = gy * block;
        let bh = block.min(height - y0);
        for gx in 0..gw {
            let x0 = gx * block;
            let bw = block.min(width - x0);
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            for row in y0..y0 + bh {
                let start = (row * width + x0) * 3;
                for col in 0..bw {
                    let i = start + col * 3;
                    sr += u32::from(rgb[i]);
                    sg += u32::from(rgb[i + 1]);
                    sb += u32::from(rgb[i + 2]);
                }
            }
            let n = (bw * bh) as u32;
            let o = (gy * gw + gx) * 3;
            grid[o] = ((sr + n / 2) / n) as u8;
            grid[o + 1] = ((sg + n / 2) / n) as u8;
            grid[o + 2] = ((sb + n / 2) / n) as u8;
        }
    }
}

fn upsample_u8(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    block: usize,
    gw: usize,
    gh: usize,
    grid: &[u8],
) {
    if block == 1 && grid.len() == rgb.len() {
        rgb.copy_from_slice(grid);
        return;
    }
    for gy in 0..gh {
        let y0 = gy * block;
        let bh = block.min(height - y0);
        for gx in 0..gw {
            let x0 = gx * block;
            let bw = block.min(width - x0);
            let o = (gy * gw + gx) * 3;
            let colour = [grid[o], grid[o + 1], grid[o + 2]];
            let first = (y0 * width + x0) * 3;
            for col in 0..bw {
                let i = first + col * 3;
                rgb[i] = colour[0];
                rgb[i + 1] = colour[1];
                rgb[i + 2] = colour[2];
            }
            let row_bytes = bw * 3;
            for row in 1..bh {
                let dst = ((y0 + row) * width + x0) * 3;
                rgb.copy_within(first..first + row_bytes, dst);
            }
        }
    }
}

fn posterize_u8(grid: &mut [u8]) {
    for value in grid {
        let stepped = (u16::from(*value) + 16) / 32 * 32;
        *value = stepped.min(255) as u8;
    }
}

fn ordered_u8(grid: &mut [u8], gw: usize, gh: usize, style: Style) {
    let palette = style.look.palette();
    if palette.is_empty() {
        return;
    }
    let table = lut::rgb(style.look);
    let spread = palette_spread(palette) * style.strength01();
    let map = dither::threshold_map(style.dither);
    let spread_q = (spread * 255.0).round() as i32;
    for y in 0..gh {
        for x in 0..gw {
            let o = (y * gw + x) * 3;
            let (mut r, mut g, mut b) = (
                i32::from(grid[o]),
                i32::from(grid[o + 1]),
                i32::from(grid[o + 2]),
            );
            if let Some(map) = &map {
                if spread_q > 0 {
                    let t = i32::from(map.values[(y % map.h) * map.w + (x % map.w)]);
                    let levels = map.levels as i32;
                    // (map + 0.5) / levels - 0.5, in 8-bit units.
                    let amt = (t * 2 + 1 - levels) * spread_q / (levels * 2);
                    r = (r + amt).clamp(0, 255);
                    g = (g + amt).clamp(0, 255);
                    b = (b + amt).clamp(0, 255);
                }
            }
            let colour = match table {
                Some(table) => {
                    let i = lut::index(r as u8, g as u8, b as u8) * 3;
                    [table[i], table[i + 1], table[i + 2]]
                }
                None => nearest(palette, r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0),
            };
            grid[o] = colour[0];
            grid[o + 1] = colour[1];
            grid[o + 2] = colour[2];
        }
    }
}

fn downsample(
    rgb: &[u8],
    width: usize,
    height: usize,
    block: usize,
    gw: usize,
    gh: usize,
    grid: &mut [f32],
) {
    for gy in 0..gh {
        let y0 = gy * block;
        let bh = block.min(height - y0);
        for gx in 0..gw {
            let x0 = gx * block;
            let bw = block.min(width - x0);
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            for row in y0..y0 + bh {
                let start = (row * width + x0) * 3;
                for col in 0..bw {
                    let i = start + col * 3;
                    sr += u32::from(rgb[i]);
                    sg += u32::from(rgb[i + 1]);
                    sb += u32::from(rgb[i + 2]);
                }
            }
            let n = (bw * bh) as f32;
            let o = (gy * gw + gx) * 3;
            grid[o] = sr as f32 / n / 255.0;
            grid[o + 1] = sg as f32 / n / 255.0;
            grid[o + 2] = sb as f32 / n / 255.0;
        }
    }
}

fn upsample(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    block: usize,
    gw: usize,
    gh: usize,
    grid: &[f32],
) {
    for gy in 0..gh {
        let y0 = gy * block;
        let bh = block.min(height - y0);
        for gx in 0..gw {
            let x0 = gx * block;
            let bw = block.min(width - x0);
            let o = (gy * gw + gx) * 3;
            let colour = [
                (grid[o] * 255.0).round().clamp(0.0, 255.0) as u8,
                (grid[o + 1] * 255.0).round().clamp(0.0, 255.0) as u8,
                (grid[o + 2] * 255.0).round().clamp(0.0, 255.0) as u8,
            ];
            for row in y0..y0 + bh {
                let start = (row * width + x0) * 3;
                for col in 0..bw {
                    let i = start + col * 3;
                    rgb[i] = colour[0];
                    rgb[i + 1] = colour[1];
                    rgb[i + 2] = colour[2];
                }
            }
        }
    }
}

fn diffuse(
    grid: &mut [f32],
    gw: usize,
    gh: usize,
    palette: &[[u8; 3]],
    style: Style,
    acc: &mut [f32],
) {
    let kernel = dither::kernel(style.dither);
    let strength = style.strength01();
    for y in 0..gh {
        for x in 0..gw {
            let p = y * gw + x;
            let o = p * 3;
            let r = (grid[o] + acc[o] * strength).clamp(0.0, 1.0);
            let g = (grid[o + 1] + acc[o + 1] * strength).clamp(0.0, 1.0);
            let b = (grid[o + 2] + acc[o + 2] * strength).clamp(0.0, 1.0);
            let colour = nearest(palette, r, g, b);
            let nr = f32::from(colour[0]) / 255.0;
            let ng = f32::from(colour[1]) / 255.0;
            let nb = f32::from(colour[2]) / 255.0;
            let er = r - nr;
            let eg = g - ng;
            let eb = b - nb;
            for &(dx, dy, wt) in kernel {
                let xx = x as i32 + dx;
                let yy = y as i32 + dy;
                if xx < 0 || yy < 0 || xx >= gw as i32 || yy >= gh as i32 {
                    continue;
                }
                let q = ((yy as usize) * gw + xx as usize) * 3;
                acc[q] += er * wt;
                acc[q + 1] += eg * wt;
                acc[q + 2] += eb * wt;
            }
            grid[o] = nr;
            grid[o + 1] = ng;
            grid[o + 2] = nb;
        }
    }
}

fn palette_spread(palette: &[[u8; 3]]) -> f32 {
    if palette.len() < 2 {
        return 1.0;
    }
    let mut sum = 0.0f32;
    for (i, a) in palette.iter().enumerate() {
        let mut best = f32::INFINITY;
        for (j, b) in palette.iter().enumerate() {
            if i == j {
                continue;
            }
            let dr = f32::from(a[0]) - f32::from(b[0]);
            let dg = f32::from(a[1]) - f32::from(b[1]);
            let db = f32::from(a[2]) - f32::from(b[2]);
            let d = (dr * dr + dg * dg + db * db).sqrt();
            if d < best {
                best = d;
            }
        }
        sum += best;
    }
    (sum / palette.len() as f32 / 255.0).clamp(0.04, 1.0)
}

/// Luma-weighted RGB, Image-to-Pixel's `rgb` distance.
fn nearest(palette: &[[u8; 3]], r: f32, g: f32, b: f32) -> [u8; 3] {
    let pr = r * 255.0;
    let pg = g * 255.0;
    let pb = b * 255.0;
    let mut best = palette[0];
    let mut best_d = f32::INFINITY;
    for entry in palette {
        let dr = pr - f32::from(entry[0]);
        let dg = pg - f32::from(entry[1]);
        let db = pb - f32::from(entry[2]);
        let d = 0.299 * dr * dr + 0.587 * dg * dg + 0.114 * db * db;
        if d < best_d {
            best = *entry;
            best_d = d;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use palettes::{GAME_BOY, GREY4, ONE_BIT, PICO8};

    fn solid(w: u32, h: u32, colour: [u8; 3]) -> Vec<u8> {
        let mut rgb = Vec::new();
        for _ in 0..w * h {
            rgb.extend_from_slice(&colour);
        }
        rgb
    }

    fn pixels_are(rgb: &[u8], colour: [u8; 3]) -> bool {
        rgb.chunks(3).all(|pixel| pixel == colour)
    }

    #[test]
    fn tokens_round_trip_and_unknown_is_none() {
        for look in Look::ALL {
            assert_eq!(Look::from_token(look.token()), Some(look));
            assert!(!look.label().is_empty());
        }
        for dither in Dither::ALL {
            assert_eq!(Dither::from_token(dither.token()), Some(dither));
            assert!(!dither.label().is_empty());
        }
        assert_eq!(Look::from_token("crt"), None);
        assert_eq!(Dither::from_token("ordered"), None);
        assert_eq!(Look::default(), Look::Off);
        assert_eq!(Dither::default(), Dither::Bayer4);
    }

    #[test]
    fn off_does_not_write() {
        let mut rgb = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let before = rgb.clone();
        apply(&mut rgb, 2, 2, Look::Off);
        assert_eq!(rgb, before);
        apply(&mut rgb, 0, 2, Look::GameBoy);
        apply(&mut rgb, 2, 0, Look::Poster);
        apply(&mut rgb[..6], 2, 2, Look::Pico8);
        assert_eq!(rgb, before);
    }

    #[test]
    fn onebit_snaps_black_and_white() {
        let mut black = solid(2, 2, [0, 0, 0]);
        apply(&mut black, 2, 2, Look::OneBit);
        assert!(pixels_are(&black, ONE_BIT[0]), "{black:?}");
        let mut white = solid(2, 2, [255, 255, 255]);
        apply(&mut white, 2, 2, Look::OneBit);
        assert!(pixels_are(&white, ONE_BIT[1]), "{white:?}");
    }

    #[test]
    fn pico8_red_is_the_palette_red() {
        let mut rgb = solid(4, 2, [255, 0, 0]);
        apply_style(
            &mut rgb,
            4,
            2,
            Style {
                look: Look::Pico8,
                dither: Dither::None,
                pixel_size: 2,
                strength: 0,
            },
        );
        let red = super::rgb(0xff004d);
        assert!(pixels_are(&rgb, red), "{rgb:?}");
        assert_eq!(rgb.len(), 4 * 2 * 3);
        assert!(PICO8.contains(&red));
    }

    #[test]
    fn gameboy_blocks_are_flat_and_stay_on_the_palette() {
        let mut rgb = Vec::new();
        for y in 0..8u32 {
            for x in 0..8u32 {
                let v = (x * 28 + y * 12) as u8;
                rgb.extend_from_slice(&[v, v.saturating_add(40), v / 3]);
            }
        }
        apply(&mut rgb, 8, 8, Look::GameBoy);
        assert_eq!(rgb.len(), 8 * 8 * 3);
        for pixel in rgb.chunks(3) {
            assert!(
                GAME_BOY.iter().any(|entry| pixel == entry),
                "pixel {pixel:?} left the Game Boy palette"
            );
        }
        for by in 0..2 {
            for bx in 0..2 {
                let mut first = None;
                for y in 0..4 {
                    for x in 0..4 {
                        let i = ((by * 4 + y) * 8 + bx * 4 + x) * 3;
                        let pixel = [rgb[i], rgb[i + 1], rgb[i + 2]];
                        match first {
                            None => first = Some(pixel),
                            Some(colour) => assert_eq!(pixel, colour, "block {bx},{by}"),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn newsprint_is_only_the_four_greys() {
        let mut rgb = solid(2, 2, [128, 140, 100]);
        apply_style(
            &mut rgb,
            2,
            2,
            Style {
                look: Look::Newsprint,
                dither: Dither::None,
                pixel_size: 2,
                strength: 0,
            },
        );
        let pixel = [rgb[0], rgb[1], rgb[2]];
        assert!(GREY4.contains(&pixel), "{pixel:?}");
        assert!(pixels_are(&rgb, pixel));
    }

    #[test]
    fn poster_quantizes_each_channel_and_keeps_every_pixel() {
        let mut rgb = vec![0u8, 16, 100, 255, 224, 240];
        apply(&mut rgb, 2, 1, Look::Poster);
        assert_eq!(rgb, vec![0, 32, 96, 255, 224, 255]);
    }

    #[test]
    fn blocks_average_without_a_palette() {
        let mut rgb = Vec::new();
        rgb.extend_from_slice(&[0, 0, 0, 255, 255, 255, 0, 0, 0, 255, 255, 255]);
        apply_style(
            &mut rgb,
            2,
            2,
            Style {
                look: Look::Blocks,
                dither: Dither::None,
                pixel_size: 2,
                strength: 0,
            },
        );
        assert!(pixels_are(&rgb, [128, 128, 128]), "{rgb:?}");
    }

    #[test]
    fn bayer4_threshold_order_matches_image_to_pixel() {
        assert_eq!(
            dither::BAYER4,
            [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5]
        );
        let map = dither::threshold_map(Dither::BlueNoise).unwrap();
        assert_eq!(map.w, 64);
        assert_eq!(map.h, 64);
        assert_eq!(map.levels, 256);
        assert_eq!(map.values.len(), 4096);
    }

    #[test]
    fn floyd_steinberg_on_a_grey_ramp_uses_both_onebit_colours() {
        let mut rgb = Vec::new();
        for x in 0..16u32 {
            let v = (x * 17) as u8;
            rgb.extend_from_slice(&[v, v, v]);
        }
        apply_style(
            &mut rgb,
            16,
            1,
            Style {
                look: Look::OneBit,
                dither: Dither::FloydSteinberg,
                pixel_size: 1,
                strength: 100,
            },
        );
        let mut saw_dark = false;
        let mut saw_light = false;
        for pixel in rgb.chunks(3) {
            if pixel == ONE_BIT[0] {
                saw_dark = true;
            } else if pixel == ONE_BIT[1] {
                saw_light = true;
            } else {
                panic!("pixel {pixel:?} left 1-bit");
            }
        }
        assert!(saw_dark && saw_light, "{rgb:?}");
    }

    #[test]
    fn hd_ordered_look_stays_under_a_frame() {
        lut::warm();
        let width = 1280u32;
        let height = 720u32;
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        for (i, byte) in rgb.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let start = std::time::Instant::now();
        apply_style(
            &mut rgb,
            width,
            height,
            Style {
                look: Look::Pico8,
                dither: Dither::Bayer4,
                pixel_size: 2,
                strength: 60,
            },
        );
        let elapsed = start.elapsed();
        let budget = if cfg!(debug_assertions) {
            std::time::Duration::from_millis(80)
        } else {
            std::time::Duration::from_millis(8)
        };
        assert!(
            elapsed < budget,
            "look paint took {elapsed:?} (budget {budget:?})"
        );
        assert!(PICO8.contains(&[rgb[0], rgb[1], rgb[2]]));
    }

    #[test]
    fn palette_lut_snaps_to_the_look() {
        lut::warm();
        for look in Look::ALL {
            let palette = look.palette();
            if palette.is_empty() {
                continue;
            }
            let table = lut::rgb(look).expect(look.token());
            let i = lut::index(255, 0, 0) * 3;
            let colour = [table[i], table[i + 1], table[i + 2]];
            assert!(
                palette.iter().any(|entry| entry == &colour),
                "{look:?} lut left the palette at {colour:?}"
            );
        }
    }

    #[test]
    fn every_ordered_pattern_stays_on_the_palette() {
        for dither in [
            Dither::None,
            Dither::Bayer2,
            Dither::Bayer8,
            Dither::Bayer16,
            Dither::Cluster4,
            Dither::Cluster8,
            Dither::Lines,
            Dither::BlueNoise,
            Dither::Atkinson,
            Dither::SierraLite,
            Dither::Sierra,
            Dither::Burkes,
            Dither::Jarvis,
            Dither::Stucki,
        ] {
            let mut rgb = Vec::new();
            for y in 0..8u32 {
                for x in 0..8u32 {
                    let v = (x * 20 + y * 9) as u8;
                    rgb.extend_from_slice(&[v, 80, 200u8.saturating_sub(v)]);
                }
            }
            apply_style(
                &mut rgb,
                8,
                8,
                Style {
                    look: Look::Pico8,
                    dither,
                    pixel_size: 1,
                    strength: 80,
                },
            );
            for pixel in rgb.chunks(3) {
                assert!(
                    PICO8.iter().any(|entry| pixel == entry),
                    "{dither:?} left the palette at {pixel:?}"
                );
            }
        }
    }
}
