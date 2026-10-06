//! Attach-replay serializer: regenerate a self-contained escape-sequence byte
//! stream that reconstructs the session emulator's **full** state in a fresh
//! emulator.
//!
//! The session's emulator is the authority (plan, "Attach replay"). When a
//! client attaches — or is dropped to a fresh replay by backpressure — the
//! session serializes its emulator here and the client feeds the bytes into its
//! own emulator, ending up identical on **both** screen buffers. This is the
//! production form of the Phase 0 spike's stand-in
//! (`docs/removing-tmux/phase0/emulator-replay.md`): where the spike regenerated
//! only the visible cells, colors, and core SGR, this reconstructs everything
//! replay must carry per the plan — both buffers with scrollback, both saved
//! cursors, the scroll region, tab stops, charsets, every term mode, and both
//! kitty keyboard-protocol stacks — and carries the full cell [`Flags`] plus
//! [`CellExtra`] (underline styles/color, strikeout, zero-width combining marks).
//!
//! The approach is deliberately **write-side-free**: it emits ordinary escape
//! sequences a fresh emulator parses, rather than reaching into the vendored
//! crate to set private state. That is why the fork stays read-only (see
//! `vendor/alacritty_terminal/VENDORING.md`).
//!
//! ## Reconstruction order
//!
//! A fresh emulator starts on the normal screen with default modes, charsets,
//! tabs (every 8 columns), a full-height scroll region, and empty keyboard
//! stacks. The stream begins with RIS (`ESC c`) to land any reused emulator in
//! that same state, then:
//!
//! 1. the **normal** grid: scrollback + visible cells, its saved cursor, its
//!    charsets, and the normal-screen keyboard-mode stack;
//! 2. if the session is on the alternate screen, switch to it (`ESC[?1049h`) and
//!    reconstruct the **alt** grid the same way (no scrollback);
//! 3. term-global modes (cursor visibility, app-cursor/keypad, bracketed paste,
//!    autowrap, origin, insert, LNM, focus, mouse) that do not swap with 1049;
//! 4. the scroll region and tab stops of the active screen;
//! 5. the active cursor's pen (SGR) and its final position.
//!
//! What is intentionally *not* reproduced: the `WRAPLINE` logical-line-wrap flag
//! (rows are separated with CRLF, so the client's reflow on a later resize can
//! differ — replay happens at unchanged size and a resize nudge is a harmless
//! extra, never a correctness dependency); the `input_needs_wrap` pending-wrap
//! latch (not observable through a snapshot); and, when the session is on the
//! alternate screen, the *normal* grid's saved cursor — entering the alternate
//! screen (`ESC[?1049h`) overwrites the normal grid's saved cursor with its live
//! cursor, which is the same thing that happened in the session, so there is no
//! escape-only way to set it independently. The active grid's saved cursor is
//! always restored.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Cursor, Dimensions, Grid, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    CharsetIndex, Color, KeyboardModes, NamedColor, StandardCharset,
};
use std::fmt::Write as _;
use std::ops::Range;

/// Serialize `term`'s full state to a replay byte stream. Feeding the result
/// into a fresh emulator of the same size reproduces `term` cell-for-cell on
/// both buffers, with the same cursor, modes, and keyboard-protocol state.
pub fn serialize<L: EventListener>(term: &Term<L>) -> Vec<u8> {
    let mode = *term.mode();
    let on_alt = mode.contains(TermMode::ALT_SCREEN);

    // The active grid is `term.grid()`; the other screen is `inactive_grid()`.
    // When alt is active, the normal screen is the inactive grid (and its
    // keyboard stack is the inactive stack); otherwise the normal screen is the
    // active grid.
    let (normal, normal_kbd, alt) = if on_alt {
        (
            term.inactive_grid(),
            term.inactive_keyboard_mode_stack(),
            Some((term.grid(), term.keyboard_mode_stack())),
        )
    } else {
        (term.grid(), term.keyboard_mode_stack(), None)
    };

    let cols = term.columns();
    let mut out = Vec::new();

    // Land any reused emulator in the fresh-start state the stream assumes.
    out.extend_from_slice(b"\x1bc");

    // (1) Normal screen: scrollback + visible cells, then its per-grid state.
    paint_with_scrollback(&mut out, normal, cols);
    restore_charset_designations(&mut out, normal);
    push_keyboard_stack(&mut out, normal_kbd);
    restore_saved_cursor(&mut out, &normal.saved_cursor);

    // (2) Alternate screen, if the session is on it.
    if let Some((alt_grid, alt_kbd)) = alt {
        out.extend_from_slice(b"\x1b[?1049h");
        paint_screen_absolute(&mut out, alt_grid, cols);
        restore_charset_designations(&mut out, alt_grid);
        push_keyboard_stack(&mut out, alt_kbd);
        restore_saved_cursor(&mut out, &alt_grid.saved_cursor);
    }

    // The grid whose live cursor/pen/region/tabs the client ends on.
    let active = term.grid();

    // (3) Term-global modes (everything except ALT_SCREEN, already handled).
    restore_modes(&mut out, &mode);

    // (4) Scroll region and tab stops of the active screen.
    restore_scroll_region(&mut out, term.scroll_region(), term.screen_lines());
    restore_tabs(&mut out, term, cols);

    // (5) Active pen, active-charset invoke, and the final cursor position.
    //     The designations were restored per grid above; the invoke (SI/SO/LS2/
    //     LS3) is single term state that applies to the active screen.
    restore_pen(&mut out, &active.cursor.template);
    invoke_active_charset(&mut out, term.active_charset());
    position_cursor(&mut out, active.cursor.point, &mode, term.scroll_region());
    out.extend_from_slice(if mode.contains(TermMode::SHOW_CURSOR) {
        b"\x1b[?25h"
    } else {
        b"\x1b[?25l"
    });

    out
}

/// Paint a grid's scrollback and visible rows top to bottom, letting CRLF scroll
/// the history rows off into the client's scrollback.
fn paint_with_scrollback(out: &mut Vec<u8>, grid: &Grid<Cell>, cols: usize) {
    let screen = grid.screen_lines() as i32;
    let hist = grid.history_size() as i32;
    let last = screen - 1;
    for line in (-hist)..screen {
        emit_row(out, &grid[Line(line)], cols);
        if line != last {
            out.extend_from_slice(b"\r\n");
        }
    }
}

/// Paint a grid's visible rows with absolute cursor positioning. Used for the
/// alternate screen, which has no scrollback.
fn paint_screen_absolute(out: &mut Vec<u8>, grid: &Grid<Cell>, cols: usize) {
    for line in 0..grid.screen_lines() as i32 {
        let _ = write!(fmt_to(out), "\x1b[{};1H", line + 1);
        emit_row(out, &grid[Line(line)], cols);
    }
}

/// Emit one row's cells, from a reset pen, coalescing runs of like-styled cells
/// behind a single SGR. Trailing cells that are indistinguishable from a fresh
/// default cell are dropped: the client leaves those positions default, which is
/// exactly what they are, and a blank-padded screen stays small.
fn emit_row(out: &mut Vec<u8>, row: &Row<Cell>, cols: usize) {
    out.extend_from_slice(b"\x1b[0m");
    let mut pen = Pen::default();

    let end = row_end(row, cols);
    let mut col = 0usize;
    while col < end {
        let cell = &row[Column(col)];
        // A wide glyph owns two columns: emit it once, skip its spacer. The
        // leading spacer (padding before a wide glyph that would overflow the
        // line) carries no glyph either.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            col += 1;
            continue;
        }

        let want = Pen::of(cell);
        if want != pen {
            emit_sgr(out, &want);
            pen = want;
        }

        let c = if cell.c == '\0' { ' ' } else { cell.c };
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        // Zero-width combining marks live in the same cell, after the base char.
        if let Some(zw) = cell.zerowidth() {
            for &z in zw {
                out.extend_from_slice(z.encode_utf8(&mut buf).as_bytes());
            }
        }
        col += 1;
    }
}

/// The exclusive column past the last cell that differs from a fresh default
/// cell. Trailing default cells need not be painted.
fn row_end(row: &Row<Cell>, cols: usize) -> usize {
    let mut end = cols;
    while end > 0 && is_replay_default(&row[Column(end - 1)]) {
        end -= 1;
    }
    end
}

/// Whether a cell is indistinguishable from a freshly reset cell for replay
/// purposes (so the client reproduces it by simply not writing the position).
/// Stricter than the crate's `GridCell::is_empty`, which ignores bold/italic/dim
/// on a space — those are tracked by replay, so a styled space is not "default".
fn is_replay_default(cell: &Cell) -> bool {
    (cell.c == ' ' || cell.c == '\0')
        && matches!(cell.fg, Color::Named(NamedColor::Foreground))
        && matches!(cell.bg, Color::Named(NamedColor::Background))
        && (cell.flags & TRACKED_FLAGS).is_empty()
        && cell.underline_color().is_none()
        && cell.hyperlink().is_none()
        && cell.zerowidth().is_none()
}

/// SGR-bearing flags replay reproduces (everything that changes how a cell
/// renders; `WRAPLINE` and the wide-char spacers are structural, not visual).
const TRACKED_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::DOTTED_UNDERLINE)
    .union(Flags::DASHED_UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// A cell's visual pen: colors, tracked flags, and underline color.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
    underline_color: Option<Color>,
}

impl Default for Pen {
    fn default() -> Self {
        // Matches a cell straight after `ESC[0m`.
        Pen {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
            underline_color: None,
        }
    }
}

impl Pen {
    fn of(cell: &Cell) -> Pen {
        Pen {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & TRACKED_FLAGS,
            underline_color: cell.underline_color(),
        }
    }
}

/// Emit an SGR that resets, then sets exactly this pen.
fn emit_sgr(out: &mut Vec<u8>, pen: &Pen) {
    let mut s = String::from("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if pen.flags.contains(flag) {
            let _ = write!(s, ";{code}");
        }
    }
    // Underline style: the kitty/VTE extended underline SGR (4:n) carries the
    // non-straight variants; plain underline is 4.
    if pen.flags.contains(Flags::DOUBLE_UNDERLINE) {
        s.push_str(";4:2");
    } else if pen.flags.contains(Flags::UNDERCURL) {
        s.push_str(";4:3");
    } else if pen.flags.contains(Flags::DOTTED_UNDERLINE) {
        s.push_str(";4:4");
    } else if pen.flags.contains(Flags::DASHED_UNDERLINE) {
        s.push_str(";4:5");
    } else if pen.flags.contains(Flags::UNDERLINE) {
        s.push_str(";4");
    }
    push_sgr_color(&mut s, pen.fg, ColorRole::Fg);
    push_sgr_color(&mut s, pen.bg, ColorRole::Bg);
    if let Some(uc) = pen.underline_color {
        push_sgr_color(&mut s, uc, ColorRole::Underline);
    }
    s.push('m');
    out.extend_from_slice(s.as_bytes());
}

enum ColorRole {
    Fg,
    Bg,
    Underline,
}

fn push_sgr_color(s: &mut String, c: Color, role: ColorRole) {
    match c {
        Color::Named(n) => {
            let d = n as usize;
            match role {
                ColorRole::Fg => {
                    if d <= 7 {
                        let _ = write!(s, ";{}", 30 + d);
                    } else if (8..=15).contains(&d) {
                        let _ = write!(s, ";{}", 90 + (d - 8));
                    } else {
                        // Default or an exotic named slot: default foreground.
                        s.push_str(";39");
                    }
                }
                ColorRole::Bg => {
                    if d <= 7 {
                        let _ = write!(s, ";{}", 40 + d);
                    } else if (8..=15).contains(&d) {
                        let _ = write!(s, ";{}", 100 + (d - 8));
                    } else {
                        s.push_str(";49");
                    }
                }
                // No named form for the underline color; map to its indexed slot
                // when it is one of the 16, else drop (default underline color).
                ColorRole::Underline => {
                    if d <= 15 {
                        let _ = write!(s, ";58:5:{d}");
                    } else {
                        s.push_str(";59");
                    }
                }
            }
        }
        Color::Indexed(i) => {
            let tag = match role {
                ColorRole::Fg => "38:5:",
                ColorRole::Bg => "48:5:",
                ColorRole::Underline => "58:5:",
            };
            let _ = write!(s, ";{tag}{i}");
        }
        Color::Spec(rgb) => {
            let tag = match role {
                ColorRole::Fg => "38:2::",
                ColorRole::Bg => "48:2::",
                ColorRole::Underline => "58:2::",
            };
            let _ = write!(s, ";{tag}{}:{}:{}", rgb.r, rgb.g, rgb.b);
        }
    }
}

/// Restore a grid's G0–G3 charset designations (`ESC ( ) * +`). The cells
/// already hold pre-mapped glyphs (painting ran in ASCII), so this only sets the
/// state subsequent *live* output maps through.
fn restore_charset_designations(out: &mut Vec<u8>, grid: &Grid<Cell>) {
    for (index, intro) in [
        (CharsetIndex::G0, &b"\x1b("[..]),
        (CharsetIndex::G1, &b"\x1b)"[..]),
        (CharsetIndex::G2, &b"\x1b*"[..]),
        (CharsetIndex::G3, &b"\x1b+"[..]),
    ] {
        out.extend_from_slice(intro);
        out.push(match grid.cursor.charsets[index] {
            StandardCharset::Ascii => b'B',
            StandardCharset::SpecialCharacterAndLineDrawing => b'0',
        });
    }
}

/// Invoke a charset slot into GL (SI=G0, SO=G1, LS2/LS3 for G2/G3). Single term
/// state applying to the active screen; G0 is the default after RIS.
fn invoke_active_charset(out: &mut Vec<u8>, index: CharsetIndex) {
    match index {
        CharsetIndex::G0 => out.push(0x0f),
        CharsetIndex::G1 => out.push(0x0e),
        CharsetIndex::G2 => out.extend_from_slice(b"\x1bn"),
        CharsetIndex::G3 => out.extend_from_slice(b"\x1bo"),
    }
}

/// Push a keyboard-mode stack bottom to top so the client ends with the same
/// stack and the same active (top) mode. Empty stack = nothing to do (RIS left
/// it empty).
fn push_keyboard_stack(out: &mut Vec<u8>, stack: &[KeyboardModes]) {
    for mode in stack {
        let _ = write!(fmt_to(out), "\x1b[>{}u", mode.bits());
    }
}

/// Restore a grid's saved cursor (what DECRC, `ESC 8`, would restore). DECSC
/// snapshots the cursor position, pen, and charsets; we set those, save, then
/// the caller's later steps re-establish the live cursor.
fn restore_saved_cursor(out: &mut Vec<u8>, saved: &Cursor<Cell>) {
    // Nothing distinguishes a never-saved cursor from one saved at the home
    // position with a default pen; emitting DECSC for the default case is
    // harmless, so always reconstruct it for fidelity.
    restore_pen(out, &saved.template);
    let row = saved.point.line.0 + 1;
    let col = saved.point.column.0 + 1;
    let _ = write!(fmt_to(out), "\x1b[{};{}H", row.max(1), col.max(1));
    out.extend_from_slice(b"\x1b7");
}

/// Set the pen (SGR) to a template cell's colors and flags.
fn restore_pen(out: &mut Vec<u8>, template: &Cell) {
    emit_sgr(out, &Pen::of(template));
}

/// Restore term-global modes. ALT_SCREEN is handled by the caller; cursor
/// visibility is emitted last by the caller. Defaults (per the crate's
/// `TermMode::default`): SHOW_CURSOR, LINE_WRAP, ALTERNATE_SCROLL on.
fn restore_modes(out: &mut Vec<u8>, mode: &TermMode) {
    let set = |out: &mut Vec<u8>, on: bool, seq: &[u8]| {
        if on {
            out.extend_from_slice(seq);
        }
    };
    set(out, mode.contains(TermMode::APP_CURSOR), b"\x1b[?1h");
    set(out, mode.contains(TermMode::APP_KEYPAD), b"\x1b=");
    set(out, mode.contains(TermMode::BRACKETED_PASTE), b"\x1b[?2004h");
    // Autowrap is on by default; only an explicit disable needs replaying.
    if !mode.contains(TermMode::LINE_WRAP) {
        out.extend_from_slice(b"\x1b[?7l");
    }
    set(out, mode.contains(TermMode::INSERT), b"\x1b[4h");
    set(out, mode.contains(TermMode::LINE_FEED_NEW_LINE), b"\x1b[20h");
    set(out, mode.contains(TermMode::FOCUS_IN_OUT), b"\x1b[?1004h");
    // Mouse reporting.
    set(out, mode.contains(TermMode::MOUSE_REPORT_CLICK), b"\x1b[?1000h");
    set(out, mode.contains(TermMode::MOUSE_DRAG), b"\x1b[?1002h");
    set(out, mode.contains(TermMode::MOUSE_MOTION), b"\x1b[?1003h");
    set(out, mode.contains(TermMode::SGR_MOUSE), b"\x1b[?1006h");
    set(out, mode.contains(TermMode::UTF8_MOUSE), b"\x1b[?1005h");
    // Alternate scroll is on by default; only an explicit disable replays.
    if !mode.contains(TermMode::ALTERNATE_SCROLL) {
        out.extend_from_slice(b"\x1b[?1007l");
    }
    // Origin mode moves the cursor home, so set it before the final CUP.
    set(out, mode.contains(TermMode::ORIGIN), b"\x1b[?6h");
}

/// Restore the vertical scroll region (DECSTBM). A full-height region is the
/// default after RIS, so only a narrowed one is replayed. DECSTBM homes the
/// cursor, so this runs before the final CUP.
fn restore_scroll_region(out: &mut Vec<u8>, region: &Range<Line>, rows: usize) {
    let top = region.start.0;
    let bottom = region.end.0; // exclusive end == 1-based bottom row
    if top == 0 && bottom == rows as i32 {
        return;
    }
    let _ = write!(fmt_to(out), "\x1b[{};{}r", top + 1, bottom);
}

/// Restore tab stops. The default (a stop every 8 columns) is what RIS leaves,
/// so a stop set matching the default replays nothing; otherwise clear all
/// (`ESC[3g`) and re-set each stop via CHA + HTS.
fn restore_tabs<L: EventListener>(out: &mut Vec<u8>, term: &Term<L>, cols: usize) {
    let tabs = term.tabs();
    let is_default = (0..cols).all(|c| tabs[Column(c)] == (c % 8 == 0));
    if is_default {
        return;
    }
    out.extend_from_slice(b"\x1b[3g");
    for c in 0..cols {
        if tabs[Column(c)] {
            let _ = write!(fmt_to(out), "\x1b[{}G\x1bH", c + 1);
        }
    }
}

/// Position the active cursor, honoring origin mode (which makes CUP relative to
/// the scroll region's top).
fn position_cursor(out: &mut Vec<u8>, point: alacritty_terminal::index::Point, mode: &TermMode, region: &Range<Line>) {
    let col = point.column.0 as i32 + 1;
    let row = if mode.contains(TermMode::ORIGIN) {
        point.line.0 - region.start.0 + 1
    } else {
        point.line.0 + 1
    };
    let _ = write!(fmt_to(out), "\x1b[{};{}H", row.max(1), col.max(1));
}

/// A `fmt::Write` adapter over a byte vec, for the escape-sequence builders.
/// Writing is UTF-8, which escape sequences always are.
struct ByteWriter<'a>(&'a mut Vec<u8>);
impl std::fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}
fn fmt_to(out: &mut Vec<u8>) -> ByteWriter<'_> {
    ByteWriter(out)
}
