//! Render-side emulator wrapper.
//!
//! A thin layer over `vt100` that (a) feeds child output into a grid, (b)
//! exposes the mode flags the input encoder needs (bracketed paste, mouse
//! protocol), (c) serves the cursor position to the responder, and (d) paints
//! the grid into a ratatui buffer.
//!
//! vt100 is a deliberate convenience here, NOT the emulator-crate decision:
//! that is `rt-spike-emulator-replay` (alacritty_terminal vs a vt100-family
//! crate). This spike only needs *a* grid to prove a ratatui widget can render
//! one and to count wide-character columns.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::responder::CursorSource;

pub struct Term {
    parser: vt100::Parser,
}

impl Term {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    pub fn mouse_enabled(&self) -> bool {
        !matches!(
            self.parser.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::None
        )
    }

    pub fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// Column span of the lead glyph at (row, col): 2 for a wide cell, 1
    /// otherwise. Used to prove wide-character / emoji accounting.
    pub fn glyph_width(&self, row: u16, col: u16) -> usize {
        match self.parser.screen().cell(row, col) {
            Some(cell) if cell.is_wide() => 2,
            _ => 1,
        }
    }

    pub fn cell_text(&self, row: u16, col: u16) -> String {
        self.parser
            .screen()
            .cell(row, col)
            .map(|c| c.contents().to_string())
            .unwrap_or_default()
    }

    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }
}

impl CursorSource for Term {
    fn cursor_1based(&self) -> (u16, u16) {
        let (r, c) = self.parser.screen().cursor_position();
        (r + 1, c + 1)
    }
}

/// Paint the current grid. Kept simple: fg/bg/bold/underline/reverse, enough to
/// show a full-screen agent renders faithfully through the widget.
pub struct TermWidget<'a>(pub &'a Term);

impl Widget for TermWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let screen = self.0.parser.screen();
        let (rows, cols) = screen.size();
        for row in 0..rows.min(area.height) {
            for col in 0..cols.min(area.width) {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let x = area.x + col;
                let y = area.y + row;
                let target = &mut buf[(x, y)];
                let contents = cell.contents();
                target.set_symbol(if contents.is_empty() { " " } else { contents });
                let mut style = Style::default()
                    .fg(conv(cell.fgcolor()))
                    .bg(conv(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                target.set_style(style);
            }
        }
    }
}

fn conv(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_single_width_and_advances_one() {
        let mut t = Term::new(4, 20);
        t.process(b"AB");
        assert_eq!(t.glyph_width(0, 0), 1);
        assert_eq!(t.cell_text(0, 0), "A");
        assert_eq!(t.cell_text(0, 1), "B");
        // cursor at col 3 (1-based) after two single-width glyphs.
        assert_eq!(t.cursor_1based(), (1, 3));
    }

    #[test]
    fn emoji_is_double_width() {
        let mut t = Term::new(4, 20);
        // U+1F600 grinning face, a wide emoji.
        t.process("😀X".as_bytes());
        assert_eq!(t.glyph_width(0, 0), 2, "emoji should occupy two columns");
        // Column 1 is the wide continuation; the next real glyph is at col 2.
        assert_eq!(t.cell_text(0, 2), "X");
        // cursor advanced by 2 (emoji) + 1 (X) => col 4 (1-based).
        assert_eq!(t.cursor_1based(), (1, 4));
    }

    #[test]
    fn cjk_is_double_width() {
        let mut t = Term::new(4, 20);
        t.process("中文".as_bytes());
        assert_eq!(t.glyph_width(0, 0), 2);
        assert_eq!(t.glyph_width(0, 2), 2);
        assert_eq!(t.cursor_1based(), (1, 5));
    }

    #[test]
    fn detects_bracketed_paste_and_mouse_modes() {
        let mut t = Term::new(4, 20);
        assert!(!t.bracketed_paste());
        assert!(!t.mouse_enabled());
        t.process(b"\x1b[?2004h"); // enable bracketed paste
        t.process(b"\x1b[?1000h"); // enable mouse click reporting
        assert!(t.bracketed_paste());
        assert!(t.mouse_enabled());
    }

    #[test]
    fn detects_alternate_screen() {
        let mut t = Term::new(4, 20);
        assert!(!t.alternate_screen());
        t.process(b"\x1b[?1049h");
        assert!(t.alternate_screen());
    }
}
