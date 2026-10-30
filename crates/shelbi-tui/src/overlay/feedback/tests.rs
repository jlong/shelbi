use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use shelbi_app::theme::Appearance;

fn render(dialog: &mut FeedbackDialog, w: u16, h: u16) -> (Buffer, Rect) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut rect = Rect::default();
    term.draw(|f| {
        rect = dialog.rect(f.area());
        dialog.render(f, rect);
    })
    .unwrap();
    (term.backend().buffer().clone(), rect)
}

fn row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()
}

/// The first cell of `needle`, scanning rows top to bottom.
fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    (0..buf.area.height).find_map(|y| {
        let cells: Vec<String> = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
        (0..cells.len()).find(|&x| cells[x..].concat().starts_with(needle)).map(|x| (x as u16, y))
    })
}

fn ch(c: char) -> KeyChord {
    KeyChord::new(Code::Char(c), Mods::NONE)
}

fn key(code: Code) -> KeyChord {
    KeyChord::new(code, Mods::NONE)
}

fn typed(d: &mut FeedbackDialog, text: &str) {
    for c in text.chars() {
        assert_eq!(d.key(ch(c)), Step::Stay);
    }
}

fn remembered(name: &str, email: &str) -> Remembered {
    Remembered { name: name.into(), email: email.into() }
}

/// A draft with every required field filled, the keyboard on Message.
fn filled() -> FeedbackDialog {
    let mut d = FeedbackDialog::new(remembered("Ada", "ada@example.com"), true);
    typed(&mut d, "It froze.");
    d
}

#[test]
fn the_form_lays_out_the_desktops_controls_in_its_order() {
    let mut d = FeedbackDialog::new(Remembered::default(), true);
    let (buf, rect) = render(&mut d, 100, 45);
    let order = [
        "📣",
        form::TITLE,
        form::NAME,
        form::EMAIL,
        "Bug Report",
        form::MESSAGE,
        form::system_info_label(),
        form::SEND,
        form::CANCEL,
    ];
    let ys: Vec<u16> = order.iter().map(|t| find(&buf, t).unwrap_or_else(|| panic!("{t} shows")).1).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "top to bottom in the desktop's order: {ys:?}");
    // The kinds share one row, in the control's order.
    let xs: Vec<(u16, u16)> = ["Bug Report", "Feature Request", "Other"].iter().map(|t| find(&buf, t).unwrap()).collect();
    assert!(xs.iter().all(|(_, y)| *y == xs[0].1) && xs[0].0 < xs[1].0 && xs[1].0 < xs[2].0, "{xs:?}");
    // Centered, PANEL_W wide, in the dialogs' border on the dialog fill.
    assert_eq!(rect.width, PANEL_W);
    assert_eq!(rect.x, (100 - PANEL_W) / 2);
    crate::overlay::assert_dialog_border(&buf, rect).unwrap();
    assert_eq!(buf[(rect.x, rect.y)].bg, palette().dialog, "the border's on the fill");
    assert_eq!(buf[(rect.x + 1, rect.y + 1)].bg, palette().dialog, "the panel is filled");
    // The content sits a column inside the border, as the palette's does.
    let (nx, _) = find(&buf, form::NAME).unwrap();
    assert!(nx >= rect.x + 2, "Name inside the padding");
    // The title's centered over the form.
    let (tx, _) = find(&buf, form::TITLE).unwrap();
    let mid = rect.x + rect.width / 2;
    assert!(tx < mid && tx + form::TITLE.len() as u16 > mid);
}

#[test]
fn the_fields_rest_on_the_dialogs_control_fill_and_focus_tints_them() {
    let mut d = FeedbackDialog::new(remembered("Ada", ""), true);
    assert_eq!(d.form().focus(), Focus::Email, "the first empty field has the keyboard");
    let (buf, _) = render(&mut d, 100, 45);
    let (x, y) = find(&buf, "Ada").unwrap();
    assert_eq!(buf[(x, y)].bg, palette().dialog_control, "Name at rest");
    assert_eq!(buf[(x, y - 1)].symbol(), BLEED_ABOVE);
    assert_eq!(buf[(x, y - 1)].fg, palette().dialog_control);
    assert_eq!(buf[(x, y - 1)].bg, palette().dialog, "its cap sits on the panel");
    let (x, y) = find(&buf, form::EMAIL).unwrap();
    assert_eq!(buf[(x, y)].bg, palette().input_focus, "Email has the keyboard");
    assert_eq!(buf[(x - 1, y)].bg, palette().white, "the caret before its placeholder");
}

#[test]
fn a_short_terminal_drops_the_badge_then_the_padding_and_pins_the_buttons() {
    let mut d = FeedbackDialog::new(Remembered::default(), true);
    let (buf, rect) = render(&mut d, 100, 30);
    assert!(find(&buf, "📣").is_none(), "no room for the badge");
    assert!(find(&buf, form::MESSAGE).is_some() && find(&buf, form::CANCEL).is_some(), "the rest fits");
    assert!(rect.height <= 30);

    let (buf, rect) = render(&mut d, 60, 20);
    assert_eq!((rect.width, rect.height), (56, 20), "the whole height, two columns in");
    let (_, send) = find(&buf, form::SEND).expect("Send Message stays in view");
    let (_, cancel) = find(&buf, form::CANCEL).expect("Cancel too");
    assert_eq!(cancel, rect.bottom() - 3, "pinned to the bottom");
    assert!(send < cancel);
    assert!(find(&buf, form::NAME).is_some(), "the focused field shows");
    // Tab on into Message: the fields scroll it into view.
    d.key(key(Code::Tab));
    d.key(key(Code::Tab));
    d.key(key(Code::Tab));
    assert_eq!(d.form().focus(), Focus::Message);
    let (buf, _) = render(&mut d, 60, 20);
    assert!(find(&buf, form::MESSAGE).is_some(), "{}", (0..20).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n"));
    assert!(find(&buf, form::NAME).is_none(), "Name scrolled away");
}

#[test]
fn without_emoji_the_header_is_its_title() {
    let mut d = FeedbackDialog::new(Remembered::default(), false);
    let (buf, _) = render(&mut d, 100, 45);
    assert!(find(&buf, "📣").is_none());
    assert!(find(&buf, form::TITLE).is_some());
}

#[test]
fn tab_walks_the_desktops_order_and_each_control_shows_its_focus() {
    let mut d = FeedbackDialog::new(Remembered::default(), true);
    let mut seen = vec![d.form().focus()];
    for _ in 0..7 {
        d.key(key(Code::Tab));
        seen.push(d.form().focus());
    }
    assert_eq!(seen, [Focus::Name, Focus::Email, Focus::Kind, Focus::Message, Focus::SystemInfo, Focus::Send, Focus::Cancel, Focus::Name]);
    d.key(KeyChord::new(Code::BackTab, Mods::SHIFT));
    assert_eq!(d.form().focus(), Focus::Cancel, "⇧Tab goes back, wrapping");
    d.key(KeyChord::new(Code::Tab, Mods::SHIFT));
    assert_eq!(d.form().focus(), Focus::Send);

    // Each focus paints: the track, the checkbox's label, the buttons.
    let at = |d: &mut FeedbackDialog, focus: Focus, needle: &str| {
        d.form.set_focus(focus);
        let (buf, _) = render(d, 100, 45);
        let (x, y) = find(&buf, needle).unwrap();
        buf[(x, y)].clone()
    };
    assert_eq!(at(&mut d, Focus::Kind, "Feature Request").bg, palette().input_focus);
    assert_eq!(at(&mut d, Focus::Kind, "Bug Report").bg, palette().segment, "the picked one's chip");
    assert_eq!(at(&mut d, Focus::SystemInfo, "Include").fg, palette().cyan);
    // Send and Cancel are Settings' buttons: Primary and Secondary, focus
    // a solid fill.
    assert_eq!(at(&mut d, Focus::Send, form::SEND).bg, palette().button_focus);
    assert_eq!(at(&mut d, Focus::Cancel, form::SEND).bg, palette().button_primary, "Send at rest");
    assert_eq!(at(&mut d, Focus::Cancel, form::CANCEL).bg, palette().button_focus);
    assert_eq!(at(&mut d, Focus::Send, form::CANCEL).bg, palette().button);
}

#[test]
fn the_keys_pick_the_kind_and_toggle_system_info_off_the_text_fields() {
    let mut d = FeedbackDialog::new(Remembered::default(), true);
    // In a text field Space and the arrows are its own.
    d.key(key(Code::Right));
    typed(&mut d, "A d");
    assert_eq!(d.form().name(), "A d");
    assert_eq!(d.form().kind(), Kind::Bug);
    d.form.set_focus(Focus::Kind);
    d.key(key(Code::Right));
    assert_eq!(d.form().kind(), Kind::Feature);
    d.form.set_focus(Focus::SystemInfo);
    d.key(ch(' '));
    assert!(!d.form().include_system_info());
}

#[test]
fn typing_edits_with_a_caret_and_a_paste_lands_in_the_field() {
    let mut d = FeedbackDialog::new(Remembered::default(), true);
    typed(&mut d, "Ada");
    d.key(key(Code::Left));
    d.key(key(Code::Backspace));
    assert_eq!(d.form().name(), "Aa");
    assert!(d.paste("\r\nLovelace\n"), "the field takes it");
    assert_eq!(d.form().name(), "A Lovelace a", "on one line");
    d.form.set_focus(Focus::Message);
    assert!(d.paste("one\r\ntwo"));
    assert_eq!(d.form().message(), "one\ntwo", "Message keeps its lines");
    d.key(key(Code::Enter));
    typed(&mut d, "three");
    assert_eq!(d.form().message(), "one\ntwo\nthree", "Return is Message's new line");
    d.form.set_focus(Focus::Send);
    assert!(!d.paste("x"), "no field has the keyboard");
}

#[test]
fn a_remembered_name_types_on_from_its_end() {
    let mut d = FeedbackDialog::new(remembered("Ada", ""), true);
    d.key(KeyChord::new(Code::BackTab, Mods::SHIFT));
    assert_eq!(d.form().focus(), Focus::Name);
    typed(&mut d, "!");
    assert_eq!(d.form().name(), "Ada!");
}

#[test]
fn a_missing_email_shows_under_the_form_and_tints_the_field() {
    let mut d = FeedbackDialog::new(Remembered::default(), true);
    d.form.set_focus(Focus::Message);
    typed(&mut d, "hello");
    d.form.set_focus(Focus::Name);
    assert_eq!(d.key(key(Code::Enter)), Step::Stay, "nothing sends");
    assert_eq!((d.form().error(), d.form().focus()), (Some(form::NO_EMAIL), Focus::Email));
    let (buf, _) = render(&mut d, 100, 45);
    let (x, y) = find(&buf, form::NO_EMAIL).expect("the error shows");
    assert_eq!(buf[(x, y)].fg, palette().red);
    let (_, sy) = find(&buf, form::SEND).unwrap();
    assert!(y < sy, "over the buttons");
    let (ex, ey) = find(&buf, form::EMAIL).unwrap();
    assert_eq!(buf[(ex, ey)].bg, palette().input_invalid, "Email is refused");
    // A cramped terminal keeps it in view with the buttons, under Email.
    let (buf, _) = render(&mut d, 60, 20);
    let (_, ey) = find(&buf, form::EMAIL).expect("the refused field shows");
    let (_, y) = find(&buf, form::NO_EMAIL).expect("so does why");
    assert!(ey < y && y < find(&buf, form::SEND).unwrap().1);
    typed(&mut d, "a@b.c");
    assert_eq!(d.form().error(), None, "typing clears it");
}

#[test]
fn return_sends_the_trimmed_draft_and_ctrl_return_sends_from_the_message() {
    let mut d = filled();
    assert_eq!(d.form().focus(), Focus::Message);
    assert_eq!(d.key(key(Code::Enter)), Step::Stay, "a new line, not a send");
    let Step::Send(sub) = d.key(KeyChord::new(Code::Enter, Mods::CONTROL)) else {
        panic!("Ctrl+Return sends");
    };
    assert_eq!(sub.message, "It froze.");
    assert_eq!(sub.name.as_deref(), Some("Ada"));
    let mut d = filled();
    assert!(matches!(d.key(KeyChord::new(Code::Enter, Mods::SUPER)), Step::Send(_)), "⌘Return too");
    let mut d = filled();
    d.form.set_focus(Focus::Kind);
    assert!(matches!(d.key(key(Code::Enter)), Step::Send(_)), "Return elsewhere is Send Message");
}

#[test]
fn clicks_reach_every_control() {
    let mut d = filled();
    let (buf, _) = render(&mut d, 100, 45);
    let click = |d: &mut FeedbackDialog, (x, y): (u16, u16)| d.handle_click(x, y);
    assert_eq!(click(&mut d, find(&buf, "Feature Request").unwrap()), Step::Stay);
    assert_eq!((d.form().kind(), d.form().focus()), (Kind::Feature, Focus::Kind));
    // A segment answers on its caps' rows too.
    let (ox, oy) = find(&buf, "Other").unwrap();
    click(&mut d, (ox, oy + 1));
    assert_eq!(d.form().kind(), Kind::Other);
    let (cx, cy) = find(&buf, "Include").unwrap();
    click(&mut d, (cx, cy));
    assert!(!d.form().include_system_info());
    assert_eq!(d.form().focus(), Focus::SystemInfo);
    let (nx, ny) = find(&buf, "Ada").unwrap();
    click(&mut d, (nx + 10, ny - 1));
    assert_eq!(d.form().focus(), Focus::Name, "the box's cap is on target");
    let (ex, ey) = find(&buf, "ada@example.com").unwrap();
    click(&mut d, (ex, ey + 1));
    assert_eq!(d.form().focus(), Focus::Email);
    let (mx, my) = find(&buf, "It froze.").unwrap();
    click(&mut d, (mx + 20, my + 3));
    assert_eq!(d.form().focus(), Focus::Message, "anywhere in Message's box");
    let (bx, by) = find(&buf, form::CANCEL).unwrap();
    assert_eq!(click(&mut d, (bx, by)), Step::Close);
    let (bx, by) = find(&buf, form::SEND).unwrap();
    let Step::Send(sub) = click(&mut d, (bx, by + 1)) else {
        panic!("Send Message sends");
    };
    assert_eq!((sub.kind, sub.include_system_info), (Kind::Other, false));
    assert_eq!(click(&mut d, (0, 0)), Step::Stay, "off the panel, nothing");
}

#[test]
fn sending_holds_then_the_answer_shows_the_error_or_the_thank_you() {
    let mut d = filled();
    assert!(matches!(d.key(KeyChord::new(Code::Enter, Mods::CONTROL)), Step::Send(_)));
    let (buf, _) = render(&mut d, 100, 45);
    assert!(find(&buf, form::SENDING).is_some(), "Sending… stands in for Send Message");
    let (x, y) = find(&buf, form::CANCEL).unwrap();
    assert_eq!(buf[(x, y)].fg, palette().muted, "Cancel waits");
    assert_eq!(d.key(key(Code::Esc)), Step::Stay, "a send that's out can't be left");
    assert_eq!(d.handle_click(x, y), Step::Stay);
    d.form.set_focus(Focus::Send);
    assert_eq!(d.key(key(Code::Enter)), Step::Stay, "nor sent again");

    d.finished(Err(FeedbackError::RateLimited));
    let (buf, _) = render(&mut d, 100, 45);
    let error = "Too many submissions. Please try again later.";
    let (ex, ey) = find(&buf, error).expect("the failure shows inline");
    assert_eq!(buf[(ex, ey)].fg, palette().red);
    assert!(find(&buf, "It froze.").is_some(), "the draft is kept");
    assert!(find(&buf, form::SEND).is_some());
    assert!(matches!(d.key(key(Code::Enter)), Step::Send(_)), "it can try again");

    d.finished(Ok(()));
    let (buf, rect) = render(&mut d, 100, 45);
    assert!(find(&buf, form::NAME).is_none(), "the form is gone");
    let title = find(&buf, form::THANKS_TITLE).expect("Thank you!").1;
    let heart = find(&buf, HEART).expect("the heart").1;
    let line = find(&buf, "Your input helps us build a better app.").expect("its line").1;
    let close = find(&buf, form::CLOSE).expect("Close").1;
    assert!(heart < title && title < line && line < close);
    assert!(find(&buf, "follow up if we have questions.").is_some());
    assert!(rect.height < 20, "sized to the thank-you");
    assert_eq!(d.key(key(Code::Tab)), Step::Stay);
    assert_eq!(d.form().focus(), Focus::Close);
    let (cx, cy) = find(&buf, form::CLOSE).unwrap();
    assert_eq!(d.handle_click(cx, cy), Step::Close);
    assert_eq!(d.key(key(Code::Enter)), Step::Close);
}

#[test]
fn cancel_and_esc_close_unsent() {
    let mut d = filled();
    assert_eq!(d.key(key(Code::Esc)), Step::Close);
    d.form.set_focus(Focus::Cancel);
    assert_eq!(d.key(key(Code::Enter)), Step::Close);
    assert_eq!(d.key(ch(' ')), Step::Close);
}

#[test]
fn a_long_message_grows_then_scrolls_with_its_caret() {
    let mut d = filled();
    for i in 0..12 {
        d.paste(&format!("line {i}\n"));
    }
    let (buf, _) = render(&mut d, 100, 50);
    assert!(find(&buf, "line 11").is_some(), "the caret's line shows");
    assert!(find(&buf, "It froze.").is_none(), "the first lines scroll away");
    let top = find(&buf, "line 4").expect("nine lines in view").1;
    let last = find(&buf, "line 11").unwrap().1;
    assert_eq!(last - top, 7, "nine rows tall at most: {top}..{last}");
}

#[test]
fn both_themes_paint_the_dialog_from_their_tokens() {
    for appearance in [Appearance::Dark, Appearance::Light] {
        crate::theme::with_appearance(appearance, || {
            let mut d = FeedbackDialog::new(Remembered::default(), true);
            let (buf, rect) = render(&mut d, 100, 45);
            assert_eq!(buf[(rect.x + 1, rect.y + 1)].bg, palette().dialog, "{appearance:?}");
            let (x, y) = find(&buf, form::EMAIL).unwrap();
            assert_eq!(buf[(x, y)].bg, palette().dialog_control, "{appearance:?}");
            let (x, y) = find(&buf, form::SEND).unwrap();
            assert_eq!((buf[(x, y)].bg, buf[(x, y)].fg), (palette().button_primary, palette().button_primary_text));
        });
    }
}

#[test]
fn the_pointer_lifts_the_buttons_and_never_a_kind_or_a_field() {
    // In light the fields and the kind's track share a button's fill.
    for appearance in [Appearance::Dark, Appearance::Light] {
        crate::theme::with_appearance(appearance, || {
            let mut d = FeedbackDialog::new(Remembered::default(), true);
            let (mut buf, _) = render(&mut d, 100, 45);
            let not_buttons = d.not_buttons();
            let mut buttons = crate::button::drawn(&buf, buf.area);
            buttons.retain(|b| !not_buttons.iter().any(|r| r.intersects(*b)));
            let (sx, sy) = find(&buf, form::SEND).unwrap();
            let (cx, cy) = find(&buf, form::CANCEL).unwrap();
            assert_eq!(buttons.len(), 2, "{appearance:?}: Send and Cancel only");
            assert!(crate::panel::in_rect(buttons[0], sx, sy) && crate::panel::in_rect(buttons[1], cx, cy));
            let (fx, fy) = find(&buf, "Feature Request").unwrap();
            let before = buf[(fx, fy)].bg;
            assert_eq!(crate::button::light_hovered(&mut buf, &buttons, Some((fx, fy))), None, "{appearance:?}");
            assert_eq!(buf[(fx, fy)].bg, before, "{appearance:?}: an unpicked kind keeps the track's fill");
        });
    }
}

/// Where Message's caret (the block in the strong text color) is drawn.
fn caret_at(buf: &Buffer) -> (u16, u16) {
    (0..buf.area.height)
        .find_map(|y| (0..buf.area.width).find(|&x| buf[(x, y)].bg == palette().white).map(|x| (x, y)))
        .expect("a caret")
}

#[test]
fn a_long_message_wraps_at_words_and_the_caret_moves_by_wrapped_row() {
    let mut d = FeedbackDialog::new(remembered("Ada", "ada@example.com"), true);
    let long = "It froze when I dragged a card. ".repeat(4);
    let long = long.trim_end();
    typed(&mut d, long);
    let (buf, _) = render(&mut d, 100, 45);
    let (first_x, first_y) = find(&buf, "It froze when").unwrap();
    let rows: Vec<String> = (first_y..first_y + 3).map(|y| row(&buf, y)).collect();
    assert!(rows.iter().all(|r| r.contains("froze") || r.contains("card")), "over several rows: {rows:#?}");
    // Each row breaks at a word: its text, read on from the box's left
    // edge, is whole words of the message.
    for r in &rows {
        let text: String = r.chars().skip(first_x as usize).collect::<String>();
        let text = text.trim().trim_end_matches(['▌', '▐', '│']).trim();
        assert!(long.contains(text), "{text:?} is a whole run of the message");
    }
    assert_eq!(d.form().message(), long, "wrapping adds no newline");
    let (_, caret_y) = caret_at(&buf);
    assert!(caret_y > first_y, "the caret ends the last wrapped row");
    d.key(key(Code::Up));
    let (buf, _) = render(&mut d, 100, 45);
    assert_eq!(caret_at(&buf).1, caret_y - 1, "up one wrapped row");
    // A click on the first row puts the caret there.
    d.handle_click(first_x + 3, first_y);
    let (buf, _) = render(&mut d, 100, 45);
    assert_eq!(caret_at(&buf), (first_x + 3, first_y));
}
