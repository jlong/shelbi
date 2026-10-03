//! Regenerate a replay byte stream from an emulator's observable state.
//!
//! This is the spike's stand-in for the Phase 1 serializer. It reads the
//! emulator's grids (via the vendored fork's accessors) and emits escape
//! sequences that rebuild the same *visible* state in a fresh emulator: the
//! normal screen (with scrollback), the alternate screen if one is active, and
//! the cursor. It reconstructs the on-screen cells, colors, and core SGR
//! attributes; the write side for saved cursors, tab stops, charsets, and the
//! keyboard-mode stack is out of spike scope (that is the real Phase 1
//! serializer). The point proven here: every grid replay needs, including the
//! inactive one, is *reachable* and round-trips.

use crate::emu::{color_snap, ColorSnap, Emu};
use alacritty_terminal::grid::{Dimensions, Grid, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};

/// Build a replay stream for client emulator `B` from session emulator `A`.
pub fn replay_stream(a: &Emu) -> Vec<u8> {
    let mut out = Vec::new();

    // Full reset: B lands on the normal screen, parser at Ground, default pen.
    out.extend_from_slice(b"\x1bc");

    let (normal, alt) = if a.is_alt() {
        (a.inactive_grid(), Some(a.active_grid()))
    } else {
        (a.active_grid(), None)
    };

    // Normal screen, including scrollback.
    paint_with_scrollback(&mut out, normal);

    // Alternate screen, if the session is on it.
    if let Some(alt) = alt {
        out.extend_from_slice(b"\x1b[?1049h");
        paint_screen_absolute(&mut out, alt);
    }

    // Cursor of the active grid (display offset 0, so line is 0-based visible).
    let active = a.active_grid();
    let p = active.cursor.point;
    let row = (p.line.0 + 1).max(1);
    let col = (p.column.0 + 1).max(1);
    out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
    out.extend_from_slice(if a.show_cursor() {
        b"\x1b[?25h"
    } else {
        b"\x1b[?25l"
    });

    out
}

/// Paint a grid's scrollback and visible rows by emitting them top to bottom,
/// letting newlines scroll the history rows off into B's scrollback.
fn paint_with_scrollback(out: &mut Vec<u8>, grid: &Grid<Cell>) {
    let cols = grid.columns();
    let screen = grid.screen_lines() as i32;
    let hist = grid.history_size() as i32;
    let total = (hist + screen) as usize;

    let mut idx = 0usize;
    for line in (-hist)..screen {
        emit_row(out, &grid[Line(line)], cols);
        idx += 1;
        if idx < total {
            out.extend_from_slice(b"\r\n");
        }
    }
}

/// Paint a grid's visible rows with absolute cursor positioning (no scroll).
/// Used for the alternate screen, which has no scrollback.
fn paint_screen_absolute(out: &mut Vec<u8>, grid: &Grid<Cell>) {
    let cols = grid.columns();
    for line in 0..grid.screen_lines() as i32 {
        out.extend_from_slice(format!("\x1b[{};1H", line + 1).as_bytes());
        emit_row(out, &grid[Line(line)], cols);
    }
}

/// Emit one row's cells, starting from a reset pen and tracking attribute
/// changes so each run of like-styled cells carries one SGR.
fn emit_row(out: &mut Vec<u8>, row: &Row<Cell>, cols: usize) {
    // Reset the pen at the start of every row.
    out.extend_from_slice(b"\x1b[0m");
    let mut pen = Pen::default();

    let mut col = 0usize;
    while col < cols {
        let cell = &row[Column(col)];
        // A wide character occupies two columns: emit the char once and skip
        // its trailing spacer cell.
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
        {
            col += 1;
            continue;
        }

        let want = Pen {
            fg: color_snap(cell.fg),
            bg: color_snap(cell.bg),
            attrs: (cell.flags & crate::emu::TRACKED_FLAGS).bits(),
        };
        if want != pen {
            out.extend_from_slice(&sgr(&want));
            pen = want;
        }

        let c = if cell.c == '\0' { ' ' } else { cell.c };
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        col += 1;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: ColorSnap,
    bg: ColorSnap,
    attrs: u16,
}

impl Default for Pen {
    fn default() -> Self {
        // Matches a cell straight after `\x1b[0m`: default fg/bg, no attrs.
        Pen {
            fg: ColorSnap::Named(alacritty_terminal::vte::ansi::NamedColor::Foreground as usize),
            bg: ColorSnap::Named(alacritty_terminal::vte::ansi::NamedColor::Background as usize),
            attrs: 0,
        }
    }
}

/// Build an SGR that resets then sets exactly this pen.
fn sgr(pen: &Pen) -> Vec<u8> {
    let mut s = String::from("\x1b[0");

    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if pen.attrs & flag.bits() != 0 {
            s.push(';');
            s.push_str(code);
        }
    }

    push_color(&mut s, pen.fg, false);
    push_color(&mut s, pen.bg, true);

    s.push('m');
    s.into_bytes()
}

fn push_color(s: &mut String, c: ColorSnap, background: bool) {
    use std::fmt::Write;
    match c {
        ColorSnap::Named(d) => {
            // 0..=7 standard, 8..=15 bright, 256 = default fg, 257 = default bg.
            if d <= 7 {
                let base = if background { 40 } else { 30 };
                let _ = write!(s, ";{}", base + d);
            } else if (8..=15).contains(&d) {
                let base = if background { 100 } else { 90 };
                let _ = write!(s, ";{}", base + (d - 8));
            } else {
                // Default or an exotic named slot (Cursor, Dim*). Round-trips
                // the common default; exotic named slots fall back to default.
                s.push_str(if background { ";49" } else { ";39" });
            }
        }
        ColorSnap::Indexed(i) => {
            let base = if background { "48;5;" } else { "38;5;" };
            let _ = write!(s, ";{base}{i}");
        }
        ColorSnap::Rgb(r, g, b) => {
            let base = if background { "48;2;" } else { "38;2;" };
            let _ = write!(s, ";{base}{r};{g};{b}");
        }
    }
}
