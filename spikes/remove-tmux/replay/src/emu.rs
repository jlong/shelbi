//! A thin wrapper over the vendored `alacritty_terminal`, plus a
//! toolkit-neutral snapshot used to compare two emulators cell-for-cell.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

/// Fixed emulator size for the spike. Replay happens at unchanged size (the
/// plan locks every client emulator to the session's size), so both emulators
/// use this.
#[derive(Clone, Copy)]
pub struct Size {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// An `alacritty_terminal` instance driven by a byte stream.
pub struct Emu {
    term: Term<VoidListener>,
    parser: Processor,
    size: Size,
}

impl Emu {
    pub fn new(cols: usize, lines: usize) -> Self {
        let size = Size { cols, lines };
        Emu {
            term: Term::new(Config::default(), &size, VoidListener),
            parser: Processor::new(),
            size,
        }
    }

    /// Feed a chunk of output bytes. The parser buffers partial sequences
    /// across calls, so any chunking is safe for a *single* emulator; replay
    /// framing (see [`crate::boundary`]) is a different concern.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn is_alt(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn show_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::SHOW_CURSOR)
    }

    /// The active grid (what a renderer currently shows).
    pub fn active_grid(&self) -> &Grid<Cell> {
        self.term.grid()
    }

    /// The inactive grid (the screen underneath, when the alternate screen is
    /// active). Reachable only through the vendored fork's accessor.
    pub fn inactive_grid(&self) -> &Grid<Cell> {
        self.term.inactive_grid()
    }

    /// Snapshot of the currently displayed screen and cursor.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::of(self.term.grid(), self.show_cursor())
    }
}

/// A cell reduced to the attributes a renderer and a diff care about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellSnap {
    pub c: char,
    pub fg: ColorSnap,
    pub bg: ColorSnap,
    /// Core SGR flags (bold/dim/italic/underline/inverse/hidden/strikeout).
    pub attrs: u16,
}

/// A color reduced to a comparable value that survives escape-sequence replay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSnap {
    Named(usize),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cursor {
    pub line: i32,
    pub col: usize,
    pub visible: bool,
}

/// The displayed screen (row-major cells) plus the cursor.
pub struct Snapshot {
    pub cols: usize,
    pub lines: usize,
    pub cells: Vec<CellSnap>,
    pub cursor: Cursor,
}

/// SGR-relevant subset of [`Flags`], so replay and diff agree on what matters.
pub const TRACKED_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

pub fn color_snap(c: alacritty_terminal::vte::ansi::Color) -> ColorSnap {
    use alacritty_terminal::vte::ansi::Color;
    match c {
        Color::Named(n) => ColorSnap::Named(n as usize),
        Color::Indexed(i) => ColorSnap::Indexed(i),
        Color::Spec(rgb) => ColorSnap::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

impl Snapshot {
    /// Snapshot the visible region (display offset 0) of a grid.
    pub fn of(grid: &Grid<Cell>, cursor_visible: bool) -> Snapshot {
        let cols = grid.columns();
        let lines = grid.screen_lines();
        let mut cells = Vec::with_capacity(cols * lines);
        for line in 0..lines as i32 {
            let row = &grid[Line(line)];
            for col in 0..cols {
                let cell = &row[Column(col)];
                cells.push(CellSnap {
                    c: if cell.c == '\0' { ' ' } else { cell.c },
                    fg: color_snap(cell.fg),
                    bg: color_snap(cell.bg),
                    attrs: (cell.flags & TRACKED_FLAGS).bits(),
                });
            }
        }
        let point = grid.cursor.point;
        Snapshot {
            cols,
            lines,
            cells,
            cursor: Cursor {
                line: point.line.0,
                col: point.column.0,
                visible: cursor_visible,
            },
        }
    }

    /// Render to text rows (whitespace-trimmed), for readable assertions and
    /// debugging. Not used for cell-exact equality.
    pub fn text_rows(&self) -> Vec<String> {
        (0..self.lines)
            .map(|r| {
                let start = r * self.cols;
                let line: String = self.cells[start..start + self.cols]
                    .iter()
                    .map(|c| c.c)
                    .collect();
                line.trim_end().to_string()
            })
            .collect()
    }

    pub fn contains_text(&self, needle: &str) -> bool {
        self.text_rows().iter().any(|r| r.contains(needle))
    }

    /// First divergence between two snapshots, as a human-readable string, or
    /// `None` if the visible cells and cursor match exactly.
    pub fn diff(&self, other: &Snapshot) -> Option<String> {
        if self.cols != other.cols || self.lines != other.lines {
            return Some(format!(
                "size {}x{} vs {}x{}",
                self.cols, self.lines, other.cols, other.lines
            ));
        }
        for i in 0..self.cells.len() {
            let (a, b) = (self.cells[i], other.cells[i]);
            if a != b {
                let (row, col) = (i / self.cols, i % self.cols);
                return Some(format!(
                    "cell ({row},{col}): {:?}/fg{:?}/bg{:?}/attr{:#06b} vs {:?}/fg{:?}/bg{:?}/attr{:#06b}",
                    a.c, a.fg, a.bg, a.attrs, b.c, b.fg, b.bg, b.attrs
                ));
            }
        }
        if self.cursor != other.cursor {
            return Some(format!("cursor {:?} vs {:?}", self.cursor, other.cursor));
        }
        None
    }
}
