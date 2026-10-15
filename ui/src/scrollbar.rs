//! Scrollbar geometry. The window draws the bar as an overlay in space the
//! content always leaves free, so showing or hiding it never moves anything.

/// Narrowest thumb, in pixels, so it stays easy to grab on a long page.
pub const MIN_THUMB: f32 = 28.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    /// From the top of the track.
    pub top: f32,
    pub height: f32,
    /// Track length the thumb can move through.
    pub travel: f32,
}

/// `viewport` is the visible height, `max_offset` how far the content can
/// scroll, and `scrolled` how far it has (both positive). `None` when it all fits.
// `!(x > 0.0)` is meant: a NaN size from layout counts as no room.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn thumb(viewport: f32, max_offset: f32, scrolled: f32) -> Option<Thumb> {
    if !(viewport > 0.0) || !(max_offset > 0.5) {
        return None;
    }
    let content = viewport + max_offset;
    let height = (viewport * viewport / content).max(MIN_THUMB).min(viewport);
    let travel = (viewport - height).max(0.0);
    let fraction = (scrolled / max_offset).clamp(0.0, 1.0);
    Some(Thumb {
        top: travel * fraction,
        height,
        travel,
    })
}

/// Where the content should be after the thumb is dragged `dy` pixels from
/// where the drag began (`start` scrolled).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn drag_to(start: f32, dy: f32, max_offset: f32, travel: f32) -> f32 {
    if !(travel > 0.0) {
        return start.clamp(0.0, max_offset.max(0.0));
    }
    (start + dy * max_offset / travel).clamp(0.0, max_offset.max(0.0))
}

/// A click on the track above or below the thumb moves one page that way.
pub fn page(scrolled: f32, viewport: f32, max_offset: f32, down: bool) -> f32 {
    let step = (viewport * 0.9).max(1.0);
    let next = if down { scrolled + step } else { scrolled - step };
    next.clamp(0.0, max_offset.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_bar_when_everything_fits() {
        assert_eq!(thumb(500.0, 0.0, 0.0), None);
        assert_eq!(thumb(0.0, 100.0, 0.0), None);
        assert_eq!(thumb(f32::NAN, 100.0, 0.0), None);
    }

    #[test]
    fn thumb_is_proportional_and_tracks_the_offset() {
        let top = thumb(400.0, 400.0, 0.0).unwrap();
        assert_eq!(top.height, 200.0);
        assert_eq!(top.top, 0.0);
        assert_eq!(top.travel, 200.0);
        let middle = thumb(400.0, 400.0, 200.0).unwrap();
        assert_eq!(middle.top, 100.0);
        let bottom = thumb(400.0, 400.0, 400.0).unwrap();
        assert_eq!(bottom.top + bottom.height, 400.0);
        let past = thumb(400.0, 400.0, 9999.0).unwrap();
        assert_eq!(past.top, bottom.top);
    }

    #[test]
    fn long_content_keeps_a_grabbable_thumb() {
        let t = thumb(300.0, 100_000.0, 0.0).unwrap();
        assert_eq!(t.height, MIN_THUMB);
        assert_eq!(t.travel, 300.0 - MIN_THUMB);
    }

    #[test]
    fn dragging_maps_back_to_content() {
        assert_eq!(drag_to(0.0, 100.0, 400.0, 200.0), 200.0);
        assert_eq!(drag_to(200.0, -1000.0, 400.0, 200.0), 0.0);
        assert_eq!(drag_to(200.0, 1000.0, 400.0, 200.0), 400.0);
        assert_eq!(drag_to(50.0, 10.0, 400.0, 0.0), 50.0);
    }

    #[test]
    fn paging_stays_in_range() {
        assert_eq!(page(0.0, 100.0, 1000.0, true), 90.0);
        assert_eq!(page(50.0, 100.0, 1000.0, false), 0.0);
        assert_eq!(page(990.0, 100.0, 1000.0, true), 1000.0);
    }
}
