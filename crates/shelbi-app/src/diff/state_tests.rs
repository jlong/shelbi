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
    d.press("b");
    d.press("page-up");
    assert_eq!(d.s.cursor(), 0);
    // As in Skim with its sidebar open, Ctrl-b is the file list's and paging
    // up stays on `b` and PageUp.
    assert!(d.s.file_list_open());
    d.press("ctrl-b");
    assert!(!d.s.file_list_open());
    assert_eq!(d.s.cursor(), 0);
    d.press("\\");
    assert!(d.s.file_list_open());
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
        "defaults:\n  diff:\n    cursor_down: e\n    top: 'q y'\n    search: ctrl-s\n    next_match: '+'\n    next_change: ', h'\n",
        None,
    );
    assert!(diags.is_empty(), "{diags:?}");
    let mut d = Driver::new();
    d.km = km.diff;
    d.press("j");
    assert_eq!(d.s.cursor(), 0, "j isn't bound any more");
    d.press("e e");
    assert_eq!(d.s.cursor(), 2);
    d.press("q y");
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

#[test]
fn shift_tab_reads_the_same_however_a_client_reports_it() {
    let mut d = Driver::new();
    d.press("tab");
    assert_eq!(d.s.filter(), HunkFilter::Additions);
    d.s.key(KeyChord::new(Key::BackTab, Mods::SHIFT), &d.km);
    assert_eq!(d.s.filter(), HunkFilter::All);
}

#[test]
fn it_wants_what_it_would_act_on_and_leaves_the_rest() {
    let mut d = Driver::new();
    let km = keymap();
    let chord = KeyChord::new;
    let wants = |d: &Driver, c| d.s.wants_key(c, &km);
    assert!(wants(&d, chord(Key::Char('j'), Mods::NONE)));
    assert!(wants(&d, chord(Key::Char('p'), Mods::CONTROL)), "the file palette");
    assert!(wants(&d, chord(Key::Char('g'), Mods::NONE)), "a two-key binding's first");
    assert!(wants(&d, chord(Key::Char('3'), Mods::NONE)), "a count");
    assert!(wants(&d, chord(Key::BackTab, Mods::SHIFT)));
    assert!(!wants(&d, chord(Key::Char('k'), Mods::SUPER)));
    assert!(!wants(&d, chord(Key::Char(' '), Mods::CONTROL)));
    assert!(!wants(&d, chord(Key::Esc, Mods::NONE)), "nothing's held");
    // Held, the next key is the state's whatever it is.
    d.press("z");
    assert!(wants(&d, chord(Key::Char('q'), Mods::NONE)));
    assert!(wants(&d, chord(Key::Esc, Mods::NONE)));
    d.press("esc");
    // A modal mode holds the keyboard, short of the window's Cmd chords.
    d.press("/");
    assert!(d.s.takes_text());
    assert!(wants(&d, chord(Key::Char('x'), Mods::NONE)));
    assert!(!wants(&d, chord(Key::Char('1'), Mods::SUPER)));
}

#[test]
fn a_palette_click_chooses_its_row() {
    let mut d = Driver::new();
    d.press("ctrl-p");
    d.s.palette_choose(1);
    assert_eq!(d.s.mode().label(), "NORMAL");
    assert_eq!(d.s.current_file(), Some(1));
}

#[test]
fn the_scope_label_names_what_is_shown() {
    let mut d = Driver::new();
    assert_eq!(d.s.scope_label(), "all");
    let mut cs = (**d.s.changeset()).clone();
    cs.scope = CommitScope::Uncommitted;
    d.s.set_changeset(Arc::new(cs.clone()));
    assert_eq!(d.s.scope_label(), "uncommitted");
    cs.scope = CommitScope::Commit("abcdef1234".into());
    d.s.set_changeset(Arc::new(cs.clone()));
    assert_eq!(d.s.scope_label(), "abcdef1", "not in the list: the sha's start");
    cs.scope = CommitScope::Commit("abcdef1".into());
    d.s.set_changeset(Arc::new(cs));
    assert_eq!(d.s.scope_label(), "abcdef1");
}

// ---- The pull request ------------------------------------------------------

mod pr {
    use std::sync::Arc;

    use chrono::DateTime;
    use shelbi_core::{IssuePr, PrState};
    use shelbi_state::keymap::{Key, KeyChord, Mods};

    use super::Driver;
    use crate::diff::draft::DraftTarget;
    use crate::diff::model::Side;
    use crate::diff::pr::{
        CheckState, PrCheck, PrDetails, PrPart, PrReview, PrThread, PrThreadComment, PrTone, PrWrite, ReviewEvent,
        ReviewState,
    };
    use crate::diff::rows::{Row, ThreadPart};
    use crate::diff::state::{DiffEffect, Mode};
    use crate::diff::comments::CommentOp;

    fn said(author: &str, body: &str) -> PrThreadComment {
        PrThreadComment {
            id: format!("c-{author}"),
            author: author.into(),
            body: body.into(),
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
            pending: false,
        }
    }

    fn thread(id: &str, path: &str, line: u32, code: &str) -> PrThread {
        PrThread {
            id: id.into(),
            path: path.into(),
            side: Side::New,
            line: Some(line),
            original_line: Some(line),
            start_line: None,
            code: Some(code.into()),
            resolved: false,
            outdated: false,
            file_level: false,
            comments: vec![said("bob", "this leaks on early return"), said("jlong", "fixed in 3f2a1c")],
        }
    }

    fn details() -> PrDetails {
        PrDetails {
            number: 2155,
            title: "Record a task's PR".into(),
            url: "https://github.com/jlong/shelbi/pull/2155".into(),
            body: "Why.\n\nAnd how.".into(),
            author: "jlong".into(),
            head: "jlong/task-pr-metadata".into(),
            base: "main".into(),
            state: PrState::Open,
            draft: false,
            reviews: vec![PrReview { author: "alice".into(), state: ReviewState::Approved, body: String::new() }],
            checks: vec![PrCheck { name: "lint".into(), state: CheckState::Failed, detail: "failure".into() }],
            threads: vec![thread("T1", "a.rs", 4, "new thing")],
            threads_cut: false,
        }
    }

    fn with_pr() -> Driver {
        let mut d = Driver::new();
        d.s.set_pr_lookup(Ok(Some(IssuePr::new(2155, None))));
        d.s.set_pr_details(Ok(Arc::new(details())));
        d
    }

    fn head_row(d: &Driver) -> usize {
        d.s.table()
            .rows
            .iter()
            .position(|r| matches!(r, Row::Thread { part: ThreadPart::Head, .. }))
            .expect("a thread's head row")
    }

    fn to_head(d: &mut Driver) {
        let row = head_row(d);
        d.s.click_row(row);
    }

    fn text_of(d: &Driver, part: PrPart) -> String {
        d.s.pr_row(part).iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn the_block_sits_above_the_first_file_and_file_rows_still_point_at_files() {
        let d = with_pr();
        let rows = &d.s.table().rows;
        assert_eq!(rows[0], Row::Pr { part: PrPart::Title });
        assert_eq!(rows[1], Row::Pr { part: PrPart::Status });
        assert_eq!(rows[2], Row::Pr { part: PrPart::Gap });
        assert_eq!(rows[3], Row::Pr { part: PrPart::Body(0) });
        let first = d.s.table().file_rows[0] as usize;
        assert!(matches!(rows[first], Row::File { file: 0, .. }));
        assert!(matches!(rows[d.s.table().file_rows[1] as usize], Row::File { file: 1, .. }));
    }

    #[test]
    fn no_pr_means_no_rows_and_the_keys_say_so() {
        let mut d = Driver::new();
        assert!(!d.s.table().rows.iter().any(|r| matches!(r, Row::Pr { .. })));
        for key in ["x", "shift-c", "shift-r", "i"] {
            assert_eq!(d.press(key), []);
            assert_eq!(d.s.mode().label(), "NORMAL", "{key}");
            assert!(d.s.flash().is_some(), "{key}");
        }
        assert!(!d.s.comment_on_github());
    }

    #[test]
    fn an_empty_changeset_has_no_block_so_the_client_says_its_empty() {
        let mut d = Driver::new();
        d.s.set_changeset(super::changeset(""));
        d.s.set_pr_details(Ok(Arc::new(details())));
        assert!(d.s.table().is_empty());
    }

    #[test]
    fn the_block_arriving_doesnt_move_the_cursor_off_its_line() {
        let mut d = Driver::new();
        d.goto_text("foo again");
        let top_offset = d.s.cursor() - d.s.top();
        d.s.set_pr_lookup(Ok(Some(IssuePr::new(2155, None))));
        d.s.set_pr_details(Ok(Arc::new(details())));
        assert_eq!(d.text(), "foo again");
        assert_eq!(d.s.cursor() - d.s.top(), top_offset, "same height in the view");
    }

    #[test]
    fn o_folds_the_block_from_any_of_its_rows() {
        let mut d = with_pr();
        d.s.click_row(4);
        assert_eq!(d.row(), Row::Pr { part: PrPart::Body(1) });
        d.press("o");
        assert!(d.s.pr().folded);
        assert_eq!(d.s.table().rows[2], Row::File { file: 0, folded: false });
        assert_eq!(d.row(), Row::Pr { part: PrPart::Title }, "its row is gone: the title's left");
        d.press("o");
        assert!(!d.s.pr().folded);
        assert_eq!(d.s.table().rows[2], Row::Pr { part: PrPart::Gap });
    }

    #[test]
    fn stepping_off_the_block_lands_on_the_first_file_not_the_second() {
        let mut d = with_pr();
        assert_eq!(d.s.current_file(), None, "the block is in no file");
        assert!(d.s.status().contains(" · 3 files · "), "{}", d.s.status());
        d.press("l");
        assert_eq!(d.s.current_file(), Some(0));
        assert!(matches!(d.row(), Row::File { file: 0, .. }));
        d.press("l");
        assert_eq!(d.s.current_file(), Some(1));
    }

    #[test]
    fn keys_about_a_file_do_nothing_on_the_block() {
        let mut d = with_pr();
        assert_eq!(d.press("ctrl-g"), []);
        assert_eq!(d.press("m"), []);
        d.press("z shift-c");
        assert!(d.s.folds().files.is_empty());
        d.press("enter");
        assert_eq!(d.s.mode().label(), "NORMAL");
        assert!(d.s.flash().is_some());
    }

    #[test]
    fn a_thread_shows_at_its_line_open_with_its_tag() {
        let d = with_pr();
        let head = head_row(&d);
        let Row::Line { file, hunk, line } = d.s.table().rows[head - 1] else { panic!("{:?}", d.s.table().rows[head - 1]) };
        assert_eq!(d.s.line(file, hunk, line).unwrap().text, "new thing");
        let Row::Thread { thread, part: ThreadPart::Head, .. } = d.s.table().rows[head] else { panic!() };
        assert_eq!(d.s.thread_row_text(thread, ThreadPart::Head).unwrap(), "⎇ bob (GitHub): this leaks on early return");
        assert_eq!(d.s.thread_row_text(thread, ThreadPart::Reply(0)).unwrap(), "  ↳ jlong: fixed in 3f2a1c");
        assert!(matches!(d.s.table().rows[head + 1], Row::Thread { part: ThreadPart::Reply(0), .. }), "open by default");
        assert_eq!(d.s.thread_tag(thread), Some(("[unresolved]", PrTone::Pending)));
    }

    #[test]
    fn a_resolved_thread_starts_closed_and_o_opens_it() {
        let mut d = Driver::new();
        let mut pull = details();
        pull.threads[0].resolved = true;
        d.s.set_pr_details(Ok(Arc::new(pull)));
        to_head(&mut d);
        let head = d.s.cursor();
        assert!(matches!(d.s.table().rows[head + 1], Row::Thread { part: ThreadPart::Summary, .. }));
        d.press("o");
        assert!(matches!(d.s.table().rows[head + 1], Row::Thread { part: ThreadPart::Reply(0), .. }));
        d.press("o");
        assert!(matches!(d.s.table().rows[head + 1], Row::Thread { part: ThreadPart::Summary, .. }));
    }

    #[test]
    fn comment_keys_jump_to_github_threads_too() {
        let mut d = with_pr();
        d.press("] c");
        assert!(matches!(d.row(), Row::Thread { part: ThreadPart::Head, .. }));
    }

    #[test]
    fn local_and_github_threads_sit_side_by_side_and_keep_their_own_keys() {
        let mut d = with_pr();
        d.goto_text("foo again");
        d.press("enter");
        d.typed("mine");
        let effects = d.press("enter");
        assert!(matches!(&effects[..], [DiffEffect::SaveComment(CommentOp::Add(_))]), "{effects:?}");
        assert_eq!(d.s.comments().len(), 1, "GitHub's aren't the task's");
        // Enter edits the task's, and answers GitHub's.
        d.press("enter");
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert!(matches!(draft.target, DraftTarget::Edit { .. }));
        d.press("esc esc");
        to_head(&mut d);
        d.press("enter");
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert_eq!(draft.target, DraftTarget::GithubReply { thread: "T1".into() });
    }

    #[test]
    fn enter_and_r_reply_on_github() {
        for key in ["enter", "r"] {
            let mut d = with_pr();
            to_head(&mut d);
            d.press(key);
            d.typed("on it");
            let effects = d.press("enter");
            assert_eq!(effects, [DiffEffect::PrWrite(PrWrite::Reply { thread: "T1".into(), body: "on it".into() })]);
            assert_eq!(d.s.mode().label(), "NORMAL");
            assert_eq!(d.s.flash(), Some("Replying on GitHub…"));
        }
    }

    #[test]
    fn x_resolves_and_reopens_the_thread_here() {
        let mut d = with_pr();
        to_head(&mut d);
        assert_eq!(d.press("x"), [DiffEffect::PrWrite(PrWrite::Resolve { thread: "T1".into(), resolved: true })]);
        let mut pull = details();
        pull.threads[0].resolved = true;
        d.s.set_pr_details(Ok(Arc::new(pull)));
        assert_eq!(d.press("x"), [DiffEffect::PrWrite(PrWrite::Resolve { thread: "T1".into(), resolved: false })]);
    }

    #[test]
    fn x_wants_a_github_thread() {
        let mut d = with_pr();
        d.goto_text("foo again");
        d.press("enter");
        d.typed("mine");
        d.press("enter");
        assert_eq!(d.press("x"), []);
        assert_eq!(d.s.flash(), Some("Move onto a GitHub thread to resolve it"));
    }

    #[test]
    fn a_github_thread_cant_be_deleted_from_here() {
        let mut d = with_pr();
        to_head(&mut d);
        assert_eq!(d.press("d"), []);
        assert!(d.s.flash().unwrap().contains("GitHub's"));
        assert_eq!(d.s.table().rows[head_row(&d)], d.row());
    }

    #[test]
    fn y_copies_a_github_thread() {
        let mut d = with_pr();
        to_head(&mut d);
        let effects = d.press("y");
        let [DiffEffect::Yank(text)] = &effects[..] else { panic!("{effects:?}") };
        assert!(text.contains("⎇ bob (GitHub): this leaks") && text.contains("↳ jlong: fixed"), "{text}");
    }

    #[test]
    fn gy_sends_only_the_tasks_own_comments() {
        let mut d = with_pr();
        assert_eq!(d.press("g shift-y"), []);
        assert_eq!(d.s.flash(), Some("No comments to send"));
    }

    #[test]
    fn c_sends_new_comments_to_the_pending_review_and_back() {
        let mut d = with_pr();
        d.press("shift-c");
        assert!(d.s.comment_on_github());
        assert!(d.s.status().contains("new comments go to GitHub"), "{}", d.s.status());
        d.goto_text("foo again");
        d.press("enter");
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert!(matches!(draft.target, DraftTarget::GithubNew(_)));
        d.typed("look");
        let effects = d.press("enter");
        let [DiffEffect::PrWrite(PrWrite::AddThread { anchor, body })] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!((anchor.path.as_str(), anchor.side, anchor.end, body.as_str()), ("b.rs", Side::New, 1, "look"));
        assert!(d.s.comments().is_empty(), "nothing kept locally unless GitHub refuses");

        d.press("shift-c");
        assert!(!d.s.comment_on_github());
        d.press("enter");
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert!(matches!(draft.target, DraftTarget::New(_)));
    }

    #[test]
    fn a_comment_github_refuses_stays_as_a_local_one() {
        let mut d = with_pr();
        d.press("shift-c");
        d.goto_text("foo again");
        d.press("enter");
        d.typed("look");
        let effects = d.press("enter");
        let [DiffEffect::PrWrite(change)] = &effects[..] else { panic!("{effects:?}") };
        let change = change.clone();
        let effects = d.s.pr_written(change, Err("line must be part of the diff".to_string()));
        assert!(matches!(&effects[..], [DiffEffect::SaveComment(CommentOp::Add(_))]), "{effects:?}");
        assert_eq!(d.s.comments()[0].body, "look");
        assert_eq!(d.s.flash(), Some("Kept as a local comment: line must be part of the diff"));
    }

    #[test]
    fn a_reply_github_refuses_goes_back_to_its_editor() {
        let mut d = with_pr();
        let change = PrWrite::Reply { thread: "T1".into(), body: "my words".into() };
        assert_eq!(d.s.pr_written(change, Err("no".to_string())), []);
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert_eq!(draft.text(), "my words");
        assert_eq!(draft.target, DraftTarget::GithubReply { thread: "T1".into() });
        assert_eq!(d.s.flash(), Some("GitHub didn't take that: no"));
    }

    #[test]
    fn a_review_refused_while_typing_elsewhere_keeps_the_words_on_the_clipboard() {
        let mut d = with_pr();
        d.press("/");
        let change = PrWrite::SubmitReview { event: ReviewEvent::Approve, body: "lgtm".into() };
        assert_eq!(d.s.pr_written(change, Err("no".to_string())), [DiffEffect::Yank("lgtm".into())]);
        assert_eq!(d.s.mode().label(), "SEARCH", "the keyboard stays where it was");
    }

    #[test]
    fn a_change_that_landed_says_so() {
        let mut d = with_pr();
        let change = PrWrite::Resolve { thread: "T1".into(), resolved: true };
        assert_eq!(d.s.pr_written(change, Ok("Resolved the thread on GitHub".to_string())), []);
        assert_eq!(d.s.flash(), Some("Resolved the thread on GitHub"));
    }

    #[test]
    fn r_opens_the_review_and_tab_picks_the_verdict() {
        let mut d = with_pr();
        d.press("shift-r");
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert_eq!(draft.target, DraftTarget::Review { event: ReviewEvent::Comment });
        d.s.key(KeyChord::new(Key::Tab, Mods::NONE), &d.km);
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert_eq!(draft.target, DraftTarget::Review { event: ReviewEvent::Approve });
        d.s.key(KeyChord::new(Key::BackTab, Mods::NONE), &d.km);
        d.s.key(KeyChord::new(Key::BackTab, Mods::NONE), &d.km);
        let Mode::Comment(draft) = d.s.mode() else { panic!() };
        assert_eq!(draft.target, DraftTarget::Review { event: ReviewEvent::RequestChanges });
    }

    #[test]
    fn a_comment_or_a_request_needs_words_and_an_approval_doesnt() {
        let mut d = with_pr();
        d.press("shift-r");
        assert_eq!(d.press("enter"), []);
        assert_eq!(d.s.mode().label(), "COMMENT", "still open");
        assert!(d.s.flash().unwrap().contains("needs some words"));
        d.s.key(KeyChord::new(Key::Tab, Mods::NONE), &d.km);
        assert_eq!(
            d.press("enter"),
            [DiffEffect::PrWrite(PrWrite::SubmitReview { event: ReviewEvent::Approve, body: String::new() })]
        );
        assert_eq!(d.s.mode().label(), "NORMAL");

        d.press("shift-r");
        d.typed("looks wrong");
        assert_eq!(
            d.press("enter"),
            [DiffEffect::PrWrite(PrWrite::SubmitReview { event: ReviewEvent::Comment, body: "looks wrong".into() })]
        );
    }

    #[test]
    fn with_github_silent_the_writing_keys_say_why() {
        let mut d = Driver::new();
        d.s.set_pr_lookup(Ok(Some(IssuePr::new(2155, None))));
        d.press("shift-r");
        assert_eq!(d.s.flash(), Some("Still reading the pull request"));
        d.s.set_pr_details(Err("Can't reach GitHub".to_string()));
        d.press("shift-c");
        assert_eq!(d.s.flash(), Some("The pull request isn't here: Can't reach GitHub"));
        assert!(!d.s.comment_on_github());
        assert_eq!(d.s.mode().label(), "NORMAL");
        assert_eq!(
            text_of(&d, PrPart::Status),
            "PR details aren't here: Can't reach GitHub",
            "and the diff is still here"
        );
        assert!(d.s.table().rows.iter().any(|r| matches!(r, Row::Line { .. })));
    }

    #[test]
    fn i_opens_the_panel_and_its_keys_scroll_refresh_and_close() {
        let mut d = with_pr();
        d.s.set_pr_info_height(3);
        d.press("i");
        assert!(matches!(d.s.mode(), Mode::PrInfo { scroll: 0 }));
        let last = d.s.pr_info_rows().len() - 3;
        d.press("j j");
        assert!(matches!(d.s.mode(), Mode::PrInfo { scroll: 2 }));
        d.press("k");
        assert!(matches!(d.s.mode(), Mode::PrInfo { scroll: 1 }));
        d.press("shift-g");
        assert!(matches!(d.s.mode(), Mode::PrInfo { scroll } if *scroll == last));
        d.press("j");
        assert!(matches!(d.s.mode(), Mode::PrInfo { scroll } if *scroll == last), "stops at the end");
        d.press("g");
        assert!(matches!(d.s.mode(), Mode::PrInfo { scroll: 0 }));
        assert_eq!(d.press("r"), [DiffEffect::Refresh]);
        assert_eq!(d.s.mode().label(), "PR INFO");
        for close in ["i", "q", "esc"] {
            d.s.apply(shelbi_state::keymap::DiffAction::PrInfo, None);
            d.press(close);
            assert_eq!(d.s.mode().label(), "NORMAL", "{close}");
        }
    }

    #[test]
    fn the_panel_opens_to_say_why_when_all_there_is_is_a_reason() {
        let mut d = Driver::new();
        d.s.set_pr_lookup(Err("Can't reach GitHub".to_string()));
        d.press("i");
        assert_eq!(d.s.mode().label(), "PR INFO");
        let rows = d.s.pr_info_rows();
        assert_eq!(rows[0][0].text, "Not current: Can't reach GitHub");
    }

    #[test]
    fn the_pr_going_away_takes_the_block_and_the_target_with_it() {
        let mut d = with_pr();
        d.press("shift-c");
        d.press("j j j");
        d.s.set_pr_lookup(Ok(None));
        assert!(!d.s.comment_on_github());
        assert!(!d.s.table().rows.iter().any(|r| matches!(r, Row::Pr { .. } | Row::Thread { .. })));
        assert!(d.s.cursor() < d.s.table().len());
    }

    #[test]
    fn the_keys_are_in_the_help_and_the_palette() {
        use crate::diff::help::help_sections;
        use crate::diff::palette::{DiffPalette, PaletteMode, PaletteTarget};
        use crate::diff::TargetKind;
        use shelbi_state::keymap::DiffAction;
        let d = Driver::new();
        let sections = help_sections(&d.km, TargetKind::Review, |c| c.canonical());
        let section = sections.iter().find(|s| s.title == "Pull request").unwrap();
        let keys: Vec<&str> = section.rows.iter().map(|r| r.keys[0].as_str()).collect();
        assert_eq!(keys, ["x", "shift-c", "shift-r", "i"]);
        let items = DiffPalette::new(PaletteMode::Commands).for_kind(TargetKind::Review).items(d.s.changeset());
        for action in [DiffAction::ToggleResolved, DiffAction::ToggleCommentTarget, DiffAction::SubmitReview, DiffAction::PrInfo] {
            assert!(items.iter().any(|i| i.target == PaletteTarget::Action(action)), "{action:?}");
        }
    }
}
