//! Picture looks the host paints before it encodes.
//!
//! The frame keeps its width and height. Pointer coordinates are fitted-frame
//! pixels, so a smaller grid would move every click. Each look fills solid
//! blocks instead: flatter blocks compress better, and the picture stays the
//! same size.
//!
//! The named palettes are the published Lospec sets (Game Boy, PICO-8, CGA,
//! 1-bit monitor, 4-level grey, Endesga 32, Sweetie 16). Image-to-Pixel
//! (<https://github.com/Tezumie/Image-to-Pixel>, MIT) is the catalogue this
//! list follows. Ordered dither is a 4×4 Bayer bias on the block average.
//! Poster flattens each channel and uses no palette.

/// What the host does to the picture before sending it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// The captured picture, unchanged.
    #[default]
    Off,
    /// DMG green, large blocks.
    GameBoy,
    /// PICO-8.
    Pico8,
    /// CGA's 16 colours, large blocks.
    Cga,
    /// Black and an off-white.
    OneBit,
    /// Four greys, like a coarse halftone.
    Newsprint,
    /// Endesga 32.
    Endesga,
    /// Sweetie 16.
    Sweetie,
    /// Fewer steps per channel, full resolution. Flatter for the codec.
    Poster,
}

impl Look {
    pub const ALL: [Look; 9] = [
        Self::Off,
        Self::GameBoy,
        Self::Pico8,
        Self::Cga,
        Self::OneBit,
        Self::Newsprint,
        Self::Endesga,
        Self::Sweetie,
        Self::Poster,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::GameBoy => "gameboy",
            Self::Pico8 => "pico8",
            Self::Cga => "cga",
            Self::OneBit => "onebit",
            Self::Newsprint => "newsprint",
            Self::Endesga => "endesga",
            Self::Sweetie => "sweetie",
            Self::Poster => "poster",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|look| look.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::GameBoy => "Game Boy",
            Self::Pico8 => "PICO-8",
            Self::Cga => "CGA",
            Self::OneBit => "1-bit",
            Self::Newsprint => "Newsprint",
            Self::Endesga => "Endesga",
            Self::Sweetie => "Sweetie",
            Self::Poster => "Poster",
        }
    }
}

const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// 4×4 Bayer, values 0..=15. Index is `(y & 3) * 4 + (x & 3)`.
const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

const ONE_BIT: [[u8; 3]; 2] = [rgb(0x222323), rgb(0xf0f6f0)];
const CGA: [[u8; 3]; 16] = [
    rgb(0x000000),
    rgb(0x0000aa),
    rgb(0x00aa00),
    rgb(0x00aaaa),
    rgb(0xaa0000),
    rgb(0xaa00aa),
    rgb(0xaa5500),
    rgb(0xaaaaaa),
    rgb(0x555555),
    rgb(0x5555ff),
    rgb(0x55ff55),
    rgb(0x55ffff),
    rgb(0xff5555),
    rgb(0xff55ff),
    rgb(0xffff55),
    rgb(0xffffff),
];
const ENDESGA: [[u8; 3]; 32] = [
    rgb(0xbe4a2f),
    rgb(0xd77643),
    rgb(0xead4aa),
    rgb(0xe4a672),
    rgb(0xb86f50),
    rgb(0x733e39),
    rgb(0x3e2731),
    rgb(0xa22633),
    rgb(0xe43b44),
    rgb(0xf77622),
    rgb(0xfeae34),
    rgb(0xfee761),
    rgb(0x63c74d),
    rgb(0x3e8948),
    rgb(0x265c42),
    rgb(0x193c3e),
    rgb(0x124e89),
    rgb(0x0099db),
    rgb(0x2ce8f5),
    rgb(0xffffff),
    rgb(0xc0cbdc),
    rgb(0x8b9bb4),
    rgb(0x5a6988),
    rgb(0x3a4466),
    rgb(0x262b44),
    rgb(0x181425),
    rgb(0xff0044),
    rgb(0x68386c),
    rgb(0xb55088),
    rgb(0xf6757a),
    rgb(0xe8b796),
    rgb(0xc28569),
];
const GAME_BOY: [[u8; 3]; 4] = [rgb(0x0f380f), rgb(0x306230), rgb(0x8bac0f), rgb(0x9bbc0f)];
const GREY4: [[u8; 3]; 4] = [rgb(0x000000), rgb(0x555555), rgb(0xaaaaaa), rgb(0xffffff)];
const PICO8: [[u8; 3]; 16] = [
    rgb(0x000000),
    rgb(0x1d2b53),
    rgb(0x7e2553),
    rgb(0x008751),
    rgb(0xab5236),
    rgb(0x5f574f),
    rgb(0xc2c3c7),
    rgb(0xfff1e8),
    rgb(0xff004d),
    rgb(0xffa300),
    rgb(0xffec27),
    rgb(0x00e436),
    rgb(0x29adff),
    rgb(0x83769c),
    rgb(0xff77a8),
    rgb(0xffccaa),
];
const SWEETIE: [[u8; 3]; 16] = [
    rgb(0x1a1c2c),
    rgb(0x5d275d),
    rgb(0xb13e53),
    rgb(0xef7d57),
    rgb(0xffcd75),
    rgb(0xa7f070),
    rgb(0x38b764),
    rgb(0x257179),
    rgb(0x29366f),
    rgb(0x3b5dc9),
    rgb(0x41a6f6),
    rgb(0x73eff7),
    rgb(0xf4f4f4),
    rgb(0x94b0c2),
    rgb(0x566c86),
    rgb(0x333c57),
];

struct Recipe {
    palette: &'static [[u8; 3]],
    /// Pixels on a side. The block is one colour.
    block: u32,
    /// Peak Bayer bias, in channel levels. 0 is no dither.
    strength: i16,
}

fn recipe(look: Look) -> Recipe {
    match look {
        Look::GameBoy => Recipe {
            palette: &GAME_BOY,
            block: 4,
            strength: 40,
        },
        Look::Pico8 => Recipe {
            palette: &PICO8,
            block: 2,
            strength: 16,
        },
        Look::Cga => Recipe {
            palette: &CGA,
            block: 4,
            strength: 36,
        },
        Look::OneBit => Recipe {
            palette: &ONE_BIT,
            block: 2,
            strength: 48,
        },
        Look::Newsprint => Recipe {
            palette: &GREY4,
            block: 2,
            strength: 40,
        },
        Look::Endesga => Recipe {
            palette: &ENDESGA,
            block: 2,
            strength: 12,
        },
        Look::Sweetie => Recipe {
            palette: &SWEETIE,
            block: 2,
            strength: 16,
        },
        Look::Off | Look::Poster => Recipe {
            palette: &[],
            block: 1,
            strength: 0,
        },
    }
}

/// Paint `look` into packed RGB. `Off` does not write. A short buffer is left
/// alone. Width and height stay the caller's.
pub fn apply(rgb: &mut [u8], width: u32, height: u32, look: Look) {
    if look == Look::Off || width == 0 || height == 0 {
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
    if look == Look::Poster {
        posterize(rgb, width, height);
    } else {
        quantize(rgb, width, height, recipe(look));
    }
}

fn poster_channel(value: u8) -> u8 {
    let stepped = (u16::from(value) + 16) / 32 * 32;
    if stepped > 255 {
        255
    } else {
        stepped as u8
    }
}

fn posterize(rgb: &mut [u8], width: u32, height: u32) {
    let width = width as usize;
    let count = width * height as usize;
    for pixel in 0..count {
        let i = pixel * 3;
        rgb[i] = poster_channel(rgb[i]);
        rgb[i + 1] = poster_channel(rgb[i + 1]);
        rgb[i + 2] = poster_channel(rgb[i + 2]);
    }
}

fn bayer_bias(x: usize, y: usize, strength: i16) -> i16 {
    let b = i16::from(BAYER4[((y & 3) * 4) + (x & 3)]);
    (b - 8) * strength / 8
}

fn shift(average: u32, bias: i16) -> u8 {
    (average as i16 + bias).clamp(0, 255) as u8
}

fn dist(colour: [u8; 3], entry: [u8; 3]) -> u32 {
    let dr = i32::from(colour[0]) - i32::from(entry[0]);
    let dg = i32::from(colour[1]) - i32::from(entry[1]);
    let db = i32::from(colour[2]) - i32::from(entry[2]);
    (dr * dr * 3 + dg * dg * 6 + db * db) as u32
}

fn nearest(palette: &[[u8; 3]], colour: [u8; 3]) -> [u8; 3] {
    let mut best = colour;
    let mut best_d = u32::MAX;
    for entry in palette {
        let d = dist(colour, *entry);
        if d < best_d {
            best = *entry;
            best_d = d;
        }
    }
    best
}

fn quantize(rgb: &mut [u8], width: u32, height: u32, recipe: Recipe) {
    let width = width as usize;
    let height = height as usize;
    let block = recipe.block.max(1) as usize;
    let mut y = 0;
    while y < height {
        let bh = block.min(height - y);
        let mut x = 0;
        while x < width {
            let bw = block.min(width - x);
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            for row in y..y + bh {
                let start = (row * width + x) * 3;
                for col in 0..bw {
                    let i = start + col * 3;
                    sr += u32::from(rgb[i]);
                    sg += u32::from(rgb[i + 1]);
                    sb += u32::from(rgb[i + 2]);
                }
            }
            let n = (bw * bh) as u32;
            let bias = bayer_bias(x / block, y / block, recipe.strength);
            let colour = nearest(
                recipe.palette,
                [
                    shift(sr / n, bias),
                    shift(sg / n, bias),
                    shift(sb / n, bias),
                ],
            );
            for row in y..y + bh {
                let start = (row * width + x) * 3;
                for col in 0..bw {
                    let i = start + col * 3;
                    rgb[i] = colour[0];
                    rgb[i + 1] = colour[1];
                    rgb[i + 2] = colour[2];
                }
            }
            x += block;
        }
        y += block;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(Look::from_token("crt"), None);
        assert_eq!(Look::default(), Look::Off);
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
        apply(&mut rgb, 4, 2, Look::Pico8);
        assert!(pixels_are(&rgb, rgb_const(0xff004d)), "{rgb:?}");
        assert_eq!(rgb.len(), 4 * 2 * 3);
    }

    fn rgb_const(hex: u32) -> [u8; 3] {
        rgb(hex)
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
        apply(&mut rgb, 2, 2, Look::Newsprint);
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
}
