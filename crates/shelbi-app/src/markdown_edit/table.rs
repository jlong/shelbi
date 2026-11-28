//! GFM tables read as tables: a header row, a delimiter row
//! (`| --- | :---: | ---: |`) and body rows, each line a
//! [`RowKind::Table`] row holding its cells and the table's alignments.
//!
//! As with a fenced code block's fences ([`super::code`]), the table's own
//! syntax comes back to be edited: [`Doc::grid_rows`] says a view draws a
//! table's lines as a grid unless the selection touches the table (from its
//! header's first byte to its last line's end), and a view without the keys
//! draws the grid always. The grid leaves the delimiter line out
//! ([`Doc::hidden_rows`]); a move into the table (a click on a cell, an
//! arrow) puts the selection in it, and its lines show as the source they
//! are. Drawing either way never touches the source.

use std::ops::{Range, RangeInclusive};

use super::blocks::{Doc, RowKind, TablePart};

impl Doc {
    /// Each table's rows: the first and last row index (inclusive).
    pub fn tables(&self) -> Vec<RangeInclusive<usize>> {
        let mut out: Vec<(usize, RangeInclusive<usize>)> = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            let Some(block) = row.block.filter(|_| row.table().is_some()) else {
                continue;
            };
            match out.last_mut() {
                Some((b, rows)) if *b == block => *rows = *rows.start()..=i,
                _ => out.push((block, i..=i)),
            }
        }
        out.into_iter().map(|(_, rows)| rows).collect()
    }

    /// Which rows a view draws as a table's grid: every table's lines,
    /// unless `selection` (`None` for a view without the keys) touches the
    /// table. One entry per row.
    pub fn grid_rows(&self, selection: Option<Range<usize>>) -> Vec<bool> {
        let mut grid = vec![false; self.rows.len()];
        for rows in self.tables() {
            let start = self.rows[*rows.start()].lines.start;
            let end = self.rows[*rows.end()].lines.end;
            let touched = selection
                .as_ref()
                .is_some_and(|s| s.start <= end && start <= s.end);
            if !touched {
                for g in &mut grid[rows] {
                    *g = true;
                }
            }
        }
        grid
    }

    /// Hide the delimiter line of each table drawn as a grid (`grid`, from
    /// [`Self::grid_rows`]).
    pub(super) fn hide_table_delimiters(&self, grid: &[bool], hidden: &mut [bool]) {
        for (i, row) in self.rows.iter().enumerate() {
            let delimiter = matches!(&row.kind, RowKind::Table(t) if t.part == TablePart::Delimiter);
            if delimiter && grid.get(i).copied().unwrap_or(false) {
                hidden[i] = true;
            }
        }
    }
}

/// The widths of a table's columns, its cells' padding included, in a room
/// `room` wide (points in the desktop, cells in the TUI): `want` each
/// column's unwrapped width, `least` the widest word each holds. Every
/// column gets what it wants when that fits. Else a column wanting no more
/// than an even share of the room left gets it (a short column next to a
/// long one stays on one line), and the rest each get their least and share
/// what's left by how much more each wants. When even their least doesn't
/// fit, they get it, and the grid scrolls (or, in the TUI, wraps hard).
pub fn column_widths(want: &[f32], least: &[f32], room: f32) -> Vec<f32> {
    let least: Vec<f32> = least.iter().zip(want).map(|(l, w)| l.min(*w)).collect();
    if want.iter().sum::<f32>() <= room {
        return want.to_vec();
    }
    let mut out: Vec<Option<f32>> = vec![None; want.len()];
    loop {
        let open: Vec<usize> = (0..want.len()).filter(|&j| out[j].is_none()).collect();
        let left = room - out.iter().flatten().sum::<f32>();
        let share = left / open.len().max(1) as f32;
        let fits: Vec<usize> = open.iter().copied().filter(|&j| want[j] <= share).collect();
        if fits.is_empty() || fits.len() == open.len() {
            break;
        }
        for j in fits {
            out[j] = Some(want[j]);
        }
    }
    let open: Vec<usize> = (0..want.len()).filter(|&j| out[j].is_none()).collect();
    let left = room - out.iter().flatten().sum::<f32>();
    let floor: f32 = open.iter().map(|&j| least[j]).sum();
    let slack: f32 = open.iter().map(|&j| want[j] - least[j]).sum();
    for j in open {
        out[j] = Some(if floor >= left || slack <= 0.0 {
            least[j]
        } else {
            least[j] + (left - floor) * (want[j] - least[j]) / slack
        });
    }
    out.into_iter().map(|w| w.unwrap_or(0.0)).collect()
}

#[cfg(test)]
mod tests {
    use super::super::blocks::{Align, Row, TableRow};
    use super::super::inline::MarkKind;
    use super::super::Editor;
    use super::*;

    const TABLE: &str = "Intro.\n\n| Element | Shelbi |\n| :--- | ---: |\n| **Use case** | Shipping `code` |\n| Tool | Tabs<br>one each |\n\nAfter.";

    fn table(row: &Row) -> &TableRow {
        row.table().expect("a table row")
    }

    fn cells(row: &Row) -> Vec<&str> {
        table(row).cells.iter().map(|c| c.display.as_str()).collect()
    }

    /// Columns get what they want while the table fits; past that a short
    /// column keeps its width, the long ones share the rest down to their
    /// widest words, and below that the grid is its words' width (it
    /// scrolls).
    #[test]
    fn columns_size_to_content_then_share_the_room() {
        let sum = |v: &[f32]| v.iter().sum::<f32>();
        // Fits: each its own width, the table narrower than the room.
        assert_eq!(column_widths(&[80.0, 120.0], &[40.0, 60.0], 600.0), [80.0, 120.0]);
        // A short column beside a long one keeps its width; the long one
        // takes the rest and wraps.
        let w = column_widths(&[90.0, 1400.0], &[50.0, 120.0], 600.0);
        assert_eq!(w[0], 90.0);
        assert!((sum(&w) - 600.0).abs() < 0.01, "{w:?}");
        // Two long ones share by how much more each wants.
        let w = column_widths(&[90.0, 1000.0, 500.0], &[50.0, 100.0, 100.0], 600.0);
        assert_eq!(w[0], 90.0);
        assert!((sum(&w) - 600.0).abs() < 0.01, "{w:?}");
        assert!(w[1] > w[2] && w[2] >= 100.0, "{w:?}");
        // Too many columns: each its widest word, wider than the room.
        let w = column_widths(&[300.0; 10], &[150.0; 10], 600.0);
        assert_eq!(w, [150.0; 10]);
        assert!(sum(&w) > 600.0);
        // A word never holds a column wider than its content.
        assert_eq!(column_widths(&[60.0, 2000.0], &[90.0, 100.0], 300.0)[0], 60.0);
    }

    #[test]
    fn a_table_reads_as_header_delimiter_and_body_rows() {
        let doc = Doc::parse(TABLE);
        let parts: Vec<Option<TablePart>> = doc.rows.iter().map(|r| r.table().map(|t| t.part)).collect();
        assert_eq!(
            parts,
            [
                None,
                Some(TablePart::Header),
                Some(TablePart::Delimiter),
                Some(TablePart::Body),
                Some(TablePart::Body),
                None
            ]
        );
        assert_eq!(cells(&doc.rows[1]), ["Element", "Shelbi"]);
        assert!(table(&doc.rows[2]).cells.is_empty());
        // Inline Markdown in cells: bold, code, a `<br>` as a line break.
        assert_eq!(cells(&doc.rows[3]), ["Use case", "Shipping code"]);
        assert!(table(&doc.rows[3]).cells[0].style_at(0).bold);
        assert!(table(&doc.rows[3]).cells[1].style_at("Shipping ".len()).code);
        assert_eq!(cells(&doc.rows[4]), ["Tool", "Tabs\none each"]);
        // One block, one set of alignments on every line.
        let block = doc.rows[1].block;
        assert!(block.is_some());
        for r in &doc.rows[1..5] {
            assert_eq!(r.block, block);
            assert_eq!(table(r).aligns, [Align::Left, Align::Right]);
        }
        // A row's own display is its source line, for editing it as text.
        assert_eq!(doc.rows[2].display, "| :--- | ---: |");
    }

    #[test]
    fn alignments_come_from_the_delimiter_line() {
        let doc = Doc::parse("a | b | c | d\n--- | :-- | :-: | --:");
        assert_eq!(
            table(&doc.rows[0]).aligns,
            [Align::None, Align::Left, Align::Center, Align::Right]
        );
    }

    #[test]
    fn outer_pipes_are_optional() {
        for src in [
            "| a | b |\n| - | - |\n| 1 | 2 |",
            "a | b\n- | -\n1 | 2",
            "| a | b\n|---|---\n| 1 | 2",
            "a | b |\n---|---|\n1 | 2 |",
        ] {
            let doc = Doc::parse(src);
            assert_eq!(doc.rows.len(), 3, "{src:?}");
            assert_eq!(cells(&doc.rows[0]), ["a", "b"], "{src:?}");
            assert_eq!(cells(&doc.rows[2]), ["1", "2"], "{src:?}");
        }
        // A one-column table needs its pipes.
        let doc = Doc::parse("| a |\n| --- |\n| 1 |");
        assert_eq!(cells(&doc.rows[2]), ["1"]);
    }

    #[test]
    fn escaped_pipes_stay_in_the_cell() {
        let src = "| a | b |\n|---|---|\n| x \\| y | z |";
        let doc = Doc::parse(src);
        assert_eq!(cells(&doc.rows[2]), ["x | y", "z"]);
        let cell = &table(&doc.rows[2]).cells[0];
        assert!(cell.marks.iter().any(|m| m.kind == MarkKind::Escape));
        // The cell maps back to its source bytes.
        assert_eq!(&src[cell.lines.clone()], "x \\| y");
        assert_eq!(&src[cell.source_of(2)..cell.source_of(3)], "\\|");
    }

    #[test]
    fn short_rows_get_empty_cells_and_long_rows_lose_the_rest() {
        let src = "| a | b | c |\n|---|---|---|\n| 1 |\n| 1 | 2 | 3 | 4 |\nloose";
        let doc = Doc::parse(src);
        assert_eq!(cells(&doc.rows[2]), ["1", "", ""]);
        // An empty cell sits at the line's end, after the last one written.
        let empty = &table(&doc.rows[2]).cells[1];
        assert_eq!(empty.source_of(0), src.find("| 1 |").unwrap() + "| 1".len());
        assert_eq!(cells(&doc.rows[3]), ["1", "2", "3"]);
        // A line with no pipes under a table is a one-cell body row.
        assert_eq!(cells(&doc.rows[4]), ["loose", "", ""]);
    }

    #[test]
    fn a_table_ends_at_a_blank_line_or_another_block() {
        let kinds = |src: &str| -> Vec<bool> { Doc::parse(src).rows.iter().map(|r| r.table().is_some()).collect() };
        assert_eq!(kinds("a | b\n--|--\n1 | 2\n\nafter"), [true, true, true, false]);
        assert_eq!(kinds("a | b\n--|--\n# Heading"), [true, true, false]);
        assert_eq!(kinds("a | b\n--|--\n> quote"), [true, true, false]);
        assert_eq!(kinds("a | b\n--|--\n- item"), [true, true, false]);
        assert_eq!(kinds("a | b\n--|--\n```\ncode\n```"), [true, true, false, false, false]);
        assert_eq!(kinds("a | b\n--|--\n***"), [true, true, false]);
        // It can interrupt a paragraph: the line above stays its own.
        let doc = Doc::parse("text\na | b\n--|--");
        assert_eq!(doc.rows[0].kind, RowKind::Paragraph);
        assert_eq!(doc.rows[0].display, "text");
        assert!(doc.rows[1].table().is_some());
    }

    #[test]
    fn pipe_text_that_isnt_a_table_stays_what_it_was() {
        // No delimiter line: prose, or the source of a table being written.
        let doc = Doc::parse("use a | b here\nand c | d");
        assert_eq!(doc.rows.len(), 1);
        assert_eq!(doc.rows[0].kind, RowKind::Paragraph);
        let doc = Doc::parse("| a | b |\n| 1 | 2 |");
        assert!(doc.rows.iter().all(|r| r.kind == RowKind::Raw));
        // Cell counts differ.
        let doc = Doc::parse("| a | b |\n|---|\n");
        assert!(doc.rows.iter().all(|r| r.table().is_none()));
        // Not a delimiter: no dashes, other characters, no pipe.
        for src in ["a | b\n| : | : |", "a | b\n|-x-|---|", "a\n---", "a | b\n--- ---"] {
            let doc = Doc::parse(src);
            assert!(doc.rows.iter().all(|r| r.table().is_none()), "{src:?}");
        }
        // A setext heading still is one.
        let doc = Doc::parse("Title\n---");
        assert_eq!(doc.rows[0].kind, RowKind::Heading(2));
        // In a code block, a table is code.
        let doc = Doc::parse("```\na | b\n--|--\n```");
        assert!(doc.rows.iter().all(|r| r.table().is_none()));
        // Indented four: code.
        let doc = Doc::parse("    a | b\n    --|--");
        assert!(doc.rows.iter().all(|r| r.kind == RowKind::Code));
    }

    #[test]
    fn a_table_draws_as_a_grid_unless_the_selection_is_in_it() {
        let doc = Doc::parse(TABLE);
        let all = [false, true, true, true, true, false];
        assert_eq!(doc.grid_rows(None), all);
        assert_eq!(doc.grid_rows(Some(0..0)), all);
        let after = TABLE.find("After").unwrap();
        assert_eq!(doc.grid_rows(Some(after..after)), all);
        // In a cell, at the table's first byte, at its last line's end, or
        // a selection running in from outside: the source shows.
        let none = [false; 6];
        let cell = TABLE.find("Tool").unwrap();
        assert_eq!(doc.grid_rows(Some(cell..cell)), none);
        let start = TABLE.find("| Element").unwrap();
        assert_eq!(doc.grid_rows(Some(start..start)), none);
        let end = doc.rows[4].lines.end;
        assert_eq!(doc.grid_rows(Some(end..end)), none);
        assert_eq!(doc.grid_rows(Some(0..cell)), none);
        // The grid leaves the delimiter line out; the source keeps it.
        assert_eq!(doc.hidden_rows(None), [false, false, true, false, false, false]);
        assert_eq!(doc.hidden_rows(Some(cell..cell)), none);
    }

    #[test]
    fn two_tables_show_their_source_one_at_a_time() {
        let src = "a | b\n--|--\n1 | 2\n\nc | d\n--|--\n3 | 4";
        let doc = Doc::parse(src);
        assert_eq!(doc.tables(), [0..=2, 3..=5]);
        let at = src.find('3').unwrap();
        assert_eq!(doc.grid_rows(Some(at..at)), [true, true, true, false, false, false]);
    }

    #[test]
    fn drawing_and_editing_elsewhere_never_touches_the_table() {
        let mut e = Editor::markdown(TABLE);
        for at in [0, TABLE.find("Tool").unwrap(), TABLE.len()] {
            e.move_to(at, false);
            let _ = e.doc().grid_rows(Some(e.selection()));
            let _ = e.doc().hidden_rows(Some(e.selection()));
            assert_eq!(e.text(), TABLE);
        }
        // Typing in the paragraph under it leaves its bytes as they were.
        e.move_to(TABLE.len(), false);
        e.insert(" More.");
        assert!(e.text().starts_with(&TABLE[..TABLE.find("\n\nAfter").unwrap()]));
        // Typing in a cell's source edits that cell alone.
        let mut e = Editor::markdown(TABLE);
        let at = TABLE.find("Tool").unwrap() + "Tool".len();
        e.move_to(at, false);
        e.insert("s");
        let doc = e.doc();
        assert_eq!(cells(&doc.rows[4]), ["Tools", "Tabs\none each"]);
    }
}
