//! Rows of cells into paintable runs, cached per row and invalidated by
//! damage.
//!
//! [`layout_row`] turns one viewport row of the emulator's grid into
//! background runs (adjacent cells sharing a non-default background) and glyph
//! runs (adjacent single-width cells sharing a style, shaped as one line and
//! pinned to the cell grid; a double-width character is its own run).
//! Box-drawing characters leave the runs: they are drawn as lines
//! ([`super::boxdraw`]).
//! [`RowCache`] keeps the result per row and rebuilds only the rows the
//! emulator reports as damaged, so an idle or slowly changing screen costs
//! nothing to lay out.
//!
//! Colors resolve through a [`Palette`]: the theme's [`TerminalPalette`] for
//! the default colors and ANSI sixteen, the fixed xterm cube and ramp for
//! 16..=255, true color as is, and any OSC redefinition the program made on
//! top.

use std::sync::Arc;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color as VtColor, NamedColor};
use gpui::Hsla;
use shelbi_term::emulator::TermEmulator;
use shelbi_term::render::{xterm_256, CellAttrs, Damage, Underline};

use super::boxdraw::{self, BoxGlyph};
use crate::theme::{rgb_hsla, TerminalPalette};

/// Alpha applied to a dim (SGR 2) foreground, so it reads as faint against
/// whatever background it sits on.
const DIM_ALPHA: f32 = 0.6;

const FOREGROUND: usize = NamedColor::Foreground as usize;
const BACKGROUND: usize = NamedColor::Background as usize;
const CURSOR: usize = NamedColor::Cursor as usize;

/// Every color a cell can name, resolved for one frame: palette indices
/// 0..=255 then the default foreground, background, and cursor; and the
/// default foreground's bold and dim colors.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    colors: Vec<Hsla>,
    bold_foreground: Hsla,
    dim_foreground: Hsla,
}

impl Palette {
    /// Resolve the theme's palette, with the program's OSC overrides applied.
    pub fn resolve(theme: &TerminalPalette, emu: &TermEmulator) -> Self {
        let colors = (0..=CURSOR)
            .map(|i| {
                let base = match i {
                    0..=15 => theme.ansi[i],
                    16..=255 => xterm_256(i as u8).unwrap_or(theme.foreground),
                    FOREGROUND => theme.foreground,
                    BACKGROUND => theme.background,
                    _ => theme.cursor,
                };
                rgb_hsla(emu.color_override(i).unwrap_or(base))
            })
            .collect::<Vec<_>>();
        // A program that redefines the foreground (OSC 10) gets its own color
        // for bold and dim text too.
        let (bold_foreground, dim_foreground) = match emu.color_override(FOREGROUND) {
            Some(fg) => (rgb_hsla(fg), dim(rgb_hsla(fg))),
            None => (rgb_hsla(theme.bold_foreground), rgb_hsla(theme.dim_foreground)),
        };
        Self {
            colors,
            bold_foreground,
            dim_foreground,
        }
    }

    /// The default foreground.
    pub fn foreground(&self) -> Hsla {
        self.colors[FOREGROUND]
    }

    /// The default background.
    pub fn background(&self) -> Hsla {
        self.colors[BACKGROUND]
    }

    /// Bold text in the default foreground: the theme's loud text, as the
    /// Figma terminal frames draw their bold prompt lines.
    pub fn bold_foreground(&self) -> Hsla {
        self.bold_foreground
    }

    /// Dim (SGR 2) text in the default foreground: the theme's muted text.
    pub fn dim_foreground(&self) -> Hsla {
        self.dim_foreground
    }

    /// The cursor color.
    pub fn cursor(&self) -> Hsla {
        self.colors[CURSOR]
    }

    /// A cell color.
    pub fn color(&self, c: VtColor) -> Hsla {
        match c {
            VtColor::Spec(rgb) => rgb_hsla(rgb.into()),
            VtColor::Indexed(i) => self.colors[i as usize],
            VtColor::Named(n) => {
                use NamedColor::*;
                match n {
                    Foreground => self.foreground(),
                    BrightForeground => self.bold_foreground(),
                    DimForeground => self.dim_foreground(),
                    Background => self.background(),
                    Cursor => self.cursor(),
                    DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan
                    | DimWhite => dim(self.colors[n as usize - DimBlack as usize]),
                    _ => self.colors[n as usize],
                }
            }
        }
    }
}

fn dim(c: Hsla) -> Hsla {
    Hsla {
        a: c.a * DIM_ALPHA,
        ..c
    }
}

/// A run of adjacent cells sharing a non-default background.
#[derive(Debug, Clone, PartialEq)]
pub struct BgRun {
    /// First column.
    pub col: u16,
    /// Width in cells.
    pub len: u16,
    /// The fill.
    pub color: Hsla,
}

/// How a glyph run is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphStyle {
    /// Text color (already swapped for reverse video, alpha-reduced for dim).
    pub color: Hsla,
    /// Bold weight.
    pub bold: bool,
    /// Italic style.
    pub italic: bool,
    /// Underline style.
    pub underline: Underline,
    /// Strikethrough.
    pub strikethrough: bool,
}

/// A run of glyphs painted as one shaped line.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphRun {
    /// First column.
    pub col: u16,
    /// Width in cells. For a single-width run this is also its character
    /// count (each base glyph is pinned to its cell); a wide run is one
    /// character two cells wide.
    pub cells: u16,
    /// The text, combining marks included.
    pub text: String,
    /// Drawing style.
    pub style: GlyphStyle,
    /// A double-width character (CJK, most emoji): painted unpinned at its
    /// cell, since pinning assumes one cell per glyph.
    pub wide: bool,
}

/// A box-drawing character the pane draws as lines ([`boxdraw`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxCell {
    /// Its column.
    pub col: u16,
    /// What it draws.
    pub glyph: BoxGlyph,
    /// The line color (as a glyph's: reverse video and dim applied).
    pub color: Hsla,
}

/// One viewport row, ready to paint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowLayout {
    /// Non-default backgrounds, left to right.
    pub backgrounds: Vec<BgRun>,
    /// Glyph runs, left to right.
    pub glyphs: Vec<GlyphRun>,
    /// Box-drawing characters, left to right (not in any glyph run).
    pub boxes: Vec<BoxCell>,
}

/// Lay out viewport row `row` (0 = top of what is shown), its first `cols`
/// columns.
pub fn layout_row(emu: &TermEmulator, row: u16, cols: u16, palette: &Palette) -> RowLayout {
    let grid = emu.grid();
    let line = Line(row as i32 - grid.display_offset() as i32);
    let cols = cols.min(grid.columns() as u16);
    let cells = &grid[line];
    let mut out = RowLayout::default();
    let default_bg = palette.background();

    for col in 0..cols {
        let cell: &Cell = &cells[Column(col as usize)];
        // The trailing half of a wide character: its owner painted both cells.
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let a = CellAttrs::from_flags(cell.flags);
        // Bold and dim text in the default foreground take the theme's loud
        // and muted colors (the terminal frames' bold prompts and dim tool
        // output); colored text keeps its color, dimmed by alpha.
        let plain = !a.inverse && cell.fg == VtColor::Named(NamedColor::Foreground);
        let mut fg = match (plain, a.dim, a.bold) {
            (true, true, _) => palette.dim_foreground(),
            (true, false, true) => palette.bold_foreground(),
            _ => palette.color(cell.fg),
        };
        let mut bg = palette.color(cell.bg);
        if a.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if a.dim && !plain {
            fg = dim(fg);
        }
        let width = if a.wide { 2 } else { 1 };

        if bg != default_bg {
            match out.backgrounds.last_mut() {
                Some(run) if run.color == bg && run.col + run.len == col => run.len += width,
                _ => out.backgrounds.push(BgRun {
                    col,
                    len: width,
                    color: bg,
                }),
            }
        }

        let decorated = a.underline != Underline::None || a.strikethrough;
        let blank = matches!(cell.c, ' ' | '\0');
        if a.hidden || a.spacer || (blank && !decorated) {
            continue;
        }
        let style = GlyphStyle {
            color: fg,
            bold: a.bold,
            italic: a.italic,
            underline: a.underline,
            strikethrough: a.strikethrough,
        };
        let ch = if blank { ' ' } else { cell.c };
        let marks = cell.zerowidth();

        // Box drawing: lines, not glyphs (unless decorated or carrying marks).
        if let Some(glyph) = boxdraw::builtin(ch).filter(|_| !a.wide && !decorated && marks.is_none_or(|m| m.is_empty())) {
            out.boxes.push(BoxCell { col, glyph, color: fg });
            continue;
        }

        // Characters go straight into their run's text: no per-cell string.
        if !a.wide {
            if let Some(run) = out
                .glyphs
                .last_mut()
                .filter(|r| !r.wide && r.style == style && r.col + r.cells == col)
            {
                run.text.push(ch);
                run.text.extend(marks.into_iter().flatten());
                run.cells += 1;
                continue;
            }
        }
        let mut text = String::with_capacity(16);
        text.push(ch);
        text.extend(marks.into_iter().flatten());
        out.glyphs.push(GlyphRun {
            col,
            cells: width,
            text,
            style,
            wide: a.wide,
        });
    }
    out
}

/// Laid-out rows, kept between frames and rebuilt from damage.
#[derive(Default)]
pub struct RowCache {
    rows: Vec<Option<Arc<RowLayout>>>,
    /// The viewport row the window starts at (non-zero when a shorter viewer
    /// clips a taller session to its bottom rows).
    first: u16,
    cols: u16,
    palette: Option<Palette>,
    /// Rows laid out since the counter was last read (for the frame stats).
    rebuilt: usize,
}

impl RowCache {
    /// Bring the cache up to date for a `rows` x `cols` window of the session
    /// starting at viewport row `first`, and return the rows (index 0 is
    /// viewport row `first`). Rows outside `damage` keep their layout unless
    /// the window or the palette changed.
    pub fn update(
        &mut self,
        emu: &TermEmulator,
        first: u16,
        rows: u16,
        cols: u16,
        palette: &Palette,
        damage: &Damage,
    ) -> Vec<Arc<RowLayout>> {
        let reset = self.rows.len() != rows as usize
            || self.first != first
            || self.cols != cols
            || self.palette.as_ref() != Some(palette)
            || matches!(damage, Damage::Full);
        if reset {
            self.rows = vec![None; rows as usize];
            self.first = first;
            self.cols = cols;
            self.palette = Some(palette.clone());
        } else if let Damage::Rows(damaged) = damage {
            // Damage is in viewport rows; the cache holds the window's.
            for d in damaged {
                let Some(row) = d.row.checked_sub(first) else {
                    continue;
                };
                if let Some(slot) = self.rows.get_mut(row as usize) {
                    *slot = None;
                }
            }
        }
        (0..rows)
            .map(|row| {
                let slot = &mut self.rows[row as usize];
                if slot.is_none() {
                    self.rebuilt += 1;
                }
                slot.get_or_insert_with(|| Arc::new(layout_row(emu, first + row, cols, palette)))
                    .clone()
            })
            .collect()
    }

    /// Rows laid out since the last call.
    pub fn take_rebuilt(&mut self) -> usize {
        std::mem::take(&mut self.rebuilt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_term::render::Rgb;
    use shelbi_term::Size;

    fn emu(bytes: &[u8]) -> TermEmulator {
        let mut e = TermEmulator::new(Size::new(20, 4));
        e.feed(bytes);
        e
    }

    fn palette(e: &TermEmulator) -> Palette {
        Palette::resolve(
            &TerminalPalette::for_palette(&crate::theme::Palette::dark()),
            e,
        )
    }

    fn hsla(hex: u32) -> Hsla {
        rgb_hsla(Rgb::hex(hex))
    }

    #[test]
    fn plain_text_is_one_run_on_the_default_background() {
        let e = emu(b"hello world");
        let p = palette(&e);
        let row = layout_row(&e, 0, 20, &p);
        assert!(row.backgrounds.is_empty());
        assert_eq!(row.glyphs.len(), 2, "split at the blank: {:?}", row.glyphs);
        assert_eq!(row.glyphs[0].text, "hello");
        assert_eq!((row.glyphs[0].col, row.glyphs[0].cells), (0, 5));
        assert_eq!(row.glyphs[1].text, "world");
        assert_eq!(row.glyphs[1].col, 6);
        assert_eq!(row.glyphs[0].style.color, p.foreground());
    }

    #[test]
    fn sgr_attributes_split_runs() {
        // bold "ab", italic "cd", underline "ef", strike "gh"
        let e = emu(b"\x1b[1mab\x1b[0;3mcd\x1b[0;4mef\x1b[0;9mgh");
        let row = layout_row(&e, 0, 20, &palette(&e));
        let styles: Vec<(String, bool, bool, Underline, bool)> = row
            .glyphs
            .iter()
            .map(|g| {
                (
                    g.text.clone(),
                    g.style.bold,
                    g.style.italic,
                    g.style.underline,
                    g.style.strikethrough,
                )
            })
            .collect();
        assert_eq!(
            styles,
            vec![
                ("ab".into(), true, false, Underline::None, false),
                ("cd".into(), false, true, Underline::None, false),
                ("ef".into(), false, false, Underline::Single, false),
                ("gh".into(), false, false, Underline::None, true),
            ]
        );
    }

    #[test]
    fn colors_resolve_through_ansi_256_and_true_color() {
        // ANSI red fg, 256-color 196 fg, true-color fg, ANSI blue bg.
        let e = emu(b"\x1b[31mA\x1b[38;5;196mB\x1b[38;2;1;2;3mC\x1b[0;44mD");
        let p = palette(&e);
        let row = layout_row(&e, 0, 20, &p);
        let colors: Vec<Hsla> = row.glyphs.iter().map(|g| g.style.color).collect();
        assert_eq!(colors[0], hsla(0xe04f52), "ANSI red from the theme (theme.red)");
        assert_eq!(colors[1], hsla(0xff0000), "xterm 196");
        assert_eq!(colors[2], hsla(0x010203), "true color");
        assert_eq!(
            row.backgrounds,
            vec![BgRun {
                col: 3,
                len: 1,
                color: hsla(0x5893d6)
            }],
            "ANSI blue from the theme (ansi/regular/blue)"
        );
    }

    #[test]
    fn reverse_video_swaps_and_paints_the_background() {
        let e = emu(b"\x1b[7mX");
        let p = palette(&e);
        let row = layout_row(&e, 0, 20, &p);
        assert_eq!(
            row.backgrounds,
            vec![BgRun {
                col: 0,
                len: 1,
                color: p.foreground()
            }]
        );
        assert_eq!(row.glyphs[0].style.color, p.background());
    }

    #[test]
    fn adjacent_backgrounds_merge() {
        let e = emu(b"\x1b[41mab cd\x1b[0m e");
        let row = layout_row(&e, 0, 20, &palette(&e));
        assert_eq!(row.backgrounds.len(), 1);
        assert_eq!((row.backgrounds[0].col, row.backgrounds[0].len), (0, 5));
    }

    #[test]
    fn wide_characters_and_emoji_take_two_cells() {
        let e = emu("a漢b👍c".as_bytes());
        let row = layout_row(&e, 0, 20, &palette(&e));
        let runs: Vec<(String, u16, u16, bool)> = row
            .glyphs
            .iter()
            .map(|g| (g.text.clone(), g.col, g.cells, g.wide))
            .collect();
        assert_eq!(
            runs,
            vec![
                ("a".into(), 0, 1, false),
                ("漢".into(), 1, 2, true),
                ("b".into(), 3, 1, false),
                ("👍".into(), 4, 2, true),
                ("c".into(), 6, 1, false),
            ]
        );
    }

    #[test]
    fn combining_marks_stay_with_their_base() {
        // "e" + COMBINING ACUTE ACCENT, then "x".
        let e = emu("e\u{301}x".as_bytes());
        let row = layout_row(&e, 0, 20, &palette(&e));
        assert_eq!(row.glyphs.len(), 1);
        assert_eq!(row.glyphs[0].text, "e\u{301}x");
        assert_eq!(row.glyphs[0].cells, 2);
    }

    #[test]
    fn box_drawing_leaves_the_runs_as_lines() {
        // A dim "│ a ─╮", then an underlined "─" (decorated: the font's).
        let e = emu("\x1b[2m│ a ─╮\x1b[0;4m─".as_bytes());
        let p = palette(&e);
        let row = layout_row(&e, 0, 20, &p);
        let boxes: Vec<_> = row.boxes.iter().map(|b| (b.col, b.glyph)).collect();
        assert_eq!(
            boxes,
            [
                (0, boxdraw::builtin('│').unwrap()),
                (4, boxdraw::builtin('─').unwrap()),
                (5, boxdraw::builtin('╮').unwrap()),
            ]
        );
        assert!(row.boxes.iter().all(|b| b.color == p.dim_foreground()), "in the cell's color");
        let glyphs: Vec<_> = row.glyphs.iter().map(|g| (g.col, g.text.as_str())).collect();
        assert_eq!(glyphs, [(2, "a"), (6, "─")]);
    }

    #[test]
    fn dim_and_hidden() {
        let e = emu(b"\x1b[2mD\x1b[0;8mH\x1b[0;2;31mR");
        let p = palette(&e);
        let row = layout_row(&e, 0, 20, &p);
        assert_eq!(row.glyphs.len(), 2, "hidden text paints nothing");
        // Dim default text is the theme's muted (the frames' tool output).
        assert_eq!(row.glyphs[0].style.color, hsla(0x777a86), "theme.muted");
        // Dim colored text keeps its hue, faded.
        let red = p.color(VtColor::Named(NamedColor::Red));
        assert_eq!(row.glyphs[1].style.color.h, red.h);
        assert!(row.glyphs[1].style.color.a < red.a);
    }

    #[test]
    fn bold_default_text_is_the_loud_color() {
        // The frames' bold prompt lines are theme.white; bold colored text
        // keeps its color.
        let e = emu(b"\x1b[1mB\x1b[0;1;32mG\x1b[0mP");
        let p = palette(&e);
        let row = layout_row(&e, 0, 20, &p);
        assert_eq!(row.glyphs[0].style.color, hsla(0xf8f8fa), "theme.white");
        assert_eq!(row.glyphs[1].style.color, hsla(0x5acd25), "theme.green");
        assert_eq!(row.glyphs[2].style.color, p.foreground());
        // A program's own foreground (OSC 10) wins for bold too.
        let e = emu(b"\x1b]10;rgb:12/34/56\x07\x1b[1mB");
        let row = layout_row(&e, 0, 20, &palette(&e));
        assert_eq!(row.glyphs[0].style.color, hsla(0x123456));
    }

    #[test]
    fn osc_overrides_win_over_the_theme() {
        let e = emu(b"\x1b]4;1;rgb:12/34/56\x07\x1b[31mR");
        let row = layout_row(&e, 0, 20, &palette(&e));
        assert_eq!(row.glyphs[0].style.color, hsla(0x123456));
    }

    #[test]
    fn layout_is_clipped_to_the_shown_columns() {
        let e = emu(b"0123456789abcdef");
        let row = layout_row(&e, 0, 4, &palette(&e));
        assert_eq!(row.glyphs[0].text, "0123");
    }

    #[test]
    fn cache_rebuilds_only_damaged_rows() {
        let mut e = TermEmulator::new(Size::new(20, 4));
        let p = palette(&e);
        let mut cache = RowCache::default();
        let d = e.take_damage();
        let first = cache.update(&e, 0, 4, 20, &p, &d);
        assert_eq!(
            cache.take_rebuilt(),
            4,
            "the first frame lays out every row"
        );

        // Jump straight to row 2 (CUP), so row 1 is never touched.
        e.feed(b"\x1b[3;1Hchanged");
        let d = e.take_damage();
        let second = cache.update(&e, 0, 4, 20, &p, &d);
        assert!(
            cache.take_rebuilt() <= 2,
            "only the cursor's old and new rows"
        );
        assert!(
            Arc::ptr_eq(&first[1], &second[1]),
            "an untouched row is reused"
        );
        assert_eq!(second[2].glyphs[0].text, "changed");

        // Nothing changed: at most the cursor's row is rebuilt.
        let d = e.take_damage();
        cache.update(&e, 0, 4, 20, &p, &d);
        assert!(cache.take_rebuilt() <= 1);

        // A different window size rebuilds everything.
        cache.update(&e, 0, 3, 20, &p, &Damage::Rows(vec![]));
        assert_eq!(cache.take_rebuilt(), 3);
    }

    /// A window clipped to a taller session's bottom rows lays out those
    /// rows, maps damage into the window, and starts over when the window
    /// moves (`dt-terminal-bottom-clipped`).
    #[test]
    fn a_window_starting_below_the_top_lays_out_its_own_rows() {
        let mut e = TermEmulator::new(Size::new(20, 6));
        e.feed(b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");
        let p = palette(&e);
        let mut cache = RowCache::default();
        let d = e.take_damage();
        let rows = cache.update(&e, 3, 3, 20, &p, &d);
        let text: Vec<&str> = rows.iter().map(|r| r.glyphs[0].text.as_str()).collect();
        assert_eq!(text, ["r3", "r4", "r5"]);
        cache.take_rebuilt();

        // Damage on viewport row 5 is window row 2; row 1 (viewport 4) is
        // reused, and damage above the window is ignored.
        e.feed(b"\x1b[6;1Hzz\x1b[1;1Hqq");
        let d = e.take_damage();
        let again = cache.update(&e, 3, 3, 20, &p, &d);
        assert!(Arc::ptr_eq(&rows[1], &again[1]), "an untouched row is reused");
        assert_eq!(again[2].glyphs[0].text, "zz");

        // The window moving rebuilds it.
        cache.take_rebuilt();
        let moved = cache.update(&e, 2, 3, 20, &p, &Damage::Rows(vec![]));
        assert_eq!(cache.take_rebuilt(), 3);
        assert_eq!(moved[0].glyphs[0].text, "r2");
    }
}
