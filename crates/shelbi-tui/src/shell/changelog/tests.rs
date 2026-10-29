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
        // focused first entry, so look under the second).
        let rule = &rows(&buf)[second.1 as usize + 1];
        assert!(rule.trim().chars().all(|c| c == '─') && !rule.trim().is_empty(), "{w}x{h}: {rule:?}");
        // The links as chips: the issue and the PR from the frontmatter.
        assert!(text.contains("⧉ tui-settings-polish"), "{w}x{h}\n{text}");
        assert!(text.contains("↗ #1942"), "{w}x{h}\n{text}");
        // The markdown renderer's inline code.
        let (cx, cy) = find(&buf, "textbox").expect("code span");
        assert_eq!(buf[(cx, cy)].bg, palette().code_bg, "{w}x{h}: code span tinted");
    }
}

#[test]
fn the_reading_column_is_centered_and_capped() {
    let mut page = page_with(sample());
    let buf = render(&mut page, 160, 45);
    let (x, _) = find(&buf, "Changelog").unwrap();
    assert_eq!(x, (160 - MAX_MEASURE) / 2, "a blog's measure, centered");
    let buf = render(&mut page, 60, 20);
    let (x, _) = find(&buf, "Changelog").unwrap();
    assert_eq!(x, 2, "a narrow screen keeps a two-column gutter");
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
