//! The scrollbar's geometry: where the thumb sits for a scrollback position,
//! and which position a dragged thumb or a track click means.
//!
//! Plain arithmetic over logical pixels (`f32`), so it is unit-tested
//! without a window. The thumb's height is the visible share of the buffer
//! (rows over history plus rows), at least [`MIN_THUMB`]; at the live bottom
//! (offset 0) it sits at the bottom of the track, at the oldest line at the
//! top.

/// The scrollbar's width, painted over the grid's right edge.
pub const WIDTH: f32 = 6.0;

/// Gap between the thumb and the element's right edge.
pub const INSET: f32 = 2.0;

/// The width from the right edge where a press grabs the scrollbar instead
/// of reaching the terminal (wider than the thumb, so it's easy to hit).
pub const HIT_WIDTH: f32 = 12.0;

/// The shortest thumb, so a long history still leaves something to grab.
pub const MIN_THUMB: f32 = 24.0;

/// Where the thumb is, in pixels from the top of the track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    /// The thumb's top edge.
    pub top: f32,
    /// The thumb's height.
    pub height: f32,
}

/// The thumb for a buffer with `history` lines above `rows` on-screen rows,
/// scrolled `offset` lines up from the live bottom, on a track `track`
/// pixels tall. `None` when there is no history (nothing to scroll).
pub fn thumb(history: usize, offset: usize, rows: usize, track: f32) -> Option<Thumb> {
    if history == 0 || track <= 0.0 {
        return None;
    }
    let total = (history + rows.max(1)) as f32;
    let height = (track * rows.max(1) as f32 / total).clamp(MIN_THUMB.min(track), track);
    let travel = track - height;
    let from_top = (history - offset.min(history)) as f32 / history as f32;
    Some(Thumb {
        top: travel * from_top,
        height,
    })
}

/// The scrollback offset that puts the thumb's top at `top` (a drag). Clamped
/// to the track.
pub fn offset_for_thumb_top(top: f32, history: usize, rows: usize, track: f32) -> usize {
    let Some(t) = thumb(history, 0, rows, track) else {
        return 0;
    };
    let travel = track - t.height;
    if travel <= 0.0 {
        return 0;
    }
    let from_top = (top / travel).clamp(0.0, 1.0);
    (history as f32 * (1.0 - from_top)).round() as usize
}

/// The offset a press at `y` on the track (outside the thumb) jumps to: the
/// thumb centered on the press, as macOS does.
pub fn offset_for_track_click(y: f32, history: usize, rows: usize, track: f32) -> usize {
    let Some(t) = thumb(history, 0, rows, track) else {
        return 0;
    };
    offset_for_thumb_top(y - t.height / 2.0, history, rows, track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_history_no_scrollbar() {
        assert_eq!(thumb(0, 0, 24, 400.0), None);
    }

    #[test]
    fn the_thumb_is_the_visible_share_and_moves_with_the_offset() {
        // 100 history + 100 rows on a 400px track: a 200px thumb.
        let bottom = thumb(100, 0, 100, 400.0).unwrap();
        assert_eq!(bottom, Thumb { top: 200.0, height: 200.0 });
        let top = thumb(100, 100, 100, 400.0).unwrap();
        assert_eq!(top, Thumb { top: 0.0, height: 200.0 });
        let middle = thumb(100, 50, 100, 400.0).unwrap();
        assert_eq!(middle.top, 100.0);
    }

    #[test]
    fn a_long_history_keeps_a_grabbable_thumb() {
        let t = thumb(10_000, 0, 24, 400.0).unwrap();
        assert_eq!(t.height, MIN_THUMB);
        assert_eq!(t.top, 400.0 - MIN_THUMB);
        // A tiny track: the thumb is the track.
        assert_eq!(thumb(10_000, 0, 24, 10.0).unwrap().height, 10.0);
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_an_offset() {
        assert_eq!(offset_for_thumb_top(0.0, 100, 100, 400.0), 100);
        assert_eq!(offset_for_thumb_top(200.0, 100, 100, 400.0), 0);
        assert_eq!(offset_for_thumb_top(100.0, 100, 100, 400.0), 50);
        // Past either end clamps.
        assert_eq!(offset_for_thumb_top(-50.0, 100, 100, 400.0), 100);
        assert_eq!(offset_for_thumb_top(900.0, 100, 100, 400.0), 0);
        // Round trip.
        for offset in [0, 1, 37, 99, 100] {
            let t = thumb(100, offset, 100, 400.0).unwrap();
            assert_eq!(offset_for_thumb_top(t.top, 100, 100, 400.0), offset);
        }
    }

    #[test]
    fn a_track_click_centers_the_thumb_on_the_press() {
        // Thumb 200px: a press at y=100 puts its top at 0, the oldest line.
        assert_eq!(offset_for_track_click(100.0, 100, 100, 400.0), 100);
        assert_eq!(offset_for_track_click(300.0, 100, 100, 400.0), 0);
    }
}
