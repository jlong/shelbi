//! What the editing operations do to the source.
//!
//! `|` marks the caret in a fixture (and in what a test expects back);
//! `[` ... `]`-free fixtures keep the marker unambiguous, so a selection is
//! set with [`Editor::select`] instead.

use super::*;

/// An editor on `fixture` with the caret where its `|` is.
fn ed(fixture: &str) -> Editor {
    let at = fixture.find('|').expect("a caret");
    let text = fixture.replacen('|', "", 1);
    let mut e = Editor::markdown(&text);
    e.select(at, at);
    e
}

/// The text with the caret drawn in.
fn show(e: &Editor) -> String {
    let mut s = e.text().to_string();
    s.insert(e.head(), '|');
    s
}

fn typed(e: &mut Editor, text: &str) {
    for c in text.chars() {
        e.insert(&c.to_string());
    }
}

/// The row texts the user sees.
fn shown(e: &Editor) -> Vec<String> {
    e.doc().rows.iter().map(|r| r.display.clone()).collect()
}

// ---- no churn -------------------------------------------------------------------

const ROUND_TRIP: &str = "# Title\n\nSome *emphasis*, __strong__, `code`, ~~gone~~, an escaped \\*star\\* and a [link](https://x.dev/a_(b) \"t\").\nA soft-wrapped line\nand its continuation.\n\n## Acceptance criteria\n- [ ] first\n  - [x] nested done\n    1. deep ordered\n- plain item\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n\n<details><summary>more</summary>\n\nhidden\n</details>\n\n> quote\n\n[^1]: a footnote\n\n---\n\n![img](a.png) <kbd>x</kbd> https://bare.url/x\n";

#[test]
fn reading_moving_and_clicking_change_nothing() {
    let mut e = Editor::markdown(ROUND_TRIP);
    let before = e.revision();
    for src in (0..ROUND_TRIP.len()).filter(|i| ROUND_TRIP.is_char_boundary(*i)) {
        e.select(src, src);
    }
    for _ in 0..400 {
        e.move_right(false, false);
    }
    for _ in 0..100 {
        e.move_left(true, true);
    }
    e.select_all();
    let _ = e.selected_markdown();
    e.move_to_edge(false, false);
    assert_eq!(e.text(), ROUND_TRIP);
    assert_eq!(e.revision(), before);
}

#[test]
fn an_edit_touches_only_its_own_bytes() {
    let mut e = Editor::markdown(ROUND_TRIP);
    let at = ROUND_TRIP.find("first").unwrap() + "first".len();
    e.select(at, at);
    typed(&mut e, " item");
    let expected = ROUND_TRIP.replacen("- [ ] first\n", "- [ ] first item\n", 1);
    assert_eq!(e.text(), expected);
    assert!(e.undo());
    assert_eq!(e.text(), ROUND_TRIP);
}

#[test]
fn unsupported_markdown_shows_as_its_source() {
    let e = Editor::markdown(ROUND_TRIP);
    let rows = shown(&e);
    for raw in [
        "| a | b |",
        "|---|---|",
        "<details><summary>more</summary>",
        "[^1]: a footnote",
    ] {
        assert!(rows.iter().any(|r| r == raw), "{raw} missing from {rows:?}");
    }
    assert!(rows
        .iter()
        .any(|r| r.contains("![img](a.png) <kbd>x</kbd>")));
}

// ---- typing --------------------------------------------------------------------------

#[test]
fn typing_lands_in_the_text_not_the_syntax() {
    let mut e = ed("## |Heading");
    typed(&mut e, "A ");
    assert_eq!(show(&e), "## A |Heading");

    // At the end of bold text, typing stays bold.
    let mut e = ed("**bold|** after");
    typed(&mut e, "er");
    assert_eq!(show(&e), "**bolder|** after");

    // A caret placed after the closing delimiter snaps back inside it.
    let mut e = ed("**bold**| after");
    typed(&mut e, "!");
    assert_eq!(show(&e), "**bold!|** after");

    let mut e = ed("- [ ] |task");
    typed(&mut e, "a ");
    assert_eq!(show(&e), "- [ ] a |task");
}

#[test]
fn markdown_typed_in_formats_as_it_goes() {
    let mut e = ed("|");
    typed(&mut e, "# Hi");
    assert_eq!(shown(&e), ["Hi"]);
    assert_eq!(e.doc().rows[0].kind, RowKind::Heading(1));
    let mut e = ed("|");
    typed(&mut e, "a **b** c");
    assert_eq!(shown(&e), ["a b c"]);
}

#[test]
fn a_pasted_body_comes_in_formatted_and_a_title_takes_one_line() {
    let mut e = ed("|");
    e.insert("- [ ] one\r\n- [x] two");
    assert_eq!(e.text(), "- [ ] one\n- [x] two");
    assert_eq!(shown(&e), ["one", "two"]);
    let mut t = Editor::plain("Title");
    t.move_to_edge(true, false);
    t.insert(" and\nmore");
    assert_eq!(t.text(), "Title and more");
    // A title's text is never syntax.
    let t = Editor::plain("# not a heading **x**");
    assert_eq!(t.doc().rows[0].display, "# not a heading **x**");
}

// ---- Enter ------------------------------------------------------------------------------------

#[test]
fn enter_at_the_end_of_a_paragraph_opens_an_empty_one() {
    let mut e = ed("para|");
    e.enter();
    assert_eq!(show(&e), "para\n\n|\n");
    assert_eq!(e.doc().rows.len(), 2);
    assert_eq!(e.doc().rows[1].kind, RowKind::Empty);
    typed(&mut e, "next");
    assert_eq!(show(&e), "para\n\nnext|\n");
    assert_eq!(shown(&e), ["para", "next"]);

    let mut e = ed("para|\n\nafter");
    e.enter();
    typed(&mut e, "x");
    assert_eq!(e.text(), "para\n\nx\n\nafter");

    let mut e = ed("## Head|\n- [ ] a");
    e.enter();
    typed(&mut e, "x");
    assert_eq!(e.text(), "## Head\n\nx\n\n- [ ] a");
}

#[test]
fn enter_inside_a_paragraph_splits_it_keeping_its_formatting() {
    let mut e = ed("one **two| three** four");
    e.enter();
    assert_eq!(show(&e), "one **two**\n\n**|three** four");
    assert_eq!(shown(&e), ["one two", "three four"]);

    let mut e = ed("|para");
    e.enter();
    assert_eq!(show(&e), "\n\n|para");
    assert_eq!(e.doc().rows[0].kind, RowKind::Empty);
}

#[test]
fn enter_in_a_list_continues_it_and_an_empty_item_leaves_it() {
    let mut e = ed("- [x] done|");
    e.enter();
    assert_eq!(show(&e), "- [x] done\n- [ ] |");
    e.enter();
    assert_eq!(show(&e), "- [x] done\n\n|\n");
    typed(&mut e, "after");
    assert_eq!(shown(&e), ["done", "after"]);

    let mut e = ed("1. one|\n2. two");
    e.enter();
    assert_eq!(show(&e), "1. one\n2. |\n2. two");

    let mut e = ed("- a\n  - b|");
    e.enter();
    assert_eq!(show(&e), "- a\n  - b\n  - |");
    e.enter();
    assert_eq!(
        show(&e),
        "- a\n  - b\n- |",
        "an empty nested item outdents first"
    );
}

#[test]
fn enter_in_code_adds_a_line_and_a_new_fence_closes_itself() {
    let mut e = ed("```|");
    e.enter();
    assert_eq!(show(&e), "```\n|\n```");
    typed(&mut e, "  x");
    e.enter();
    assert_eq!(show(&e), "```\n  x\n  |\n```");
}

#[test]
fn shift_enter_breaks_the_line_inside_the_row() {
    let mut e = ed("one|two");
    e.line_break();
    assert_eq!(show(&e), "one\\\n|two");
    assert_eq!(shown(&e), ["one\ntwo"]);
}

// ---- Backspace and Delete ---------------------------------------------------------------------

#[test]
fn backspace_deletes_what_shows_and_drops_emptied_marks() {
    let mut e = ed("a **b|** c");
    e.backspace(false);
    assert_eq!(show(&e), "a | c");
    let mut e = ed(r"price \*|");
    e.backspace(false);
    assert_eq!(show(&e), "price |");
    let mut e = ed("see [x|](https://x.dev) now");
    e.backspace(false);
    assert_eq!(show(&e), "see | now");
    let mut e = ed("caf\u{0065}\u{0301}|");
    e.backspace(false);
    assert_eq!(show(&e), "caf|");
}

#[test]
fn backspace_at_a_rows_start_unformats_then_joins() {
    let mut e = ed("- [ ] |task");
    e.backspace(false);
    assert_eq!(show(&e), "|task");
    let mut e = ed("- a\n- |b\n- c");
    e.backspace(false);
    assert_eq!(show(&e), "- a\n\n|b\n\n- c");
    e.backspace(false);
    assert_eq!(show(&e), "- a|b\n\n- c");

    let mut e = ed("## |Head\ntext");
    e.backspace(false);
    assert_eq!(show(&e), "|Head\n\ntext");

    let mut e = ed("one\n\n|two");
    e.backspace(false);
    assert_eq!(show(&e), "one|two");

    let mut e = ed("- a\n  - |b");
    e.backspace(false);
    assert_eq!(show(&e), "- a\n- |b", "a nested item outdents first");
}

#[test]
fn backspace_in_an_empty_paragraph_removes_it() {
    let mut e = ed("para\n\n|\n");
    e.backspace(false);
    assert_eq!(show(&e), "para|\n");
    let mut e = ed("a\n\n|\n\nb");
    e.backspace(false);
    assert_eq!(show(&e), "a|\n\nb");
}

#[test]
fn delete_at_a_rows_end_joins_the_next() {
    let mut e = ed("one|\n\n**two**");
    e.delete(false);
    assert_eq!(show(&e), "one|**two**");
    let mut e = ed("x|\n\n\n\ny");
    e.delete(false);
    assert_eq!(show(&e), "x|\n\ny");
}

#[test]
fn deleting_a_selection_across_rows_joins_them_and_keeps_marks_balanced() {
    let src = "# Head\n\nab **cd** ef\n\n- [ ] gh\n- [ ] ij";
    let mut e = Editor::markdown(src);
    let a = src.find("cd").unwrap() + 1;
    let b = src.find("ij").unwrap() + 1;
    e.select(a, b);
    e.delete_selection();
    assert_eq!(show(&e), "# Head\n\nab **c|**j");
    assert_eq!(shown(&e), ["Head", "ab cj"]);
}

#[test]
fn word_deletion_and_delete_to_line_start() {
    let mut e = ed("one two three|");
    e.backspace(true);
    assert_eq!(show(&e), "one two |");
    e.delete_back_to(0);
    assert_eq!(show(&e), "|");
}

// ---- formatting commands ---------------------------------------------------------------------

#[test]
fn bold_wraps_a_selection_and_unwraps_it_again() {
    let src = "make this bold";
    let mut e = Editor::markdown(src);
    e.select(5, 14);
    e.toggle_mark(MarkKind::Bold);
    assert_eq!(e.text(), "make **this bold**");
    assert_eq!(e.selected_markdown(), "**this bold**");
    e.toggle_mark(MarkKind::Bold);
    assert_eq!(e.text(), src);

    // Part of a bold span comes out of it.
    let mut e = Editor::markdown("**abcd**");
    e.select(3, 5);
    e.toggle_mark(MarkKind::Bold);
    assert_eq!(e.text(), "**a**bc**d**");

    // A selection's surrounding spaces stay outside.
    let mut e = Editor::markdown("a b c");
    e.select(1, 4);
    e.toggle_mark(MarkKind::Italic);
    assert_eq!(e.text(), "a *b* c");

    let mut e = Editor::markdown("run cargo test");
    e.select(4, 14);
    e.toggle_mark(MarkKind::Code);
    assert_eq!(e.text(), "run `cargo test`");
}

#[test]
fn bold_with_nothing_selected_formats_what_comes_next() {
    let mut e = ed("a |");
    e.toggle_mark(MarkKind::Bold);
    typed(&mut e, "bold");
    assert_eq!(show(&e), "a **bold|**");
    // At the end of the bold text, Bold steps out of it.
    e.toggle_mark(MarkKind::Bold);
    typed(&mut e, " plain");
    assert_eq!(show(&e), "a **bold** plain|");
    assert_eq!(shown(&e), ["a bold plain"]);
}

#[test]
fn block_commands_rewrite_only_the_prefix() {
    let mut e = ed("some |text\n\nmore");
    e.set_block(BlockKind::Heading(2));
    assert_eq!(show(&e), "## some |text\n\nmore");
    e.set_block(BlockKind::Heading(2));
    assert_eq!(
        show(&e),
        "some |text\n\nmore",
        "toggling the same kind goes back"
    );
    e.set_block(BlockKind::Task);
    assert_eq!(show(&e), "- [ ] some |text\n\nmore");
    e.set_block(BlockKind::Bullet);
    assert_eq!(show(&e), "- some |text\n\nmore");

    let src = "a\n\nb\n\nc";
    let mut e = Editor::markdown(src);
    e.select(0, src.len());
    e.set_block(BlockKind::Ordered);
    assert_eq!(e.text(), "1. a\n\n2. b\n\n3. c");

    let mut e = ed("- [ ] a\n- [ ] |b\n- [ ] c");
    e.set_block(BlockKind::Paragraph);
    assert_eq!(show(&e), "- [ ] a\n\n|b\n\n- [ ] c");
}

#[test]
fn code_block_command_wraps_and_unwraps() {
    let mut e = ed("let |x = 1;");
    e.set_block(BlockKind::Code);
    assert_eq!(show(&e), "```\nlet |x = 1;\n```");
    e.set_block(BlockKind::Code);
    assert_eq!(show(&e), "let |x = 1;");
}

#[test]
fn checkboxes_toggle_in_place() {
    let src = "- [ ] one\n- [X] two";
    let mut e = Editor::markdown(src);
    assert!(e.toggle_task(0));
    assert_eq!(e.text(), "- [x] one\n- [X] two");
    assert!(e.toggle_task(1));
    assert_eq!(e.text(), "- [x] one\n- [ ] two");
    assert!(!Editor::markdown("para").toggle_task(0));
    assert!(e.undo());
    assert_eq!(e.text(), "- [x] one\n- [X] two");
}

#[test]
fn tab_nests_an_item_under_the_one_above_and_shift_tab_undoes_it() {
    let mut e = ed("- a\n- |b\n  - c");
    assert!(e.indent());
    assert_eq!(show(&e), "- a\n  - |b\n    - c");
    assert_eq!(e.doc().rows[1].level, 1);
    assert!(e.outdent());
    assert_eq!(show(&e), "- a\n- |b\n  - c");
    assert!(!e.outdent(), "the outer level hands Shift+Tab back");
    assert!(
        !ed("text|").indent(),
        "outside a list Tab isn't the editor's"
    );

    let mut e = ed("1. a\n2. |b");
    e.indent();
    assert_eq!(show(&e), "1. a\n   2. |b");
}

#[test]
fn links_are_made_edited_and_removed() {
    let src = "see the docs";
    let mut e = Editor::markdown(src);
    e.select(8, 12);
    let t = e.link_target().unwrap();
    assert!(!t.existing);
    e.set_link("https://x.dev");
    assert_eq!(e.text(), "see the [docs](https://x.dev)");
    assert_eq!(shown(&e), ["see the docs"]);

    let at = e.text().find("ocs").unwrap();
    e.select(at, at);
    let t = e.link_target().unwrap();
    assert!(t.existing);
    assert_eq!(t.url, "https://x.dev");
    assert_eq!(e.link_at(at).as_deref(), Some("https://x.dev"));
    e.set_link("https://y.dev");
    assert_eq!(e.text(), "see the [docs](https://y.dev)");
    e.set_link("");
    assert_eq!(e.text(), src);

    let e = Editor::markdown("go https://bare.dev/x now");
    assert_eq!(e.link_at(6).as_deref(), Some("https://bare.dev/x"));
}

// ---- undo, outside changes, input methods ------------------------------------------------------

#[test]
fn undo_and_redo_step_through_edits() {
    let mut e = ed("|");
    typed(&mut e, "hello");
    e.enter();
    typed(&mut e, "world");
    assert_eq!(e.text(), "hello\n\nworld\n");
    assert!(e.undo());
    assert_eq!(e.text(), "hello\n\n\n");
    assert!(e.undo());
    assert_eq!(e.text(), "hello");
    assert!(e.redo());
    assert!(e.redo());
    assert_eq!(e.text(), "hello\n\nworld\n");
    assert!(!e.redo());
}

#[test]
fn a_change_from_outside_keeps_the_caret_and_the_users_undo() {
    let mut e = ed("## Notes\n\nmine|");
    typed(&mut e, " too");
    // An agent appends a section while the editor is open.
    let theirs = format!("{}\n\n## Agent\nadded", e.text());
    assert!(e.replace_from_outside(&theirs));
    assert_eq!(show(&e), "## Notes\n\nmine too|\n\n## Agent\nadded");
    // Undo takes back the user's typing, not the agent's section.
    assert!(e.undo());
    assert_eq!(e.text(), "## Notes\n\nmine\n\n## Agent\nadded");
    // A change at the start moves the caret along with the text.
    let mut e = ed("text|");
    e.replace_from_outside("new text");
    assert_eq!(show(&e), "new text|");
}

#[test]
fn a_composition_is_one_undo_step() {
    let mut e = ed("a|");
    e.set_marked(None, "k", None);
    e.set_marked(None, "か", None);
    assert_eq!(e.marked(), Some(1..4));
    e.commit(None, "漢");
    assert_eq!(show(&e), "a漢|");
    assert_eq!(e.marked(), None);
    assert!(e.undo());
    assert_eq!(e.text(), "a");
    assert_eq!(e.from_utf16(e.to_utf16(1)), 1);
}

#[test]
fn moving_steps_through_what_shows_across_rows() {
    let mut e = ed("**ab**\n\n- [ ] |cd");
    e.move_left(false, false);
    assert_eq!(
        show(&e),
        "**ab|**\n\n- [ ] cd",
        "left from a row's start goes to the end of the row above"
    );
    e.move_left(true, false);
    // A row's start is before its opening delimiter (typing there is plain).
    assert_eq!(show(&e), "|**ab**\n\n- [ ] cd");
    e.move_right(true, false);
    assert_eq!(show(&e), "**ab|**\n\n- [ ] cd");
    e.move_right(false, true);
    assert_eq!(&e.text()[e.selection()], "**\n\n- [ ] ");
    assert_eq!(e.selected_markdown(), "**\n\n- [ ] ");
}

#[test]
fn copying_a_whole_mark_keeps_its_markdown() {
    let src = "a **bold** b";
    let mut e = Editor::markdown(src);
    e.select(4, 8);
    assert_eq!(e.selected_markdown(), "**bold**");
    assert_eq!(e.cut(), "**bold**");
    assert_eq!(e.text(), "a  b");
}

#[test]
fn an_empty_body_is_one_empty_paragraph_ready_for_typing() {
    let mut e = Editor::markdown("");
    assert_eq!(e.doc().rows.len(), 1);
    assert_eq!(e.doc().rows[0].kind, RowKind::Empty);
    typed(&mut e, "hi");
    assert_eq!(e.text(), "hi");
}

// ---- the shipped task bodies ---------------------------------------------------------------------

/// The orchestrator's Feature template (`default_orchestrator.md.template`,
/// "Task body templates"), filled in.
const FEATURE: &str = "\
# Board keyboard navigation

Move between columns and cards with the TUI's keys.

## Wireframes

```
| BACKLOG | TO DO | IN PROGRESS |
```

## Technical Details

- Keys come from `keys.yaml`
- See [the plan](https://example.com/plan)

## Acceptance Criteria
- [ ] h/l move between columns
- [x] j/k move within a column
";

/// The Bug template, filled in.
const BUG: &str = "\
# Done column flickers

The done column blanks on refresh.

## Current Behavior

1. Open the board
2. Wait a tick

## Expected Behavior

The column keeps its cards.

## Acceptance Criteria
- [ ] The done column never blanks
";

fn kinds(src: &str) -> Vec<String> {
    Editor::markdown(src)
        .doc()
        .rows
        .iter()
        .map(|r| match &r.kind {
            RowKind::Heading(n) => format!("h{n}"),
            RowKind::Item(i) => match i.task {
                Some(true) => "checked".into(),
                Some(false) => "unchecked".into(),
                None => "item".into(),
            },
            RowKind::Fence { .. } => "fence".into(),
            RowKind::Code => "code".into(),
            RowKind::Paragraph => "p".into(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn the_shipped_templates_lay_out_as_formatted_blocks() {
    assert_eq!(
        kinds(FEATURE),
        [
            "h1",
            "p",
            "h2",
            "fence",
            "code",
            "fence",
            "h2",
            "item",
            "item",
            "h2",
            "unchecked",
            "checked"
        ]
    );
    assert_eq!(
        kinds(BUG),
        [
            "h1",
            "p",
            "h2",
            "item",
            "item",
            "h2",
            "p",
            "h2",
            "unchecked"
        ]
    );
    assert_eq!(kinds(shelbi_state::WELCOME_TASK_BODY)[0], "h1");
    let e = Editor::markdown(FEATURE);
    let rows: Vec<&str> = e.doc().rows.iter().map(|r| r.display.as_str()).collect();
    assert!(rows.contains(&"Keys come from keys.yaml"));
    assert!(rows.contains(&"See the plan"));
}

#[test]
fn checking_every_template_box_touches_only_the_boxes() {
    for src in [FEATURE, BUG] {
        let mut e = Editor::markdown(src);
        let tasks: Vec<usize> = (0..e.doc().rows.len())
            .filter(|&r| e.doc().rows[r].task().is_some())
            .collect();
        for r in &tasks {
            assert!(e.toggle_task(*r));
        }
        let diff: Vec<(char, char)> = src
            .chars()
            .zip(e.text().chars())
            .filter(|(a, b)| a != b)
            .collect();
        assert!(
            diff.iter()
                .all(|(a, b)| matches!((a, b), (' ', 'x') | ('x', ' '))),
            "{diff:?}"
        );
        assert_eq!(e.text().len(), src.len());
    }
}

/// Many random operations on real bodies: nothing panics, the caret stays
/// on a character boundary, and undoing everything gives the body back.
#[test]
fn random_editing_never_breaks_the_text() {
    for start in 1..=40u64 {
        random_session(start.wrapping_mul(0x9e37_79b9_7f4a_7c15), true);
        random_session(start.wrapping_mul(0x2545_f491_4f6c_dd1d), false);
    }
}

/// `outside`: changes from elsewhere come in too; without them, undoing
/// every step must give the body back exactly.
fn random_session(mut seed: u64, outside: bool) {
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    let bodies = [
        ROUND_TRIP,
        FEATURE,
        BUG,
        "",
        "é漢👍 **b** `c`\n\n- [ ] x",
        "> q\n> r\n\n1. a\n   - b",
    ];
    for body in bodies {
        let mut e = Editor::markdown(body);
        for _ in 0..if outside { 600 } else { 250 } {
            let len = e.text().len();
            let before = (e.text().to_string(), e.anchor(), e.head());
            let op = next(24);
            match op {
                0..=4 => {
                    e.insert(["a", " ", "é", "*", "\n", "- ", "`", "漢", "[", "**x**"][next(10)]);
                    true
                }
                5 => e.enter(),
                6 => {
                    e.line_break();
                    true
                }
                7 | 8 => {
                    e.backspace(next(3) == 0);
                    true
                }
                9 => {
                    e.delete(next(3) == 0);
                    true
                }
                10 => {
                    e.move_left(next(2) == 0, next(3) == 0);
                    true
                }
                11 => {
                    e.move_right(next(2) == 0, next(3) == 0);
                    true
                }
                12 => {
                    let (a, b) = (next(len + 1), next(len + 1));
                    let (a, b) = (floor(e.text(), a), floor(e.text(), b));
                    e.select(a, b);
                    true
                }
                13 => {
                    e.toggle_mark(
                        [
                            MarkKind::Bold,
                            MarkKind::Italic,
                            MarkKind::Code,
                            MarkKind::Strike,
                        ][next(4)],
                    );
                    true
                }
                14 => {
                    let k = [
                        BlockKind::Paragraph,
                        BlockKind::Heading(2),
                        BlockKind::Bullet,
                        BlockKind::Ordered,
                        BlockKind::Task,
                        BlockKind::Quote,
                        BlockKind::Code,
                    ][next(7)];
                    e.set_block(k);
                    true
                }
                15 => {
                    let r = next(e.doc().rows.len());
                    e.toggle_task(r)
                }
                16 => e.indent(),
                17 => e.outdent(),
                18 => e.undo(),
                19 => e.redo(),
                20 => e.set_link(["https://x.dev", ""][next(2)]),
                21 => {
                    let _ = e.cut();
                    true
                }
                22 if outside => {
                    let t = format!("{}\n\nagent note", e.text());
                    e.replace_from_outside(&t)
                }
                22 => e.undo(),
                _ => {
                    e.set_marked(None, "か", None);
                    e.commit(None, "漢");
                    true
                }
            };
            assert!(
                e.text().is_char_boundary(e.head()) && e.text().is_char_boundary(e.anchor()),
                "op {op} on {before:?} gave {:?} {}..{}",
                e.text(),
                e.anchor(),
                e.head()
            );
            assert!(e.head() <= e.text().len() && e.anchor() <= e.text().len());
            // The layout always has a row for the caret.
            let (r, d) = e.doc().locate(e.head());
            assert!(d <= e.doc().rows[r].display.len());
        }
        if !outside {
            while e.undo() {}
            assert_eq!(e.text(), body, "undoing every step gives the body back");
        }
    }
}

fn floor(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
