//! The host cursor, sent to the player on the negotiated video stream.
//!
//! The payload rides an `EVI2` record whose first byte is
//! [`crate::negotiate::RECORD_CURSOR`]. An older player skips that record.
//! Position is in the picture the player is decoding, not the host's virtual
//! screen: the host maps it with [`CursorUpdate::map_to_frame`].

use std::sync::{Arc, Mutex};

/// Longest cursor edge this record will carry. Larger system cursors are sent
/// as a shape id only.
pub const MAX_EDGE: u16 = 256;

const VERSION: u8 = 1;
const FLAG_VISIBLE: u8 = 1;
const HEADER: usize = 24;

/// A system cursor the player can draw with its own toolkit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CursorShape {
    Arrow = 0,
    Hand = 1,
    IBeam = 2,
    Wait = 3,
    Cross = 4,
    SizeNs = 5,
    SizeWe = 6,
    SizeNwse = 7,
    SizeNesw = 8,
    SizeAll = 9,
    No = 10,
    Help = 11,
    UpArrow = 12,
    AppStarting = 13,
    Custom = 14,
}

impl CursorShape {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Arrow,
            1 => Self::Hand,
            2 => Self::IBeam,
            3 => Self::Wait,
            4 => Self::Cross,
            5 => Self::SizeNs,
            6 => Self::SizeWe,
            7 => Self::SizeNwse,
            8 => Self::SizeNesw,
            9 => Self::SizeAll,
            10 => Self::No,
            11 => Self::Help,
            12 => Self::UpArrow,
            13 => Self::AppStarting,
            _ => Self::Custom,
        }
    }
}

/// One cursor report. An empty `bgra` keeps the player's previous bitmap when
/// `image_id` is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorUpdate {
    pub visible: bool,
    pub shape: CursorShape,
    /// Picture pixel, after [`Self::map_to_frame`].
    pub x: i32,
    pub y: i32,
    pub hotspot_x: i16,
    pub hotspot_y: i16,
    pub image_id: u32,
    pub width: u16,
    pub height: u16,
    /// Top-down BGRA. Empty when the bitmap did not change.
    pub bgra: Vec<u8>,
}

impl CursorUpdate {
    pub fn encode(&self) -> Vec<u8> {
        let pixels = if self.width == 0 || self.height == 0 {
            &[][..]
        } else {
            self.bgra.as_slice()
        };
        let mut out = Vec::with_capacity(HEADER + pixels.len());
        out.push(VERSION);
        out.push(if self.visible { FLAG_VISIBLE } else { 0 });
        out.push(self.shape as u8);
        out.push(0);
        out.extend_from_slice(&self.x.to_le_bytes());
        out.extend_from_slice(&self.y.to_le_bytes());
        out.extend_from_slice(&self.hotspot_x.to_le_bytes());
        out.extend_from_slice(&self.hotspot_y.to_le_bytes());
        let (width, height) =
            if pixels.len() == usize::from(self.width) * usize::from(self.height) * 4 {
                (self.width, self.height)
            } else {
                (0, 0)
            };
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&self.image_id.to_le_bytes());
        if width > 0 {
            out.extend_from_slice(pixels);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER {
            return Err("cursor record is short".into());
        }
        if bytes[0] != VERSION {
            return Err(format!("cursor version {}", bytes[0]));
        }
        let x = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let y = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let hotspot_x = i16::from_le_bytes(bytes[12..14].try_into().unwrap());
        let hotspot_y = i16::from_le_bytes(bytes[14..16].try_into().unwrap());
        let width = u16::from_le_bytes(bytes[16..18].try_into().unwrap());
        let height = u16::from_le_bytes(bytes[18..20].try_into().unwrap());
        let image_id = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        if width > MAX_EDGE || height > MAX_EDGE {
            return Err(format!("cursor is {width}x{height}"));
        }
        let pixels = usize::from(width) * usize::from(height) * 4;
        if bytes.len() != HEADER + pixels {
            return Err("cursor bitmap length does not match".into());
        }
        Ok(Self {
            visible: bytes[1] & FLAG_VISIBLE != 0,
            shape: CursorShape::from_u8(bytes[2]),
            x,
            y,
            hotspot_x,
            hotspot_y,
            image_id,
            width,
            height,
            bgra: bytes[HEADER..].to_vec(),
        })
    }

    /// `x` and `y` are virtual-screen pixels. The result is in the captured
    /// picture, which may be a smaller copy of that monitor.
    pub fn map_to_frame(
        &self,
        origin_x: i32,
        origin_y: i32,
        screen_w: u32,
        screen_h: u32,
        frame_w: u32,
        frame_h: u32,
    ) -> Self {
        let mut next = self.clone();
        let (x, y) = frame_point(
            self.x, self.y, origin_x, origin_y, screen_w, screen_h, frame_w, frame_h,
        );
        next.x = x;
        next.y = y;
        next
    }
}

/// Virtual-screen point to a pixel of the picture the player decodes.
pub fn frame_point(
    x: i32,
    y: i32,
    origin_x: i32,
    origin_y: i32,
    screen_w: u32,
    screen_h: u32,
    frame_w: u32,
    frame_h: u32,
) -> (i32, i32) {
    if screen_w == 0 || screen_h == 0 || frame_w == 0 || frame_h == 0 {
        return (x.saturating_sub(origin_x), y.saturating_sub(origin_y));
    }
    let x = (i64::from(x) - i64::from(origin_x)) * i64::from(frame_w) / i64::from(screen_w);
    let y = (i64::from(y) - i64::from(origin_y)) * i64::from(frame_h) / i64::from(screen_h);
    (x as i32, y as i32)
}

/// Where the picture sits in the window. Matches `play`'s letterbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub src_w: u32,
    pub src_h: u32,
    pub view_w: u32,
    pub view_h: u32,
    pub crop_x: u32,
    pub crop_y: u32,
    pub dst_x: u32,
    pub dst_y: u32,
}

/// Window pixel of the cursor bitmap's top left, and its drawn size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpriteBox {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// The remote cursor drawn on the letterboxed picture. Hotspot and bitmap
/// scale with the picture.
pub fn sprite_box(
    place: Placed,
    cursor_x: i32,
    cursor_y: i32,
    hot_x: i32,
    hot_y: i32,
    bmp_w: u32,
    bmp_h: u32,
) -> SpriteBox {
    let src_w = place.src_w.max(1);
    let src_h = place.src_h.max(1);
    let view_w = place.view_w.max(1);
    let view_h = place.view_h.max(1);
    let wx = i64::from(place.dst_x) + i64::from(cursor_x) * i64::from(view_w) / i64::from(src_w)
        - i64::from(place.crop_x);
    let wy = i64::from(place.dst_y) + i64::from(cursor_y) * i64::from(view_h) / i64::from(src_h)
        - i64::from(place.crop_y);
    let hx = i64::from(hot_x) * i64::from(view_w) / i64::from(src_w);
    let hy = i64::from(hot_y) * i64::from(view_h) / i64::from(src_h);
    let w = (i64::from(bmp_w.max(1)) * i64::from(view_w) + i64::from(src_w) / 2) / i64::from(src_w);
    let h = (i64::from(bmp_h.max(1)) * i64::from(view_h) + i64::from(src_h) / 2) / i64::from(src_h);
    SpriteBox {
        x: (wx - hx) as i32,
        y: (wy - hy) as i32,
        w: w.max(1) as u32,
        h: h.max(1) as u32,
    }
}

/// Draw `bgra` into `0x00RRGGBB` window pixels. A zero alpha leaves the pixel.
pub fn paint_sprite(
    dst: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    sprite: SpriteBox,
    bgra: &[u8],
    src_w: u32,
    src_h: u32,
) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 || sprite.w == 0 || sprite.h == 0 {
        return;
    }
    let need = (dst_w as usize).saturating_mul(dst_h as usize);
    if dst.len() < need || bgra.len() < (src_w as usize) * (src_h as usize) * 4 {
        return;
    }
    for y in 0..sprite.h {
        let dy = sprite.y + y as i32;
        if dy < 0 || dy as u32 >= dst_h {
            continue;
        }
        let sy = (u64::from(y) * u64::from(src_h)) / u64::from(sprite.h);
        for x in 0..sprite.w {
            let dx = sprite.x + x as i32;
            if dx < 0 || dx as u32 >= dst_w {
                continue;
            }
            let sx = (u64::from(x) * u64::from(src_w)) / u64::from(sprite.w);
            let si = ((sy * u64::from(src_w) + sx) * 4) as usize;
            let pixel = [bgra[si], bgra[si + 1], bgra[si + 2], bgra[si + 3]];
            let at = (dy as u32 * dst_w + dx as u32) as usize;
            dst[at] = blend(dst[at], pixel);
        }
    }
}

fn blend(dst: u32, bgra: [u8; 4]) -> u32 {
    let alpha = u32::from(bgra[3]);
    if alpha == 0 {
        return dst;
    }
    if alpha == 255 {
        return (u32::from(bgra[2]) << 16) | (u32::from(bgra[1]) << 8) | u32::from(bgra[0]);
    }
    let dr = (dst >> 16) & 255;
    let dg = (dst >> 8) & 255;
    let db = dst & 255;
    let r = (u32::from(bgra[2]) * alpha + dr * (255 - alpha)) / 255;
    let g = (u32::from(bgra[1]) * alpha + dg * (255 - alpha)) / 255;
    let b = (u32::from(bgra[0]) * alpha + db * (255 - alpha)) / 255;
    (r << 16) | (g << 8) | b
}

/// BGRA to straight RGBA for a toolkit cursor.
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = bgra.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    rgba
}

pub fn image_id(shape: CursorShape, bgra: &[u8]) -> u32 {
    let mut hash = 2_166_136_261u32;
    hash ^= u32::from(shape as u8);
    hash = hash.wrapping_mul(16_777_619);
    for byte in bgra {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    if hash == 0 {
        1
    } else {
        hash
    }
}

/// A set AND bit makes that pixel transparent. `mask` is 32-bit expanded.
pub fn apply_and_mask(bgra: &mut [u8], mask: &[u8]) {
    for (pixel, mask) in bgra.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        let white = mask[0] != 0 || mask[1] != 0 || mask[2] != 0;
        if pixel[3] == 0 {
            pixel[3] = if white { 0 } else { 255 };
        }
    }
}

/// Monochrome cursor. `mask` is the AND plane then the XOR plane, each
/// `width * height` pixels, expanded to 32-bit.
pub fn mono_pixels(mask: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixels = width as usize * height as usize;
    let mut out = vec![0u8; pixels * 4];
    if mask.len() < pixels * 8 {
        return out;
    }
    for i in 0..pixels {
        let and = &mask[i * 4..i * 4 + 4];
        let xor = &mask[(pixels + i) * 4..(pixels + i) * 4 + 4];
        let and_set = and[0] != 0 || and[1] != 0 || and[2] != 0;
        let xor_set = xor[0] != 0 || xor[1] != 0 || xor[2] != 0;
        let (b, g, r, a) = match (and_set, xor_set) {
            (false, false) => (0, 0, 0, 255),
            (false, true) => (255, 255, 255, 255),
            (true, false) => (0, 0, 0, 0),
            (true, true) => (0, 0, 0, 255),
        };
        let at = i * 4;
        out[at] = b;
        out[at + 1] = g;
        out[at + 2] = r;
        out[at + 3] = a;
    }
    out
}

/// Stand-in bitmap when the host could not read the cursor picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorBitmap {
    pub width: u16,
    pub height: u16,
    pub hotspot_x: i16,
    pub hotspot_y: i16,
    pub bgra: Vec<u8>,
}

pub fn fallback(shape: CursorShape) -> CursorBitmap {
    let mut bgra = vec![0u8; 16 * 16 * 4];
    let ink = [0u8, 0, 0, 255];
    let paper = [255u8, 255, 255, 255];
    match shape {
        CursorShape::IBeam => {
            hline(&mut bgra, 4, 11, 1, paper);
            hline(&mut bgra, 4, 11, 14, paper);
            vline(&mut bgra, 7, 1, 14, ink);
        }
        CursorShape::SizeNs | CursorShape::UpArrow => {
            vline(&mut bgra, 7, 1, 14, ink);
            vline(&mut bgra, 8, 1, 14, ink);
            for n in 0..5 {
                put(&mut bgra, 7 - n, 1 + n, paper);
                put(&mut bgra, 8 + n, 1 + n, paper);
                put(&mut bgra, 7 - n, 14 - n, paper);
                put(&mut bgra, 8 + n, 14 - n, paper);
            }
        }
        CursorShape::SizeWe => {
            hline(&mut bgra, 1, 14, 7, ink);
            hline(&mut bgra, 1, 14, 8, ink);
            for n in 0..5 {
                put(&mut bgra, 1 + n, 7 - n, paper);
                put(&mut bgra, 1 + n, 8 + n, paper);
                put(&mut bgra, 14 - n, 7 - n, paper);
                put(&mut bgra, 14 - n, 8 + n, paper);
            }
        }
        CursorShape::SizeNwse | CursorShape::SizeNesw => {
            for n in 1..15 {
                let x = if shape == CursorShape::SizeNwse {
                    n
                } else {
                    15 - n
                };
                put(&mut bgra, x, n, ink);
                put(&mut bgra, x.saturating_sub(1), n, paper);
            }
        }
        CursorShape::SizeAll => {
            vline(&mut bgra, 7, 1, 14, ink);
            hline(&mut bgra, 1, 14, 7, ink);
        }
        CursorShape::Cross => {
            vline(&mut bgra, 7, 2, 13, ink);
            hline(&mut bgra, 2, 13, 7, ink);
        }
        CursorShape::No => {
            for n in 2..14 {
                put(&mut bgra, n, 2, ink);
                put(&mut bgra, n, 13, ink);
                put(&mut bgra, 2, n, ink);
                put(&mut bgra, 13, n, ink);
                put(&mut bgra, n, n, paper);
            }
        }
        CursorShape::Hand => {
            for y in 6..14 {
                hline(&mut bgra, 4, 11, y, ink);
            }
            vline(&mut bgra, 6, 2, 8, ink);
            vline(&mut bgra, 9, 3, 8, ink);
            put(&mut bgra, 2, 12, paper);
        }
        CursorShape::Wait | CursorShape::AppStarting => {
            hline(&mut bgra, 3, 12, 2, ink);
            hline(&mut bgra, 3, 12, 13, ink);
            for n in 0..6 {
                put(&mut bgra, 4 + n, 3 + n, paper);
                put(&mut bgra, 11 - n, 3 + n, paper);
                put(&mut bgra, 4 + n, 12 - n, paper);
                put(&mut bgra, 11 - n, 12 - n, paper);
            }
        }
        CursorShape::Help => {
            hline(&mut bgra, 4, 10, 2, ink);
            vline(&mut bgra, 10, 2, 6, ink);
            hline(&mut bgra, 6, 10, 6, ink);
            vline(&mut bgra, 6, 6, 9, ink);
            put(&mut bgra, 6, 12, ink);
        }
        CursorShape::Arrow | CursorShape::Custom => arrow(&mut bgra, ink, paper),
    }
    CursorBitmap {
        width: 16,
        height: 16,
        hotspot_x: if matches!(shape, CursorShape::Cross | CursorShape::SizeAll) {
            7
        } else {
            1
        },
        hotspot_y: if matches!(shape, CursorShape::Cross | CursorShape::SizeAll) {
            7
        } else {
            1
        },
        bgra,
    }
}

fn arrow(bgra: &mut [u8], ink: [u8; 4], paper: [u8; 4]) {
    for y in 0..11 {
        for x in 0..=y {
            let color = if x == 0 || x == y || y == 10 {
                paper
            } else {
                ink
            };
            put(bgra, x, y, color);
        }
    }
    for y in 11..16 {
        for x in 3..6 {
            put(bgra, x, y, ink);
        }
    }
}

fn put(bgra: &mut [u8], x: i32, y: i32, pixel: [u8; 4]) {
    if x < 0 || y < 0 || x >= 16 || y >= 16 {
        return;
    }
    let at = (y as usize * 16 + x as usize) * 4;
    bgra[at..at + 4].copy_from_slice(&pixel);
}

fn hline(bgra: &mut [u8], x0: i32, x1: i32, y: i32, pixel: [u8; 4]) {
    for x in x0..=x1 {
        put(bgra, x, y, pixel);
    }
}

fn vline(bgra: &mut [u8], x: i32, y0: i32, y1: i32, pixel: [u8; 4]) {
    for y in y0..=y1 {
        put(bgra, x, y, pixel);
    }
}

/// What the picture window has heard, including a bitmap from an earlier report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorView {
    pub seq: u64,
    pub visible: bool,
    pub shape: CursorShape,
    pub x: i32,
    pub y: i32,
    pub hotspot_x: i16,
    pub hotspot_y: i16,
    pub image_id: u32,
    pub width: u16,
    pub height: u16,
    pub bgra: Vec<u8>,
}

impl Default for CursorView {
    fn default() -> Self {
        Self {
            seq: 0,
            visible: true,
            shape: CursorShape::Arrow,
            x: 0,
            y: 0,
            hotspot_x: 0,
            hotspot_y: 0,
            image_id: 0,
            width: 0,
            height: 0,
            bgra: Vec::new(),
        }
    }
}

impl CursorView {
    pub fn apply(&mut self, update: CursorUpdate) {
        self.seq = self.seq.wrapping_add(1);
        self.visible = update.visible;
        self.shape = update.shape;
        self.x = update.x;
        self.y = update.y;
        self.hotspot_x = update.hotspot_x;
        self.hotspot_y = update.hotspot_y;
        let pixels = usize::from(update.width) * usize::from(update.height) * 4;
        if update.width > 0 && update.bgra.len() == pixels {
            self.bgra = update.bgra;
            self.width = update.width;
            self.height = update.height;
            self.image_id = update.image_id;
        } else if update.image_id != self.image_id {
            self.bgra.clear();
            self.width = 0;
            self.height = 0;
            self.image_id = update.image_id;
        }
    }
}

pub fn share() -> Arc<Mutex<CursorView>> {
    Arc::new(Mutex::new(CursorView::default()))
}

/// Samples the host cursor. `poll` is `None` when nothing changed, and on a
/// system this build does not read.
pub struct Watch {
    #[cfg(windows)]
    inner: win::Inner,
}

impl Watch {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            inner: win::Inner::new(),
        }
    }

    pub fn poll(&mut self) -> Option<CursorUpdate> {
        #[cfg(windows)]
        {
            self.inner.poll()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::ptr::null_mut;

    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, RGBQUAD,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorInfo, GetIconInfo, LoadCursorW, CURSORINFO, CURSOR_SHOWING, ICONINFO,
        IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_NO, IDC_SIZEALL,
        IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_UPARROW, IDC_WAIT,
    };

    use super::{image_id, CursorShape, CursorUpdate, MAX_EDGE};

    struct Held {
        handle: isize,
        shape: CursorShape,
        hotspot_x: i16,
        hotspot_y: i16,
        image_id: u32,
        x: i32,
        y: i32,
        visible: bool,
    }

    pub struct Inner {
        shapes: Vec<(isize, CursorShape)>,
        held: Option<Held>,
    }

    impl Inner {
        pub fn new() -> Self {
            Self {
                shapes: standard_shapes(),
                held: None,
            }
        }

        pub fn poll(&mut self) -> Option<CursorUpdate> {
            let Some(read) = read_cursor() else {
                return None;
            };
            let shape = self
                .shapes
                .iter()
                .find(|(handle, _)| *handle == read.handle)
                .map(|(_, shape)| *shape)
                .unwrap_or(CursorShape::Custom);
            let changed = self
                .held
                .as_ref()
                .map(|held| held.handle != read.handle)
                .unwrap_or(true);
            let moved = self
                .held
                .as_ref()
                .map(|held| {
                    held.x != read.x
                        || held.y != read.y
                        || held.visible != read.visible
                        || held.shape != shape
                })
                .unwrap_or(true);
            if !changed && !moved {
                return None;
            }
            let (hotspot_x, hotspot_y, image_id, width, height, bgra) = if changed {
                match capture(read.handle, shape) {
                    Some(bitmap) => bitmap,
                    None => (0, 0, image_id(shape, &[]), 0, 0, Vec::new()),
                }
            } else if let Some(held) = &self.held {
                (
                    held.hotspot_x,
                    held.hotspot_y,
                    held.image_id,
                    0,
                    0,
                    Vec::new(),
                )
            } else {
                (0, 0, 0, 0, 0, Vec::new())
            };
            self.held = Some(Held {
                handle: read.handle,
                shape,
                hotspot_x,
                hotspot_y,
                image_id,
                x: read.x,
                y: read.y,
                visible: read.visible,
            });
            Some(CursorUpdate {
                visible: read.visible,
                shape,
                x: read.x,
                y: read.y,
                hotspot_x,
                hotspot_y,
                image_id,
                width,
                height,
                bgra,
            })
        }
    }

    struct Read {
        handle: isize,
        x: i32,
        y: i32,
        visible: bool,
    }

    fn read_cursor() -> Option<Read> {
        let mut info = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            flags: 0,
            hCursor: null_mut(),
            ptScreenPos: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
        };
        // SAFETY: `info.cbSize` is the struct size and the pointer is live.
        if unsafe { GetCursorInfo(&mut info) } == 0 {
            return None;
        }
        Some(Read {
            handle: info.hCursor as isize,
            x: info.ptScreenPos.x,
            y: info.ptScreenPos.y,
            visible: info.flags & CURSOR_SHOWING != 0,
        })
    }

    fn standard_shapes() -> Vec<(isize, CursorShape)> {
        let pairs = [
            (IDC_ARROW, CursorShape::Arrow),
            (IDC_HAND, CursorShape::Hand),
            (IDC_IBEAM, CursorShape::IBeam),
            (IDC_WAIT, CursorShape::Wait),
            (IDC_CROSS, CursorShape::Cross),
            (IDC_SIZENS, CursorShape::SizeNs),
            (IDC_SIZEWE, CursorShape::SizeWe),
            (IDC_SIZENWSE, CursorShape::SizeNwse),
            (IDC_SIZENESW, CursorShape::SizeNesw),
            (IDC_SIZEALL, CursorShape::SizeAll),
            (IDC_NO, CursorShape::No),
            (IDC_HELP, CursorShape::Help),
            (IDC_UPARROW, CursorShape::UpArrow),
            (IDC_APPSTARTING, CursorShape::AppStarting),
        ];
        pairs
            .into_iter()
            .filter_map(|(id, shape)| {
                // SAFETY: a null instance loads a shared system cursor. The
                // handle is not destroyed.
                let cursor = unsafe { LoadCursorW(null_mut(), id) };
                if cursor.is_null() {
                    None
                } else {
                    Some((cursor as isize, shape))
                }
            })
            .collect()
    }

    fn capture(handle: isize, shape: CursorShape) -> Option<(i16, i16, u32, u16, u16, Vec<u8>)> {
        if handle == 0 {
            return None;
        }
        let mut icon = ICONINFO {
            fIcon: 0,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: null_mut(),
            hbmColor: null_mut(),
        };
        // SAFETY: `icon` is a live ICONINFO. GetIconInfo fills the two bitmap
        // handles, which this function deletes.
        if unsafe { GetIconInfo(handle as _, &mut icon) } == 0 {
            return None;
        }
        let color = Bitmap(icon.hbmColor);
        let mask = Bitmap(icon.hbmMask);
        let hot_x = i16::try_from(icon.xHotspot).unwrap_or(0);
        let hot_y = i16::try_from(icon.yHotspot).unwrap_or(0);
        let bgra = if !color.0.is_null() {
            let (width, height, mut pixels) = dib(color.0)?;
            if pixels.chunks_exact(4).all(|pixel| pixel[3] == 0) {
                if let Some((_, _, mask_pixels)) = dib(mask.0) {
                    super::apply_and_mask(&mut pixels, &mask_pixels);
                }
            }
            Some((width, height, pixels))
        } else if !mask.0.is_null() {
            let (width, height, pixels) = dib(mask.0)?;
            if height % 2 != 0 {
                return None;
            }
            let cursor_h = height / 2;
            let mono = super::mono_pixels(&pixels, width, cursor_h);
            Some((width, cursor_h, mono))
        } else {
            None
        }?;
        let (width, height, bgra) = bgra;
        if width == 0
            || height == 0
            || width > u32::from(MAX_EDGE)
            || height > u32::from(MAX_EDGE)
            || bgra.len() != width as usize * height as usize * 4
        {
            return Some((hot_x, hot_y, image_id(shape, &[]), 0, 0, Vec::new()));
        }
        let id = image_id(shape, &bgra);
        Some((hot_x, hot_y, id, width as u16, height as u16, bgra))
    }

    fn dib(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
        if bitmap.is_null() {
            return None;
        }
        let mut info = BITMAP {
            bmType: 0,
            bmWidth: 0,
            bmHeight: 0,
            bmWidthBytes: 0,
            bmPlanes: 0,
            bmBitsPixel: 0,
            bmBits: null_mut(),
        };
        // SAFETY: `info` is a live BITMAP and `bitmap` is a GDI bitmap.
        let got = unsafe {
            GetObjectW(
                bitmap as HGDIOBJ,
                std::mem::size_of::<BITMAP>() as i32,
                &mut info as *mut BITMAP as *mut _,
            )
        };
        if got == 0 || info.bmWidth <= 0 || info.bmHeight == 0 {
            return None;
        }
        let width = info.bmWidth as u32;
        let height = info.bmHeight.unsigned_abs();
        if width > 4096 || height > 4096 {
            return None;
        }
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: info.bmWidth,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let dc = Dc(unsafe { CreateCompatibleDC(null_mut()) });
        if dc.0.is_null() {
            return None;
        }
        // SAFETY: `dc` is a memory DC, `pixels` fits `width * height` 32-bit
        // pixels, and `header` describes that buffer. A negative height asks
        // for top-down rows.
        let rows = unsafe {
            GetDIBits(
                dc.0,
                bitmap,
                0,
                height,
                pixels.as_mut_ptr() as *mut _,
                &mut header,
                DIB_RGB_COLORS,
            )
        };
        if rows == 0 {
            None
        } else {
            Some((width, height, pixels))
        }
    }

    struct Bitmap(HBITMAP);

    impl Drop for Bitmap {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { DeleteObject(self.0 as HGDIOBJ) };
            }
        }
    }

    struct Dc(HDC);

    impl Drop for Dc {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { DeleteDC(self.0) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CursorUpdate {
        CursorUpdate {
            visible: true,
            shape: CursorShape::Hand,
            x: 10,
            y: -4,
            hotspot_x: 3,
            hotspot_y: 1,
            image_id: 9,
            width: 1,
            height: 1,
            bgra: vec![1, 2, 3, 255],
        }
    }

    #[test]
    fn a_cursor_record_round_trips() {
        let update = sample();
        let back = CursorUpdate::decode(&update.encode()).unwrap();
        assert_eq!(back, update);
        let mut position = update.clone();
        position.width = 0;
        position.height = 0;
        position.bgra.clear();
        position.x = 40;
        let back = CursorUpdate::decode(&position.encode()).unwrap();
        assert_eq!(back, position);
        assert!(CursorUpdate::decode(&[1, 0]).is_err());
        assert_eq!(CursorShape::from_u8(99), CursorShape::Custom);
    }

    #[test]
    fn frame_point_scales_from_the_monitor_origin() {
        assert_eq!(
            frame_point(960, 540, 0, 0, 1920, 1080, 1280, 720),
            (640, 360)
        );
        assert_eq!(frame_point(0, 0, -1920, 0, 1920, 1080, 960, 540), (960, 0));
        assert_eq!(frame_point(-10, 4, 0, 0, 100, 100, 100, 100), (-10, 4));
    }

    #[test]
    fn the_sprite_follows_the_letterbox_and_the_hotspot() {
        let place = Placed {
            src_w: 1920,
            src_h: 1080,
            view_w: 1280,
            view_h: 720,
            crop_x: 0,
            crop_y: 0,
            dst_x: 0,
            dst_y: 152,
        };
        let sprite = sprite_box(place, 960, 540, 0, 0, 32, 32);
        assert_eq!(
            sprite,
            SpriteBox {
                x: 640,
                y: 512,
                w: 21,
                h: 21
            }
        );
        let hot = sprite_box(place, 960, 540, 16, 16, 32, 32);
        assert_eq!(hot.x, 640 - 10);
        assert_eq!(hot.y, 512 - 10);
    }

    #[test]
    fn paint_blends_and_skips_a_clear_pixel() {
        let mut dst = [0u32, 0x00FF_0000];
        paint_sprite(
            &mut dst,
            2,
            1,
            SpriteBox {
                x: 0,
                y: 0,
                w: 2,
                h: 1,
            },
            &[255, 0, 0, 255, 0, 0, 0, 0],
            2,
            1,
        );
        assert_eq!(dst[0], 0x0000_00FF);
        assert_eq!(dst[1], 0x00FF_0000);
    }

    #[test]
    fn a_later_position_keeps_the_bitmap() {
        let mut view = CursorView::default();
        view.apply(sample());
        assert_eq!(view.bgra, vec![1, 2, 3, 255]);
        view.apply(CursorUpdate {
            x: 80,
            y: 70,
            width: 0,
            height: 0,
            bgra: Vec::new(),
            ..sample()
        });
        assert_eq!((view.x, view.y), (80, 70));
        assert_eq!(view.width, 1);
        assert_eq!(view.bgra, vec![1, 2, 3, 255]);
        view.apply(CursorUpdate {
            image_id: 3,
            width: 0,
            height: 0,
            bgra: Vec::new(),
            shape: CursorShape::SizeNwse,
            ..sample()
        });
        assert!(view.bgra.is_empty());
        assert_eq!(view.shape, CursorShape::SizeNwse);
    }

    #[test]
    fn mono_and_mask_pixels() {
        let mut color = vec![0, 0, 0, 0, 10, 20, 30, 0];
        let mask = vec![255, 255, 255, 0, 0, 0, 0, 0];
        apply_and_mask(&mut color, &mask);
        assert_eq!(color[3], 0);
        assert_eq!(color[7], 255);
        let mono = mono_pixels(&[0, 0, 0, 0, 255, 255, 255, 0], 1, 1);
        assert_eq!(&mono, &[255, 255, 255, 255]);
        let clear = mono_pixels(&[255, 255, 255, 0, 0, 0, 0, 0], 1, 1);
        assert_eq!(clear[3], 0);
    }

    #[test]
    fn rgba_swaps_blue_and_red() {
        assert_eq!(bgra_to_rgba(&[1, 2, 3, 4]), vec![3, 2, 1, 4]);
        let arrow = fallback(CursorShape::Arrow);
        assert_eq!(arrow.bgra.len(), 16 * 16 * 4);
        assert!(arrow.bgra.iter().any(|byte| *byte != 0));
    }

    #[cfg(windows)]
    #[test]
    fn the_live_cursor_reports_a_position() {
        let mut watch = Watch::new();
        let update = watch.poll().expect("cursor");
        let back = CursorUpdate::decode(&update.encode()).unwrap();
        assert_eq!(back.shape, update.shape);
        assert_eq!(back.x, update.x);
        let again = watch.poll();
        if let Some(again) = again {
            if again.width == 0 {
                assert_eq!(again.image_id, update.image_id);
            }
        }
    }
}
