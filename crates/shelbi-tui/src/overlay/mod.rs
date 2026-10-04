//! In-process overlays (removing-tmux Phase 4d).
//!
//! The five popups that were separate `shelbi-cli` processes launched in a tmux
//! `display-popup` — the command palette, the review-confirm dialog, the
//! reject-reason prompt, the error log, and the first-run Zen intro — are ported
//! here as one shared implementation. Each submodule holds the overlay's pure
//! state machine plus a `render` that paints into a caller-supplied [`Rect`],
//! with no terminal ownership, event loop, or result IO of its own.
//!
//! Two callers share this one implementation:
//!
//! - the **single-process TUI shell** (`crate::shell`), which holds an open
//!   overlay in its model, feeds it keys from the one event loop, draws it over
//!   the dimmed main area, and turns its outcome into a value / [`shelbi_app`]
//!   `Effect`; the shell owns the runtime holder that routes keys/mouse to the
//!   active overlay and renders it (see the shell's `overlays` submodule); and
//! - the legacy **tmux popups** in `shelbi-cli`, which keep their own terminal
//!   setup, event loop, and temp-file result IO but render and decide through
//!   these same types, so the tmux runtime is byte-identical until cutover.

// Submodules are added as each overlay is ported (removing-tmux Phase 4d).
pub mod error_log;
pub mod palette;
pub mod review_confirm;
pub mod review_reject;
pub mod zen_intro;

use ratatui::layout::Rect;

/// Center a `width`×`height` box inside `area`, clamped so it never exceeds the
/// available space (and never underflows on a tiny terminal). Every overlay is
/// centered over the main area the same way, so the geometry lives here.
pub fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Center a box sized as a percentage of `area`, with a minimum so the box stays
/// usable on small terminals. Used by the larger overlays (palette, error log)
/// that the tmux popups sized with `-w 70% -h 60%` and similar.
pub fn centered_pct(area: Rect, pct_w: u16, pct_h: u16, min_w: u16, min_h: u16) -> Rect {
    let w = (area.width * pct_w / 100).max(min_w);
    let h = (area.height * pct_h / 100).max(min_h);
    centered_rect(area, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rect_is_centered_and_clamped() {
        let area = Rect::new(0, 0, 100, 40);
        let r = centered_rect(area, 60, 20);
        assert_eq!(r, Rect::new(20, 10, 60, 20));
        // Clamps to the area on a box larger than the space.
        let r = centered_rect(area, 200, 200);
        assert_eq!(r, Rect::new(0, 0, 100, 40));
    }

    #[test]
    fn centered_rect_survives_a_tiny_area() {
        let area = Rect::new(0, 0, 3, 2);
        let r = centered_rect(area, 60, 20);
        // Never exceeds the area; no panic on the saturating math.
        assert!(r.width <= 3 && r.height <= 2);
    }

    #[test]
    fn centered_pct_honors_the_minimum() {
        let area = Rect::new(0, 0, 40, 10);
        // 70% of 40 = 28, above the min; 60% of 10 = 6, below min 9 -> 9.
        let r = centered_pct(area, 70, 60, 20, 9);
        assert_eq!(r.width, 28);
        assert_eq!(r.height, 9);
    }
}
