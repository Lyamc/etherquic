//! Map an EQIN pointer onto a screen.
//!
//! EQIN positions run from `0` to `65535`. A virtual pointer reports `x / x_extent`,
//! so the extent has to be the screen size. `65535 / u32::MAX` is about zero, and
//! that is where a center point lands when the extent is `u32::MAX`.

/// Absolute pointer position in screen pixels, plus the extent a virtual pointer must report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerPlace {
    pub x: u32,
    pub y: u32,
    pub x_extent: u32,
    pub y_extent: u32,
}

/// Place `x` and `y` on a screen of `width` by `height`.
///
/// The extents are that screen. Both `32767` and `32768` land on the center pixel.
pub fn place_pointer(x: u16, y: u16, width: u32, height: u32) -> PointerPlace {
    let x_extent = width.max(1);
    let y_extent = height.max(1);
    PointerPlace {
        x: axis(x, x_extent),
        y: axis(y, y_extent),
        x_extent,
        y_extent,
    }
}

fn axis(pos: u16, extent: u32) -> u32 {
    let scaled = (u64::from(pos) * u64::from(extent) + 32_767) / 65_535;
    u32::try_from(scaled).unwrap_or(extent - 1).min(extent - 1)
}

/// Degrees in one wheel notch.
///
/// KWin's fake-input axis value is in degrees, and one click is 15 when the
/// high-resolution delta is zero. Wayland's vertical axis is positive down.
const NOTCH_DEGREES: f64 = 15.0;

/// Wayland axis values for an EQIN wheel report.
///
/// `wheel_y` is positive away from the user, so the vertical axis is negated.
/// libei absolute position inside one device region.
///
/// `place` is in screen pixels. The region is the compositor's logical pixel
/// rectangle. A center of 1920×1080 stays (960, 540) in a region of that size.
/// Coordinates are not divided by `u32::MAX`.
pub fn ei_absolute(
    place: PointerPlace,
    offset_x: u32,
    offset_y: u32,
    region_width: u32,
    region_height: u32,
) -> (f32, f32) {
    let region_width = region_width.max(1);
    let region_height = region_height.max(1);
    let x = u64::from(offset_x)
        + u64::from(place.x) * u64::from(region_width) / u64::from(place.x_extent.max(1));
    let y = u64::from(offset_y)
        + u64::from(place.y) * u64::from(region_height) / u64::from(place.y_extent.max(1));
    (x as f32, y as f32)
}

pub fn wayland_wheel(wheel_y: i16, wheel_x: i16) -> (f64, f64) {
    (
        -f64::from(wheel_y) * NOTCH_DEGREES,
        f64::from(wheel_x) * NOTCH_DEGREES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_of_1920x1080_is_mid_screen() {
        for pos in [32_767u16, 32_768] {
            let place = place_pointer(pos, pos, 1920, 1080);
            assert_eq!(place.x, 960, "x for {pos}");
            assert_eq!(place.y, 540, "y for {pos}");
            assert_eq!(place.x_extent, 1920);
            assert_eq!(place.y_extent, 1080);
            assert_ne!(place.x_extent, u32::MAX);
            assert_ne!(place.y_extent, u32::MAX);
            assert_eq!(place.x * 2, place.x_extent);
            assert_eq!(place.y * 2, place.y_extent);
        }
    }

    #[test]
    fn edges_stay_on_the_screen() {
        let origin = place_pointer(0, 0, 1920, 1080);
        assert_eq!((origin.x, origin.y), (0, 0));
        let far = place_pointer(u16::MAX, u16::MAX, 1920, 1080);
        assert_eq!((far.x, far.y), (1919, 1079));
        assert_eq!((far.x_extent, far.y_extent), (1920, 1080));
    }

    #[test]
    fn ei_absolute_center_uses_the_screen_region() {
        let place = place_pointer(32_768, 32_768, 1920, 1080);
        assert_eq!(ei_absolute(place, 0, 0, 1920, 1080), (960.0, 540.0));
        assert_eq!(ei_absolute(place, 1920, 0, 1920, 1080), (2880.0, 540.0));
    }

    #[test]
    fn wheel_away_from_the_user_is_up() {
        let (vertical, horizontal) = wayland_wheel(1, -2);
        assert_eq!(vertical, -15.0);
        assert_eq!(horizontal, -30.0);
    }
}
