//! # shelbi-term
//!
//! Everything about *showing* a session that is not tied to a UI toolkit: the
//! client-side terminal emulator, scrollback navigation, selection, search, and
//! input encoding. Both the ratatui TUI and the future gpui desktop app render
//! over this crate, so its public API holds **no** ratatui, crossterm, or gpui
//! types. See the "Removing tmux" plan, "Shared client crates".
//!
//! ## What lives here vs. the client
//!
//! [`shelbi_client`] moves frames; this crate interprets them for display. It
//! owns a *client-side* emulator fed by the output stream, and the input
//! encoding that turns key, mouse, paste, and focus events into the bytes an
//! [`Input`](shelbi_proto::Input) frame carries (or the text a
//! [`Paste`](shelbi_proto::Paste) frame carries).
//!
//! The emulator is a vendored `alacritty_terminal` fork driven headless (see
//! [`emulator`]). It is fed replay plus the sequenced output stream, and locked
//! to the session's size — a `resized` frame travels in that same stream so
//! every client reflows at the same byte offset. Until `rt-replay` lands, a
//! client develops against a snapshot plus live output instead of a full replay.
//!
//! **The client emulator never answers terminal queries.** Replies it would
//! generate (cursor-position reports, device attributes, color and text-area
//! queries, clipboard loads) are discarded: only the session process answers
//! queries, because it is the one sitting beside the real PTY. See
//! [`emulator::TermEmulator::discarded_query_replies`].
//!
//! ## Modules
//!
//! - [`emulator`]: the client-side emulator (fed output, queried for the grid
//!   and mode to render).
//! - [`scrollback`]: scrollback navigation for sessions on the normal screen.
//! - [`selection`]: text selection and the OSC 52 copy encoder.
//! - [`search`]: search within scrollback and the visible screen.
//! - [`links`]: URLs and file paths (with an optional `:line`) in the output,
//!   for Cmd+click.
//! - [`input`]: encode keys, mouse, paste, and focus into PTY bytes, with the
//!   mouse-ownership policy as a pure function.
//! - [`gesture`]: the mouse gesture state machine both clients share (a drag
//!   selects, a click reaches the program, Shift and Alt override, the wheel
//!   scrolls Shelbi's scrollback unless the program owns the mouse).
//! - [`render`]: render-facing plain data (cursor, damage, cell attributes,
//!   the xterm-256 palette).
//! - [`viewport`]: clipping and letterboxing for viewers whose size differs
//!   from the session's.
//! - [`view`]: [`view::TerminalView`], which ties the emulator to scrollback,
//!   selection, and search and enforces the normal-screen-only rule.
//!
//! [`shelbi_client`]: https://docs.rs/shelbi-client

pub mod emulator;
pub mod gesture;
pub mod input;
pub mod links;
pub mod render;
pub mod scrollback;
pub mod search;
pub mod selection;
pub mod view;
pub mod viewport;

/// A terminal size in character cells.
///
/// Toolkit-neutral; a viewer converts its own pixel/cell geometry into this
/// before handing it to the emulator or the viewport math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in columns.
    pub cols: u16,
    /// Height in rows.
    pub rows: u16,
}

impl Size {
    /// A size, clamped so neither dimension is zero (an emulator needs at
    /// least a 1x1 grid).
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// The grid that fits a `width` x `height` pixel area with cells of
    /// `cell_width` x `cell_height` pixels: whole cells only (a partial cell at
    /// the right or bottom edge is left blank), at least 1x1, and saturating at
    /// `u16::MAX`. Toolkit-neutral: a viewer passes its own pixel units (logical
    /// points in a window) as plain floats. A non-positive or non-finite cell
    /// size yields 1x1 rather than dividing by zero.
    pub fn from_pixels(width: f32, height: f32, cell_width: f32, cell_height: f32) -> Self {
        fn cells(extent: f32, cell: f32) -> u16 {
            if !cell.is_finite() || cell <= 0.0 || extent.is_nan() || extent <= 0.0 {
                return 1;
            }
            // A hair of tolerance so an area that is an exact multiple of the
            // cell size (but lands a rounding error short) keeps its last cell.
            let n = ((extent + 0.001) / cell).floor();
            if n >= u16::MAX as f32 {
                u16::MAX
            } else {
                (n as u16).max(1)
            }
        }
        Self { cols: cells(width, cell_width), rows: cells(height, cell_height) }
    }

    /// This size with each axis forced to at least 1.
    pub(crate) fn non_zero(self) -> Self {
        Self { cols: self.cols.max(1), rows: self.rows.max(1) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_pixels_counts_whole_cells() {
        // 800x600 with 8x16 cells: exactly 100x37 (37.5 rows -> 37).
        assert_eq!(Size::from_pixels(800.0, 600.0, 8.0, 16.0), Size::new(100, 37));
        // A partial cell at the edge is dropped, not rounded up.
        assert_eq!(Size::from_pixels(807.9, 31.9, 8.0, 16.0), Size::new(100, 1));
    }

    #[test]
    fn from_pixels_keeps_an_exact_multiple_despite_float_error() {
        // 7.2 * 125 = 900, but in f32 the division lands just under 125.
        let cell_w = 7.2_f32;
        let width = cell_w * 125.0;
        assert_eq!(Size::from_pixels(width, 100.0, cell_w, 20.0).cols, 125);
    }

    #[test]
    fn from_pixels_is_at_least_one_by_one() {
        assert_eq!(Size::from_pixels(0.0, 0.0, 8.0, 16.0), Size::new(1, 1));
        assert_eq!(Size::from_pixels(3.0, 3.0, 8.0, 16.0), Size::new(1, 1));
        assert_eq!(Size::from_pixels(-50.0, 100.0, 8.0, 16.0), Size::new(1, 6));
    }

    #[test]
    fn from_pixels_survives_degenerate_cells() {
        assert_eq!(Size::from_pixels(800.0, 600.0, 0.0, 16.0), Size::new(1, 37));
        assert_eq!(Size::from_pixels(800.0, 600.0, f32::NAN, f32::INFINITY), Size::new(1, 1));
    }

    #[test]
    fn from_pixels_saturates() {
        assert_eq!(Size::from_pixels(1.0e9, 1.0e9, 1.0, 1.0), Size::new(u16::MAX, u16::MAX));
    }
}
