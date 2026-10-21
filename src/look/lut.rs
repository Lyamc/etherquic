//! 64³ nearest-colour tables. Ordered dither becomes one lookup per cell.

use super::{nearest, Look};

pub const SIDE: usize = 32;
pub const SHIFT: u32 = 3;
pub const LEN: usize = SIDE * SIDE * SIDE;

pub fn index(r: u8, g: u8, b: u8) -> usize {
    ((r as usize) >> SHIFT) * SIDE * SIDE + ((g as usize) >> SHIFT) * SIDE + ((b as usize) >> SHIFT)
}

pub fn pack(colour: [u8; 3]) -> u32 {
    u32::from(colour[0]) | u32::from(colour[1]) << 8 | u32::from(colour[2]) << 16
}

pub fn unpack(packed: u32) -> [u8; 3] {
    [
        packed as u8,
        (packed >> 8) as u8,
        (packed >> 16) as u8,
    ]
}

struct Table {
    rgb: Box<[u8]>,
    packed: Box<[u32]>,
}

fn slot(look: Look) -> usize {
    Look::ALL
        .iter()
        .position(|&row| row == look)
        .unwrap_or(0)
}

fn table(look: Look) -> Option<&'static Table> {
    use std::sync::OnceLock;
    static TABLES: [OnceLock<Option<Table>>; 27] = [const { OnceLock::new() }; 27];
    TABLES[slot(look)].get_or_init(|| build_one(look)).as_ref()
}

fn build_one(look: Look) -> Option<Table> {
    let palette = look.palette();
    if palette.is_empty() {
        return None;
    }
    let mut bytes = vec![0u8; LEN * 3];
    let mut words = vec![0u32; LEN];
    for ri in 0..SIDE {
        for gi in 0..SIDE {
            for bi in 0..SIDE {
                let r = ((ri << SHIFT) | 4).min(255) as f32 / 255.0;
                let g = ((gi << SHIFT) | 4).min(255) as f32 / 255.0;
                let b = ((bi << SHIFT) | 4).min(255) as f32 / 255.0;
                let colour = nearest(palette, r, g, b);
                let i = ri * SIDE * SIDE + gi * SIDE + bi;
                bytes[i * 3] = colour[0];
                bytes[i * 3 + 1] = colour[1];
                bytes[i * 3 + 2] = colour[2];
                words[i] = pack(colour);
            }
        }
    }
    Some(Table {
        rgb: bytes.into_boxed_slice(),
        packed: words.into_boxed_slice(),
    })
}

pub fn rgb(look: Look) -> Option<&'static [u8]> {
    table(look).map(|table| table.rgb.as_ref())
}

pub fn packed(look: Look) -> Option<&'static [u32]> {
    table(look).map(|table| table.packed.as_ref())
}

/// Build every palette table so the first painted frame does not wait.
pub fn warm() {
    for look in Look::ALL {
        let _ = table(look);
    }
}
