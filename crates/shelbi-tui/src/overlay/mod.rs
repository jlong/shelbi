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
// About Shelbi is the single-process TUI's own (the tmux popups never had it).
pub mod about;
mod about_icon;
pub mod add_project;
pub mod error_log;
pub mod feedback;
pub mod palette;
pub mod review_confirm;
pub mod review_reject;
pub mod zen_intro;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Widget},
};

use crate::theme::palette;

/// The columns between a dialog's border and its content, each side: the
/// command palette's.
pub const DIALOG_PAD_X: u16 = 1;

/// The frame every dialog over the app draws, the command palette's: a
/// muted single-line border around `rect`, the panel filled with `fill`
/// (`None` keeps the terminal's background), `title` (bold, the strongest
/// text) in the top border. Every modal goes through here, so their
/// borders can't drift apart. Returns [`dialog_content`]'s rect.
pub fn dialog_frame(buf: &mut Buffer, rect: Rect, fill: Option<Color>, title: Option<&str>) -> Rect {
    let mut style = Style::default();
    if let Some(fill) = fill {
        style = style.bg(fill);
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette().muted))
        .style(style);
    if let Some(title) = title {
        block = block.title(Span::styled(
            format!(" {} ", title.trim()),
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        ));
    }
    block.render(rect, buf);
    dialog_content(rect)
}

/// A dialog's content within its `rect`: inside the border, and
/// [`DIALOG_PAD_X`] columns in from it each side. Hit-tests use this, so
/// they agree with [`dialog_frame`].
pub fn dialog_content(rect: Rect) -> Rect {
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    let pad = DIALOG_PAD_X.min(inner.width / 2);
    Rect {
        x: inner.x + pad,
        width: inner.width - 2 * pad,
        ..inner
    }
}

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

/// Test support: whether `rect`'s edges in `buf` are [`dialog_frame`]'s
/// border (the single-line glyphs, muted). `Err` says the first cell that
/// isn't.
#[cfg(test)]
pub(crate) fn assert_dialog_border(buf: &Buffer, rect: Rect) -> Result<(), String> {
    let (l, r, t, b) = (rect.left(), rect.right() - 1, rect.top(), rect.bottom() - 1);
    let mut want = vec![((l, t), "┌"), ((r, t), "┐"), ((l, b), "└"), ((r, b), "┘")];
    // The top edge may carry a title, so only its corners are checked.
    want.extend((l + 1..r).map(|x| ((x, b), "─")));
    want.extend((t + 1..b).flat_map(|y| [((l, y), "│"), ((r, y), "│")]));
    for ((x, y), glyph) in want {
        let cell = &buf[(x, y)];
        if cell.symbol() != glyph || cell.fg != palette().muted {
            return Err(format!("({x},{y}) is {:?} in {:?}, not the muted {glyph:?}", cell.symbol(), cell.fg));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only [`dialog_frame`] draws a box border, so the dialogs can't drift
    /// apart again: any other `Borders::` in the crate is a dialog bypassing
    /// it. The reject prompt's reason field is a text box, not a dialog.
    #[test]
    fn only_the_shared_helper_draws_a_border() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        let allowed = [("overlay/mod.rs", usize::MAX), ("overlay/review_reject.rs", 1)];
        let needle = ["Borders", "::"].concat();
        for file in files {
            let rel = file.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            let n = std::fs::read_to_string(&file).unwrap().matches(&needle).count();
            let limit = allowed.iter().find(|(f, _)| *f == rel).map_or(0, |(_, n)| *n);
            assert!(n <= limit, "{rel} draws its own border ({n}x); use overlay::dialog_frame");
        }
    }

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
