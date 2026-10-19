//! How the player shows the picture and what it sends. The UI holds a
//! [`LiveView`] and the picture window reads it on every frame and event, so
//! a change applies to a session that is already open.

use std::sync::{Arc, Mutex};

/// How the remote picture fills the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScaleMode {
    /// Whole picture, bars where the shapes differ. The old behaviour.
    #[default]
    Fit,
    /// Fill the window and crop the overflow.
    Fill,
    /// Fill the window and ignore the shape.
    Stretch,
    /// One remote pixel per window pixel, centred, cropped when larger.
    Native,
}

impl ScaleMode {
    pub const ALL: [ScaleMode; 4] = [Self::Fit, Self::Fill, Self::Stretch, Self::Native];

    pub fn token(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Stretch => "stretch",
            Self::Native => "native",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fit => "Fit",
            Self::Fill => "Fill",
            Self::Stretch => "Stretch",
            Self::Native => "1:1",
        }
    }
}

/// Which side follows the other's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SizeMatch {
    /// Neither changes. The picture is scaled.
    #[default]
    Independent,
    /// The picture window takes the size of the remote picture.
    WindowToHost,
    /// The host changes its resolution to this window. Hosts do not do this yet.
    HostToWindow,
}

impl SizeMatch {
    pub const ALL: [SizeMatch; 3] = [Self::Independent, Self::WindowToHost, Self::HostToWindow];

    pub fn token(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::WindowToHost => "window-to-host",
            Self::HostToWindow => "host-to-window",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Independent => "Scale only",
            Self::WindowToHost => "Window matches host",
            Self::HostToWindow => "Host matches window",
        }
    }

    /// `false` for the choices nothing implements yet.
    pub fn available(self) -> bool {
        !matches!(self, Self::HostToWindow)
    }
}

/// A physical controller on this computer, by name, sent as a fixed slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadSlot {
    pub name: String,
    /// 0 is the host's first controller.
    pub slot: u8,
}

/// Highest slot a host takes. XInput has four.
pub const PAD_SLOTS: u8 = 4;

/// Which cursor the picture window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorMode {
    /// This computer's cursor. Its shape follows the remote desktop: a hand
    /// on a link, arrows on a window edge.
    #[default]
    Native,
    /// The remote cursor's picture, used as this computer's cursor.
    Copy,
    /// This computer's cursor is hidden. The remote cursor is drawn, including
    /// where it sits.
    Remote,
}

impl CursorMode {
    pub const ALL: [CursorMode; 3] = [Self::Native, Self::Copy, Self::Remote];

    pub fn token(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Copy => "copy",
            Self::Remote => "remote",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "Native cursor",
            Self::Copy => "Copy remote cursor",
            Self::Remote => "Remote cursor",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewOptions {
    pub scale: ScaleMode,
    pub size_match: SizeMatch,
    pub send_keyboard: bool,
    pub send_mouse: bool,
    pub send_pads: bool,
    pub cursor: CursorMode,
    /// Painted on the host before the picture is encoded.
    pub look: etherquic::look::Look,
    /// Picture quality 1–100. MJPEG uses this as JPEG quality. Other codecs
    /// scale their default bitrate. 40 is the previous fixed JPEG quality.
    pub quality: u8,
    pub pad_slots: Vec<PadSlot>,
    /// Output device by name. `None` is the default device. Read when sound starts.
    pub output_device: Option<String>,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            scale: ScaleMode::Fit,
            size_match: SizeMatch::Independent,
            send_keyboard: true,
            send_mouse: true,
            send_pads: true,
            cursor: CursorMode::Native,
            look: etherquic::look::Look::Off,
            quality: etherquic::negotiate::DEFAULT_QUALITY,
            pad_slots: Vec::new(),
            output_device: None,
        }
    }
}

pub type LiveView = Arc<Mutex<ViewOptions>>;

pub fn live(options: ViewOptions) -> LiveView {
    Arc::new(Mutex::new(options))
}

/// A copy of the current options. A poisoned lock still has usable options.
pub fn snapshot(view: &LiveView) -> ViewOptions {
    match view.lock() {
        Ok(options) => options.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Where the picture goes in the window.
///
/// The picture is scaled to `view_w`×`view_h`, then the `w`×`h` region at
/// `crop_x`,`crop_y` of that is drawn at `dst_x`,`dst_y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub view_w: u32,
    pub view_h: u32,
    pub crop_x: u32,
    pub crop_y: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub w: u32,
    pub h: u32,
}

/// Largest scaled picture side. Fill on a very wide window stays bounded.
const MAX_VIEW: u32 = 16_384;

pub fn place(mode: ScaleMode, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Placement {
    let dst_w = dst_w.max(1);
    let dst_h = dst_h.max(1);
    if src_w == 0 || src_h == 0 {
        return Placement {
            view_w: dst_w,
            view_h: dst_h,
            crop_x: 0,
            crop_y: 0,
            dst_x: 0,
            dst_y: 0,
            w: dst_w,
            h: dst_h,
        };
    }
    let src_aspect = f64::from(src_w) / f64::from(src_h);
    let dst_aspect = f64::from(dst_w) / f64::from(dst_h);
    let (view_w, view_h) = match mode {
        ScaleMode::Stretch => (dst_w, dst_h),
        ScaleMode::Native => (src_w, src_h),
        ScaleMode::Fit => {
            if src_aspect > dst_aspect {
                (dst_w, (f64::from(dst_w) / src_aspect).round() as u32)
            } else {
                ((f64::from(dst_h) * src_aspect).round() as u32, dst_h)
            }
        }
        ScaleMode::Fill => {
            if src_aspect > dst_aspect {
                ((f64::from(dst_h) * src_aspect).round() as u32, dst_h)
            } else {
                (dst_w, (f64::from(dst_w) / src_aspect).round() as u32)
            }
        }
    };
    let view_w = view_w.clamp(1, MAX_VIEW);
    let view_h = view_h.clamp(1, MAX_VIEW);
    let w = view_w.min(dst_w);
    let h = view_h.min(dst_h);
    Placement {
        view_w,
        view_h,
        crop_x: (view_w - w) / 2,
        crop_y: (view_h - h) / 2,
        dst_x: (dst_w - w) / 2,
        dst_y: (dst_h - h) / 2,
        w,
        h,
    }
}

/// A window position as the 0..=65535 remote pointer coordinates.
pub fn to_remote(place: &Placement, x: f64, y: f64) -> (u16, u16) {
    let local_x = (x - f64::from(place.dst_x)).clamp(0.0, f64::from(place.w.max(1)) - 1.0)
        + f64::from(place.crop_x);
    let local_y = (y - f64::from(place.dst_y)).clamp(0.0, f64::from(place.h.max(1)) - 1.0)
        + f64::from(place.crop_y);
    let fx = local_x / f64::from(place.view_w.max(1));
    let fy = local_y / f64::from(place.view_h.max(1));
    (
        (fx.clamp(0.0, 1.0) * 65535.0).round() as u16,
        (fy.clamp(0.0, 1.0) * 65535.0).round() as u16,
    )
}

/// The slot a controller is sent as: its pinned slot, else the lowest one
/// that no pinned or connected controller holds.
pub fn slot_for(pins: &[PadSlot], name: &str, taken: &[u8]) -> u8 {
    if let Some(pin) = pins.iter().find(|pin| pin.name == name) {
        return pin.slot;
    }
    (0..PAD_SLOTS * 4)
        .find(|slot| !taken.contains(slot) && !pins.iter().any(|pin| pin.slot == *slot))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_matches_the_old_letterbox() {
        let p = place(ScaleMode::Fit, 1920, 1080, 1280, 1024);
        assert_eq!((p.view_w, p.view_h), (1280, 720));
        assert_eq!((p.dst_x, p.dst_y, p.w, p.h), (0, 152, 1280, 720));
        assert_eq!((p.crop_x, p.crop_y), (0, 0));
        let tall = place(ScaleMode::Fit, 1080, 1920, 1280, 720);
        assert_eq!((tall.w, tall.h, tall.dst_x), (405, 720, 437));
    }

    #[test]
    fn fill_crops_and_stretch_ignores_shape() {
        let p = place(ScaleMode::Fill, 1920, 1080, 1000, 1000);
        assert_eq!((p.view_w, p.view_h), (1778, 1000));
        assert_eq!((p.w, p.h, p.dst_x, p.dst_y), (1000, 1000, 0, 0));
        assert_eq!(p.crop_x, 389);
        let s = place(ScaleMode::Stretch, 1920, 1080, 1000, 1000);
        assert_eq!((s.view_w, s.view_h, s.w, s.h), (1000, 1000, 1000, 1000));
    }

    #[test]
    fn native_centres_or_crops() {
        let small = place(ScaleMode::Native, 800, 600, 1280, 720);
        assert_eq!(
            (small.w, small.h, small.dst_x, small.dst_y),
            (800, 600, 240, 60)
        );
        let big = place(ScaleMode::Native, 2560, 1440, 1280, 720);
        assert_eq!(
            (big.w, big.h, big.crop_x, big.crop_y),
            (1280, 720, 640, 360)
        );
        let none = place(ScaleMode::Native, 0, 0, 0, 0);
        assert_eq!((none.w, none.h), (1, 1));
    }

    #[test]
    fn pointer_maps_through_bars_and_crops() {
        let fit = place(ScaleMode::Fit, 1920, 1080, 1280, 1024);
        assert_eq!(to_remote(&fit, 0.0, 0.0), (0, 0));
        assert_eq!(to_remote(&fit, 1279.0, 1023.0).0, 65484);
        // Fill: the window centre is the picture centre.
        let fill = place(ScaleMode::Fill, 1920, 1080, 1000, 1000);
        let (x, y) = to_remote(&fill, 500.0, 500.0);
        assert!((32_700..32_840).contains(&x), "{x}");
        assert!((32_700..32_840).contains(&y), "{y}");
        // Left edge of a filled window is not the picture's left edge.
        assert!(to_remote(&fill, 0.0, 500.0).0 > 14_000);
    }

    #[test]
    fn pads_keep_pinned_slots() {
        let pins = vec![
            PadSlot {
                name: "Xbox".into(),
                slot: 1,
            },
            PadSlot {
                name: "DualSense".into(),
                slot: 0,
            },
        ];
        assert_eq!(slot_for(&pins, "Xbox", &[]), 1);
        assert_eq!(slot_for(&pins, "DualSense", &[1]), 0);
        // Unpinned pads skip pinned and taken slots.
        assert_eq!(slot_for(&pins, "8BitDo", &[]), 2);
        assert_eq!(slot_for(&pins, "8BitDo", &[2]), 3);
        assert_eq!(slot_for(&[], "Any", &[]), 0);
    }

    #[test]
    fn tokens_round_trip() {
        for mode in ScaleMode::ALL {
            assert_eq!(ScaleMode::from_token(mode.token()), Some(mode));
        }
        for size in SizeMatch::ALL {
            assert_eq!(SizeMatch::from_token(size.token()), Some(size));
        }
        for cursor in CursorMode::ALL {
            assert_eq!(CursorMode::from_token(cursor.token()), Some(cursor));
        }
        assert!(!SizeMatch::HostToWindow.available());
    }
}
