//! The reject-reason prompt — shared overlay logic + rendering.
//!
//! Reviewers routinely need multi-paragraph feedback (findings, repro steps,
//! rationale), so the reason field is a real multi-line editor: `Enter` inserts
//! a line break, and submitting is a distinct action — `Ctrl-D` from anywhere,
//! or `Tab` to the `[ Reject ]` button and press `Enter`/`Space`. Arrow keys,
//! `Home`/`End`, `Backspace` (across line boundaries) and `Delete` edit the
//! text. A blank reason can't submit.
//!
//! One implementation, two callers: the in-process TUI overlay (removing-tmux
//! Phase 4d) returns the typed reason to the review flow as a value (reading
//! [`RejectPrompt::reason`] on [`Step::Submit`]); the legacy tmux
//! `shelbi __review-reject-reason` popup writes it (newlines preserved) to its
//! `--out` temp file. [`RejectPrompt::handle_key`] / [`RejectPrompt::handle_click`]
//! fold the decision and its application into one step so both callers share the
//! driver, not just the renderer.

use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use crate::theme::palette;

// The prompt's state machine is shared with the desktop app
// (`shelbi_app::overlay::review_reject`).
pub use shelbi_app::overlay::review_reject::{RejectFocus as Focus, Step};
use shelbi_app::overlay::review_reject::{self as shared, CANCEL_LABEL, REJECT_LABEL};

/// A clickable region of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Reject,
    Cancel,
}

/// The reject-reason prompt — the shared multi-line reason buffer and focus.
#[derive(Debug, Clone, Default)]
pub struct RejectPrompt {
    inner: shared::RejectPrompt,
}

impl RejectPrompt {
    pub fn new() -> Self {
        Self::default()
    }

    /// The composed reason, newlines preserved verbatim. Read this on
    /// [`Step::Submit`].
    pub fn reason(&self) -> String {
        self.inner.reason()
    }

    /// The focused control.
    pub fn focus(&self) -> Focus {
        self.inner.focus()
    }

    /// Feed one key event, applying edits/navigation in place.
    pub fn handle_key(&mut self, key: KeyEvent) -> Step {
        match crate::keymap::chord_from_event(key) {
            Some(chord) => self.inner.handle_key(chord),
            None => Step::Continue,
        }
    }

    /// Feed a left-click at `(col, row)` within the popup `area`. A click on
    /// `[ Reject ]` submits (a blank reason is still rejected); `[ Cancel ]`
    /// cancels. Focus follows the click so the highlight tracks what was pressed.
    pub fn handle_click(&mut self, area: Rect, col: u16, row: u16) -> Step {
        match hit_test(area, col, row) {
            Some(Hit::Reject) => self.inner.click_reject(),
            Some(Hit::Cancel) => self.inner.click_cancel(),
            None => Step::Continue,
        }
    }

    /// Paint the prompt into `area` (the whole popup pane in the tmux runtime, a
    /// centered overlay rect in the single-process TUI).
    pub fn render(&self, f: &mut Frame, area: Rect) {
        render(f, area, self)
    }
}

/// Spaces drawn between the two buttons — must match the group centering below.
const BUTTON_GAP: u16 = 3;

/// Split the modal's inner area into its five stacked rows: label / text area
/// (grows to fill) / spacer / buttons / hint. Shared by [`render`] and
/// [`hit_test`] so a click lands on exactly the button cells that were drawn.
fn body_rows(inner: Rect) -> std::rc::Rc<[Rect]> {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner)
}

/// Rects of the `[ Reject ]` / `[ Cancel ]` buttons within their row, centered
/// as a group so drawing and hit-testing agree on the exact cells.
fn button_row(row: Rect) -> (Rect, Rect) {
    let reject_w = REJECT_LABEL.chars().count() as u16;
    let cancel_w = CANCEL_LABEL.chars().count() as u16;
    let total = reject_w + BUTTON_GAP + cancel_w;
    let start = row.x + row.width.saturating_sub(total) / 2;
    (
        Rect {
            x: start,
            y: row.y,
            width: reject_w,
            height: 1,
        },
        Rect {
            x: start + reject_w + BUTTON_GAP,
            y: row.y,
            width: cancel_w,
            height: 1,
        },
    )
}

/// Map a click at `(col, row)` to the button under it, or `None` for dead space.
fn hit_test(area: Rect, col: u16, row: u16) -> Option<Hit> {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let rows = body_rows(inner);
    let (reject, cancel) = button_row(rows[3]);
    if within(reject, col, row) {
        return Some(Hit::Reject);
    }
    if within(cancel, col, row) {
        return Some(Hit::Cancel);
    }
    None
}

fn within(rect: Rect, col: u16, row: u16) -> bool {
    row == rect.y && col >= rect.x && col < rect.x + rect.width
}

fn render(f: &mut Frame, area: Rect, prompt: &RejectPrompt) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette().ansi_red))
        .title(format!(" {} ", shared::TITLE));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // label / text area (grows to fill) / spacer / buttons / hint.
    let rows = body_rows(inner);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            shared::LABEL,
            Style::default().fg(palette().ansi_gray),
        ))),
        rows[0],
    );

    render_textarea(f, prompt, rows[1]);

    let (reject_rect, cancel_rect) = button_row(rows[3]);
    f.render_widget(
        Paragraph::new(Line::from(button(
            REJECT_LABEL,
            prompt.focus() == Focus::Reject,
        ))),
        reject_rect,
    );
    f.render_widget(
        Paragraph::new(Line::from(button(
            CANCEL_LABEL,
            prompt.focus() == Focus::Cancel,
        ))),
        cancel_rect,
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            shared::HINT,
            Style::default().fg(palette().ansi_dark_gray),
        ))),
        rows[4],
    );
}

/// The bordered, multi-line reason area. Its border brightens to cyan when
/// focused, and a reverse-video block cursor marks the caret. When the caret
/// scrolls past the visible height the content scrolls to keep it in view.
fn render_textarea(f: &mut Frame, prompt: &RejectPrompt, area: Rect) {
    let focused = prompt.focus() == Focus::Text;
    let border = if focused { palette().ansi_cyan } else { palette().ansi_dark_gray };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let text_area = block.inner(area);
    f.render_widget(block, area);

    let visible = text_area.height.max(1) as usize;
    let (row, _) = prompt.inner.text().cursor();
    let scroll = row.saturating_sub(visible.saturating_sub(1));

    let lines = textarea_lines(prompt, focused);
    f.render_widget(
        Paragraph::new(Text::from(lines)).scroll((scroll as u16, 0)),
        text_area,
    );
}

/// Render each buffer line to a styled `Line`. On the caret's line (when
/// focused) the char under the caret — or a trailing space at end-of-line — is
/// drawn reverse-video so the caret is visible mid-line and at the end.
fn textarea_lines(prompt: &RejectPrompt, focused: bool) -> Vec<Line<'static>> {
    let ta = prompt.inner.text();
    let (row, col) = ta.cursor();
    let cursor = Style::default().add_modifier(Modifier::REVERSED);
    let plain = Style::default().fg(palette().ansi_white);
    ta.lines()
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if !(focused && i == row) {
                return Line::from(Span::styled(line.iter().collect::<String>(), plain));
            }
            let before: String = line[..col].iter().collect();
            let (under, after): (String, String) = if col < line.len() {
                (line[col].to_string(), line[col + 1..].iter().collect())
            } else {
                (" ".to_string(), String::new())
            };
            Line::from(vec![
                Span::styled(before, plain),
                Span::styled(under, cursor),
                Span::styled(after, plain),
            ])
        })
        .collect()
}

/// A button span. The focused button is reverse-video + bold; an unfocused
/// button is plain red text (Reject/Cancel share the destructive tint).
fn button(label: &str, focused: bool) -> Span<'_> {
    let style = if focused {
        Style::default()
            .fg(palette().ansi_red)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(palette().ansi_red)
    };
    Span::styled(label.to_string(), style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::Step as Outcome;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn press(prompt: &mut RejectPrompt, code: KeyCode) -> Outcome {
        prompt.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn press_ctrl(prompt: &mut RejectPrompt, code: KeyCode) -> Outcome {
        prompt.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL))
    }

    fn typed(prompt: &mut RejectPrompt, text: &str) {
        for c in text.chars() {
            assert_eq!(press(prompt, KeyCode::Char(c)), Outcome::Continue);
        }
    }

    fn text_prompt(reason: &str) -> RejectPrompt {
        let mut prompt = RejectPrompt::default();
        for c in reason.chars() {
            if c == '\n' {
                press(&mut prompt, KeyCode::Enter);
            } else {
                press(&mut prompt, KeyCode::Char(c));
            }
        }
        prompt
    }

    /// The prompt (border, title, the textarea focused and unfocused with its
    /// cursor, Reject and Cancel each focused) is readable on a light
    /// terminal.
    #[test]
    fn renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            for (name, focus) in [
                ("review-reject-text", Focus::Text),
                ("review-reject-reject", Focus::Reject),
                ("review-reject-cancel", Focus::Cancel),
            ] {
                let mut prompt = text_prompt("Tests fail on CI.\nPlease rebase.");
                prompt.inner.set_focus(focus);
                let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
                term.draw(|f| render(f, f.area(), &prompt)).unwrap();
                crate::theme::assert_light_readable(name, term.backend().buffer());
            }
            let empty = RejectPrompt::default();
            let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
            term.draw(|f| render(f, f.area(), &empty)).unwrap();
            crate::theme::assert_light_readable("review-reject-empty", term.backend().buffer());
        });
    }

    fn render_to_string(prompt: &RejectPrompt, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, f.area(), prompt)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn popover_renders_frame_textarea_and_both_buttons() {
        let prompt = text_prompt("needs a test");
        let out = render_to_string(&prompt, 60, 16);
        assert!(out.contains("Reject task"), "titled frame: {out}");
        assert!(out.contains("Reason for rejecting:"), "label: {out}");
        assert!(out.contains("needs a test"), "text area shows the reason: {out}");
        assert!(out.contains("[ Reject ]"), "reject button: {out}");
        assert!(out.contains("[ Cancel ]"), "cancel button: {out}");
    }

    #[test]
    fn hint_advertises_the_submit_and_newline_keys() {
        let out = render_to_string(&RejectPrompt::default(), 60, 16);
        assert!(out.contains("Ctrl-D reject"), "submit hint shown: {out}");
        assert!(out.contains("Enter newline"), "newline hint shown: {out}");
    }

    #[test]
    fn multiple_lines_render_on_separate_rows() {
        let prompt = text_prompt("line one\nline two");
        let out = render_to_string(&prompt, 60, 16);
        let one = out.lines().position(|r| r.contains("line one"));
        let two = out.lines().position(|r| r.contains("line two"));
        assert!(one.is_some() && two.is_some(), "both lines rendered: {out}");
        assert!(one.unwrap() < two.unwrap(), "second line is below first: {out}");
    }

    #[test]
    fn focused_button_is_visibly_highlighted() {
        let mut prompt = text_prompt("x");
        prompt.inner.set_focus(Focus::Reject);
        let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
        term.draw(|f| render(f, f.area(), &prompt)).unwrap();
        let buf = term.backend().buffer().clone();
        let reversed_cells = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED))
        });
        assert!(reversed_cells, "the focused button must be reverse-video");
    }

    #[test]
    fn textarea_is_focused_by_default() {
        assert_eq!(RejectPrompt::default().focus(), Focus::Text);
    }

    #[test]
    fn typing_appends_to_the_reason_and_backspace_deletes() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "abc");
        assert_eq!(prompt.reason(), "abc");
        assert_eq!(press(&mut prompt, KeyCode::Backspace), Outcome::Continue);
        assert_eq!(prompt.reason(), "ab");
    }

    #[test]
    fn space_is_typed_into_the_textarea_not_treated_as_activate() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "a b");
        assert_eq!(prompt.reason(), "a b");
    }

    #[test]
    fn enter_inserts_a_newline_in_the_textarea_rather_than_submitting() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "first");
        assert_eq!(press(&mut prompt, KeyCode::Enter), Outcome::Continue);
        typed(&mut prompt, "second");
        assert_eq!(prompt.reason(), "first\nsecond");
    }

    #[test]
    fn newline_splits_the_line_at_the_cursor() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "abcd");
        press(&mut prompt, KeyCode::Left);
        press(&mut prompt, KeyCode::Left);
        assert_eq!(press(&mut prompt, KeyCode::Enter), Outcome::Continue);
        assert_eq!(prompt.reason(), "ab\ncd");
    }

    #[test]
    fn backspace_at_line_start_joins_with_the_previous_line() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "one");
        press(&mut prompt, KeyCode::Enter);
        typed(&mut prompt, "two");
        press(&mut prompt, KeyCode::Home);
        assert_eq!(press(&mut prompt, KeyCode::Backspace), Outcome::Continue);
        assert_eq!(prompt.reason(), "onetwo");
    }

    #[test]
    fn delete_at_line_end_pulls_up_the_next_line() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "one");
        press(&mut prompt, KeyCode::Enter);
        typed(&mut prompt, "two");
        press(&mut prompt, KeyCode::Up);
        press(&mut prompt, KeyCode::End);
        assert_eq!(press(&mut prompt, KeyCode::Delete), Outcome::Continue);
        assert_eq!(prompt.reason(), "onetwo");
    }

    #[test]
    fn arrow_up_down_move_between_lines_and_clamp_the_column() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "long line");
        press(&mut prompt, KeyCode::Enter);
        typed(&mut prompt, "hi");
        press(&mut prompt, KeyCode::Up);
        typed(&mut prompt, "X");
        assert_eq!(prompt.reason(), "loXng line\nhi");
    }

    #[test]
    fn ctrl_d_submits_a_non_blank_reason_from_the_textarea() {
        let mut prompt = RejectPrompt::default();
        assert_eq!(press_ctrl(&mut prompt, KeyCode::Char('d')), Outcome::Continue);
        typed(&mut prompt, "please fix\nthe null deref");
        assert_eq!(press_ctrl(&mut prompt, KeyCode::Char('d')), Outcome::Submit);
        assert_eq!(prompt.reason(), "please fix\nthe null deref");
    }

    #[test]
    fn ctrl_d_submits_even_when_a_button_holds_focus() {
        let mut prompt = text_prompt("fix it");
        prompt.inner.set_focus(Focus::Cancel);
        assert_eq!(press_ctrl(&mut prompt, KeyCode::Char('d')), Outcome::Submit);
    }

    #[test]
    fn esc_cancels_from_any_focus() {
        for focus in [Focus::Text, Focus::Reject, Focus::Cancel] {
            let mut prompt = text_prompt("some reason");
            prompt.inner.set_focus(focus);
            assert_eq!(press(&mut prompt, KeyCode::Esc), Outcome::Cancel, "focus={focus:?}");
        }
    }

    #[test]
    fn tab_cycles_focus_through_textarea_and_both_buttons() {
        let mut prompt = RejectPrompt::default();
        assert_eq!(prompt.focus(), Focus::Text);
        press(&mut prompt, KeyCode::Tab);
        assert_eq!(prompt.focus(), Focus::Reject);
        press(&mut prompt, KeyCode::Tab);
        assert_eq!(prompt.focus(), Focus::Cancel);
        press(&mut prompt, KeyCode::Tab);
        assert_eq!(prompt.focus(), Focus::Text);
        press(&mut prompt, KeyCode::BackTab);
        assert_eq!(prompt.focus(), Focus::Cancel);
    }

    #[test]
    fn enter_or_space_on_the_reject_button_submits_a_non_blank_reason() {
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let mut prompt = text_prompt("fix it");
            prompt.inner.set_focus(Focus::Reject);
            assert_eq!(press(&mut prompt, code), Outcome::Submit, "{code:?}");
        }
    }

    #[test]
    fn reject_button_cannot_submit_a_blank_reason() {
        let mut prompt = text_prompt("   ");
        prompt.inner.set_focus(Focus::Reject);
        assert_eq!(press(&mut prompt, KeyCode::Enter), Outcome::Continue);
        assert_eq!(press(&mut prompt, KeyCode::Char(' ')), Outcome::Continue);
    }

    #[test]
    fn enter_or_space_on_the_cancel_button_cancels() {
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let mut prompt = text_prompt("fix it");
            prompt.inner.set_focus(Focus::Cancel);
            assert_eq!(press(&mut prompt, code), Outcome::Cancel, "{code:?}");
        }
    }

    #[test]
    fn hit_test_maps_clicks_to_the_reject_and_cancel_buttons() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 16,
        };
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let rows = body_rows(inner);
        let (reject, cancel) = button_row(rows[3]);

        assert_eq!(hit_test(area, reject.x, reject.y), Some(Hit::Reject));
        assert_eq!(
            hit_test(area, reject.x + reject.width - 1, reject.y),
            Some(Hit::Reject)
        );
        assert_eq!(hit_test(area, cancel.x, cancel.y), Some(Hit::Cancel));
        assert_eq!(
            hit_test(area, cancel.x + cancel.width - 1, cancel.y),
            Some(Hit::Cancel)
        );
        assert_eq!(hit_test(area, reject.x + reject.width, reject.y), None);
        assert_eq!(hit_test(area, reject.x, reject.y - 1), None);
    }

    #[test]
    fn handle_click_submits_or_cancels_via_the_buttons() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 16,
        };
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let rows = body_rows(inner);
        let (reject, cancel) = button_row(rows[3]);

        // Clicking Reject with a non-blank reason submits.
        let mut prompt = text_prompt("fix it");
        assert_eq!(prompt.handle_click(area, reject.x, reject.y), Step::Submit);
        // Clicking Reject with a blank reason keeps it open (focus moves).
        let mut prompt = RejectPrompt::default();
        assert_eq!(prompt.handle_click(area, reject.x, reject.y), Step::Continue);
        // Clicking Cancel cancels.
        let mut prompt = text_prompt("fix it");
        assert_eq!(prompt.handle_click(area, cancel.x, cancel.y), Step::Cancel);
    }

    #[test]
    fn handle_key_exposes_submit_and_cancel_as_steps() {
        let mut prompt = text_prompt("a reason");
        prompt.inner.set_focus(Focus::Reject);
        assert_eq!(
            prompt.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Step::Submit
        );
        assert_eq!(prompt.reason(), "a reason");
        let mut prompt = RejectPrompt::default();
        assert_eq!(
            prompt.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Step::Cancel
        );
    }

    #[test]
    fn render_draws_the_buttons_at_their_hit_test_rects() {
        let (w, h) = (60u16, 16u16);
        let prompt = text_prompt("x");
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, f.area(), &prompt)).unwrap();
        let buf = term.backend().buffer().clone();

        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let rows = body_rows(inner);
        let (reject, _cancel) = button_row(rows[3]);
        let drawn: String = (0..reject.width)
            .map(|dx| buf[(reject.x + dx, reject.y)].symbol().to_string())
            .collect();
        assert_eq!(drawn, REJECT_LABEL, "reject label sits on its hit rect");
    }

    #[test]
    fn arrow_keys_move_between_buttons_but_edit_the_cursor_in_the_textarea() {
        let mut prompt = RejectPrompt::default();
        prompt.inner.set_focus(Focus::Reject);
        press(&mut prompt, KeyCode::Right);
        assert_eq!(prompt.focus(), Focus::Cancel);
        press(&mut prompt, KeyCode::Left);
        assert_eq!(prompt.focus(), Focus::Reject);
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "ab");
        press(&mut prompt, KeyCode::Left);
        assert_eq!(prompt.focus(), Focus::Text, "arrows keep textarea focus");
        typed(&mut prompt, "X");
        assert_eq!(prompt.reason(), "aXb");
    }
}
