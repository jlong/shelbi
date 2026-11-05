//! The state machine, driven by keys through the shipped keymap.

use std::sync::Arc;

use shelbi_state::keymap::{validate_keymaps_yaml, DiffAction, Key, KeyChord, Mods, ModeKeymap};

use super::comments::{Author, CommentDoc, CommentOp};
use super::model::{Changeset, CommitInfo, CommitScope, FileStatus};
use super::palette::PaletteMode;
use super::parse::parse_patch;
use super::rows::{HunkFilter, Row, ThreadPart, ViewMode};
use super::state::{normalize_key, DiffEffect, DiffState, Mode};
use super::store::ViewedOp;

fn keymap() -> ModeKeymap<DiffAction> {
    validate_keymaps_yaml("", None).0.diff
}

const PATCH: &str = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,6 +1,7 @@ fn one
 first(foo, bar);

-old thing
+new thing
+another thing

 keep
 last
@@ -20,2 +21,2 @@ fn two
 ctx
-Foo here
+foo there
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-gone
+foo again
diff --git a/c.png b/c.png
Binary files a/c.png and b/c.png differ
";

fn changeset(patch: &str) -> Arc<Changeset> {
    let files = parse_patch(patch);
    Arc::new(Changeset {
        additions: files.iter().map(|f| f.additions).sum(),
        deletions: files.iter().map(|f| f.deletions).sum(),
        files,
        commits: vec![CommitInfo {
            sha: "abcdef1".into(),
            short: "abcdef1".into(),
            author: "Ada".into(),
            time: 0,
            subject: "A commit".into(),
        }],
        ..Changeset::default()
    })
}

struct Driver {
    s: DiffState,
    km: ModeKeymap<DiffAction>,
}

impl Driver {
    fn new() -> Self {
        let mut s = DiffState::new(Author::human("jlong"));
        s.set_changeset(changeset(PATCH));
        s.set_viewport(10);
        Driver { s, km: keymap() }
    }

    /// Keys in `keys.yaml`'s spelling, one per word (`g`, `ctrl-d`, `enter`).
    fn press(&mut self, keys: &str) -> Vec<DiffEffect> {
        let mut out = Vec::new();
        for key in keys.split_whitespace() {
            let chord = KeyChord::parse(key).unwrap();
            out.extend(self.s.key(chord, &self.km));
        }
        out
    }

    /// Text typed into whatever has the keys.
    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.s.key(KeyChord::new(Key::Char(c), Mods::NONE), &self.km);
        }
    }

    fn row(&self) -> Row {
        self.s.table().rows[self.s.cursor()]
    }

    /// The line under the cursor.
    fn text(&self) -> String {
        match self.row() {
            Row::Line { file, hunk, line } => self.s.line(file, hunk, line).unwrap().text.clone(),
            Row::Split { file, hunk, old, new } => self.s.line(file, hunk, new.or(old).unwrap()).unwrap().text.clone(),
            other => format!("{other:?}"),
        }
    }

    fn goto_text(&mut self, text: &str) {
        self.press("g g");
        for _ in 0..self.s.table().len() {
            if self.text() == text {
                return;
            }
            self.press("j");
        }
        panic!("no line {text}");
    }
}

#[test]
fn line_and_file_motion() {
    let mut d = Driver::new();
    assert_eq!(d.row(), Row::File { file: 0, folded: false });
    d.press("j");
    assert_eq!(d.row(), Row::Hunk { file: 0, hunk: 0, folded: false });
    d.press("j");
    assert_eq!(d.text(), "first(foo, bar);");
    d.press("k");
    d.press("k");
    d.press("k");
    assert_eq!(d.s.cursor(), 0, "stops at the top");

    d.press("l");
    assert_eq!(d.row(), Row::File { file: 1, folded: false });
    d.press("ctrl-n");
    assert_eq!(d.row(), Row::File { file: 2, folded: false });
    d.press("l");
    assert_eq!(d.row(), Row::File { file: 2, folded: false }, "stops at the last file");
    d.press("h");
    assert_eq!(d.row(), Row::File { file: 1, folded: false });
}

#[test]
fn counts_multiply_a_motion() {
    let mut d = Driver::new();
    d.press("3 j");
    assert_eq!(d.s.cursor(), 3);
    assert_eq!(d.s.count(), None, "spent");
    d.press("1 0 j");
    assert_eq!(d.s.cursor(), 13);
    d.press("1 0 0 0 k");
    assert_eq!(d.s.cursor(), 0);
    d.press("2 esc j");
    assert_eq!(d.s.cursor(), 1, "esc lets go of a count");
}

#[test]
fn pages_and_centering() {
    let mut d = Driver::new();
    d.s.set_viewport(4);
    let last = d.s.table().len() - 1;
    d.press("space");
    assert_eq!((d.s.cursor(), d.s.top()), (4, 4));
    d.press("b");
    assert_eq!((d.s.cursor(), d.s.top()), (0, 0));
    d.press("ctrl-d");
    assert_eq!((d.s.cursor(), d.s.top()), (2, 2));
    d.press("ctrl-u");
    assert_eq!((d.s.cursor(), d.s.top()), (0, 0));
    d.press("ctrl-f");
    assert_eq!(d.s.cursor(), 4);
    d.press("page-down");
    assert_eq!(d.s.cursor(), 8);
    d.press("ctrl-b");
    d.press("page-up");
    assert_eq!(d.s.cursor(), 0);
    d.press("G");
    assert_eq!(d.s.cursor(), last);
    assert_eq!(d.s.top(), last + 1 - 4);
    d.press("g g");
    assert_eq!((d.s.cursor(), d.s.top()), (0, 0));

    d.press("9 j");
    d.press("z z");
    assert_eq!(d.s.top(), 7, "the viewport centers on the cursor");
    d.press("M");
    assert_eq!(d.s.cursor(), 9, "the cursor centers in the viewport");
    d.s.scroll_by(-5);
    assert_eq!(d.s.top(), 2);
    assert!(d.s.cursor() <= 5, "the cursor stays in view");
}

#[test]
fn a_first_key_waits_and_esc_lets_go() {
    let mut d = Driver::new();
    d.press("j");
    d.press("g");
    assert_eq!(d.s.pending(), Some(KeyChord::parse("g").unwrap()));
    assert_eq!(d.s.cursor(), 1, "nothing yet");
    d.press("esc");
    assert_eq!(d.s.pending(), None);
    d.press("g k");
    assert_eq!(d.s.cursor(), 1, "an unbound second key does nothing");
    d.press("g g");
    assert_eq!(d.s.cursor(), 0);
}

#[test]
fn change_motion_lands_on_each_run_of_changes() {
    let mut d = Driver::new();
    d.press("] h");
    assert_eq!(d.text(), "old thing");
    d.press("] h");
    assert_eq!(d.text(), "Foo here");
    d.press("[ h");
    assert_eq!(d.text(), "old thing");
    d.press("g g 2 ] h");
    assert_eq!(d.text(), "Foo here");
    d.press("] h");
    assert_eq!(d.text(), "gone");
    d.press("] h");
    assert_eq!(d.text(), "gone", "stays at the last one");
}

#[test]
fn empty_line_motion() {
    let mut d = Driver::new();
    d.press("}");
    let first = d.s.cursor();
    assert_eq!(d.text().trim(), "");
    d.press("}");
    assert!(d.s.cursor() > first);
    assert_eq!(d.text().trim(), "");
    d.press("{");
    assert_eq!(d.s.cursor(), first);
}

#[test]
fn character_find_and_repeat() {
    let mut d = Driver::new();
    d.goto_text("first(foo, bar);");
    d.press("f");
    assert!(matches!(d.s.mode(), Mode::Find { .. }));
    d.typed("o");
    assert_eq!(d.s.col(), 7);
    d.press(";");
    assert_eq!(d.s.col(), 8);
    d.press("t");
    d.typed(",");
    assert_eq!(d.s.col(), 8, "already just before the comma's run");
    d.press("g g");
    d.goto_text("first(foo, bar);");
    d.press("t");
    d.typed(",");
    assert_eq!(d.s.col(), 8);
    d.press("F");
    d.typed("(");
    assert_eq!(d.s.col(), 5);
    d.press("f");
    d.typed(";");
    assert_eq!(d.s.col(), 15);
    d.press("T");
    d.typed("r");
    assert_eq!(d.s.col(), 14);
    d.press("F");
    d.typed("Z");
    assert_eq!(d.s.col(), 14, "nothing to find, nothing moves");
    d.goto_text("first(foo, bar);");
    d.press("2 f");
    d.typed("o");
    assert_eq!(d.s.col(), 8, "a count finds the nth");
    d.press("G g g");
    d.goto_text("first(foo, bar);");
    d.press("$".replace('$', "f").as_str());
    d.typed(";");
    d.press("2 T");
    d.typed("o");
    assert_eq!(d.s.col(), 8, "a count on a till stops short of the nth");
    d.press("t");
    d.typed("o");
    assert_eq!(d.s.col(), 8, "nothing further on");
}

#[test]
fn search_spans_files_with_smart_case() {
    let mut d = Driver::new();
    d.press("/");
    d.typed("foo");
    assert!(matches!(d.s.mode(), Mode::Search { .. }));
    d.press("enter");
    assert_eq!(d.s.match_count(), 4);
    assert_eq!(d.text(), "first(foo, bar);");
    assert_eq!(d.s.col(), 6);
    assert_eq!(d.s.current_match(), Some(1));
    d.press("n");
    assert_eq!(d.text(), "Foo here");
    d.press("n n");
    assert_eq!(d.text(), "foo again");
    d.press("n");
    assert_eq!(d.text(), "first(foo, bar);", "wraps");
    d.press("N");
    assert_eq!(d.text(), "foo again", "wraps back");

    d.press("/");
    d.typed("Foo");
    d.press("enter");
    assert_eq!(d.s.match_count(), 1, "a capital makes it exact");
    assert_eq!(d.text(), "Foo here");
    assert!(d.s.status().contains("/Foo 1/1"));

    d.press("/");
    d.typed("nope");
    d.press("enter");
    assert_eq!(d.s.match_count(), 0);
    assert!(d.s.flash().unwrap().contains("nope"));

    d.press("esc");
    assert_eq!(d.s.query(), "", "esc clears the marks");
    d.press("/ esc");
    assert!(matches!(d.s.mode(), Mode::Normal));
}

#[test]
fn searching_opens_what_hides_the_match() {
    let mut d = Driver::new();
    d.press("z M");
    d.press("/");
    d.typed("again");
    d.press("enter");
    assert_eq!(d.text(), "foo again");

    d.press("g g tab tab");
    assert_eq!(d.s.filter(), HunkFilter::Deletions);
    d.press("/");
    d.typed("foo there");
    d.press("enter");
    assert_eq!(d.text(), "foo there");
    assert_eq!(d.s.filter(), HunkFilter::All, "the filter hid it");
}

#[test]
fn split_and_filter_keep_the_cursor_on_its_line() {
    let mut d = Driver::new();
    d.goto_text("new thing");
    d.press("s");
    assert_eq!(d.s.view(), ViewMode::Split);
    assert_eq!(d.text(), "new thing");
    assert!(matches!(d.row(), Row::Split { .. }));
    d.press("s");
    assert_eq!(d.s.view(), ViewMode::Unified);
    assert_eq!(d.text(), "new thing");

    d.press("tab");
    assert_eq!(d.s.filter(), HunkFilter::Additions);
    assert_eq!(d.text(), "new thing");
    d.press("back-tab");
    assert_eq!(d.s.filter(), HunkFilter::All);
    d.press("back-tab");
    assert_eq!(d.s.filter(), HunkFilter::Deletions);
    assert!(matches!(d.row(), Row::Hunk { .. } | Row::Line { .. }), "its line is hidden, its hunk isn't");
}

#[test]
fn folds() {
    let mut d = Driver::new();
    d.goto_text("old thing");
    let rows = d.s.table().len();
    d.press("z a");
    assert_eq!(d.row(), Row::Hunk { file: 0, hunk: 0, folded: true });
    assert!(d.s.table().len() < rows);
    d.press("z a");
    assert_eq!(d.s.table().len(), rows);
    d.press("z c");
    assert_eq!(d.row(), Row::Hunk { file: 0, hunk: 0, folded: true });
    d.press("z o");
    assert_eq!(d.row(), Row::Hunk { file: 0, hunk: 0, folded: false });

    d.goto_text("old thing");
    d.press("z C");
    assert_eq!(d.row(), Row::File { file: 0, folded: true });
    d.press("z O");
    assert_eq!(d.row(), Row::File { file: 0, folded: false });
    d.press("z a");
    assert_eq!(d.row(), Row::File { file: 0, folded: true }, "on a file header, za folds the file");
    d.press("z O z M");
    let files = d.s.changeset().files.len();
    assert_eq!(d.s.table().len(), files);
    d.press("l");
    assert_eq!(d.row(), Row::File { file: 1, folded: false }, "going to a file opens it");
    d.press("z R");
    assert_eq!(d.s.table().len(), rows);
}

#[test]
fn comment_on_a_line_edit_reply_expand_delete() {
    let mut d = Driver::new();
    d.goto_text("new thing");
    d.press("enter");
    assert!(matches!(d.s.mode(), Mode::Comment(_)));
    d.typed("why not");
    let effects = d.press("enter");
    let [DiffEffect::SaveComment(CommentOp::Add(comment))] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(comment.body, "why not");
    assert_eq!(comment.author.name, "jlong");
    assert_eq!(comment.anchor.location(), "a.rs:3");
    assert_eq!(comment.anchor.code, ["new thing"]);
    assert!(matches!(d.row(), Row::Thread { part: ThreadPart::Head, .. }), "the cursor lands on it");
    assert!(d.s.status().contains("1 comment (gY send to agent)"));

    // Enter on it edits it, keeping the text.
    d.press("enter");
    d.typed("?");
    let effects = d.press("enter");
    assert!(matches!(&effects[..], [DiffEffect::SaveComment(CommentOp::Edit { body, reply: None, .. })] if body == "why not?"), "{effects:?}");

    // Enter on the line it hangs from edits too.
    d.goto_text("new thing");
    d.press("enter");
    assert!(matches!(d.s.mode(), Mode::Comment(_)));
    d.press("esc esc");
    assert!(matches!(d.s.mode(), Mode::Normal));

    // `r` replies, and shows the replies.
    let id = d.s.comments()[0].id.clone();
    d.press("r");
    d.typed("it drops the slot");
    let effects = d.press("enter");
    assert!(matches!(&effects[..], [DiffEffect::SaveComment(CommentOp::Reply { id: i, .. })] if *i == id), "{effects:?}");
    assert!(d.s.thread_expanded(&id));
    let replies = d.s.table().rows.iter().filter(|r| matches!(r, Row::Thread { part: ThreadPart::Reply(_), .. })).count();
    assert_eq!(replies, 1);

    // `o` collapses to a summary, and back.
    d.press("o");
    assert!(d.s.table().rows.iter().any(|r| matches!(r, Row::Thread { part: ThreadPart::Summary, .. })));
    d.press("o");

    // `d` on a reply removes the reply; on the comment, the thread.
    d.press("j");
    assert!(matches!(d.row(), Row::Thread { part: ThreadPart::Reply(0), .. }));
    let effects = d.press("d");
    assert!(matches!(&effects[..], [DiffEffect::SaveComment(CommentOp::Delete { reply: Some(_), .. })]), "{effects:?}");
    assert!(d.s.comments()[0].replies.is_empty());
    d.press("d");
    assert!(d.s.comments().is_empty());
    assert!(!d.s.table().rows.iter().any(|r| matches!(r, Row::Thread { .. })));
}

#[test]
fn a_comment_takes_capitals_however_the_client_spells_them() {
    let mut d = Driver::new();
    d.goto_text("new thing");
    d.press("enter");
    d.s.key(KeyChord::new(Key::Char('W'), Mods::NONE), &d.km);
    d.s.key(KeyChord::new(Key::Char('h'), Mods::SHIFT), &d.km);
    d.s.key(KeyChord::new(Key::Char('?'), Mods::SHIFT), &d.km);
    d.press("enter");
    assert_eq!(d.s.comments()[0].body, "WH?");
}

#[test]
fn comment_on_a_deleted_line_anchors_to_the_old_side() {
    let mut d = Driver::new();
    d.goto_text("old thing");
    d.press("enter");
    d.typed("x");
    d.press("enter");
    let anchor = &d.s.comments()[0].anchor;
    assert_eq!((anchor.side, anchor.start), (super::model::Side::Old, 3));
}

#[test]
fn nothing_to_comment_on_says_so() {
    let mut d = Driver::new();
    d.press("enter");
    assert!(matches!(d.s.mode(), Mode::Normal));
    assert!(d.s.flash().is_some());
}

#[test]
fn visual_selection_yanks_or_comments_on_the_range() {
    let mut d = Driver::new();
    d.goto_text("new thing");
    d.press("v");
    assert!(matches!(d.s.mode(), Mode::Visual { .. }));
    d.press("j");
    assert_eq!(d.s.selection(), Some((d.s.cursor() - 1, d.s.cursor())));
    let effects = d.press("y");
    assert_eq!(effects, [DiffEffect::Yank("new thing\nanother thing".into())]);
    assert!(matches!(d.s.mode(), Mode::Normal));

    d.goto_text("new thing");
    d.press("v j enter");
    d.typed("range");
    d.press("enter");
    let anchor = &d.s.comments()[0].anchor;
    assert_eq!((anchor.start, anchor.end), (3, 4));
    assert_eq!(anchor.code, ["new thing", "another thing"]);

    d.goto_text("keep");
    d.press("v esc");
    assert!(matches!(d.s.mode(), Mode::Normal));
    d.press("v v");
    assert!(matches!(d.s.mode(), Mode::Normal));
}

#[test]
fn a_range_across_hunks_is_refused() {
    let mut d = Driver::new();
    d.goto_text("last");
    d.press("v j j enter");
    assert!(matches!(d.s.mode(), Mode::Normal));
    assert!(d.s.flash().unwrap().contains("one hunk"));
}

#[test]
fn comment_motion_yank_and_send() {
    let mut d = Driver::new();
    for line in ["new thing", "foo again"] {
        d.goto_text(line);
        d.press("enter");
        d.typed("note");
        d.press("enter");
    }
    d.press("g g ] c");
    assert!(matches!(d.row(), Row::Thread { thread: 0, part: ThreadPart::Head, .. }));
    d.press("] c");
    assert!(matches!(d.row(), Row::Thread { thread: 1, part: ThreadPart::Head, .. }));
    d.press("[ c");
    assert!(matches!(d.row(), Row::Thread { thread: 0, .. }));

    let effects = d.press("y");
    assert!(matches!(&effects[..], [DiffEffect::Yank(text)] if text.starts_with("a.rs:3\n")), "{effects:?}");
    let effects = d.press("Y");
    assert!(matches!(&effects[..], [DiffEffect::Yank(text)] if text.contains("a.rs:3") && text.contains("b.rs:1")));

    let effects = d.press("g Y");
    let [DiffEffect::SendToAgent { message, ids }] = effects.as_slice() else { panic!("{effects:?}") };
    assert!(message.contains("```\nfoo again\n```"));
    assert_eq!(ids.len(), 2);
    let ids = ids.clone();
    let effects = d.s.mark_sent(ids);
    assert!(matches!(&effects[..], [DiffEffect::SaveComment(CommentOp::MarkSent { .. })]));
    assert!(d.press("g Y").is_empty(), "everything's been sent");
    assert_eq!(d.s.flash(), Some("No comments to send"));
}

#[test]
fn clearing_every_comment_takes_two_presses_in_a_row() {
    let mut d = Driver::new();
    assert!(d.press("D").is_empty());
    assert!(d.s.flash().is_some());
    d.goto_text("new thing");
    d.press("enter");
    d.typed("x");
    d.press("enter");
    assert!(d.press("D").is_empty());
    assert!(d.s.flash().unwrap().contains("Press D again"));
    d.press("j");
    assert!(d.press("D").is_empty(), "another key in between starts over");
    assert_eq!(d.s.comments().len(), 1);
    let effects = d.press("D");
    assert_eq!(effects, [DiffEffect::SaveComment(CommentOp::Clear)]);
    assert!(d.s.comments().is_empty());
}

#[test]
fn r_refreshes_off_a_comment() {
    let mut d = Driver::new();
    assert_eq!(d.press("r"), [DiffEffect::Refresh]);
}

#[test]
fn ctrl_g_opens_the_file_at_the_line() {
    let mut d = Driver::new();
    d.goto_text("new thing");
    assert_eq!(d.press("ctrl-g"), [DiffEffect::OpenInEditor { path: "a.rs".into(), line: Some(3) }]);
    d.goto_text("old thing");
    assert_eq!(d.press("ctrl-g"), [DiffEffect::OpenInEditor { path: "a.rs".into(), line: Some(3) }]);
    d.press("g g");
    assert_eq!(d.press("ctrl-g"), [DiffEffect::OpenInEditor { path: "a.rs".into(), line: None }]);
}

#[test]
fn viewed_marks_clear_when_the_file_changes() {
    let mut d = Driver::new();
    let effects = d.press("m");
    assert!(matches!(&effects[..], [DiffEffect::SaveViewed(ViewedOp::Mark { path, .. })] if path == "a.rs"));
    assert!(d.s.is_viewed(&d.s.changeset().files[0].clone()));
    assert!(!d.s.is_viewed(&d.s.changeset().files[1].clone()));

    let edited = PATCH.replace("+new thing", "+newer thing");
    d.s.set_changeset(changeset(&edited));
    assert!(!d.s.is_viewed(&d.s.changeset().files[0].clone()), "it changed again");

    let effects = d.press("m");
    assert!(matches!(&effects[..], [DiffEffect::SaveViewed(ViewedOp::Mark { .. })]));
    let effects = d.press("m");
    assert!(matches!(&effects[..], [DiffEffect::SaveViewed(ViewedOp::Unmark { .. })]));
}

#[test]
fn a_refresh_keeps_the_cursor_folds_and_comments() {
    let mut d = Driver::new();
    d.goto_text("new thing");
    d.press("enter");
    d.typed("note");
    d.press("enter");
    d.press("l z C");

    // Lines above it landed; the file list grew.
    let moved = format!(
        "diff --git a/0.rs b/0.rs\n--- a/0.rs\n+++ b/0.rs\n@@ -1 +1 @@\n-x\n+y\n{}",
        PATCH.replace("@@ -1,6 +1,7 @@", "@@ -10,6 +10,7 @@")
    );
    d.s.goto_file(0);
    d.goto_text("keep");
    d.s.set_changeset(changeset(&moved));
    assert_eq!(d.text(), "keep", "the cursor is on the same text");
    assert_eq!(d.s.current_file(), Some(1));
    assert!(d.s.folds().file_folded("b.rs"), "folds go by path");
    assert!(d.s.table().rows.iter().any(|r| matches!(r, Row::Thread { .. })), "the thread found its line");
    let thread = d.s.comments()[0].id.clone();
    assert_eq!(d.s.comments()[0].anchor.end, 3, "its stored anchor isn't rewritten");
    assert!(!d.s.thread_expanded(&thread));

    // Its line is gone: outdated, at the top of its file, and still there.
    d.s.set_changeset(changeset(&moved.replace("+new thing", "+rewritten")));
    assert_eq!(d.s.comments().len(), 1);
    let row = d.s.table().rows.iter().position(|r| matches!(r, Row::Thread { .. })).unwrap();
    assert_eq!(d.s.table().rows[row - 1], Row::File { file: 1, folded: false });
    assert!(d.s.thread_outdated(0));
}

#[test]
fn a_comment_from_another_client_arrives_without_moving_the_cursor() {
    let mut d = Driver::new();
    d.goto_text("keep");
    d.press("g g");
    d.goto_text("new thing");
    d.press("enter");
    d.typed("mine");
    d.press("enter");
    let mine = d.s.comments()[0].clone();
    d.goto_text("keep");
    let mut other = mine.clone();
    other.id = "theirs".into();
    other.anchor.start = 2;
    other.anchor.end = 2;
    other.anchor.code = vec![String::new()];
    let doc = CommentDoc { comments: vec![mine, other] };
    d.s.set_comments(doc);
    assert_eq!(d.text(), "keep");
    assert_eq!(d.s.comments().len(), 2);
}

#[test]
fn the_file_palette_jumps_and_switches_to_commands() {
    let mut d = Driver::new();
    d.press("ctrl-p");
    let Mode::Palette(p) = d.s.mode() else { panic!() };
    assert_eq!(p.mode, PaletteMode::Files);
    d.typed("b.rs");
    d.press("enter");
    assert_eq!(d.row(), Row::File { file: 1, folded: false });
    assert!(matches!(d.s.mode(), Mode::Normal));

    d.press("ctrl-p");
    d.typed(">");
    let Mode::Palette(p) = d.s.mode() else { panic!() };
    assert_eq!(p.mode, PaletteMode::Commands);
    d.typed("Refresh");
    assert_eq!(d.press("enter"), [DiffEffect::Refresh]);

    d.press(":");
    let Mode::Palette(p) = d.s.mode() else { panic!() };
    assert_eq!(p.mode, PaletteMode::Commands);
    d.typed("Unified");
    d.press("enter");
    assert_eq!(d.s.view(), ViewMode::Split);
    d.press(": esc");
    assert!(matches!(d.s.mode(), Mode::Normal));
}

#[test]
fn the_commit_picker_loads_a_scope_and_blame_asks_for_the_file() {
    let mut d = Driver::new();
    d.press("c");
    d.typed("abcdef1");
    assert_eq!(d.press("enter"), [DiffEffect::LoadScope(CommitScope::Commit("abcdef1".into()))]);
    assert!(d.s.loading());

    let effects = d.press("B");
    assert_eq!(effects, [DiffEffect::LoadBlame { path: "a.rs".into(), rev: None }]);
    d.s.set_blame("a.rs".into(), Vec::new());
    assert_eq!(d.s.blame_request(), None, "it has it");
    d.press("l");
    assert_eq!(d.s.blame_request(), Some(DiffEffect::LoadBlame { path: "b.rs".into(), rev: None }));
    d.press("B");
    assert_eq!(d.s.blame_request(), None, "off");
}

#[test]
fn help_opens_and_any_other_key_closes_it() {
    let mut d = Driver::new();
    d.press("?");
    assert!(matches!(d.s.mode(), Mode::Help { scroll: 0 }));
    d.press("j");
    assert!(matches!(d.s.mode(), Mode::Help { scroll: 1 }));
    d.press("q");
    assert!(matches!(d.s.mode(), Mode::Normal));
}

#[test]
fn keys_come_from_the_keymap_not_the_code() {
    let (km, diags) = validate_keymaps_yaml(
        "defaults:\n  diff:\n    cursor_down: e\n    top: 'x y'\n    search: ctrl-s\n    next_match: '+'\n    next_change: ', h'\n",
        None,
    );
    assert!(diags.is_empty(), "{diags:?}");
    let mut d = Driver::new();
    d.km = km.diff;
    d.press("j");
    assert_eq!(d.s.cursor(), 0, "j isn't bound any more");
    d.press("e e");
    assert_eq!(d.s.cursor(), 2);
    d.press("x y");
    assert_eq!(d.s.cursor(), 0);
    d.press("ctrl-s");
    assert!(matches!(d.s.mode(), Mode::Search { .. }));
    d.press("esc , h");
    assert_eq!(d.text(), "old thing");
    assert_eq!(FileStatus::Modified, d.s.changeset().files[0].status);
}

#[test]
fn key_spellings_normalize() {
    let g = |code, mods| normalize_key(KeyChord::new(code, mods));
    assert_eq!(g(Key::Char('G'), Mods::NONE), KeyChord::parse("G").unwrap());
    assert_eq!(g(Key::Char('?'), Mods::SHIFT), KeyChord::parse("?").unwrap());
    assert_eq!(g(Key::Char('}'), Mods::SHIFT), KeyChord::parse("}").unwrap());
    assert_eq!(g(Key::Char('j'), Mods::CONTROL), KeyChord::parse("ctrl-j").unwrap());
    let mut d = Driver::new();
    d.s.key(KeyChord::new(Key::Char('G'), Mods::NONE), &d.km);
    assert_eq!(d.s.cursor(), d.s.table().len() - 1, "a terminal's bare capital");
}

#[test]
fn the_status_line_reads_like_the_wireframe() {
    let mut d = Driver::new();
    assert_eq!(d.s.status(), "NORMAL · unified · all lines · 1/3 · ? help");
    d.press("s tab l");
    assert_eq!(d.s.status(), "NORMAL · split · additions · 2/3 · ? help");
}

#[test]
fn an_empty_changeset_is_safe_to_drive() {
    let mut s = DiffState::new(Author::human("j"));
    s.set_changeset(Arc::new(Changeset::default()));
    let km = keymap();
    for action in [
        "j", "k", "h", "l", "space", "b", "G", "M", "enter", "r", "o", "d", "D", "y", "Y", "m", "B", "s", "tab",
        "ctrl-g", "n", "N", "v", "ctrl-d", "]", "h", "z", "M", "z", "a", "f", "x", ";",
    ] {
        s.key(KeyChord::parse(action).unwrap(), &km);
    }
    assert!(s.visible_rows().is_empty());
    assert_eq!(s.current_file(), None);
}

#[test]
fn clicks_and_the_file_list_drive_the_same_state() {
    let mut d = Driver::new();
    d.s.click_row(4);
    assert_eq!(d.s.cursor(), 4);
    d.s.click_row(10_000);
    assert_eq!(d.s.cursor(), 4, "off the table");
    d.s.goto_file(2);
    assert_eq!(d.row(), Row::File { file: 2, folded: false });
    assert!(d.s.file_list_open());
    d.press("\\");
    assert!(!d.s.file_list_open());
    d.s.set_file_list(true);
    let effects = d.s.set_viewed(1, true);
    assert!(matches!(&effects[..], [DiffEffect::SaveViewed(ViewedOp::Mark { path, .. })] if path == "b.rs"));
    assert_eq!(d.s.apply(DiffAction::Top, None), []);
    assert_eq!(d.s.cursor(), 0);
}

#[test]
fn rendering_only_touches_the_rows_in_view() {
    let mut d = Driver::new();
    d.s.set_viewport(3);
    d.press("G");
    assert_eq!(d.s.visible_rows().len(), 3);
    assert_eq!(d.s.visible_rows()[2], d.row());
}
