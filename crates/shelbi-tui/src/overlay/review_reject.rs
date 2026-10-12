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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// What currently holds focus. The text area is the default focus so the
/// reviewer can start typing immediately; `Tab` cycles through the two buttons
/// and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    #[default]
    Text,
    Reject,
    Cancel,
}

impl Focus {
    /// Tab order: text area → Reject → Cancel → text area.
    fn next(self) -> Focus {
        match self {
            Focus::Text => Focus::Reject,
            Focus::Reject => Focus::Cancel,
            Focus::Cancel => Focus::Text,
        }
    }

    /// Reverse tab order (Shift+Tab).
    fn prev(self) -> Focus {
        match self {
            Focus::Text => Focus::Cancel,
            Focus::Reject => Focus::Text,
            Focus::Cancel => Focus::Reject,
        }
    }
}

/// A minimal multi-line text buffer with a cursor — enough to compose a
/// rejection reason. Lines are stored as `Vec<char>` so cursor columns are plain
/// indices (no UTF-8 byte-boundary bookkeeping), and there is always at least
/// one line so `row`/`col` are never out of range.
#[derive(Debug, Clone)]
struct TextArea {
    lines: Vec<Vec<char>>,
    row: usize,
    col: usize,
}

impl Default for TextArea {
    fn default() -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
        }
    }
}

impl TextArea {
    fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_blank(&self) -> bool {
        self.text().trim().is_empty()
    }

    fn insert(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
    }

    fn newline(&mut self) {
        let tail = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.lines[self.row].remove(self.col - 1);
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].extend(cur);
        }
    }

    fn delete(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(next);
        }
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    fn move_right(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else {
            self.col = 0;
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else {
            self.col = self.lines[self.row].len();
        }
    }

    fn move_home(&mut self) {
        self.col = 0;
    }

    fn move_end(&mut self) {
        self.col = self.lines[self.row].len();
    }
}

/// The result of feeding one key to the prompt (internal decision table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Continue,
    Submit,
    Cancel,
}

/// What feeding one key/click into the prompt resolved to. The caller keeps the
/// overlay open on [`Step::Continue`], reads [`RejectPrompt::reason`] on
/// [`Step::Submit`], and discards on [`Step::Cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Submit,
    Cancel,
}

/// A clickable region of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Reject,
    Cancel,
}

/// The reject-reason prompt — the multi-line reason buffer and current focus.
#[derive(Debug, Clone, Default)]
pub struct RejectPrompt {
    text: TextArea,
    focus: Focus,
}

impl RejectPrompt {
    pub fn new() -> Self {
        Self::default()
    }

    /// The composed reason, newlines preserved verbatim. Read this on
    /// [`Step::Submit`].
    pub fn reason(&self) -> String {
        self.text.text()
    }

    /// Feed one key event, applying edits/navigation in place.
    pub fn handle_key(&mut self, key: KeyEvent) -> Step {
        match self.key(key) {
            Outcome::Continue => Step::Continue,
            Outcome::Submit => Step::Submit,
            Outcome::Cancel => Step::Cancel,
        }
    }

    /// Feed a left-click at `(col, row)` within the popup `area`. A click on
    /// `[ Reject ]` submits (a blank reason is still rejected); `[ Cancel ]`
    /// cancels. Focus follows the click so the highlight tracks what was pressed.
    pub fn handle_click(&mut self, area: Rect, col: u16, row: u16) -> Step {
        match hit_test(area, col, row) {
            Some(Hit::Reject) => {
                self.focus = Focus::Reject;
                match self.try_submit() {
                    Outcome::Submit => Step::Submit,
                    _ => Step::Continue,
                }
            }
            Some(Hit::Cancel) => Step::Cancel,
            None => Step::Continue,
        }
    }

    /// Paint the prompt into `area` (the whole popup pane in the tmux runtime, a
    /// centered overlay rect in the single-process TUI).
    pub fn render(&self, f: &mut Frame, area: Rect) {
        render(f, area, self)
    }

    /// The internal decision table. See the module docs for the bindings.
    fn key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Ctrl-D is the dedicated submit key — reachable from the text area
        // without leaving it, and from the buttons too.
        if ctrl && matches!(key.code, KeyCode::Char('d') | KeyCode::Char('D')) {
            return self.try_submit();
        }

        match key.code {
            KeyCode::Esc => Outcome::Cancel,
            KeyCode::Tab => {
                self.focus = self.focus.next();
                Outcome::Continue
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                Outcome::Continue
            }

            // --- Button focus ---------------------------------------------
            KeyCode::Enter if self.focus == Focus::Reject => self.try_submit(),
            KeyCode::Enter if self.focus == Focus::Cancel => Outcome::Cancel,
            KeyCode::Char(' ') if self.focus == Focus::Reject => self.try_submit(),
            KeyCode::Char(' ') if self.focus == Focus::Cancel => Outcome::Cancel,
            KeyCode::Left | KeyCode::Right if self.focus != Focus::Text => {
                self.focus = match self.focus {
                    Focus::Cancel => Focus::Reject,
                    _ => Focus::Cancel,
                };
                Outcome::Continue
            }

            // --- Text-area focus ------------------------------------------
            KeyCode::Enter if self.focus == Focus::Text => {
                self.text.newline();
                Outcome::Continue
            }
            KeyCode::Backspace if self.focus == Focus::Text => {
                self.text.backspace();
                Outcome::Continue
            }
            KeyCode::Delete if self.focus == Focus::Text => {
                self.text.delete();
                Outcome::Continue
            }
            KeyCode::Left if self.focus == Focus::Text => {
                self.text.move_left();
                Outcome::Continue
            }
            KeyCode::Right if self.focus == Focus::Text => {
                self.text.move_right();
                Outcome::Continue
            }
            KeyCode::Up if self.focus == Focus::Text => {
                self.text.move_up();
                Outcome::Continue
            }
            KeyCode::Down if self.focus == Focus::Text => {
                self.text.move_down();
                Outcome::Continue
            }
            KeyCode::Home if self.focus == Focus::Text => {
                self.text.move_home();
                Outcome::Continue
            }
            KeyCode::End if self.focus == Focus::Text => {
                self.text.move_end();
                Outcome::Continue
            }
            KeyCode::Char(c) if self.focus == Focus::Text && !ctrl => {
                self.text.insert(c);
                Outcome::Continue
            }

            _ => Outcome::Continue,
        }
    }

    /// A reason must be non-blank to submit; a blank submit is a no-op that keeps
    /// the popup open (matching the old inline prompt).
    fn try_submit(&self) -> Outcome {
        if self.text.is_blank() {
            Outcome::Continue
        } else {
            Outcome::Submit
        }
    }
}

const REJECT_LABEL: &str = "[ Reject ]";
const CANCEL_LABEL: &str = "[ Cancel ]";
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
        .border_style(Style::default().fg(Color::Red))
        .title(" Reject task ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // label / text area (grows to fill) / spacer / buttons / hint.
    let rows = body_rows(inner);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Reason for rejecting:",
            Style::default().fg(Color::Gray),
        ))),
        rows[0],
    );

    render_textarea(f, prompt, rows[1]);

    let (reject_rect, cancel_rect) = button_row(rows[3]);
    f.render_widget(
        Paragraph::new(Line::from(button(
            REJECT_LABEL,
            prompt.focus == Focus::Reject,
        ))),
        reject_rect,
    );
    f.render_widget(
        Paragraph::new(Line::from(button(
            CANCEL_LABEL,
            prompt.focus == Focus::Cancel,
        ))),
        cancel_rect,
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Ctrl-D reject · Enter newline · Esc cancel · Tab focus",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[4],
    );
}

/// The bordered, multi-line reason area. Its border brightens to cyan when
/// focused, and a reverse-video block cursor marks the caret. When the caret
/// scrolls past the visible height the content scrolls to keep it in view.
fn render_textarea(f: &mut Frame, prompt: &RejectPrompt, area: Rect) {
    let focused = prompt.focus == Focus::Text;
    let border = if focused { Color::Cyan } else { Color::DarkGray };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let text_area = block.inner(area);
    f.render_widget(block, area);

    let visible = text_area.height.max(1) as usize;
    let scroll = prompt.text.row.saturating_sub(visible.saturating_sub(1));

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
    let ta = &prompt.text;
    let cursor = Style::default().add_modifier(Modifier::REVERSED);
    let plain = Style::default().fg(Color::White);
    ta.lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if !(focused && i == ta.row) {
                return Line::from(Span::styled(line.iter().collect::<String>(), plain));
            }
            let before: String = line[..ta.col].iter().collect();
            let (under, after): (String, String) = if ta.col < line.len() {
                (line[ta.col].to_string(), line[ta.col + 1..].iter().collect())
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
            .fg(Color::Red)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    };
    Span::styled(label.to_string(), style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn press(prompt: &mut RejectPrompt, code: KeyCode) -> Outcome {
        prompt.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn press_ctrl(prompt: &mut RejectPrompt, code: KeyCode) -> Outcome {
        prompt.key(KeyEvent::new(code, KeyModifiers::CONTROL))
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
        prompt.focus = Focus::Reject;
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
        assert_eq!(RejectPrompt::default().focus, Focus::Text);
    }

    #[test]
    fn typing_appends_to_the_reason_and_backspace_deletes() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "abc");
        assert_eq!(prompt.text.text(), "abc");
        assert_eq!(press(&mut prompt, KeyCode::Backspace), Outcome::Continue);
        assert_eq!(prompt.text.text(), "ab");
    }

    #[test]
    fn space_is_typed_into_the_textarea_not_treated_as_activate() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "a b");
        assert_eq!(prompt.text.text(), "a b");
    }

    #[test]
    fn enter_inserts_a_newline_in_the_textarea_rather_than_submitting() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "first");
        assert_eq!(press(&mut prompt, KeyCode::Enter), Outcome::Continue);
        typed(&mut prompt, "second");
        assert_eq!(prompt.text.text(), "first\nsecond");
    }

    #[test]
    fn newline_splits_the_line_at_the_cursor() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "abcd");
        press(&mut prompt, KeyCode::Left);
        press(&mut prompt, KeyCode::Left);
        assert_eq!(press(&mut prompt, KeyCode::Enter), Outcome::Continue);
        assert_eq!(prompt.text.text(), "ab\ncd");
    }

    #[test]
    fn backspace_at_line_start_joins_with_the_previous_line() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "one");
        press(&mut prompt, KeyCode::Enter);
        typed(&mut prompt, "two");
        press(&mut prompt, KeyCode::Home);
        assert_eq!(press(&mut prompt, KeyCode::Backspace), Outcome::Continue);
        assert_eq!(prompt.text.text(), "onetwo");
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
        assert_eq!(prompt.text.text(), "onetwo");
    }

    #[test]
    fn arrow_up_down_move_between_lines_and_clamp_the_column() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "long line");
        press(&mut prompt, KeyCode::Enter);
        typed(&mut prompt, "hi");
        press(&mut prompt, KeyCode::Up);
        typed(&mut prompt, "X");
        assert_eq!(prompt.text.text(), "loXng line\nhi");
    }

    #[test]
    fn ctrl_d_submits_a_non_blank_reason_from_the_textarea() {
        let mut prompt = RejectPrompt::default();
        assert_eq!(press_ctrl(&mut prompt, KeyCode::Char('d')), Outcome::Continue);
        typed(&mut prompt, "please fix\nthe null deref");
        assert_eq!(press_ctrl(&mut prompt, KeyCode::Char('d')), Outcome::Submit);
        assert_eq!(prompt.text.text(), "please fix\nthe null deref");
    }

    #[test]
    fn ctrl_d_submits_even_when_a_button_holds_focus() {
        let mut prompt = text_prompt("fix it");
        prompt.focus = Focus::Cancel;
        assert_eq!(press_ctrl(&mut prompt, KeyCode::Char('d')), Outcome::Submit);
    }

    #[test]
    fn esc_cancels_from_any_focus() {
        for focus in [Focus::Text, Focus::Reject, Focus::Cancel] {
            let mut prompt = text_prompt("some reason");
            prompt.focus = focus;
            assert_eq!(press(&mut prompt, KeyCode::Esc), Outcome::Cancel, "focus={focus:?}");
        }
    }

    #[test]
    fn tab_cycles_focus_through_textarea_and_both_buttons() {
        let mut prompt = RejectPrompt::default();
        assert_eq!(prompt.focus, Focus::Text);
        press(&mut prompt, KeyCode::Tab);
        assert_eq!(prompt.focus, Focus::Reject);
        press(&mut prompt, KeyCode::Tab);
        assert_eq!(prompt.focus, Focus::Cancel);
        press(&mut prompt, KeyCode::Tab);
        assert_eq!(prompt.focus, Focus::Text);
        press(&mut prompt, KeyCode::BackTab);
        assert_eq!(prompt.focus, Focus::Cancel);
    }

    #[test]
    fn enter_or_space_on_the_reject_button_submits_a_non_blank_reason() {
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let mut prompt = text_prompt("fix it");
            prompt.focus = Focus::Reject;
            assert_eq!(press(&mut prompt, code), Outcome::Submit, "{code:?}");
        }
    }

    #[test]
    fn reject_button_cannot_submit_a_blank_reason() {
        let mut prompt = text_prompt("   ");
        prompt.focus = Focus::Reject;
        assert_eq!(press(&mut prompt, KeyCode::Enter), Outcome::Continue);
        assert_eq!(press(&mut prompt, KeyCode::Char(' ')), Outcome::Continue);
    }

    #[test]
    fn enter_or_space_on_the_cancel_button_cancels() {
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let mut prompt = text_prompt("fix it");
            prompt.focus = Focus::Cancel;
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
        prompt.focus = Focus::Reject;
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
        let mut prompt = RejectPrompt {
            focus: Focus::Reject,
            ..Default::default()
        };
        press(&mut prompt, KeyCode::Right);
        assert_eq!(prompt.focus, Focus::Cancel);
        press(&mut prompt, KeyCode::Left);
        assert_eq!(prompt.focus, Focus::Reject);
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "ab");
        press(&mut prompt, KeyCode::Left);
        assert_eq!(prompt.focus, Focus::Text, "arrows keep textarea focus");
        typed(&mut prompt, "X");
        assert_eq!(prompt.text.text(), "aXb");
    }
}
