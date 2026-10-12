//! Cell-exact attach-replay round trips.
//!
//! Two independent emulators: `A` (the "session"), fed a real output stream, and
//! `B` (the "client"), built only from the replay stream [`serialize`] regenerates
//! from `A`'s state. If `A` and `B` render identically — on both screen buffers,
//! with the same cursor, modes, and keyboard-protocol state — the state survived
//! the round trip. This is the production form of the Phase 0 spike
//! (`docs/removing-tmux/phase0/emulator-replay.md`); the fixtures are the spike's
//! real 80x24 PTY recordings (`nvim`, and a shell with `nvim` opened over it).

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, Processor};

use shelbi_session::replay::serialize;

const COLS: u16 = 80;
const ROWS: u16 = 24;

const NVIM: &[u8] = include_bytes!("fixtures/nvim-file.bin");
const SHELL_NVIM: &[u8] = include_bytes!("fixtures/shell-nvim.bin");

/// A terminal plus its single long-lived parser, matching the production
/// [`shelbi_session::emulator::Emulator`] config (kitty keyboard on, scrollback).
struct Emu {
    term: Term<VoidListener>,
    parser: Processor,
}

impl Emu {
    fn new(cols: u16, rows: u16) -> Self {
        let config = Config {
            scrolling_history: 10_000,
            kitty_keyboard: true,
            ..Config::default()
        };
        Emu {
            term: Term::new(config, &TermSize { cols, rows }, VoidListener),
            parser: Processor::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }
}

#[derive(Clone, Copy)]
struct TermSize {
    cols: u16,
    rows: u16,
}
impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// Feed `stream` into a fresh emulator and return it.
fn feed(stream: &[u8]) -> Emu {
    let mut e = Emu::new(COLS, ROWS);
    e.feed(stream);
    e
}

/// Build B from A's replay and return it. The client emulator is always the
/// session's size (the plan locks every client to the session's size), so B is
/// created at A's dimensions before the replay is fed.
fn replay_into_fresh(a: &Emu) -> Emu {
    let bytes = serialize(&a.term);
    let mut b = Emu::new(a.term.columns() as u16, a.term.screen_lines() as u16);
    b.feed(&bytes);
    b
}

/// A cell reduced to what a renderer (and this diff) care about.
#[derive(Clone, PartialEq, Debug)]
struct CellSnap {
    c: char,
    fg: Color,
    bg: Color,
    attrs: u16,
    underline_color: Option<Color>,
    zerowidth: Option<Vec<char>>,
}

const TRACKED: Flags = Flags::BOLD
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

fn snap_grid(grid: &Grid<Cell>) -> Vec<CellSnap> {
    let cols = grid.columns();
    let lines = grid.screen_lines() as i32;
    let mut cells = Vec::new();
    for line in 0..lines {
        let row = &grid[Line(line)];
        for col in 0..cols {
            let cell = &row[Column(col)];
            cells.push(CellSnap {
                c: if cell.c == '\0' { ' ' } else { cell.c },
                fg: cell.fg,
                bg: cell.bg,
                attrs: (cell.flags & TRACKED).bits(),
                underline_color: cell.underline_color(),
                zerowidth: cell.zerowidth().map(|z| z.to_vec()),
            });
        }
    }
    cells
}

/// First divergence between two grids, as a readable string, or `None`.
fn diff(a: &Grid<Cell>, b: &Grid<Cell>) -> Option<String> {
    let (sa, sb) = (snap_grid(a), snap_grid(b));
    if sa.len() != sb.len() {
        return Some(format!("cell count {} vs {}", sa.len(), sb.len()));
    }
    let cols = a.columns();
    for (i, (ca, cb)) in sa.iter().zip(&sb).enumerate() {
        if ca != cb {
            return Some(format!("cell ({},{}): {:?} vs {:?}", i / cols, i % cols, ca, cb));
        }
    }
    None
}

fn visible_text(grid: &Grid<Cell>) -> String {
    let cols = grid.columns();
    (0..grid.screen_lines() as i32)
        .map(|line| {
            let row = &grid[Line(line)];
            let s: String = (0..cols)
                .map(|c| {
                    let ch = row[Column(c)].c;
                    if ch == '\0' { ' ' } else { ch }
                })
                .collect();
            s.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_alt(e: &Emu) -> bool {
    e.term.mode().contains(TermMode::ALT_SCREEN)
}

fn cursor(e: &Emu) -> (i32, usize) {
    let p = e.term.grid().cursor.point;
    (p.line.0, p.column.0)
}

// ---------------------------------------------------------------------------
// (a) Reattach at unchanged size to a running full-screen program.
// ---------------------------------------------------------------------------
#[test]
fn case_a_fullscreen_roundtrip_is_cell_exact() {
    let a = feed(NVIM);
    assert!(is_alt(&a), "precondition: nvim is on the alternate screen");

    let b = replay_into_fresh(&a);
    assert!(is_alt(&b), "replay must put B on the alternate screen too");
    if let Some(d) = diff(a.term.grid(), b.term.grid()) {
        panic!("case (a): A and B diverged after replay: {d}");
    }
    assert_eq!(cursor(&a), cursor(&b), "cursor must match");
}

// ---------------------------------------------------------------------------
// (b) Attach while the full-screen program is open, quit it, and the shell
// screen underneath must be intact (needs the inactive grid).
// ---------------------------------------------------------------------------
#[test]
fn case_b_quit_reveals_shell_underneath() {
    let mut a = feed(SHELL_NVIM);
    assert!(is_alt(&a), "precondition: on the alternate screen (nvim open)");
    assert!(
        visible_text(a.term.inactive_grid()).contains("SHELL_UNDERNEATH_MARKER"),
        "precondition: A's inactive grid holds the shell markers"
    );

    let mut b = replay_into_fresh(&a);
    assert!(is_alt(&b));
    if let Some(d) = diff(a.term.grid(), b.term.grid()) {
        panic!("case (b): alternate screens diverge after replay: {d}");
    }

    // Quit the full-screen program on both (leave the alternate screen).
    a.feed(b"\x1b[?1049l");
    b.feed(b"\x1b[?1049l");
    assert!(!is_alt(&a) && !is_alt(&b), "both back on the normal screen");

    assert!(
        visible_text(b.term.grid()).contains("SHELL_UNDERNEATH_MARKER"),
        "replayed client's shell screen MUST be intact, not blank"
    );
    if let Some(d) = diff(a.term.grid(), b.term.grid()) {
        panic!("case (b): revealed shell screens differ: {d}");
    }
}

// ---------------------------------------------------------------------------
// (c) The keyboard-protocol stack survives replay (Shift+Enter still works).
// ---------------------------------------------------------------------------
#[test]
fn keyboard_protocol_mode_survives_replay() {
    let mut a = Emu::new(COLS, ROWS);
    // A program pushes the kitty "disambiguate escape codes" flag (what
    // Claude Code's Shift+Enter rides on), then draws.
    a.feed(b"\x1b[>1u");
    a.feed(b"hello");
    assert!(a.term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));

    let b = replay_into_fresh(&a);
    assert!(
        b.term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES),
        "the kitty keyboard mode must survive replay"
    );
    assert_eq!(
        a.term.keyboard_mode_stack(),
        b.term.keyboard_mode_stack(),
        "the whole keyboard-mode stack must match"
    );
    assert_eq!(visible_text(a.term.grid()), visible_text(b.term.grid()));
}

// ---------------------------------------------------------------------------
// SGR attributes, 256/truecolor, extended underlines, and underline color all
// round-trip cell-for-cell.
// ---------------------------------------------------------------------------
#[test]
fn full_sgr_attributes_and_colors_roundtrip() {
    let mut a = Emu::new(COLS, ROWS);
    a.feed(b"\x1b[1;3mbold-italic\x1b[0m\r\n");
    a.feed(b"\x1b[31;42mred-on-green\x1b[0m\r\n");
    a.feed(b"\x1b[38;5;208;48;5;21m256color\x1b[0m\r\n");
    a.feed(b"\x1b[38;2;10;20;30mtruecolor\x1b[0m\r\n");
    a.feed(b"\x1b[9mstrike\x1b[0m\r\n");
    a.feed(b"\x1b[4:3;58;5;9municurl-red\x1b[0m\r\n");
    a.feed(b"\x1b[7minverse\x1b[0m");

    let b = replay_into_fresh(&a);
    if let Some(d) = diff(a.term.grid(), b.term.grid()) {
        panic!("SGR/color round trip diverged: {d}");
    }
}

// ---------------------------------------------------------------------------
// Scrollback above the visible screen round-trips.
// ---------------------------------------------------------------------------
#[test]
fn scrollback_history_roundtrips() {
    let mut a = Emu::new(COLS, 3);
    for i in 0..50 {
        a.feed(format!("line{i}\r\n").as_bytes());
    }
    assert!(a.term.grid().history_size() > 0, "precondition: history exists");

    let b = replay_into_fresh(&a);
    assert_eq!(
        a.term.grid().history_size(),
        b.term.grid().history_size(),
        "history depth must match"
    );
    if let Some(d) = diff(a.term.grid(), b.term.grid()) {
        panic!("visible screen diverged with scrollback present: {d}");
    }
    // Spot-check a scrollback row matches too.
    let (ha, hb) = (a.term.grid(), b.term.grid());
    for line in -(ha.history_size() as i32)..0 {
        let ra: String = (0..ha.columns()).map(|c| ha[Line(line)][Column(c)].c).collect();
        let rb: String = (0..hb.columns()).map(|c| hb[Line(line)][Column(c)].c).collect();
        assert_eq!(ra.trim_end(), rb.trim_end(), "scrollback row {line} diverged");
    }
}

// ---------------------------------------------------------------------------
// Scroll region, tab stops, and charsets are carried.
// ---------------------------------------------------------------------------
#[test]
fn scroll_region_tabs_and_charsets_roundtrip() {
    let mut a = Emu::new(COLS, ROWS);
    // Narrow the scroll region, clear tabs and set custom ones, designate the
    // line-drawing charset into G0, and draw a box-drawing glyph.
    a.feed(b"\x1b[5;20r"); // DECSTBM rows 5..20
    a.feed(b"\x1b[3g"); // clear all tab stops
    a.feed(b"\x1b[10G\x1bH"); // a tab stop at column 10
    a.feed(b"\x1b[40G\x1bH"); // and column 40
    a.feed(b"\x1b(0lqk\x1b(B"); // line-drawing: draw corners, back to ASCII
    a.feed(b"\x1b[8;8H"); // park the cursor inside the region

    let b = replay_into_fresh(&a);
    assert_eq!(
        a.term.scroll_region(),
        b.term.scroll_region(),
        "scroll region must match"
    );
    for col in [0usize, 9, 10, 39, 40, 41] {
        assert_eq!(
            a.term.tabs()[Column(col)],
            b.term.tabs()[Column(col)],
            "tab stop at column {col} diverged"
        );
    }
    assert_eq!(a.term.active_charset(), b.term.active_charset());
    if let Some(d) = diff(a.term.grid(), b.term.grid()) {
        panic!("line-drawing glyphs diverged: {d}");
    }
    assert_eq!(cursor(&a), cursor(&b));
}

// ---------------------------------------------------------------------------
// Cursor visibility and bracketed-paste mode survive replay.
// ---------------------------------------------------------------------------
#[test]
fn modes_and_cursor_visibility_roundtrip() {
    let mut a = Emu::new(COLS, ROWS);
    a.feed(b"\x1b[?2004h"); // bracketed paste on
    a.feed(b"\x1b[?25l"); // cursor hidden
    a.feed(b"\x1b[?7l"); // autowrap off
    a.feed(b"content");

    let b = replay_into_fresh(&a);
    assert_eq!(
        a.term.mode().contains(TermMode::BRACKETED_PASTE),
        b.term.mode().contains(TermMode::BRACKETED_PASTE),
    );
    assert_eq!(
        a.term.mode().contains(TermMode::SHOW_CURSOR),
        b.term.mode().contains(TermMode::SHOW_CURSOR),
    );
    assert!(!b.term.mode().contains(TermMode::SHOW_CURSOR), "cursor stays hidden");
    assert_eq!(
        a.term.mode().contains(TermMode::LINE_WRAP),
        b.term.mode().contains(TermMode::LINE_WRAP),
    );
}

// ---------------------------------------------------------------------------
// The replay stream itself is parser-rest-terminated (ends at Ground), so the
// live output that follows it is never misparsed.
// ---------------------------------------------------------------------------
#[test]
fn replay_stream_ends_at_parser_rest() {
    let a = feed(NVIM);
    let bytes = serialize(&a.term);
    // Feeding the replay then an immediate printable must render that printable
    // as text, not swallow it into a dangling sequence.
    let mut b = feed(&bytes);
    let before = visible_text(b.term.grid());
    b.feed(b"Z");
    let after = visible_text(b.term.grid());
    assert_ne!(before, after, "a printable after replay must land on screen");
}
