use super::*;
use chrono::DateTime;
use crossterm::event::KeyEventKind;
use shelbi_state::changelog::{ChangelogEntry, PrRef};
use std::path::PathBuf;

/// The GitHub repository the fixtures' PR numbers resolve against.
const REPO: &str = "jlong/shelbi";

pub(crate) fn entry(
    id: &str,
    title: &str,
    date: &str,
    issues: &[&str],
    prs: &[u64],
    body: &str,
) -> ChangelogEntryView {
    ChangelogEntryView::new(
        ChangelogEntry {
            id: id.into(),
            title: title.into(),
            date: DateTime::parse_from_rfc3339(date).unwrap(),
            author: "orchestrator".into(),
            issues: issues.iter().map(|s| s.to_string()).collect(),
            prs: prs.iter().map(|n| PrRef::Number(*n)).collect(),
            urls: vec![],
            body: body.into(),
            attachments: vec![],
            path: PathBuf::new(),
        },
        Some(REPO),
    )
}

pub(crate) fn data(entries: Vec<ChangelogEntryView>) -> ChangelogData {
    ChangelogData {
        project: "demo".into(),
        entries,
        ..ChangelogData::default()
    }
}

/// Real-looking entries, newest first.
pub(crate) fn sample() -> Vec<ChangelogEntryView> {
    vec![
        entry(
            "2026-10-28-213005-settings-polish-pass",
            "Settings polish pass",
            "2026-10-28T14:30:05-07:00",
            &["tui-settings-polish"],
            &[1942],
            "Every settings page, dialog and editor now matches the desktop: one \
             half-block button, segmented controls, dropdowns and the `textbox` \
             fields, so the TUI and the desktop read as one product.\n\n\
             - Buttons lift under the pointer\n- Focus shows as a solid cyan fill",
        ),
        entry(
            "2026-10-28-170000-contact-us-feedback-dialog",
            "Contact Us feedback dialog",
            "2026-10-28T10:00:00-07:00",
            &["tui-feedback-dialog"],
            &[1933],
            "Send Feedback opens a real dialog in the TUI: your name, email and \
             message go straight to the team, and the dialog remembers who you are.",
        ),
        entry(
            "2026-10-27-190000-about-shelbi",
            "About Shelbi",
            "2026-10-27T12:00:00-07:00",
            &["tui-about"],
            &[],
            "An About panel with the icon, the version and links to the docs, \
             see https://shelbi.dev/docs for more.",
        ),
        entry(
            "2026-10-26-150000-faster-board",
            "Faster board",
            "2026-10-26T08:00:00-07:00",
            &[],
            &[],
            "The board's columns are virtualized lists now, so a project with a \
             thousand done cards scrolls without a hitch.",
        ),
    ]
}

fn page_with(entries: Vec<ChangelogEntryView>) -> ChangelogPage {
    let mut page = ChangelogPage::new();
    page.set_data(data(entries));
    page
}

pub(crate) fn render(page: &mut ChangelogPage, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    page.render(&mut buf, area);
    buf
}

pub(crate) fn rows(buf: &Buffer) -> Vec<String> {
    let a = buf.area;
    (a.top()..a.bottom())
        .map(|y| {
            let mut row = String::new();
            let mut x = a.left();
            while x < a.right() {
                let sym = buf[(x, y)].symbol();
                row.push_str(sym);
                x += (sym.width() as u16).max(1);
            }
            row.trim_end().to_string()
        })
        .collect()
}

fn find(buf: &Buffer, text: &str) -> Option<(u16, u16)> {
    rows(buf).iter().enumerate().find_map(|(y, row)| {
        let at = row.find(text)?;
        Some((row[..at].width() as u16, y as u16))
    })
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

fn press(page: &mut ChangelogPage, code: KeyCode) -> Step {
    page.handle_key(key(code))
}

fn type_str(page: &mut ChangelogPage, s: &str) {
    for c in s.chars() {
        press(page, KeyCode::Char(c));
    }
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

const SIZES: [(u16, u16); 3] = [(100, 30), (160, 45), (60, 20)];

#[test]
fn entries_read_as_a_blog_newest_first_at_every_size() {
    for (w, h) in SIZES {
        let mut page = page_with(sample());
        let buf = render(&mut page, w, h);
        let text = rows(&buf).join("\n");
        assert!(text.contains("Changelog"), "{w}x{h}: the title\n{text}");
        assert!(text.contains("4 entries"), "{w}x{h}: the count\n{text}");
        assert!(text.contains("Search"), "{w}x{h}: the search box\n{text}");
        let first = find(&buf, "Settings polish pass").expect("newest first");
        let second = find(&buf, "Contact Us feedback dialog").expect("then the next");
        assert!(first.1 < second.1, "{w}x{h}: newest on top\n{text}");
        // The date at the right of the title row; spelled out when it fits.
        let title_row = &rows(&buf)[first.1 as usize];
        assert!(
            title_row.ends_with("October 28, 2026") || title_row.ends_with("Oct 28, 2026"),
            "{w}x{h}: date at the right: {title_row:?}"
        );
        // A rule under the title (it's the selection's bleed on the
        // focused first entry, so look under the second), when it's on
        // screen.
        let vp = plan(Rect::new(0, 0, w, h)).viewport;
        if second.1 + 1 < vp.bottom() {
            // The reading column's part of the row (the timeline's rule is
            // left of it).
            let rule: String = rows(&buf)[second.1 as usize + 1].chars().skip(vp.x as usize).collect();
            assert!(rule.trim().chars().all(|c| c == '─') && !rule.trim().is_empty(), "{w}x{h}: {rule:?}");
        }
        // The links as chips: the issue and the PR from the frontmatter.
        assert!(text.contains("⧉ tui-settings-polish"), "{w}x{h}\n{text}");
        assert!(text.contains("↗ #1942"), "{w}x{h}\n{text}");
        // The markdown renderer's inline code.
        let (cx, cy) = find(&buf, "textbox").expect("code span");
        assert_eq!(buf[(cx, cy)].bg, palette().code_bg, "{w}x{h}: code span tinted");
    }
}

#[test]
fn a_blank_row_more_separates_consecutive_entries() {
    let mut entries = sample();
    // One with a body and no links, so the gap after a body shows too.
    entries.insert(
        1,
        entry("b", "Body only", "2026-10-28T12:00:00-07:00", &[], &[], "Just words."),
    );
    let mut page = page_with(entries);
    let buf = render(&mut page, 100, 45);
    let text = rows(&buf);
    // Blank but for the timeline's rule.
    let blank = |from: u16, to: u16| {
        (from..to).all(|y| text[y as usize].trim_matches(|c| c == ' ' || c == '│').is_empty())
    };
    // Chips, then the gap and the next title's top bleed (which is the
    // chips' bottom bleed too): two blanks, one more than before.
    let (_, chips) = find(&buf, "#1942").unwrap();
    let (_, next) = find(&buf, "Body only").unwrap();
    assert_eq!(next, chips + 3, "{}", text.join("\n"));
    assert!(blank(chips + 1, next), "{}", text.join("\n"));
    // A body with no links: its trailing blank, the gap, the bleed (one
    // more than before).
    let (_, body) = find(&buf, "Just words.").unwrap();
    let (_, after) = find(&buf, "Contact Us feedback dialog").unwrap();
    assert_eq!(after, body + 4, "{}", text.join("\n"));
    assert!(blank(body + 1, after), "{}", text.join("\n"));
    // Within an entry nothing moves: title, rule, body.
    let (_, title) = find(&buf, "Body only").unwrap();
    assert_eq!(body, title + 2);
    // The first entry still starts at the top of the document.
    assert_eq!(page.doc.entries[0].top, 0);
    for pair in page.doc.entries.windows(2) {
        assert_eq!(pair[1].top, pair[0].end + ENTRY_GAP, "the gap belongs to neither entry");
    }
}

#[test]
fn the_gap_row_hits_nothing_and_scrolling_onto_it_focuses_the_next_entry() {
    let mut page = page_with(sample());
    let area = Rect::new(0, 0, 100, 30);
    render(&mut page, 100, 30);
    let gap = page.doc.entries[0].end;
    let vp = plan(area).viewport;
    for x in vp.x.saturating_sub(1)..vp.right() + 1 {
        assert_eq!(page.hit(x, vp.y + gap as u16), None, "column {x}");
    }
    // A click on the next title still folds that entry, not the first.
    let title = page.doc.entries[1].title();
    page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), vp.x + 2, vp.y + title as u16), area);
    assert!(page.collapsed.contains("2026-10-28-170000-contact-us-feedback-dialog"));
    assert!(!page.collapsed.contains("2026-10-28-213005-settings-polish-pass"));
    // With the screen starting on the gap, the focus is on the next entry.
    let mut page = page_with(sample());
    render(&mut page, 60, 20);
    let gap = page.doc.entries[0].end;
    for _ in 0..gap {
        press(&mut page, KeyCode::Char('j'));
    }
    assert_eq!(page.scroll, gap);
    assert_eq!(page.selected_ix(), Some(1));
}

#[test]
fn the_reading_column_is_centered_and_capped() {
    let mut page = page_with(sample());
    let buf = render(&mut page, 160, 45);
    let (x, _) = find(&buf, "Changelog").unwrap();
    assert_eq!(x, (160 - MAX_MEASURE) / 2, "a blog's measure, centered");
    let buf = render(&mut page, 60, 20);
    let (x, _) = find(&buf, "Changelog").unwrap();
    assert_eq!(x, 4, "a narrow screen moves over for the timeline");
    // The timeline's rule, in its margin, beside every row of the list.
    let buf = render(&mut page, 160, 45);
    let vp = plan(Rect::new(0, 0, 160, 45)).viewport;
    assert_eq!(buf[(vp.x - RULE_INSET, vp.y + 4)].symbol(), RULE);
}

#[test]
fn the_focused_entry_wears_the_half_block_pill() {
    let mut page = page_with(sample());
    let buf = render(&mut page, 100, 30);
    let (x, y) = find(&buf, "Settings polish pass").unwrap();
    assert_eq!(buf[(x, y)].bg, palette().selection);
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(x, y - 1)].symbol(), BLEED_ABOVE);
    assert_eq!(buf[(x, y - 1)].fg, palette().selection);
    assert_eq!(buf[(x, y + 1)].symbol(), BLEED_BELOW);
    // The next entry's title has no fill.
    let (x2, y2) = find(&buf, "Contact Us").unwrap();
    assert_ne!(buf[(x2, y2)].bg, palette().selection);
}

#[test]
fn n_and_p_jump_between_entries_and_g_g_to_the_ends() {
    let mut page = page_with(sample());
    render(&mut page, 100, 30);
    let id = |p: &ChangelogPage| p.selected.clone().unwrap();
    assert_eq!(id(&page), "2026-10-28-213005-settings-polish-pass");
    press(&mut page, KeyCode::Char('n'));
    assert_eq!(id(&page), "2026-10-28-170000-contact-us-feedback-dialog");
    assert_eq!(page.scroll, page.doc.entries[1].top.min(page.max_scroll()));
    press(&mut page, KeyCode::Char(']'));
    assert_eq!(id(&page), "2026-10-27-190000-about-shelbi");
    press(&mut page, KeyCode::Char('p'));
    press(&mut page, KeyCode::Char('['));
    assert_eq!(id(&page), "2026-10-28-213005-settings-polish-pass");
    press(&mut page, KeyCode::Char('G'));
    assert_eq!(id(&page), "2026-10-26-150000-faster-board");
    press(&mut page, KeyCode::Char('g'));
    assert_eq!(id(&page), "2026-10-28-213005-settings-polish-pass");
    assert_eq!(page.scroll, 0);
    // The jumped-to entry's title is on screen and wears the pill.
    press(&mut page, KeyCode::Char('n'));
    let buf = render(&mut page, 100, 30);
    let (x, y) = find(&buf, "Contact Us feedback dialog").unwrap();
    assert_eq!(buf[(x, y)].bg, palette().selection);
}

#[test]
fn j_k_arrows_and_the_wheel_scroll() {
    let mut page = page_with(sample());
    render(&mut page, 60, 20);
    assert!(page.max_scroll() > 3, "the sample overflows 60x20");
    press(&mut page, KeyCode::Char('j'));
    press(&mut page, KeyCode::Down);
    assert_eq!(page.scroll, 2);
    press(&mut page, KeyCode::Char('k'));
    assert_eq!(page.scroll, 1);
    press(&mut page, KeyCode::Up);
    press(&mut page, KeyCode::Up);
    assert_eq!(page.scroll, 0, "never above the top");
    page.handle_mouse(mouse(MouseEventKind::ScrollDown, 20, 10), Rect::new(0, 0, 60, 20));
    assert_eq!(page.scroll, WHEEL_LINES);
    press(&mut page, KeyCode::End);
    press(&mut page, KeyCode::PageDown);
    assert_eq!(page.scroll, page.max_scroll(), "never past the end");
}

#[test]
fn scrolling_an_entry_off_screen_moves_the_focus_with_the_reader() {
    let mut page = page_with(sample());
    render(&mut page, 60, 20);
    let second_top = page.doc.entries[1].top;
    for _ in 0..second_top {
        press(&mut page, KeyCode::Char('j'));
    }
    assert_eq!(page.selected_ix(), Some(1), "the entry the screen starts on");
}

#[test]
fn search_narrows_as_you_type_with_a_match_count_and_esc_clears() {
    let mut page = page_with(sample());
    render(&mut page, 100, 30);
    press(&mut page, KeyCode::Char('/'));
    assert!(page.searching());
    type_str(&mut page, "dialog");
    let buf = render(&mut page, 100, 30);
    let text = rows(&buf).join("\n");
    assert!(text.contains("2 of 4"), "match count\n{text}");
    assert!(text.contains("Settings polish pass"), "dialog is in its body");
    assert!(text.contains("Contact Us feedback dialog"));
    assert!(!text.contains("Faster board"), "{text}");
    // Typing `j` or `n` types; it doesn't scroll or jump.
    type_str(&mut page, " n");
    assert_eq!(page.model().query(), "dialog n");
    press(&mut page, KeyCode::Backspace);
    press(&mut page, KeyCode::Backspace);
    // Enter keeps the query and hands the keys back.
    press(&mut page, KeyCode::Enter);
    assert!(!page.searching());
    assert_eq!(page.model().query(), "dialog");
    press(&mut page, KeyCode::Char('n'));
    assert_eq!(page.selected_ix(), Some(1), "n jumps again");
    // Esc clears it.
    press(&mut page, KeyCode::Esc);
    assert_eq!(page.model().query(), "");
    assert_eq!(page.model().match_count(), 4);
    // And in the search, Esc clears and leaves.
    press(&mut page, KeyCode::Char('/'));
    type_str(&mut page, "board");
    assert_eq!(page.model().match_count(), 1);
    press(&mut page, KeyCode::Esc);
    assert!(!page.searching());
    assert_eq!(page.model().query(), "");
}

#[test]
fn a_search_that_matches_nothing_says_so() {
    let mut page = page_with(sample());
    page.set_query("zebra");
    let buf = render(&mut page, 60, 20);
    let text = rows(&buf).join("\n");
    assert!(text.contains("No entries match “zebra”."), "{text}");
    assert!(text.contains("0 of 4"), "{text}");
}

#[test]
fn the_search_box_takes_the_caret_and_the_clear_button_clears() {
    let mut page = page_with(sample());
    let area = Rect::new(0, 0, 100, 30);
    render(&mut page, 100, 30);
    let search = plan(area).search.unwrap();
    page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), search.x + 4, search.y + 1), area);
    assert!(page.searching());
    type_str(&mut page, "about");
    let mut buf = Buffer::empty(area);
    let cursor = page.render(&mut buf, area);
    assert!(cursor.is_some_and(|(_, y)| y == search.y + 1), "caret in the box: {cursor:?}");
    assert_eq!(buf[(search.x + 1, search.y + 1)].bg, palette().input_focus);
    page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), clear_x(search), search.y + 1), area);
    assert_eq!(page.model().query(), "");
}

#[test]
fn tab_walks_the_links_and_enter_follows_them() {
    let mut page = page_with(sample());
    render(&mut page, 100, 30);
    press(&mut page, KeyCode::Tab);
    assert_eq!(press(&mut page, KeyCode::Enter), Step::OpenIssue("tui-settings-polish".into()));
    press(&mut page, KeyCode::Tab);
    assert_eq!(
        press(&mut page, KeyCode::Enter),
        Step::OpenUrl("https://github.com/jlong/shelbi/pull/1942".into())
    );
    // On into the next entry's links; Shift+Tab steps back.
    press(&mut page, KeyCode::Tab);
    assert_eq!(page.selected_ix(), Some(1));
    assert_eq!(press(&mut page, KeyCode::Enter), Step::OpenIssue("tui-feedback-dialog".into()));
    press(&mut page, KeyCode::BackTab);
    assert_eq!(page.selected_ix(), Some(0));
    // The focused link wears the pill; its entry's title doesn't.
    let buf = render(&mut page, 100, 30);
    let (x, y) = find(&buf, "#1942").unwrap();
    assert_eq!(buf[(x, y)].bg, palette().selection);
    assert_eq!(buf[(x, y - 1)].symbol(), BLEED_ABOVE);
    assert_eq!(buf[(x, y + 1)].symbol(), BLEED_BELOW);
    let (tx, ty) = find(&buf, "Settings polish pass").unwrap();
    assert_ne!(buf[(tx, ty)].bg, palette().selection);
    // Esc lets go of the link; Enter then folds the entry.
    press(&mut page, KeyCode::Esc);
    assert_eq!(page.link, None);
    press(&mut page, KeyCode::Enter);
    assert!(page.collapsed.contains("2026-10-28-213005-settings-polish-pass"));
}

#[test]
fn a_description_url_is_a_chip_too() {
    let mut page = page_with(sample());
    let buf = render(&mut page, 160, 45);
    let text = rows(&buf).join("\n");
    assert!(text.contains("↗ shelbi.dev/docs"), "{text}");
}

#[test]
fn an_unresolved_pr_is_muted_and_skipped() {
    let mut e = entry("a", "Unresolved", "2026-10-28T14:30:05-07:00", &[], &[], "");
    e = ChangelogEntryView::new(
        ChangelogEntry {
            id: e.id,
            title: e.title,
            date: e.date,
            author: e.author,
            issues: vec![],
            prs: vec![PrRef::Number(13)],
            urls: vec![],
            body: String::new(),
            attachments: vec![],
            path: PathBuf::new(),
        },
        None,
    );
    let mut page = page_with(vec![e]);
    let buf = render(&mut page, 100, 30);
    let (x, y) = find(&buf, "#13").unwrap();
    assert_eq!(buf[(x, y)].fg, palette().muted);
    press(&mut page, KeyCode::Tab);
    assert_eq!(page.link, None, "nothing to focus");
}

/// An entry with an inline image and a file, stored under `/h/changelog`.
fn with_files() -> ChangelogEntryView {
    use shelbi_state::changelog::{Attachment, AttachmentKind};
    let id = "2026-11-16-170000-board-scrolls";
    ChangelogEntryView::new(
        ChangelogEntry {
            id: id.into(),
            title: "Board columns scroll smoothly".into(),
            date: DateTime::parse_from_rfc3339("2026-11-16T09:00:00-08:00").unwrap(),
            author: "orchestrator".into(),
            issues: vec!["job-sheet-pane".into()],
            prs: vec![],
            urls: vec![],
            body: format!("Opening a job floats a pane.\n\n![Jobs sheet after the change](assets/{id}/jobs.png)"),
            attachments: vec![
                Attachment {
                    path: format!("assets/{id}/jobs.png"),
                    name: "jobs.png".into(),
                    size: 48_000,
                    kind: AttachmentKind::Image,
                },
                Attachment {
                    path: format!("assets/{id}/bench-results.csv"),
                    name: "bench-results.csv".into(),
                    size: 12 * 1024,
                    kind: AttachmentKind::File,
                },
            ],
            path: PathBuf::from(format!("/h/changelog/{id}.md")),
        },
        Some(REPO),
    )
}

#[test]
fn images_are_placeholders_and_files_are_listed_and_open() {
    let mut page = page_with(vec![with_files()]);
    let buf = render(&mut page, 100, 30);
    let text = rows(&buf).join("\n");
    assert!(text.contains("[image: Jobs sheet after the change]"), "{text}");
    assert!(!text.contains("![") && !text.contains("jobs.png"), "the image isn't listed\n{text}");
    assert!(text.contains("Attachments"), "{text}");
    assert!(text.contains("⎘ bench-results.csv  12 KB"), "{text}");
    // The label sits under the description, the file under the label, the
    // link chips after the files.
    let (_, label) = find(&buf, "Attachments").unwrap();
    let (_, file) = find(&buf, "bench-results.csv").unwrap();
    let (_, chip) = find(&buf, "job-sheet-pane").unwrap();
    assert!(label < file && file < chip, "{text}");

    // Tab reaches the file first, and Enter opens it with the system opener.
    press(&mut page, KeyCode::Tab);
    assert_eq!(
        press(&mut page, KeyCode::Enter),
        Step::OpenFile(PathBuf::from("/h/changelog/assets/2026-11-16-170000-board-scrolls/bench-results.csv"))
    );
    press(&mut page, KeyCode::Tab);
    assert_eq!(press(&mut page, KeyCode::Enter), Step::OpenIssue("job-sheet-pane".into()));
    // A click opens it too.
    let area = Rect::new(0, 0, 100, 30);
    let buf = render(&mut page, 100, 30);
    let (x, y) = find(&buf, "bench-results.csv").unwrap();
    let step = page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area);
    assert!(matches!(step, Step::OpenFile(_)), "{step:?}");
}

/// [`with_files`] stored under `dir`, its image a real 400×100 PNG there.
fn with_a_picture(dir: &std::path::Path) -> ChangelogEntryView {
    let mut e = with_files();
    let assets = dir.join("assets").join(&e.id);
    std::fs::create_dir_all(&assets).unwrap();
    image::RgbaImage::from_pixel(400, 100, image::Rgba([10, 200, 90, 255]))
        .save(assets.join("jobs.png"))
        .unwrap();
    e.dir = Some(dir.to_path_buf());
    e
}

#[test]
fn a_kitty_terminal_draws_the_picture_under_its_caption() {
    let dir = tempfile::tempdir().unwrap();
    let mut page = page_with(vec![with_a_picture(dir.path())]);
    page.set_graphics(Some(crate::kitty::Graphics { cell: (10, 20) }));
    let buf = render(&mut page, 100, 40);
    let text = rows(&buf).join("\n");
    assert!(!text.contains("[image:"), "the picture, not its placeholder\n{text}");
    // 400×100 px in 10×20 cells: 40 columns, 5 rows, then the caption.
    let placements = page.take_placements();
    assert_eq!(placements.len(), 1, "{placements:?}");
    let p = &placements[0];
    assert_eq!((p.cols, p.rows, p.skip, p.shown), (40, 5, 0, 5));
    assert!(p.path.ends_with("jobs.png"));
    let (cx, cy) = find(&buf, "Jobs sheet after the change").unwrap();
    assert_eq!((cx, cy), (p.x, p.y + 5), "the caption sits right under the picture");
    let (_, prose) = find(&buf, "Opening a job floats a pane.").unwrap();
    assert!(prose < p.y, "{text}");

    // Scrolled so the picture's top rows pass under the header: it's cut
    // (its rows start at document line 5: bleed, title, rule, prose, blank).
    page.scroll = 7;
    render(&mut page, 100, 12);
    assert_eq!(page.scroll, 7);
    let p = page.take_placements().pop().expect("still partly shown");
    assert_eq!((p.skip, p.shown), (2, 3), "{p:?}");

    // Back to placeholders (a terminal without graphics): the old text.
    page.set_graphics(None);
    page.scroll = 0;
    let text = rows(&render(&mut page, 100, 40)).join("\n");
    assert!(text.contains("[image: Jobs sheet after the change]"), "{text}");
    assert!(page.take_placements().is_empty());
}

#[test]
fn a_kitty_terminal_keeps_placeholders_for_images_it_cant_draw() {
    // The file isn't there.
    let mut page = page_with(vec![with_files()]);
    page.set_graphics(Some(crate::kitty::Graphics { cell: (10, 20) }));
    let text = rows(&render(&mut page, 100, 30)).join("\n");
    assert!(text.contains("[image: Jobs sheet after the change]"), "{text}");
    assert!(page.take_placements().is_empty());
    // A remote window's entry.
    let dir = tempfile::tempdir().unwrap();
    let mut e = with_a_picture(dir.path());
    e.dir = None;
    let mut page = page_with(vec![e]);
    page.set_graphics(Some(crate::kitty::Graphics { cell: (10, 20) }));
    let text = rows(&render(&mut page, 100, 30)).join("\n");
    assert!(text.contains("[image: Jobs sheet after the change]"), "{text}");
    assert!(page.take_placements().is_empty());
}

#[test]
fn a_remote_windows_files_are_listed_but_not_openable() {
    let mut e = with_files();
    // What a remote window has: the entry, without this machine's directory.
    e.dir = None;
    let mut page = page_with(vec![e]);
    let buf = render(&mut page, 100, 30);
    let text = rows(&buf).join("\n");
    assert!(text.contains("[image: Jobs sheet after the change]"), "{text}");
    let (x, y) = find(&buf, "bench-results.csv").unwrap();
    assert_eq!(buf[(x, y)].fg, palette().muted);
    // Tab skips it for the issue link.
    press(&mut page, KeyCode::Tab);
    assert_eq!(press(&mut page, KeyCode::Enter), Step::OpenIssue("job-sheet-pane".into()));
}

#[test]
fn clicking_a_title_folds_and_unfolds_its_entry() {
    let mut page = page_with(sample());
    let area = Rect::new(0, 0, 100, 30);
    let buf = render(&mut page, 100, 30);
    let (x, y) = find(&buf, "Contact Us feedback dialog").unwrap();
    page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area);
    let buf = render(&mut page, 100, 30);
    let text = rows(&buf).join("\n");
    assert!(!text.contains("Send Feedback opens"), "folded\n{text}");
    assert!(text.contains("Contact Us feedback dialog ⋯"), "{text}");
    assert_eq!(find(&buf, "Contact Us feedback dialog").unwrap(), (x, y), "the title stays put");
    page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area);
    let buf = render(&mut page, 100, 30);
    assert!(rows(&buf).join("\n").contains("Send Feedback opens"));
}

#[test]
fn clicking_a_chip_follows_it_and_hover_underlines_only_its_label() {
    let mut page = page_with(sample());
    let area = Rect::new(0, 0, 100, 30);
    let buf = render(&mut page, 100, 30);
    let (x, y) = find(&buf, "#1942").unwrap();
    page.handle_mouse(mouse(MouseEventKind::Moved, x + 1, y), area);
    let buf = render(&mut page, 100, 30);
    assert!(buf[(x, y)].modifier.contains(Modifier::UNDERLINED));
    assert!(!buf[(x - 1, y)].modifier.contains(Modifier::UNDERLINED), "not the padding");
    assert!(!buf[(x - 2, y)].modifier.contains(Modifier::UNDERLINED), "not the glyph");
    assert!(!buf[(x + 5, y)].modifier.contains(Modifier::UNDERLINED), "not past the label");
    assert_eq!(buf[(x, y)].bg, palette().row_hover);
    let step = page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area);
    assert_eq!(step, Step::OpenUrl("https://github.com/jlong/shelbi/pull/1942".into()));
    // From its padding too (the pill's lead cell, before the glyph).
    let step = page.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x - 3, y), area);
    assert_eq!(step, Step::OpenUrl("https://github.com/jlong/shelbi/pull/1942".into()));
}

#[test]
fn older_entries_load_as_you_scroll() {
    let many: Vec<ChangelogEntryView> = (0..120)
        .map(|i| {
            entry(
                &format!("e{i:03}"),
                &format!("Entry {i}"),
                "2026-10-28T14:30:05-07:00",
                &[],
                &[],
                "One line.",
            )
        })
        .collect();
    let mut page = page_with(many);
    let buf = render(&mut page, 100, 30);
    assert_eq!(page.model().visible().count(), shelbi_app::changelog::PAGE_SIZE);
    assert!(rows(&buf).join("\n").contains("120 entries"));
    // Scroll to the end of the first page: the next one loads.
    while page.model().visible().count() == shelbi_app::changelog::PAGE_SIZE {
        let before = page.scroll;
        press(&mut page, KeyCode::PageDown);
        assert!(page.scroll > before, "scrolling");
    }
    assert_eq!(page.model().visible().count(), 2 * shelbi_app::changelog::PAGE_SIZE);
    // G is the oldest of all of them.
    press(&mut page, KeyCode::Char('G'));
    assert_eq!(page.model().visible().count(), 120);
    assert_eq!(page.selected.as_deref(), Some("e119"));
    let buf = render(&mut page, 100, 30);
    assert!(rows(&buf).join("\n").contains("Entry 119"));
}

#[test]
fn a_new_entry_shows_on_top_or_lands_above_the_reader() {
    let mut page = page_with(sample());
    render(&mut page, 100, 30);
    let mut fresh = sample();
    fresh.insert(
        0,
        entry(
            "2026-10-29-010000-changelog",
            "Changelog in the TUI",
            "2026-10-28T18:00:00-07:00",
            &["changelog-tui"],
            &[],
            "The sidebar's Changelog row opens a blog of what shipped.",
        ),
    );
    // At the top, the new entry shows first.
    page.set_data(data(fresh.clone()));
    let buf = render(&mut page, 100, 30);
    let new = find(&buf, "Changelog in the TUI").expect("shown");
    assert!(new.1 < find(&buf, "Settings polish pass").unwrap().1);

    // Reading further down, the screen stays on what's being read.
    let mut page = page_with(sample());
    render(&mut page, 100, 30);
    press(&mut page, KeyCode::Char('n'));
    let before = find(&render(&mut page, 100, 30), "Contact Us feedback dialog").unwrap();
    page.set_data(data(fresh));
    let after = find(&render(&mut page, 100, 30), "Contact Us feedback dialog").unwrap();
    assert_eq!(before, after);
    assert_eq!(page.selected.as_deref(), Some("2026-10-28-170000-contact-us-feedback-dialog"));
}

#[test]
fn the_empty_state_says_how_to_add_one_at_every_size() {
    for (w, h) in SIZES {
        let mut page = page_with(vec![]);
        let buf = render(&mut page, w, h);
        let text = rows(&buf).join(" ");
        let squashed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(squashed.contains("No changelog entries yet."), "{w}x{h}\n{text}");
        assert!(squashed.contains("Shipped work shows up here."), "{w}x{h}\n{text}");
        assert!(squashed.contains("shelbi changelog add"), "{w}x{h}\n{text}");
        assert!(!text.contains("entries ·"), "no count");
    }
}

#[test]
fn loading_and_a_read_error_are_said() {
    let mut page = ChangelogPage::new();
    assert!(rows(&render(&mut page, 60, 20)).join("\n").contains("Loading…"));
    page.set_data(ChangelogData {
        project: "demo".into(),
        error: Some("permission denied".into()),
        ..ChangelogData::default()
    });
    let text = rows(&render(&mut page, 100, 30)).join("\n");
    assert!(text.contains("Couldn't read the changelog: permission denied"), "{text}");
}

#[test]
fn nothing_draws_outside_the_area() {
    for (w, h) in SIZES {
        let mut page = page_with(sample());
        let outer = Rect::new(0, 0, w + 20, h + 6);
        let area = Rect::new(10, 3, w, h);
        let mut buf = Buffer::empty(outer);
        page.render(&mut buf, area);
        for y in outer.top()..outer.bottom() {
            for x in outer.left()..outer.right() {
                if x < area.left() || x >= area.right() || y < area.top() || y >= area.bottom() {
                    assert_eq!(buf[(x, y)].symbol(), " ", "{w}x{h}: ({x},{y}) outside");
                }
            }
        }
        // And the page still reads.
        let text = rows(&buf).join("\n");
        assert!(text.contains("Settings polish pass"));
    }
}

#[test]
fn tiny_areas_never_panic() {
    let mut page = page_with(sample());
    for (w, h) in [(0, 0), (1, 1), (8, 4), (12, 5), (20, 3), (30, 8)] {
        render(&mut page, w, h);
        press(&mut page, KeyCode::Char('j'));
        press(&mut page, KeyCode::Tab);
        press(&mut page, KeyCode::Char('G'));
    }
}

fn release(tag: &str, date: &str) -> shelbi_app::changelog::Release {
    shelbi_app::changelog::Release {
        tag: tag.into(),
        label: tag.into(),
        date: DateTime::parse_from_rfc3339(date).unwrap(),
    }
}

fn with_releases() -> ChangelogData {
    let mut d = data(sample());
    d.releases = vec![
        // Between "Settings polish pass" (14:30) and "Contact Us" (10:00).
        release("v0.14.0", "2026-10-28T12:00:00-07:00"),
        // Between "About Shelbi" (the 27th) and "Faster board" (the 26th).
        release("v0.13.3", "2026-10-26T20:00:00Z"),
        // Older than every entry: off the timeline.
        release("v0.1.0", "2026-01-01T00:00:00Z"),
    ];
    d
}

#[test]
fn the_timeline_ticks_each_title_and_marks_releases_between_entries() {
    let mut page = ChangelogPage::new();
    page.set_data(with_releases());
    let buf = render(&mut page, 160, 60);
    let text = rows(&buf);
    let vp = plan(Rect::new(0, 0, 160, 60)).viewport;
    let rule_x = vp.x - RULE_INSET;
    let at = |title: &str| find(&buf, title).unwrap_or_else(|| panic!("{title}\n{}", text.join("\n"))).1;
    // A tick on every title row.
    for title in ["Settings polish pass", "Contact Us feedback dialog", "About Shelbi", "Faster board"] {
        assert_eq!(buf[(rule_x, at(title))].symbol(), TICK, "{title}");
    }
    // Each release between the entries before and after it, labeled with
    // its date, its marker on the rule.
    let (cx, v14) = find(&buf, "v0.14.0 · Oct 28").expect("v0.14.0");
    assert_eq!(cx, vp.x, "the caption lines up with the titles");
    assert!(at("Settings polish pass") < v14 && v14 < at("Contact Us feedback dialog"));
    let (_, v13) = find(&buf, "v0.13.3 · Oct 26").expect("v0.13.3");
    assert!(at("About Shelbi") < v13 && v13 < at("Faster board"));
    for y in [v14, v13] {
        assert_eq!(buf[(rule_x, y)].symbol(), RELEASE);
        assert_eq!(buf[(rule_x, y)].fg, palette().cyan);
        assert!(text[y as usize].ends_with('─'), "{:?}", text[y as usize]);
    }
    assert!(find(&buf, "v0.1.0").is_none(), "older than every entry");
    // The rule runs unbroken from the first title to the last entry.
    for y in at("Settings polish pass")..at("Faster board") {
        let sym = buf[(rule_x, y)].symbol();
        assert!([RULE, TICK, RELEASE].contains(&sym), "row {y}: {sym:?}");
    }
    // A blank under each marker, then the next entry's top bleed row.
    assert_eq!(at("Contact Us feedback dialog"), v14 + 3);

    // A narrow screen keeps the markers, at the column's edge.
    let buf = render(&mut page, 20, 60);
    let (x, _) = find(&buf, "◆ v0.14.0").expect("narrow marker");
    assert_eq!(x, plan(Rect::new(0, 0, 20, 60)).viewport.x);
}

#[test]
fn no_releases_shows_ticks_only_and_a_bad_block_says_so() {
    let mut page = page_with(sample());
    let buf = render(&mut page, 160, 45);
    assert!(!rows(&buf).join("\n").contains(RELEASE));
    let vp = plan(Rect::new(0, 0, 160, 45)).viewport;
    let (_, y) = find(&buf, "Settings polish pass").unwrap();
    assert_eq!(buf[(vp.x - RULE_INSET, y)].symbol(), TICK);

    let mut d = data(sample());
    d.releases_error = Some("releases.tags `v[0-9`: a `[` is never closed with `]`".into());
    page.set_data(d);
    let buf = render(&mut page, 160, 45);
    let (x, y) = find(&buf, "Couldn’t read the releases: releases.tags `v[0-9`").expect("the error");
    assert_eq!((x, y), (vp.x, vp.y));
    assert_eq!(buf[(x, y)].fg, palette().yellow);
    // The entries still show under it.
    assert!(find(&buf, "Settings polish pass").unwrap().1 > y);
}
