//! The diff view: its screens as text, its colors, and its keys through the
//! real keymap. State comes from a parsed patch, so nothing here reads git; the
//! live refresh runs over a real repository at the end.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use shelbi_app::diff::backend::{DiffBackend, HostBackend};
use shelbi_app::diff::comments::{Anchor, Author, Comment, CommentOp};
use shelbi_app::diff::highlight::highlight_file;
use shelbi_app::diff::model::Side;
use shelbi_app::diff::parse::parse_patch;
use shelbi_app::diff::store::DiffStore;
use shelbi_app::diff::{Changeset, CommitInfo, DiffOptions, DiffSession, DiffState, Mode, TargetKind, ViewMode};
use shelbi_core::Host;
use shelbi_state::keymap::{validate_keymaps_yaml, DisplayStyle, Keymaps};

use super::paint::{layout, paint, Colors, Paint};
use super::{DiffOut, DiffSetup, DiffView};

const PATCH: &str = "diff --git a/crates/app/src/review.rs b/crates/app/src/review.rs
--- a/crates/app/src/review.rs
+++ b/crates/app/src/review.rs
@@ -120,4 +120,4 @@ impl ReviewDecisions
     fn begin(&mut self, id: &str) {
-        self.task = Some(id);
+        self.running.insert(id);
         self.ready();
     }
diff --git a/src/added.rs b/src/added.rs
new file mode 100644
--- /dev/null
+++ b/src/added.rs
@@ -0,0 +1,2 @@
+fn new() {}
+// done
diff --git a/old.rs b/old.rs
deleted file mode 100644
--- a/old.rs
+++ /dev/null
@@ -1 +0,0 @@
-gone
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
diff --git a/before.txt b/after.txt
similarity index 100%
rename from before.txt
rename to after.txt
";

fn keymaps() -> Keymaps {
    validate_keymaps_yaml("", None).0
}

fn changeset() -> Changeset {
    let files = parse_patch(PATCH);
    let additions = files.iter().map(|f| f.additions).sum();
    let deletions = files.iter().map(|f| f.deletions).sum();
    Changeset { files, additions, deletions, ..Changeset::default() }
}

fn state() -> DiffState {
    let mut s = DiffState::new(Author::human("jlong"));
    s.set_changeset(Arc::new(changeset()));
    s
}

/// The buffer as lines of text, trailing spaces gone.
fn lines(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (0..area.height)
        .map(|y| {
            let mut line = String::new();
            let mut skip = 0;
            for x in 0..area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buf[(x, y)].symbol();
                skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1);
                line.push_str(symbol);
            }
            line.trim_end().to_string()
        })
        .collect()
}

fn draw(state: &mut DiffState, width: u16, height: u16, truecolor: bool) -> Buffer {
    let km = keymaps();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let at = layout(area, state.file_list_open());
    state.set_viewport(usize::from(at.rows.height));
    let p = Paint {
        state,
        keymaps: &km,
        style: DisplayStyle::Linux,
        colors: Colors::new(truecolor),
        focused: true,
        store_error: None,
        empty: "No changes",
    };
    paint(&p, &mut buf, area, &at, 0);
    buf
}

fn screen(state: &mut DiffState, width: u16, height: u16) -> Vec<String> {
    lines(&draw(state, width, height, true))
}

fn assert_screen(actual: &[String], expected: &str) {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.iter().map(String::as_str).collect();
    assert_eq!(actual, expected, "\n--- got ---\n{}\n", actual.join("\n"));
}

fn press(state: &mut DiffState, spec: &str) {
    let km = keymaps();
    for key in spec.split(' ') {
        let chord = shelbi_state::keymap::KeyChord::parse(key).unwrap();
        state.key(chord, &km.diff);
    }
}

#[test]
fn unified_with_the_file_list() {
    let mut s = state();
    assert_screen(
        &screen(&mut s, 100, 14),
        "\
┌ Files 5 ──────────────────────┐┌ crates/app/src/review.rs  +1 −1 ────────────────────────────────┐
│ Commits: all ▾                ││ ▾ crates/app/src/review.rs  +1 −1                               │
│ ▸ M app/src/review.rs   +1 −1 ││ @@ -120,4 +120,4 @@ impl ReviewDecisions                        │
│   A src/added.rs           +2 ││  120 120 │      fn begin(&mut self, id: &str) {                 │
│   D old.rs                 −1 ││  121     │ -        self.task = Some(id);                       │
│   M image.png                 ││      121 │ +        self.running.insert(id);                    │
│   R after.txt                 ││  122 122 │          self.ready();                               │
│                               ││  123 123 │      }                                               │
│                               ││ ▾ src/added.rs  +2                                              │
│                               ││ @@ -0,0 +1,2 @@                                                 │
│                               ││        1 │ +fn new() {}                                         │
│                               ││        2 │ +// done                                             │
└───────────────────────────────┘└─────────────────────────────────────────────────────────────────┘
NORMAL · unified · all lines · 1/5 · ? help",
    );
}

#[test]
fn unified_in_an_eighty_column_terminal() {
    // An 80-column terminal less the panel's column: the list goes, the code
    // still reads, and what doesn't fit is cut with a mark.
    let mut s = state();
    s.set_file_list(false);
    assert_screen(
        &screen(&mut s, 52, 9),
        "\
┌ crates/app/src/review.rs  +1 −1 ─────────────────┐
│ ▾ crates/app/src/review.rs  +1 −1                │
│ @@ -120,4 +120,4 @@ impl ReviewDecisions         │
│  120 120 │      fn begin(&mut self, id: &str) {  │
│  121     │ -        self.task = Some(id);        │
│      121 │ +        self.running.insert(id);     │
│  122 122 │          self.ready();                │
└──────────────────────────────────────────────────┘
NORMAL · unified · all lines · 1/5 · ? help",
    );
}

#[test]
fn split_pairs_an_edit_side_by_side() {
    let mut s = state();
    s.set_file_list(false);
    press(&mut s, "s");
    assert_eq!(s.view(), ViewMode::Split);
    assert_screen(
        &screen(&mut s, 80, 10),
        "\
┌ crates/app/src/review.rs  +1 −1 ─────────────────────────────────────────────┐
│ ▾ crates/app/src/review.rs  +1 −1                                            │
│ @@ -120,4 +120,4 @@ impl ReviewDecisions                                     │
│ 120      fn begin(&mut self, id: &st…│120      fn begin(&mut self, id: &str… │
│ 121 -        self.task = Some(id);   │121 +        self.running.insert(id);  │
│ 122          self.ready();           │122          self.ready();             │
│ 123      }                           │123      }                             │
│ ▾ src/added.rs  +2                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
NORMAL · split · all lines · 1/5 · ? help",
    );
}

#[test]
fn split_stays_readable_when_narrow() {
    let mut s = state();
    s.set_file_list(false);
    press(&mut s, "s");
    let rows = screen(&mut s, 52, 8);
    assert_eq!(rows[3], "│ 120      fn begin(&mut…│120      fn begin(&mut … │");
    assert_eq!(rows[4], "│ 121 -        self.task…│121 +        self.runni… │");
    for row in &rows[..7] {
        assert_eq!(unicode_width::UnicodeWidthStr::width(row.as_str()), 52, "{row}");
    }
}

#[test]
fn files_without_lines_say_why() {
    let mut s = state();
    s.set_file_list(false);
    let rows = screen(&mut s, 60, 26);
    let at = rows.iter().position(|r| r.contains("▾ image.png")).expect("the binary file");
    assert!(rows[at + 1].contains("Binary file"), "{rows:#?}");
    assert!(rows[at + 2].contains("▾ before.txt → after.txt"), "{rows:#?}");
    assert!(rows[at + 3].contains("Renamed from before.txt, no changes"), "{rows:#?}");
    // A deleted file's lines have an old number and no new one.
    let gone = rows.iter().find(|r| r.contains("-gone")).expect("the deleted line");
    assert!(gone.starts_with("│    1     │ -gone"), "{gone}");
}

#[test]
fn the_list_marks_the_current_and_viewed_files() {
    let mut s = state();
    press(&mut s, "m");
    s.goto_file(1);
    let rows = screen(&mut s, 100, 12);
    assert!(rows[2].starts_with("│ ✓ M app/src/review.rs"), "{}", rows[2]);
    assert!(rows[3].starts_with("│ ▸ A src/added.rs"), "{}", rows[3]);
    assert!(rows[4].starts_with("│   D old.rs"), "{}", rows[4]);
}

#[test]
fn a_long_list_name_keeps_its_end() {
    let mut s = state();
    let rows = screen(&mut s, 70, 8);
    // The whole path doesn't fit, so its leading directories go first.
    assert!(rows[2].starts_with("│ ▸ M review.rs"), "{}", rows[2]);
}

#[test]
fn the_help_overlay_lists_the_keys_in_effect() {
    let mut s = state();
    press(&mut s, "?");
    assert!(matches!(s.mode(), Mode::Help { .. }));
    let text = screen(&mut s, 80, 100).join("\n");
    assert!(text.contains("┌ Keys "), "{text}");
    for expected in ["Move", "j, Down", "Line down", "g g", "Go to the top", "Ctrl+B, \\", "Show or hide the file list"] {
        assert!(text.contains(expected), "no {expected} in\n{text}");
    }
    assert!(text.contains("Back to the panel"), "{text}");
    assert!(text.contains("Shelbi's command palette"), "{text}");
    press(&mut s, "x");
    assert!(matches!(s.mode(), Mode::Normal));
}

#[test]
fn the_help_overlay_shows_a_rebound_key_and_scrolls() {
    let (km, _) = validate_keymaps_yaml("defaults:\n  diff:\n    cursor_down: e\n", None);
    let mut s = state();
    s.key(shelbi_state::keymap::KeyChord::parse("?").unwrap(), &km.diff);
    let area = Rect::new(0, 0, 80, 40);
    let mut buf = Buffer::empty(area);
    let at = layout(area, false);
    let p = Paint {
        state: &s,
        keymaps: &km,
        style: DisplayStyle::Linux,
        colors: Colors::new(true),
        focused: true,
        store_error: None,
        empty: "",
    };
    paint(&p, &mut buf, area, &at, 0);
    let text = lines(&buf).join("\n");
    assert!(text.contains(" e "), "{text}");
    assert!(!text.contains("j, Down"), "{text}");

    // A short terminal scrolls, and past the end it holds the last page.
    let mut s = state();
    press(&mut s, "?");
    let first = screen(&mut s, 80, 12);
    press(&mut s, "j j j");
    assert_ne!(screen(&mut s, 80, 12), first);
    for _ in 0..200 {
        press(&mut s, "j");
    }
    assert!(screen(&mut s, 80, 12).join("\n").contains("Shelbi's command palette"));
}

#[test]
fn the_palette_lists_files_then_commands() {
    let mut s = state();
    press(&mut s, "ctrl-p");
    let text = screen(&mut s, 80, 20).join("\n");
    assert!(text.contains("┌ Files "), "{text}");
    assert!(text.contains("src/added.rs"), "{text}");
    assert!(text.contains("> for commands"), "{text}");
    press(&mut s, ">");
    let text = screen(&mut s, 80, 24).join("\n");
    assert!(text.contains("┌ Commands "), "{text}");
    assert!(text.contains("Refresh"), "{text}");
}

#[test]
fn the_commit_picker_lists_the_branchs_commits() {
    let mut cs = changeset();
    cs.commits = vec![CommitInfo {
        sha: "abc1234567".into(),
        short: "abc1234".into(),
        author: "Ada".into(),
        time: 0,
        subject: "Fix the thing".into(),
    }];
    let mut s = DiffState::new(Author::human("j"));
    s.set_changeset(Arc::new(cs));
    press(&mut s, "c");
    let text = screen(&mut s, 80, 20).join("\n");
    assert!(text.contains("┌ Commits "), "{text}");
    assert!(text.contains("All changes"), "{text}");
    assert!(text.contains("abc1234 Fix the thing"), "{text}");
}

#[test]
fn a_thread_and_a_draft_show_with_their_line() {
    let mut s = state();
    press(&mut s, "j j j");
    press(&mut s, "enter");
    assert!(matches!(s.mode(), Mode::Comment(_)));
    let mut text = screen(&mut s, 80, 14).join("\n");
    assert!(text.contains("Comment on crates/app/src/review.rs:"), "{text}");
    assert!(text.contains("Enter save"), "{text}");
    for c in "why".chars() {
        press(&mut s, &c.to_string());
    }
    press(&mut s, "enter");
    text = screen(&mut s, 80, 14).join("\n");
    assert!(text.contains("┃ jlong: why"), "{text}");
    assert!(text.contains("1 comment"), "{text}");
}

#[test]
fn search_shows_its_prompt_and_marks_its_matches() {
    let mut s = state();
    press(&mut s, "/");
    for c in "running".chars() {
        press(&mut s, &c.to_string());
    }
    assert_eq!(screen(&mut s, 80, 14)[13], "/running");
    press(&mut s, "enter");
    let buf = draw(&mut s, 100, 14, true);
    let text = lines(&buf).join("\n");
    assert!(text.contains("/running 1/1"), "{text}");
    let colors = Colors::new(true);
    let row = lines(&buf).iter().position(|l| l.contains("self.running")).unwrap() as u16;
    let at = lines(&buf)[row as usize].find("running").unwrap();
    let x = lines(&buf)[row as usize][..at].chars().count() as u16;
    assert_eq!(buf[(x, row)].bg, colors.search);
}

#[test]
fn lines_take_their_colors() {
    let mut s = state();
    s.set_file_list(false);
    let highlights = highlight_file(&s.changeset().files[0]).expect("rust highlights");
    s.set_highlights(0, Arc::new(highlights));
    let buf = draw(&mut s, 80, 12, true);
    let colors = Colors::new(true);
    let rows = lines(&buf);
    let row_of = |needle: &str| rows.iter().position(|l| l.contains(needle)).unwrap() as u16;
    let x_of = |row: u16, needle: &str| {
        let line = &rows[usize::from(row)];
        line[..line.find(needle).unwrap()].chars().count() as u16
    };
    let added = row_of("running.insert");
    assert_eq!(buf[(70, added)].bg, colors.add_bg, "the tint fills the row");
    // The changed word stands out from the rest of its line.
    assert_eq!(buf[(x_of(added, "running"), added)].bg, colors.add_word);
    let deleted = row_of("self.task");
    assert_eq!(buf[(70, deleted)].bg, colors.del_bg);
    assert_eq!(buf[(x_of(deleted, "task"), deleted)].bg, colors.del_word);
    // Syntax: `fn` is a keyword, its line context, untinted.
    let context = row_of("fn begin");
    assert_ne!(buf[(x_of(context, "fn"), context)].fg, colors.fg);
    assert_eq!(buf[(70, context)].bg, Color::Reset);
}

/// Whether a 256-color terminal shows `color` as it is: quantizing leaves it be.
fn fits_256(color: Color) -> bool {
    crate::theme::fit_color(color, false) == color
}

#[test]
fn a_256_color_terminal_gets_no_rgb_and_keeps_the_tints_apart() {
    for truecolor in [true, false] {
        let c = Colors::new(truecolor);
        let all = [
            c.add_bg, c.del_bg, c.add_word, c.del_word, c.add_fg, c.del_fg, c.fg, c.muted, c.accent, c.selection,
            c.search, c.search_fg, c.header, c.dialog, c.thread,
        ];
        if !truecolor {
            assert!(all.iter().all(|c| fits_256(*c)), "{all:?}");
        }
        assert_ne!(c.add_bg, c.del_bg);
        assert_ne!(c.add_bg, c.add_word);
        assert_ne!(c.del_bg, c.del_word);
    }
    let mut s = state();
    let buf = draw(&mut s, 100, 14, false);
    for cell in buf.content() {
        assert!(fits_256(cell.fg) && fits_256(cell.bg), "{cell:?}");
    }
    assert!(lines(&buf).join("\n").contains("self.running.insert"));
}

#[test]
fn the_cursor_shows_in_the_gutter_and_on_its_character() {
    let mut s = state();
    s.set_file_list(false);
    press(&mut s, "j j j");
    let buf = draw(&mut s, 80, 12, true);
    let colors = Colors::new(true);
    let row = lines(&buf).iter().position(|l| l.contains("self.task")).unwrap() as u16;
    assert_eq!(buf[(2, row)].bg, colors.selection, "the gutter");
    assert_eq!(buf[(70, row)].bg, colors.del_bg, "the line keeps its tint");

    // A find past the edge scrolls that line sideways to show it.
    let long = format!(
        "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+{}Z\n",
        "y".repeat(200)
    );
    let mut s = DiffState::new(Author::human("j"));
    s.set_changeset(Arc::new(Changeset { files: parse_patch(&long), ..Changeset::default() }));
    s.set_file_list(false);
    press(&mut s, "j j j f shift-z");
    let buf = draw(&mut s, 60, 10, true);
    let text = lines(&buf).join("\n");
    assert!(text.contains('Z'), "{text}");
    let row = lines(&buf).iter().position(|l| l.contains('Z')).unwrap() as u16;
    let x = (0..60).find(|x| buf[(*x, row)].symbol() == "Z").unwrap();
    assert!(buf[(x, row)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn tabs_and_wide_characters_keep_the_columns() {
    let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -0,0 +1,2 @@\n+\tindented\n+日本語 ok\n";
    let mut s = DiffState::new(Author::human("j"));
    s.set_changeset(Arc::new(Changeset { files: parse_patch(patch), ..Changeset::default() }));
    s.set_file_list(false);
    let rows = screen(&mut s, 50, 8);
    assert!(rows[3].contains("+    indented"), "{rows:#?}");
    assert!(rows[4].contains("+日本語 ok"), "{rows:#?}");
    for row in &rows[..7] {
        assert_eq!(unicode_width::UnicodeWidthStr::width(row.as_str()), 50, "{row}");
    }
}

#[test]
fn the_blame_gutter_fills_in() {
    let mut s = state();
    s.set_file_list(false);
    press(&mut s, "shift-b");
    let path = s.changeset().files[0].path.clone();
    s.set_blame(
        path,
        vec![shelbi_app::diff::git::BlameLine {
            line: 121,
            short: "abc1234".into(),
            author: "Ada Lovelace".into(),
            time: 0,
            summary: "s".into(),
            uncommitted: false,
        }],
    );
    let rows = screen(&mut s, 90, 12);
    let row = rows.iter().find(|l| l.contains("self.running")).unwrap();
    assert!(row.contains("abc1234 Ada Love"), "{row}");
}

#[test]
fn empty_loading_and_failed_views_say_so() {
    let mut s = DiffState::new(Author::human("j"));
    s.set_file_list(false);
    assert!(screen(&mut s, 60, 9).join("\n").contains("Loading diff…"));
    s.set_changeset(Arc::new(Changeset::default()));
    assert!(screen(&mut s, 60, 9).join("\n").contains("No changes"));
    s.set_error("not a git repository".into());
    let text = screen(&mut s, 60, 9).join("\n");
    assert!(text.contains("Couldn't read the diff: not a git repository"), "{text}");
    assert!(text.contains("retries"), "{text}");
}

#[test]
fn a_tiny_area_draws_without_panicking() {
    let mut s = state();
    for (w, h) in [(1, 1), (2, 2), (3, 3), (5, 2), (10, 3), (20, 4), (0, 0)] {
        for keys in ["s", "s", "?", "x", "ctrl-p", "esc", "enter", "esc", "esc"] {
            press(&mut s, keys);
            let _ = draw(&mut s, w, h, true);
        }
    }
}

// ---- The view: keys, focus, mouse ------------------------------------------

/// A session that never reads, with `changeset` set directly.
fn view() -> DiffView {
    let opener: super::DiffOpener = Arc::new(|| {
        let options = DiffOptions { author: Author::human("j"), watch: None, wake: Arc::new(|| {}) };
        DiffSession::open(|| Err("no backend in this test".to_string()), options)
    });
    let setup = DiffSetup { keymaps: keymaps(), style: DisplayStyle::Linux };
    let mut v = DiffView::new(opener, setup, Some("/nowhere".to_string()), "No changes");
    v.session().state_mut().set_changeset(Arc::new(changeset()));
    v.set_clipboard(|_| {});
    v
}

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE }
}

fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn render(v: &mut DiffView, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    v.render(&mut buf, area, true, true);
    buf
}

#[test]
fn terminal_keys_reach_every_action_as_a_terminal_sends_them() {
    let mut v = view();
    render(&mut v, 100, 20);
    let cursor = |v: &mut DiffView| v.session().state().cursor();

    v.key(ch('j'));
    assert_eq!(cursor(&mut v), 1);
    v.key(ch('k'));
    assert_eq!(cursor(&mut v), 0);
    v.key(ch('3'));
    v.key(ch('j'));
    assert_eq!(cursor(&mut v), 3, "a count");
    v.key(ch('g'));
    v.key(ch('g'));
    assert_eq!(cursor(&mut v), 0, "two keys");
    // A terminal sends a capital bare, or with Shift; both are `G`.
    v.key(ch('G'));
    let last = v.session().state().table().len() - 1;
    assert_eq!(cursor(&mut v), last);
    v.key(ch('g'));
    v.key(ch('g'));
    v.key(key(KeyCode::Char('G'), KeyModifiers::SHIFT));
    assert_eq!(cursor(&mut v), last);
    // A shifted symbol is the symbol.
    v.key(key(KeyCode::Char('?'), KeyModifiers::SHIFT));
    assert!(matches!(v.session().state().mode(), Mode::Help { .. }));
    v.key(ch('x'));
    // Tab and Shift-Tab cycle the filter (crossterm sends BackTab with Shift).
    v.key(key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(v.session().state().filter().label(), "additions");
    v.key(key(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert_eq!(v.session().state().filter().label(), "all lines");
    // Space pages.
    v.key(ch('g'));
    v.key(ch('g'));
    v.key(ch(' '));
    assert!(cursor(&mut v) > 1);
    v.key(key(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(cursor(&mut v), 0);
    v.key(ctrl('n'));
    assert_eq!(v.session().state().current_file(), Some(1));
    v.key(ch('h'));
    assert_eq!(v.session().state().current_file(), Some(0));
    v.key(ch('s'));
    assert_eq!(v.session().state().view(), ViewMode::Split);
}

#[test]
fn it_claims_what_it_binds_and_lets_the_rest_through() {
    let mut v = view();
    // Its own over the shell's.
    assert!(v.claims(&ctrl('p')));
    assert!(v.claims(&key(KeyCode::Char('?'), KeyModifiers::SHIFT)));
    assert!(v.claims(&key(KeyCode::Tab, KeyModifiers::NONE)));
    // The second key of a pair, whatever it is.
    v.key(ch('g'));
    assert!(v.claims(&ctrl('h')));
    v.key(key(KeyCode::Esc, KeyModifiers::NONE));
    // What it doesn't bind stays global.
    assert!(!v.claims(&ctrl('h')));
    assert!(!v.claims(&ctrl('l')));
    assert!(!v.claims(&key(KeyCode::Char(' '), KeyModifiers::CONTROL)));
    // Taking text, it takes everything but Shelbi's palette.
    v.key(ch('/'));
    assert!(v.claims(&ctrl('h')));
    assert!(v.claims(&ch('q')));
    assert!(!v.claims(&key(KeyCode::Char(' '), KeyModifiers::CONTROL)));
}

#[test]
fn esc_lets_go_of_what_is_held_then_leaves() {
    let mut v = view();
    let esc = key(KeyCode::Esc, KeyModifiers::NONE);
    v.key(ch('v'));
    assert_eq!(v.key(esc), DiffOut::None, "ends the selection");
    v.key(ch('3'));
    assert_eq!(v.key(esc), DiffOut::None, "drops the count");
    v.key(ch('/'));
    v.key(ch('s'));
    v.key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(v.key(esc), DiffOut::None, "clears the search");
    assert_eq!(v.key(esc), DiffOut::FocusPanel);
}

#[test]
fn the_list_starts_hidden_when_narrow_and_toggles() {
    let mut v = view();
    let has_list = |b: &Buffer| lines(b)[0].contains("Files 5");
    let wide = render(&mut v, 100, 12);
    assert!(has_list(&wide));
    let narrow = render(&mut v, 52, 12);
    assert!(!has_list(&narrow), "narrow hides it");
    // Ctrl-b opens it, and it stays while the width doesn't cross the line.
    v.key(ctrl('b'));
    assert!(has_list(&render(&mut v, 52, 12)));
    assert!(has_list(&render(&mut v, 60, 12)));
    v.key(ctrl('b'));
    assert!(!has_list(&render(&mut v, 52, 12)));
    // Backslash too.
    v.key(ch('\\'));
    assert!(has_list(&render(&mut v, 52, 12)));
    // Widening shows it again whatever it was.
    v.key(ctrl('b'));
    assert!(has_list(&render(&mut v, 100, 12)));
}

#[test]
fn ctrl_b_no_longer_pages() {
    let mut v = view();
    render(&mut v, 100, 8);
    v.key(ch(' '));
    let down = v.session().state().cursor();
    v.key(ctrl('b'));
    assert_eq!(v.session().state().cursor(), down);
    v.key(ch('b'));
    assert!(v.session().state().cursor() < down);
}

fn click(column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE }
}

#[test]
fn the_mouse_jumps_scrolls_and_places_the_cursor() {
    let mut v = view();
    render(&mut v, 100, 14);
    // A file row: row 0 frame, 1 commits, 2 the first file.
    v.mouse(click(8, 3));
    assert_eq!(v.session().state().current_file(), Some(1));
    // Its check column marks it viewed instead of jumping.
    v.mouse(click(2, 4));
    let file = v.session().state().changeset().files[2].clone();
    assert!(v.session().state().is_viewed(&file));
    assert_eq!(v.session().state().current_file(), Some(1));
    // The picker opens from its row.
    v.mouse(click(5, 1));
    assert!(matches!(v.session().state().mode(), Mode::Palette(_)));
    v.key(key(KeyCode::Esc, KeyModifiers::NONE));
    // A line takes the cursor, offset by the first row in view.
    render(&mut v, 100, 14);
    let top = v.session().state().top();
    v.mouse(click(50, 4));
    assert_eq!(v.session().state().cursor(), top + 3);
    // The wheel scrolls the rows.
    render(&mut v, 100, 6);
    let before = v.session().state().top();
    v.mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: 50, row: 3, modifiers: KeyModifiers::NONE });
    assert!(v.session().state().top() > before);
}

static COPIED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[test]
fn yank_copies_and_ctrl_g_asks_for_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().to_string_lossy().into_owned();
    fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
    fs::write(dir.path().join("crates/app/src/review.rs"), "x").unwrap();
    let mut v = view();
    v.worktree = Some(wt);
    v.set_clipboard(|t| COPIED.lock().unwrap().push(t.to_string()));
    render(&mut v, 100, 20);

    // Selecting two lines and yanking copies their text, and says so.
    for k in [ch('j'), ch('j'), ch('v'), ch('j'), ch('y')] {
        v.key(k);
    }
    let copied = COPIED.lock().unwrap().pop().unwrap();
    assert!(copied.contains("fn begin"), "{copied}");
    assert_eq!(v.session().state().flash(), Some("Copied"));
    // The next key clears the note.
    v.key(ch('j'));
    assert_eq!(v.session().state().flash(), None);

    match v.key(ctrl('g')) {
        DiffOut::Edit { path, line } => {
            assert!(path.ends_with("crates/app/src/review.rs"));
            assert!(line.is_some());
        }
        other => panic!("{other:?}"),
    }
    // Nothing to open on this machine: it says so, and nothing opens.
    v.worktree = None;
    assert_eq!(v.key(ctrl('g')), DiffOut::None);
    assert!(v.session().state().flash().is_some_and(|n| n.contains("isn't on this machine")));
}

#[test]
fn a_paste_goes_to_a_mode_taking_text_and_nowhere_else() {
    let mut v = view();
    render(&mut v, 100, 20);
    v.paste("j j");
    assert_eq!(v.session().state().cursor(), 0, "not keys");
    v.key(ch('/'));
    v.paste("run\nning");
    match v.session().state().mode() {
        Mode::Search { query } => assert_eq!(query, "running"),
        other => panic!("{other:?}"),
    }
    v.key(key(KeyCode::Esc, KeyModifiers::NONE));
    for k in [ch('j'), ch('j'), ch('j'), key(KeyCode::Enter, KeyModifiers::NONE)] {
        v.key(k);
    }
    v.paste("two\nlines");
    match v.session().state().mode() {
        Mode::Comment(draft) => assert_eq!(draft.text(), "two\nlines"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn r_reopens_a_session_that_failed_to_open() {
    let opens = Arc::new(AtomicUsize::new(0));
    let count = opens.clone();
    let opener: super::DiffOpener = Arc::new(move || {
        let n = count.fetch_add(1, Ordering::SeqCst);
        let options = DiffOptions { author: Author::human("j"), watch: None, wake: Arc::new(|| {}) };
        DiffSession::open(move || Err(format!("failure {n}")), options)
    });
    let setup = DiffSetup { keymaps: keymaps(), style: DisplayStyle::Linux };
    let mut v = DiffView::new(opener, setup, None, "");
    let until = Instant::now() + Duration::from_secs(10);
    while v.session().state().error().is_none() && Instant::now() < until {
        v.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(v.session().state().error(), Some("failure 0"));
    v.key(ch('r'));
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    let until = Instant::now() + Duration::from_secs(10);
    while v.session().state().error() != Some("failure 1") && Instant::now() < until {
        v.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(v.session().state().error(), Some("failure 1"));
}

// ---- A live workspace, over real git ---------------------------------------

fn git(path: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn wait_for(v: &mut DiffView, what: &str, done: impl Fn(&mut DiffView) -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        v.poll();
        if done(v) {
            return;
        }
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_workspace_diff_follows_the_agents_edits_and_rests_off_screen() {
    let repo = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let path = repo.path().canonicalize().unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    git(&path, &["config", "user.name", "Fixture"]);
    git(&path, &["config", "user.email", "fixture@example.com"]);
    fs::write(path.join("a.txt"), "one\ntwo\n").unwrap();
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "-m", "base"]);
    git(&path, &["checkout", "-q", "-b", "work"]);

    let store_path = store.path().to_path_buf();
    let worktree = path.to_string_lossy().into_owned();
    let opener: super::DiffOpener = Arc::new(move || {
        let backend: Arc<dyn DiffBackend> = Arc::new(HostBackend::over(
            Host::Local,
            worktree.clone(),
            TargetKind::Workspace,
            "main",
            DiffStore::at(&store_path),
        ));
        let options = DiffOptions {
            author: Author::human("j"),
            watch: Some(Duration::from_millis(40)),
            wake: Arc::new(|| {}),
        };
        DiffSession::open(move || Ok(backend), options)
    });
    let setup = DiffSetup { keymaps: keymaps(), style: DisplayStyle::Linux };
    let mut v = DiffView::new(opener, setup, Some(path.to_string_lossy().into_owned()), "No changes yet");
    wait_for(&mut v, "the first read", |v| !v.session().state().loading());
    assert!(lines(&render(&mut v, 80, 10)).join("\n").contains("No changes yet"));

    // The agent edits: it shows with no key pressed.
    fs::write(path.join("a.txt"), "one\nTWO\n").unwrap();
    wait_for(&mut v, "the edit", |v| v.session().state().changeset().files.len() == 1);
    assert!(lines(&render(&mut v, 80, 10)).join("\n").contains("TWO"));

    // Off screen, it doesn't read...
    v.set_showing(false);
    // A read already started may still land: let it.
    std::thread::sleep(Duration::from_millis(150));
    v.poll();
    fs::write(path.join("a.txt"), "one\nTHREE\n").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    v.poll();
    assert!(!lines(&render(&mut v, 80, 10)).join("\n").contains("THREE"), "paused");

    // ...and on coming back it's current at once.
    v.set_showing(true);
    wait_for(&mut v, "the catch-up", |v| {
        v.session().state().changeset().files[0].hunks[0].lines.iter().any(|l| l.text == "THREE")
    });
}

#[test]
fn comments_and_viewed_marks_come_back_with_the_task() {
    let store = tempfile::tempdir().unwrap();
    let doc = DiffStore::at(store.path());
    doc.apply_comment(&CommentOp::Add(Comment {
        id: "c1".into(),
        anchor: Anchor { path: "a.txt".into(), side: Side::New, start: 2, end: 2, code: vec!["TWO".into()] },
        author: Author::human("ada"),
        body: "first line\nsecond line".into(),
        created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        replies: Vec::new(),
        sent: false,
    }))
    .unwrap();
    let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n";
    let mut s = DiffState::new(Author::human("j"));
    s.set_changeset(Arc::new(Changeset { files: parse_patch(patch), ..Changeset::default() }));
    s.set_comments(doc.comments().unwrap());
    s.set_file_list(false);
    let text = screen(&mut s, 70, 10).join("\n");
    assert!(text.contains("┃ ada: first line …"), "{text}");
}
