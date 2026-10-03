//! The vt100 comparison candidate.
//!
//! vt100 serializes its *visible* screen out of the box
//! (`Screen::contents_formatted`), which is convenient. But that is a
//! single-buffer snapshot: it cannot carry the inactive screen, so replaying a
//! session that is on the alternate screen loses the normal screen underneath.
//! vt100 also has no kitty keyboard protocol. This module demonstrates the
//! first limitation empirically; see `emulator-replay.md` for the rest.

/// Feed a stream into a fresh vt100 parser.
pub fn feed(cols: u16, rows: u16, scrollback: usize, stream: &[u8]) -> vt100::Parser {
    let mut p = vt100::Parser::new(rows, cols, scrollback);
    p.process(stream);
    p
}

/// vt100's own replay: the formatted contents of the *current* screen.
pub fn vt_replay(p: &vt100::Parser) -> Vec<u8> {
    p.screen().contents_formatted()
}

/// Plain text of the visible screen.
pub fn screen_text(p: &vt100::Parser) -> String {
    p.screen().contents()
}

pub fn contains(p: &vt100::Parser, needle: &str) -> bool {
    screen_text(p).contains(needle)
}

pub fn on_alt_screen(p: &vt100::Parser) -> bool {
    p.screen().alternate_screen()
}
