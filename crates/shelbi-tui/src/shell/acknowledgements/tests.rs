use super::*;
use ratatui::style::Color;
use crossterm::event::KeyModifiers;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use shelbi_app::theme::Appearance;

fn view() -> Acknowledgements {
    Acknowledgements::new(crate::licenses::browser())
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn draw(v: &mut Acknowledgements, w: u16, h: u16) -> (Buffer, Option<(u16, u16)>) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut cursor = None;
    term.draw(|f| {
        let area = f.area();
        cursor = v.render(f.buffer_mut(), area);
    })
    .unwrap();
    (term.backend().buffer().clone(), cursor)
}

fn text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut skip = 0;
        for x in 0..area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            skip = sym.width().saturating_sub(1);
            out.push_str(sym);
        }
        out.push('\n');
    }
    out
}

#[test]
#[ignore]
fn print_screens() {
    for (w, h) in [(100, 30), (160, 45), (60, 20)] {
        let mut v = view();
        let (buf, _) = draw(&mut v, w, h);
        println!("{w}x{h}\n{}", text(&buf));
    }
}

/// The row of the screen holding `needle`, and its column.
fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    text(buf).lines().enumerate().find_map(|(y, line)| {
        line.find(needle).map(|byte| (line[..byte].width() as u16, y as u16))
    })
}

/// `needle` in the list's column (the screen's first `list_width(w)`
/// columns), as [`find`].
fn find_row(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let max = list_width(buf.area.width);
    text(buf).lines().enumerate().find_map(|(y, line)| {
        line.match_indices(needle)
            .map(|(byte, _)| line[..byte].width() as u16)
            .find(|x| *x < max)
            .map(|x| (x, y as u16))
    })
}

fn names_shown(v: &Acknowledgements) -> Vec<&'static str> {
    v.browser().visible().into_iter().map(|i| v.browser().entries()[i].name).collect()
}

fn ix(name: &str) -> usize {
    crate::licenses::entries().iter().position(|e| e.name == name).unwrap()
}

#[test]
fn it_opens_on_the_first_entry_at_every_size() {
    for (w, h) in [(100, 30), (160, 45), (60, 20)] {
        let mut v = view();
        let (buf, cursor) = draw(&mut v, w, h);
        let screen = text(&buf);
        assert!(screen.contains("Acknowledgements"), "{w}x{h} title\n{screen}");
        assert!(screen.contains("Done"), "{w}x{h}");
        assert!(screen.contains("🔍"), "{w}x{h} the search box");
        assert!(screen.contains("View source ↗"), "{w}x{h}");
        assert!(screen.contains("Apache License 2.0"), "{w}x{h} the license's name");
        assert_eq!(cursor, None, "no caret until the search has the keys");
        // The first entry, selected: its name in the list on the selection
        // fill, its half rows above and below in the fill's color.
        let (x, y) = find_row(&buf, "Alacritty Terminal").unwrap();
        assert_eq!(buf[(x, y)].bg, palette().selection, "{w}x{h}");
        assert_eq!(buf[(x, y + 1)].bg, palette().selection, "{w}x{h}: the license row too");
        assert_eq!(buf[(x, y - 1)].symbol(), BLEED_ABOVE);
        assert_eq!(buf[(x, y - 1)].fg, palette().selection);
        assert_eq!(buf[(x, y + 2)].symbol(), BLEED_BELOW);
        assert_eq!(buf[(x, y + 2)].fg, palette().selection);
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
        // And in the detail, loud.
        let (dx, dy) = find(&buf, "│  Alacritty Terminal").map(|(x, y)| (x + 3, y)).unwrap();
        assert_eq!(buf[(dx, dy)].fg, palette().white, "{w}x{h}");
        assert!(buf[(dx, dy)].modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn the_wide_detail_sets_view_source_beside_the_name_and_the_narrow_one_under_the_description() {
    let mut v = view();
    let (buf, _) = draw(&mut v, 100, 30);
    let (_, name_y) = find(&buf, "│  Alacritty Terminal").unwrap();
    let (_, button_y) = find(&buf, "View source").unwrap();
    assert_eq!(name_y, button_y, "beside: the label on the name's row");

    let mut v = view();
    let (buf, _) = draw(&mut v, 60, 20);
    let (_, desc_y) = find(&buf, "Reads every session").unwrap();
    let (bx, by) = find(&buf, "View source").unwrap();
    assert!(by > desc_y, "stacked under the description");
    assert_eq!(buf[(bx, by)].bg, palette().selection, "the secondary button's fill");
    assert_eq!(buf[(bx, by - 1)].symbol(), BLEED_ABOVE);
}

#[test]
fn done_is_the_primary_button() {
    let mut v = view();
    let (buf, _) = draw(&mut v, 100, 30);
    let (x, y) = find(&buf, "Done").unwrap();
    assert_eq!(y, 1, "on the title row");
    assert_eq!(buf[(x, y)].bg, palette().cyan);
    assert_eq!(buf[(x, y)].fg, palette().on_color);
    assert_eq!(buf[(x, y - 1)].symbol(), BLEED_ABOVE);
    assert_eq!(buf[(x, y + 1)].symbol(), BLEED_BELOW);
}

#[test]
fn the_search_filters_as_you_type_and_esc_backs_out_a_step_at_a_time() {
    let mut v = view();
    draw(&mut v, 100, 30);
    assert_eq!(v.handle_key(key(KeyCode::Char('/'))), Step::Stay);
    assert_eq!(v.focus(), Focus::Search);
    for c in "mpl".chars() {
        v.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(names_shown(&v), ["nucleo-matcher", "option-ext"]);
    assert_eq!(v.browser().selected(), Some(ix("nucleo-matcher")), "the first match is selected");
    let (buf, cursor) = draw(&mut v, 100, 30);
    let screen = text(&buf);
    assert!(screen.contains("🔍 mpl"), "{screen}");
    assert!(!screen.contains("Crossterm"), "hidden by the search");
    assert!(screen.contains("Mozilla Public License 2.0"));
    let (qx, qy) = find(&buf, "mpl").unwrap();
    assert_eq!(cursor, Some((qx + 3, qy)), "the caret after the query");
    assert_eq!(buf[(qx, qy)].bg, palette().input_focus, "the focused fill");
    assert!(screen.contains('×'), "a query shows the clear button");
    // Typing j / k types them, it doesn't move the list.
    v.handle_key(key(KeyCode::Backspace));
    assert_eq!(v.browser().query(), "mp");
    // ↓ from the search moves the list and leaves the keys there.
    v.handle_key(key(KeyCode::Down));
    assert_eq!(v.browser().selected(), Some(ix("option-ext")));
    assert_eq!(v.focus(), Focus::Search);
    // Esc clears; again hands the keys to the list; again leaves.
    assert_eq!(v.handle_key(key(KeyCode::Esc)), Step::Stay);
    assert_eq!(v.browser().query(), "");
    assert_eq!(v.browser().selected(), Some(ix("option-ext")), "the selection stays");
    assert_eq!(v.handle_key(key(KeyCode::Esc)), Step::Stay);
    assert_eq!(v.focus(), Focus::List);
    assert_eq!(v.handle_key(key(KeyCode::Esc)), Step::Leave);
}

#[test]
fn ctrl_f_and_cmd_f_focus_the_search_and_a_paste_types_into_it() {
    let mut v = view();
    v.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert_eq!(v.focus(), Focus::Search);
    let mut v = view();
    v.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::SUPER));
    assert_eq!(v.focus(), Focus::Search);
    let mut v = view();
    v.paste("rata\n");
    assert_eq!(v.focus(), Focus::Search);
    assert_eq!(v.browser().query(), "rata");
    assert_eq!(names_shown(&v), ["Ratatui"]);
    v.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(v.browser().query(), "", "Ctrl+U clears it");
}

#[test]
fn nothing_matching_empties_the_detail_and_tab_skips_view_source() {
    let mut v = view();
    v.set_query("zzzz-nothing");
    let (buf, _) = draw(&mut v, 100, 30);
    let screen = text(&buf);
    assert!(screen.contains("Nothing matches the search."), "{screen}");
    assert!(!screen.contains("View source"));
    // List → (no View source) → Done.
    assert_eq!(v.focus(), Focus::List);
    v.handle_key(key(KeyCode::Tab));
    assert_eq!(v.focus(), Focus::Done);
    v.handle_key(key(KeyCode::BackTab));
    assert_eq!(v.focus(), Focus::List);
}

#[test]
fn the_arrows_move_the_selection_and_the_list_follows_it() {
    let mut v = view();
    draw(&mut v, 100, 30);
    v.handle_key(key(KeyCode::Down));
    v.handle_key(key(KeyCode::Char('j')));
    assert_eq!(v.browser().selected(), Some(2));
    v.handle_key(key(KeyCode::Char('k')));
    assert_eq!(v.browser().selected(), Some(1));
    v.handle_key(key(KeyCode::End));
    let last = crate::licenses::entries().len() - 1;
    assert_eq!(v.browser().selected(), Some(last));
    let (buf, _) = draw(&mut v, 100, 30);
    let (x, y) = find_row(&buf, "Rust crates").expect("the list scrolled to the last row");
    assert_eq!(buf[(x, y)].bg, palette().selection);
    assert!(!text(&buf).contains("Alacritty Terminal"), "scrolled off the top");
    assert!(text(&buf).contains("Licenses and notices"));
    v.handle_key(key(KeyCode::Home));
    assert_eq!(v.browser().selected(), Some(0));
    let (buf, _) = draw(&mut v, 100, 30);
    assert!(text(&buf).contains("Alacritty Terminal"), "and back");
}

#[test]
fn the_pointer_highlights_a_row_and_a_click_selects_it() {
    let mut v = view();
    let area = Rect::new(0, 0, 100, 30);
    let (buf, _) = draw(&mut v, 100, 30);
    let (x, y) = find_row(&buf, "Ratatui").unwrap();
    assert_eq!(v.handle_mouse(mouse(MouseEventKind::Moved, x + 3, y + 1), area), Step::Stay);
    let (buf, _) = draw(&mut v, 100, 30);
    assert_eq!(buf[(x, y)].bg, palette().row_hover, "hovered");
    assert_eq!(buf[(x, y - 1)].symbol(), BLEED_ABOVE);
    assert_eq!(buf[(x, y - 1)].fg, palette().row_hover);
    let (sx, sy) = find_row(&buf, "Alacritty Terminal").unwrap();
    assert_eq!(buf[(sx, sy)].bg, palette().selection, "the selection keeps its fill");
    // Off the rows, nothing is hovered.
    v.handle_mouse(mouse(MouseEventKind::Moved, 60, 25), area);
    let (buf, _) = draw(&mut v, 100, 30);
    assert_eq!(buf[(x, y)].bg, Color::Reset);
    // A click selects, and the detail follows.
    v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area);
    assert_eq!(v.browser().selected(), Some(ix("Ratatui")));
    let (buf, _) = draw(&mut v, 100, 30);
    assert!(text(&buf).contains("The Ratatui Developers"));
    assert_eq!(buf[(x, y)].bg, palette().selection);
}

/// Where a selected pill meets the hovered one under it, the separator is
/// both: the selection's half over the hover's.
#[test]
fn a_hover_under_the_selection_shares_its_separator() {
    let mut v = view();
    let area = Rect::new(0, 0, 100, 30);
    let (buf, _) = draw(&mut v, 100, 30);
    let (x, y) = find_row(&buf, "Crossterm").unwrap();
    v.handle_mouse(mouse(MouseEventKind::Moved, x, y), area);
    let (buf, _) = draw(&mut v, 100, 30);
    let sep = &buf[(x, y - 1)];
    assert_eq!(sep.symbol(), BLEED_BELOW);
    assert_eq!(sep.fg, palette().selection);
    assert_eq!(sep.bg, palette().row_hover);
}

#[test]
fn clicks_focus_and_clear_the_search_and_press_the_buttons() {
    let area = Rect::new(0, 0, 100, 30);
    let mut v = view();
    let (buf, _) = draw(&mut v, 100, 30);
    let (sx, sy) = find(&buf, "Search acknowledgements").unwrap();
    v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), sx, sy), area);
    assert_eq!(v.focus(), Focus::Search);
    v.set_query("geist");
    let (buf, _) = draw(&mut v, 100, 30);
    let (cx, cy) = find(&buf, "×").unwrap();
    v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), cx, cy), area);
    assert_eq!(v.browser().query(), "", "× clears the search");

    let (buf, _) = draw(&mut v, 100, 30);
    let (bx, by) = find(&buf, "View source").unwrap();
    assert_eq!(
        v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), bx, by), area),
        Step::Open("https://github.com/alacritty/alacritty")
    );
    let (dx, dy) = find(&buf, "Done").unwrap();
    assert_eq!(v.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), dx, dy), area), Step::Leave);
}

#[test]
fn view_source_and_done_answer_the_keyboard() {
    let mut v = view();
    draw(&mut v, 100, 30);
    v.handle_key(key(KeyCode::Tab));
    assert_eq!(v.focus(), Focus::ViewSource);
    v.select(ix("Ratatui"));
    assert_eq!(v.handle_key(key(KeyCode::Enter)), Step::Open("https://github.com/ratatui/ratatui"));
    let (buf, _) = draw(&mut v, 100, 30);
    let (bx, by) = find(&buf, "View source").unwrap();
    assert_eq!(buf[(bx, by)].bg, palette().cyan, "focused: the accent fill");
    v.handle_key(key(KeyCode::Tab));
    assert_eq!(v.focus(), Focus::Done);
    assert_eq!(v.handle_key(key(KeyCode::Char(' '))), Step::Leave);
    v.handle_key(key(KeyCode::Tab));
    assert_eq!(v.focus(), Focus::Search, "round to the search");
}

#[test]
fn the_license_scrolls_with_the_keys_and_the_wheel_and_stops_at_its_end() {
    let area = Rect::new(0, 0, 100, 30);
    let mut v = view();
    let (buf, _) = draw(&mut v, 100, 30);
    let (_, title_y) = find(&buf, "Apache License 2.0").unwrap();
    v.handle_key(key(KeyCode::PageDown));
    let page = v.scroll();
    assert!(page > 5, "a page: {page}");
    let (buf, _) = draw(&mut v, 100, 30);
    assert!(!text(&buf).contains("Apache License 2.0"), "the license's name scrolled off");
    assert!(text(&buf).contains("Alacritty Terminal"), "the header stays");
    v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(v.scroll(), page + 1);
    assert_eq!(v.browser().selected(), Some(0), "Shift+↓ scrolls, it doesn't move the list");
    v.handle_key(key(KeyCode::PageUp));
    v.handle_key(key(KeyCode::PageUp));
    assert_eq!(v.scroll(), 0);
    // The wheel over the detail scrolls it; over the list, the list.
    v.handle_mouse(mouse(MouseEventKind::ScrollDown, 60, title_y), area);
    assert_eq!(v.scroll(), 3);
    v.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 20), area);
    assert_eq!(v.scroll(), 3, "the list's wheel leaves the license");
    let (buf, _) = draw(&mut v, 100, 30);
    assert!(find_row(&buf, "Alacritty Terminal").is_none(), "the list scrolled a row");
    assert!(text(&buf).contains("Rust crates"));
    // To the end, and no further: the last line at the bottom.
    for _ in 0..200 {
        v.handle_key(key(KeyCode::PageDown));
    }
    let end = v.scroll();
    v.handle_key(key(KeyCode::PageDown));
    assert_eq!(v.scroll(), end);
    let (buf, _) = draw(&mut v, 100, 30);
    let screen = text(&buf);
    let last = screen.lines().last().unwrap();
    assert!(last.contains("limitations under the License."), "{screen}");
    // Another entry starts at the top of its license.
    v.handle_key(key(KeyCode::Down));
    assert_eq!(v.scroll(), 0);
}

#[test]
fn the_long_tail_scrolls_through_every_crate() {
    let mut v = view();
    v.select(crate::licenses::entries().len() - 1);
    let (buf, _) = draw(&mut v, 100, 30);
    assert!(text(&buf).contains("Licenses and notices"));
    v.handle_key(key(KeyCode::PageDown));
    let (buf, _) = draw(&mut v, 100, 30);
    assert!(text(&buf).contains(" · MIT"), "the crate list: {}", text(&buf));
    // Further down, the notices, each with the crates it applies to.
    let reached = (0..40).any(|_| {
        v.handle_key(key(KeyCode::PageDown));
        text(&draw(&mut v, 100, 30).0).contains("Applies to")
    });
    assert!(reached, "the notices follow the list");
}

#[test]
fn it_draws_in_the_light_theme() {
    crate::theme::with_appearance(Appearance::Light, || {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('/')));
        let (buf, _) = draw(&mut v, 100, 30);
        let (x, y) = find_row(&buf, "Alacritty Terminal").unwrap();
        assert_eq!(buf[(x, y)].bg, palette().selection);
        assert_eq!(buf[(x, y)].fg, palette().white);
        let (sx, sy) = find(&buf, "Search acknowledgements").unwrap();
        assert_eq!(buf[(sx, sy)].bg, palette().input_focus);
        assert_eq!(palette().selection, crate::theme::to_color(shelbi_app::theme::Theme::light().selection));
    });
}

#[test]
fn a_tiny_window_draws_without_panicking() {
    for (w, h) in [(1, 1), (10, 3), (20, 6), (30, 8), (44, 10)] {
        let mut v = view();
        v.set_query("a");
        draw(&mut v, w, h);
        v.handle_key(key(KeyCode::PageDown));
        v.handle_key(key(KeyCode::End));
        v.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0), Rect::new(0, 0, w, h));
        draw(&mut v, w, h);
    }
}
