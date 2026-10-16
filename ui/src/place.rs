//! Where the etherquic window opens.
//!
//! The title bar has to land on screen, and the bottom edge has to sit on or
//! just above the taskbar. On Windows, GPUI treats the bounds passed at open
//! as the client area and then expands them by the caption and borders
//! (`calculate_window_rect` splits that frame in half on the top and left).
//! The rect below is the inverse of that expansion, so the outer window is
//! the work area.
//!
//! When the work area cannot be read, both sides are at most 75% of the
//! display the window opens on. That box is top-aligned, so an unknown
//! taskbar still has the remaining quarter. Linux passes the display
//! itself as the work area: Wayland does not report a taskbar rectangle,
//! and a quarter of the screen left empty is room the window should use.
//! The compositor clamps a client that would cover a panel.

use gpui::Bounds;

/// Share of the display used when the work area is unknown.
const UNKNOWN_WORK: f32 = 0.75;
/// Preferred client minimum. Capped to the fitted client on a small display.
const PREFER_MIN_W: f32 = 760.;
const PREFER_MIN_H: f32 = 560.;
/// Logical pixels the outer window stays inside the target. One pixel keeps
/// a rounding error from pushing the caption off the top or the bottom edge
/// under the taskbar.
const EDGE: f32 = 1.;
/// Title bar that sits above the rect GPUI is given. Windows includes the
/// caption in the frame instead. On macOS the content rect is shorter than
/// the window frame by about this much. On Linux a server-side caption is
/// outside the client, so the client has to be shorter by about this much
/// or the frame runs off the bottom of the work area.
#[cfg(all(not(windows), target_os = "macos"))]
const TITLE_ABOVE: f32 = 28.;
#[cfg(all(not(windows), not(target_os = "macos")))]
const TITLE_ABOVE: f32 = 32.;

/// A rectangle in one pixel space. Origin is the top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Client rect GPUI should open, plus a minimum that cannot exceed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opening {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub min_w: f32,
    pub min_h: f32,
}

/// Outer window in physical pixels. A deferred `SetWindowPos` applies this
/// after GPUI has registered the window, which is the screen-coordinate
/// placement. `SetWindowPlacement` stores workspace coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Initial client bounds, and the physical outer rect when the work area is known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub opening: Opening,
    pub outer: Option<DeviceRect>,
}

/// `screen` is the display the window opens on, in GPUI logical pixels.
/// `work` is the usable area on that display (taskbar excluded), same space.
/// `frame` is the total non-client width and height GPUI adds around the client.
/// `title_above` is space above that rect which `frame` does not include.
pub fn opening(screen: RectF, work: Option<RectF>, frame: (f32, f32), title_above: f32) -> Opening {
    let target = match work {
        Some(area) if area.w > 1. && area.h > 1. => area,
        _ => {
            let w = (screen.w * UNKNOWN_WORK).max(1.);
            let h = (screen.h * UNKNOWN_WORK).max(1.);
            RectF {
                x: screen.x + (screen.w - w) * 0.5,
                y: screen.y,
                w,
                h,
            }
        }
    };
    let mut outer = inset(target, EDGE);
    let above = title_above.max(0.);
    if above > 0. && outer.h > above + 1. {
        outer.y += above;
        outer.h -= above;
    }
    let (fw, fh) = (frame.0.max(0.), frame.1.max(0.));
    let lead_x = (fw * 0.5).floor();
    let lead_y = (fh * 0.5).floor();
    let client = RectF {
        x: outer.x + lead_x,
        y: outer.y + lead_y,
        w: (outer.w - fw).max(1.),
        h: (outer.h - fh).max(1.),
    };
    Opening {
        x: client.x,
        y: client.y,
        w: client.w,
        h: client.h,
        min_w: capped_min(PREFER_MIN_W, client.w),
        min_h: capped_min(PREFER_MIN_H, client.h),
    }
}

/// Pull every edge in by `edge`. A box that is already tiny stays at least 1×1.
fn inset(area: RectF, edge: f32) -> RectF {
    if area.w <= edge * 2. + 1. || area.h <= edge * 2. + 1. {
        return RectF {
            x: area.x,
            y: area.y,
            w: area.w.max(1.),
            h: area.h.max(1.),
        };
    }
    RectF {
        x: area.x + edge,
        y: area.y + edge,
        w: area.w - edge * 2.,
        h: area.h - edge * 2.,
    }
}

/// `prefer` until the fitted client is smaller. One pixel under the client
/// leaves room for the real border in `WM_GETMINMAXINFO`, which adds the
/// frame on top of this minimum. Without that pixel a small display cannot
/// shrink the window into the work area.
fn capped_min(prefer: f32, client: f32) -> f32 {
    let cap = if client > 2. {
        client - 1.
    } else {
        client.max(1.)
    };
    prefer.min(cap).max(1.)
}

/// Logical rect from a physical one. `monitor` is the display's full rectangle
/// in physical pixels. `screen` is that same display in GPUI logical pixels.
/// Returns nothing when the scale is not a usable DPI ratio.
pub fn logical_area(screen: RectF, monitor: DeviceRect, area: DeviceRect) -> Option<RectF> {
    if screen.w < 1. || screen.h < 1. || monitor.w < 1 || monitor.h < 1 || area.w < 1 || area.h < 1
    {
        return None;
    }
    let scale_x = monitor.w as f32 / screen.w;
    let scale_y = monitor.h as f32 / screen.h;
    if !(0.5..8.0).contains(&scale_x) || !(0.5..8.0).contains(&scale_y) {
        return None;
    }
    Some(RectF {
        x: screen.x + (area.x - monitor.x) as f32 / scale_x,
        y: screen.y + (area.y - monitor.y) as f32 / scale_y,
        w: area.w as f32 / scale_x,
        h: area.h as f32 / scale_y,
    })
}

pub fn fit(screen: Option<RectF>) -> Fit {
    #[cfg(windows)]
    {
        fit_windows(screen)
    }
    #[cfg(all(not(windows), target_os = "macos"))]
    {
        let screen = screen.unwrap_or(RectF {
            x: 0.,
            y: 0.,
            w: 1280.,
            h: 720.,
        });
        Fit {
            opening: opening(screen, None, (0., 0.), TITLE_ABOVE),
            outer: None,
        }
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        fit_in(screen, linux_work_area())
    }
}

/// `work` is the usable rectangle (panels excluded), in the same space as
/// `screen`. Without one, the display is used and the caption is still
/// reserved so the frame does not hang off the bottom.
#[cfg(all(not(windows), not(target_os = "macos")))]
pub fn fit_in(screen: Option<RectF>, work: Option<RectF>) -> Fit {
    let screen = screen.unwrap_or(RectF {
        x: 0.,
        y: 0.,
        w: 1280.,
        h: 720.,
    });
    let work = work.filter(|area| area.w > 200. && area.h > 200.);
    Fit {
        opening: opening(screen, work.or(Some(screen)), (0., 0.), TITLE_ABOVE),
        outer: None,
    }
}

/// KDE's panel-excluded rectangle for the active output, when Plasma answers.
#[cfg(all(not(windows), not(target_os = "macos")))]
fn linux_work_area() -> Option<RectF> {
    let name = std::process::Command::new("qdbus")
        .args(["org.kde.KWin", "/KWin", "activeOutputName"])
        .output()
        .ok()?;
    if !name.status.success() {
        return None;
    }
    let name = String::from_utf8(name.stdout).ok()?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let rect = std::process::Command::new("qdbus")
        .args([
            "--literal",
            "org.kde.plasmashell",
            "/StrutManager",
            "availableScreenRect",
            name,
        ])
        .output()
        .ok()?;
    if !rect.status.success() {
        return None;
    }
    parse_kde_rect(&String::from_utf8_lossy(&rect.stdout))
}

/// `[Argument: (iiii) x, y, w, h]` from `qdbus --literal`.
#[cfg(all(not(windows), not(target_os = "macos")))]
pub fn parse_kde_rect(text: &str) -> Option<RectF> {
    let rest = text.split("(iiii)").nth(1)?;
    let nums: Vec<f32> = rest
        .split(|c: char| c == ',' || c.is_whitespace() || c == '[' || c == ']')
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let [x, y, w, h, ..] = nums[..] else {
        return None;
    };
    (w > 1. && h > 1.).then_some(RectF { x, y, w, h })
}

pub fn from_bounds(bounds: Bounds<gpui::Pixels>) -> RectF {
    RectF {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        w: f32::from(bounds.size.width),
        h: f32::from(bounds.size.height),
    }
}

#[cfg(windows)]
fn fit_windows(screen: Option<RectF>) -> Fit {
    let Some(live) = read_monitor() else {
        let screen = screen.unwrap_or(RectF {
            x: 0.,
            y: 0.,
            w: 1280.,
            h: 720.,
        });
        return Fit {
            opening: opening(screen, None, fallback_frame(), 0.),
            outer: None,
        };
    };
    let Some(screen) = screen.filter(|screen| screen.w >= 1. && screen.h >= 1.) else {
        // No logical display. The physical snap still fills the work area
        // once the window exists. The client guess only has to be on screen.
        return Fit {
            opening: opening(
                RectF {
                    x: 0.,
                    y: 0.,
                    w: live.monitor.w as f32,
                    h: live.monitor.h as f32,
                },
                None,
                fallback_frame(),
                0.,
            ),
            outer: Some(inset_device(live.work, 1)),
        };
    };
    let scale_x = live.monitor.w as f32 / screen.w;
    let scale_y = live.monitor.h as f32 / screen.h;
    let work = logical_area(screen, live.monitor, live.work);
    let frame = logical_frame(scale_x, scale_y);
    let opening = opening(screen, work, frame, 0.);
    let inset_x = scale_x.round().clamp(1., 8.) as i32;
    let inset_y = scale_y.round().clamp(1., 8.) as i32;
    Fit {
        opening,
        outer: work.map(|_| inset_device_xy(live.work, inset_x, inset_y)),
    }
}

#[cfg(windows)]
struct LiveMonitor {
    monitor: DeviceRect,
    work: DeviceRect,
}

#[cfg(windows)]
fn read_monitor() -> Option<LiveMonitor> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };

    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        if monitor.is_null() {
            return None;
        }
        let mut info = std::mem::zeroed::<MONITORINFO>();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let monitor = device_rect(info.rcMonitor);
        let work = device_rect(info.rcWork);
        if monitor.w < 1 || monitor.h < 1 || work.w < 1 || work.h < 1 {
            return None;
        }
        Some(LiveMonitor { monitor, work })
    }
}

#[cfg(windows)]
fn device_rect(rect: windows_sys::Win32::Foundation::RECT) -> DeviceRect {
    DeviceRect {
        x: rect.left,
        y: rect.top,
        w: rect.right.saturating_sub(rect.left),
        h: rect.bottom.saturating_sub(rect.top),
    }
}

/// Caption plus the resize border, in logical pixels.
///
/// Measured on a window with the styles GPUI creates
/// (`WS_SYSMENU | WS_THICKFRAME | WS_MAXIMIZEBOX | WS_MINIMIZEBOX`). Windows
/// adds `WS_CAPTION`. The gap between `GetWindowRect` and `GetClientRect` was
/// `(SM_CXFRAME + SM_CXPADDEDBORDER) * 2` by
/// `(SM_CYFRAME + SM_CXPADDEDBORDER) * 2 + SM_CYCAPTION`.
/// `GetSystemMetrics` in this per-monitor-v2 process is the primary monitor's
/// physical size, so dividing by the GPUI scale leaves logical pixels.
#[cfg(windows)]
fn logical_frame(scale_x: f32, scale_y: f32) -> (f32, f32) {
    if !(0.5..8.0).contains(&scale_x) || !(0.5..8.0).contains(&scale_y) {
        return fallback_frame();
    }
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXFRAME, SM_CXPADDEDBORDER, SM_CYCAPTION, SM_CYFRAME,
    };
    let side_x = unsafe { GetSystemMetrics(SM_CXFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER) };
    let side_y = unsafe { GetSystemMetrics(SM_CYFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER) };
    let caption = unsafe { GetSystemMetrics(SM_CYCAPTION) };
    if side_x <= 0 || side_y <= 0 || caption <= 0 {
        return fallback_frame();
    }
    (
        (side_x.saturating_mul(2)) as f32 / scale_x,
        (side_y.saturating_mul(2).saturating_add(caption)) as f32 / scale_y,
    )
}

/// Logical frame at 96 DPI. The caption and borders stay about this many
/// logical pixels at higher DPI, because the metrics scale with the monitor.
#[cfg(windows)]
fn fallback_frame() -> (f32, f32) {
    (16., 39.)
}

#[cfg(windows)]
fn inset_device(area: DeviceRect, edge: i32) -> DeviceRect {
    inset_device_xy(area, edge, edge)
}

#[cfg(windows)]
fn inset_device_xy(area: DeviceRect, edge_x: i32, edge_y: i32) -> DeviceRect {
    let edge_x = edge_x.max(0);
    let edge_y = edge_y.max(0);
    if area.w <= edge_x * 2 + 1 || area.h <= edge_y * 2 + 1 {
        return area;
    }
    DeviceRect {
        x: area.x + edge_x,
        y: area.y + edge_y,
        w: area.w - edge_x * 2,
        h: area.h - edge_y * 2,
    }
}

/// Move the outer window onto `outer`. Screen coordinates, so a taskbar on
/// any edge is already accounted for by the work-area rectangle.
///
/// Call this from `cx.defer`, after `open_window` has stored the window.
/// `SetWindowPos` sends `WM_SIZE` immediately. During the open builder the
/// window is not in GPUI's map yet, so that size change never reaches
/// `viewport_size`. A defer runs once the window is stored and not borrowed.
#[cfg(not(windows))]
pub fn snap_outer(_hwnd: isize, _outer: DeviceRect) {}

#[cfg(windows)]
pub fn snap_outer(hwnd: isize, outer: DeviceRect) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};

    if hwnd == 0 || outer.w < 1 || outer.h < 1 {
        return;
    }
    unsafe {
        SetWindowPos(
            hwnd as windows_sys::Win32::Foundation::HWND,
            std::ptr::null_mut(),
            outer.x,
            outer.y,
            outer.w,
            outer.h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(w: f32, h: f32) -> RectF {
        RectF { x: 0., y: 0., w, h }
    }

    /// Outer rect GPUI will build from this client and frame.
    fn outer_of(open: Opening, frame: (f32, f32)) -> RectF {
        let lead_x = (frame.0 * 0.5).floor();
        let lead_y = (frame.1 * 0.5).floor();
        let trail_x = frame.0 - lead_x;
        let trail_y = frame.1 - lead_y;
        RectF {
            x: open.x - lead_x,
            y: open.y - lead_y,
            w: open.w + lead_x + trail_x,
            h: open.h + lead_y + trail_y,
        }
    }

    #[test]
    fn work_area_puts_the_caption_at_the_top_and_the_bottom_on_the_taskbar() {
        // 1920×1080, 40px taskbar along the bottom. Frame is the 96 DPI measurement.
        let work = RectF {
            x: 0.,
            y: 0.,
            w: 1920.,
            h: 1040.,
        };
        let frame = (16., 39.);
        let open = opening(screen(1920., 1080.), Some(work), frame, 0.);
        let outer = outer_of(open, frame);
        assert!(outer.y >= work.y && outer.y <= work.y + 2.);
        assert!(outer.x >= work.x && outer.x <= work.x + 2.);
        let bottom = outer.y + outer.h;
        let right = outer.x + outer.w;
        assert!(bottom <= work.y + work.h);
        assert!(right <= work.x + work.w);
        assert!((work.y + work.h - bottom) <= 2., "bottom {bottom}");
        assert!(open.min_w <= open.w && open.min_h <= open.h);
        assert_eq!((open.min_w, open.min_h), (760., 560.));
    }

    #[test]
    fn high_dpi_logical_screen_stays_inside_the_work_area() {
        // 1920×1080 at 150% is 1280×720 logical. A fixed 880-tall window does not fit.
        let work = RectF {
            x: 0.,
            y: 0.,
            w: 1280.,
            h: 680.,
        };
        let frame = (16., 39.);
        let open = opening(screen(1280., 720.), Some(work), frame, 0.);
        let outer = outer_of(open, frame);
        assert!(open.y >= 0.);
        assert!(outer.y + outer.h <= work.h + 0.01);
        assert!(open.h < 720.);
        assert!(open.w < 1280.);
    }

    #[test]
    fn unknown_work_area_is_three_quarters_and_top_aligned() {
        let display = screen(1280., 720.);
        let frame = (16., 39.);
        let open = opening(display, None, frame, 0.);
        let outer = outer_of(open, frame);
        assert!(outer.w <= display.w * UNKNOWN_WORK + 0.01);
        assert!(outer.h <= display.h * UNKNOWN_WORK + 0.01);
        assert!(outer.y >= display.y && outer.y <= display.y + 2.);
        let centered = (display.w - outer.w) * 0.5;
        assert!((outer.x - centered).abs() < 2.);
        assert!(outer.y + outer.h < display.h);
    }

    #[test]
    fn a_small_display_may_open_a_tiny_window() {
        let work = RectF {
            x: 0.,
            y: 0.,
            w: 200.,
            h: 150.,
        };
        let open = opening(screen(200., 150.), Some(work), (16., 39.), 0.);
        assert!(open.w < 200.);
        assert!(open.h < 150.);
        assert!(open.min_w <= open.w && open.min_w < PREFER_MIN_W);
        assert!(open.min_h <= open.h && open.min_h < PREFER_MIN_H);
        assert!(open.min_w >= 1. && open.min_h >= 1.);
    }

    #[test]
    fn a_taskbar_on_the_left_shifts_the_window_right() {
        let work = RectF {
            x: 48.,
            y: 0.,
            w: 1872.,
            h: 1080.,
        };
        let frame = (16., 39.);
        let open = opening(screen(1920., 1080.), Some(work), frame, 0.);
        let outer = outer_of(open, frame);
        assert!(outer.x >= work.x);
        assert!(outer.x + outer.w <= work.x + work.w + 0.01);
    }

    #[test]
    fn title_above_the_content_stays_inside_the_guessed_box() {
        let display = screen(1440., 900.);
        let open = opening(display, None, (0., 0.), 28.);
        assert!(open.y >= 28.);
        assert!(open.w <= display.w * UNKNOWN_WORK + 0.01);
        assert!(open.y + open.h <= display.y + display.h * UNKNOWN_WORK + 0.01);
    }

    #[test]
    fn logical_work_area_follows_the_dpi_scale() {
        // 1920×1080 physical, 1280×720 logical: scale 1.5. Taskbar is 48 physical pixels.
        let screen = screen(1280., 720.);
        let monitor = DeviceRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let work = DeviceRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1032,
        };
        let logical = logical_area(screen, monitor, work).unwrap();
        assert_eq!(logical.x, 0.);
        assert_eq!(logical.y, 0.);
        assert_eq!(logical.w, 1280.);
        assert!((logical.h - 688.).abs() < 0.01, "{}", logical.h);
    }

    #[test]
    fn a_monitor_left_of_the_origin_keeps_its_offset() {
        let screen = RectF {
            x: -1280.,
            y: 0.,
            w: 1280.,
            h: 720.,
        };
        let monitor = DeviceRect {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let work = DeviceRect {
            x: -1920,
            y: 40,
            w: 1920,
            h: 1040,
        };
        let logical = logical_area(screen, monitor, work).unwrap();
        assert!((logical.x - screen.x).abs() < 0.01);
        assert!((logical.y - (40. / 1.5)).abs() < 0.01, "{}", logical.y);
    }

    #[cfg(windows)]
    #[test]
    fn the_live_work_area_is_inside_the_monitor() {
        let live = read_monitor().expect("primary monitor");
        assert!(live.work.x >= live.monitor.x);
        assert!(live.work.y >= live.monitor.y);
        assert!(live.work.x + live.work.w <= live.monitor.x + live.monitor.w);
        assert!(live.work.y + live.work.h <= live.monitor.y + live.monitor.h);
        let screen = RectF {
            x: live.monitor.x as f32,
            y: live.monitor.y as f32,
            w: live.monitor.w as f32,
            h: live.monitor.h as f32,
        };
        let fit = fit_windows(Some(screen));
        let frame = logical_frame(1., 1.);
        let outer = outer_of(fit.opening, frame);
        let work = RectF {
            x: live.work.x as f32,
            y: live.work.y as f32,
            w: live.work.w as f32,
            h: live.work.h as f32,
        };
        assert!(outer.x >= work.x - 1.);
        assert!(outer.y >= work.y - 1.);
        assert!(outer.x + outer.w <= work.x + work.w + 1.);
        assert!(outer.y + outer.h <= work.y + work.h + 1.);
        let snap = fit.outer.expect("snap");
        assert!(snap.w <= live.work.w && snap.h <= live.work.h);
        assert!(snap.x >= live.work.x && snap.y >= live.work.y);
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn linux_fills_the_display_instead_of_leaving_a_quarter() {
        let display = RectF {
            x: 0.,
            y: 0.,
            w: 1920.,
            h: 1080.,
        };
        let open = fit_in(Some(display), None).opening;
        assert!(open.h > display.h * 0.9, "height {}", open.h);
        assert!(open.h < display.h, "caption reserved, height {}", open.h);
        assert!(open.w > display.w * 0.95, "width {}", open.w);
        assert!(open.y + open.h <= display.h + 0.5);
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn a_panel_and_caption_stay_inside_the_work_area() {
        let screen = RectF {
            x: 0.,
            y: 0.,
            w: 1920.,
            h: 1080.,
        };
        let work = RectF {
            x: 0.,
            y: 0.,
            w: 1920.,
            h: 1018.,
        };
        let open = fit_in(Some(screen), Some(work)).opening;
        assert!(open.y + open.h <= work.y + work.h + 0.5);
        assert!(open.h > 900., "{}", open.h);
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn kde_rect_parses_the_literal_qdbus_line() {
        let area = parse_kde_rect("[Argument: (iiii) 0, 0, 1920, 1018]\n").unwrap();
        assert_eq!(
            area,
            RectF {
                x: 0.,
                y: 0.,
                w: 1920.,
                h: 1018.,
            }
        );
        assert!(parse_kde_rect("no rect").is_none());
    }
}
