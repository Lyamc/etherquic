//! Changed tiles of a captured picture.
//!
//! The host compares the last encoded RGB buffer to the new one. An unchanged
//! picture is not encoded (the pointer already rides [`RECORD_CURSOR`]). When
//! only some tiles moved, MJPEG and MJ2K can send those tiles as patches.
//! Inter-frame codecs still encode a full picture when anything changed.

use crate::negotiate::RECORD_PATCH;
use crate::Frame;

/// Tile edge in pixels. Even, so a JPEG crop stays even.
const TILE: u32 = 16;
/// More than this many merged rects, or this fraction of the picture, is a
/// full frame.
const MAX_PATCHES: usize = 12;
const FULL_NUM: u64 = 1;
const FULL_DEN: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn area(self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }
}

/// What to send for one captured picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Unchanged,
    Full,
    Patches(Vec<Rect>),
}

/// Compare `next` to the last encoded picture. `prev` is `None` on the first
/// picture or after a size change.
pub fn plan(prev: Option<&[u8]>, next: &[u8], width: u32, height: u32) -> Plan {
    let Some(prev) = prev else {
        return Plan::Full;
    };
    let n = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(3);
    if prev.len() != n || next.len() != n || width == 0 || height == 0 {
        return Plan::Full;
    }
    if prev == next {
        return Plan::Unchanged;
    }
    let rects = merge(dirty_tiles(prev, next, width, height), width, height);
    if rects.is_empty() {
        return Plan::Unchanged;
    }
    let dirty: u64 = rects.iter().map(|rect| rect.area()).sum();
    let full = u64::from(width) * u64::from(height);
    if rects.len() > MAX_PATCHES || dirty.saturating_mul(FULL_DEN) >= full.saturating_mul(FULL_NUM)
    {
        Plan::Full
    } else {
        Plan::Patches(rects)
    }
}

/// RGB crop of `frame` at `rect`. Width and height stay even. The returned
/// rectangle is the one that was actually copied.
pub fn crop(frame: &Frame, rect: Rect) -> Result<(Rect, Frame), String> {
    frame.check()?;
    let rect = clamp(rect, frame.width, frame.height);
    if rect.w == 0 || rect.h == 0 {
        return Err("empty dirty rectangle".into());
    }
    let mut rgb = vec![0u8; rect.w as usize * rect.h as usize * 3];
    let src_stride = frame.width as usize * 3;
    let dst_stride = rect.w as usize * 3;
    for row in 0..rect.h as usize {
        let src = (rect.y as usize + row) * src_stride + rect.x as usize * 3;
        let dst = row * dst_stride;
        rgb[dst..dst + dst_stride].copy_from_slice(&frame.rgb[src..src + dst_stride]);
    }
    let cropped = Frame {
        width: rect.w,
        height: rect.h,
        rgb,
    };
    cropped.check()?;
    Ok((rect, cropped))
}

/// Paint `src` onto `dst` at (`x`, `y`).
pub fn blit(dst: &mut Frame, src: &Frame, x: u32, y: u32) -> Result<(), String> {
    dst.check()?;
    src.check()?;
    let x_end = x
        .checked_add(src.width)
        .filter(|end| *end <= dst.width)
        .ok_or("patch is past the right edge")?;
    let y_end = y
        .checked_add(src.height)
        .filter(|end| *end <= dst.height)
        .ok_or("patch is past the bottom edge")?;
    let _ = (x_end, y_end);
    let src_stride = src.width as usize * 3;
    let dst_stride = dst.width as usize * 3;
    for row in 0..src.height as usize {
        let from = row * src_stride;
        let to = (y as usize + row) * dst_stride + x as usize * 3;
        dst.rgb[to..to + src_stride].copy_from_slice(&src.rgb[from..from + src_stride]);
    }
    Ok(())
}

/// `RECORD_PATCH` payload after the type byte: x, y, w, h, then the coded tile.
pub fn encode_payload(rect: Rect, coded: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + coded.len());
    out.extend_from_slice(&(rect.x as u16).to_le_bytes());
    out.extend_from_slice(&(rect.y as u16).to_le_bytes());
    out.extend_from_slice(&(rect.w as u16).to_le_bytes());
    out.extend_from_slice(&(rect.h as u16).to_le_bytes());
    out.extend_from_slice(coded);
    out
}

pub fn decode_payload(bytes: &[u8]) -> Result<(Rect, &[u8]), String> {
    if bytes.len() < 8 {
        return Err("patch is short".into());
    }
    let x = u16::from_le_bytes([bytes[0], bytes[1]]) as u32;
    let y = u16::from_le_bytes([bytes[2], bytes[3]]) as u32;
    let w = u16::from_le_bytes([bytes[4], bytes[5]]) as u32;
    let h = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
    if w == 0 || h == 0 {
        return Err("empty patch".into());
    }
    Ok((Rect { x, y, w, h }, &bytes[8..]))
}

/// Record type this payload is sent as.
pub const RECORD: u8 = RECORD_PATCH;

fn dirty_tiles(prev: &[u8], next: &[u8], width: u32, height: u32) -> Vec<(u32, u32)> {
    let tiles_x = width.div_ceil(TILE);
    let tiles_y = height.div_ceil(TILE);
    let mut dirty = Vec::new();
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            if tile_changed(prev, next, width, height, tx, ty) {
                dirty.push((tx, ty));
            }
        }
    }
    dirty
}

fn tile_changed(prev: &[u8], next: &[u8], width: u32, height: u32, tx: u32, ty: u32) -> bool {
    let x0 = tx * TILE;
    let y0 = ty * TILE;
    let x1 = (x0 + TILE).min(width);
    let y1 = (y0 + TILE).min(height);
    let stride = width as usize * 3;
    let row_bytes = (x1 - x0) as usize * 3;
    for y in y0..y1 {
        let at = y as usize * stride + x0 as usize * 3;
        if prev[at..at + row_bytes] != next[at..at + row_bytes] {
            return true;
        }
    }
    false
}

fn merge(tiles: Vec<(u32, u32)>, width: u32, height: u32) -> Vec<Rect> {
    if tiles.is_empty() {
        return Vec::new();
    }
    let mut runs: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut i = 0;
    while i < tiles.len() {
        let (tx, ty) = tiles[i];
        let mut tx1 = tx + 1;
        i += 1;
        while i < tiles.len() && tiles[i].1 == ty && tiles[i].0 == tx1 {
            tx1 += 1;
            i += 1;
        }
        runs.push((tx, ty, tx1, ty + 1));
    }
    let mut merged: Vec<(u32, u32, u32, u32)> = Vec::new();
    for run in runs {
        if let Some(last) = merged.last_mut() {
            if last.0 == run.0 && last.2 == run.2 && last.3 == run.1 {
                last.3 = run.3;
                continue;
            }
        }
        merged.push(run);
    }
    merged
        .into_iter()
        .map(|(tx0, ty0, tx1, ty1)| {
            clamp(
                Rect {
                    x: tx0 * TILE,
                    y: ty0 * TILE,
                    w: (tx1 - tx0) * TILE,
                    h: (ty1 - ty0) * TILE,
                },
                width,
                height,
            )
        })
        .filter(|rect| rect.w > 0 && rect.h > 0)
        .collect()
}

fn clamp(rect: Rect, width: u32, height: u32) -> Rect {
    let x = rect.x.min(width) & !1;
    let y = rect.y.min(height) & !1;
    let x1 = (rect.x.saturating_add(rect.w)).min(width);
    let y1 = (rect.y.saturating_add(rect.h)).min(height);
    let w = x1.saturating_sub(x) & !1;
    let h = y1.saturating_sub(y) & !1;
    Rect { x, y, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        rgb.repeat(width as usize * height as usize)
    }

    fn paint(buf: &mut [u8], width: u32, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
        let stride = width as usize * 3;
        for row in y..y + h {
            for col in x..x + w {
                let at = row as usize * stride + col as usize * 3;
                buf[at..at + 3].copy_from_slice(&rgb);
            }
        }
    }

    #[test]
    fn identical_pictures_are_unchanged() {
        let prev = solid(64, 48, [10, 20, 30]);
        assert_eq!(plan(Some(&prev), &prev, 64, 48), Plan::Unchanged);
        assert_eq!(plan(None, &prev, 64, 48), Plan::Full);
    }

    #[test]
    fn a_small_dot_is_one_patch() {
        let prev = solid(64, 48, [0, 0, 0]);
        let mut next = prev.clone();
        paint(&mut next, 64, 20, 18, 4, 4, [255, 0, 0]);
        match plan(Some(&prev), &next, 64, 48) {
            Plan::Patches(rects) => {
                assert_eq!(rects.len(), 1);
                assert!(rects[0].x <= 20 && rects[0].y <= 18);
                assert!(rects[0].x + rects[0].w >= 24);
                assert!(rects[0].y + rects[0].h >= 22);
                assert!(rects[0].area() < 64 * 48 / 2);
            }
            other => panic!("expected patches, got {other:?}"),
        }
    }

    #[test]
    fn a_mostly_new_picture_is_a_full_frame() {
        let prev = solid(64, 48, [0, 0, 0]);
        let next = solid(64, 48, [1, 1, 1]);
        assert_eq!(plan(Some(&prev), &next, 64, 48), Plan::Full);
    }

    #[test]
    fn crop_and_blit_round_trip() {
        let mut rgb = solid(32, 16, [8, 8, 8]);
        paint(&mut rgb, 32, 8, 4, 8, 6, [9, 10, 11]);
        let frame = Frame {
            width: 32,
            height: 16,
            rgb,
        };
        let rect = Rect {
            x: 8,
            y: 4,
            w: 8,
            h: 6,
        };
        let (used, piece) = crop(&frame, rect).unwrap();
        assert_eq!(used, rect);
        assert_eq!(piece.width, 8);
        assert_eq!(piece.height, 6);
        let mut blank = Frame {
            width: 32,
            height: 16,
            rgb: solid(32, 16, [8, 8, 8]),
        };
        blit(&mut blank, &piece, 8, 4).unwrap();
        assert_eq!(blank.rgb, frame.rgb);
    }

    #[test]
    fn patch_payload_round_trips() {
        let rect = Rect {
            x: 16,
            y: 32,
            w: 48,
            h: 8,
        };
        let bytes = encode_payload(rect, b"jpeg");
        let (back, coded) = decode_payload(&bytes).unwrap();
        assert_eq!(back, rect);
        assert_eq!(coded, b"jpeg");
        assert_eq!(RECORD, b'R');
    }
}
