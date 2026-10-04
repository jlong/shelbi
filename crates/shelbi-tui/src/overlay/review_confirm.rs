//! The "Load for review" dialog — shared overlay logic + rendering.
//!
//! One dialog, three shapes, chosen by how many `review`-tagged slots the caller
//! passed:
//!
//! - **0 slots** — informational: "No review workspace is configured." Any key
//!   dismisses; nothing is loaded.
//! - **1 slot** — a yes/no confirm ("Load onto review workspace `review`?") with
//!   `[ Load ]` / `[ Cancel ]`.
//! - **>1 slots** — a picker listing every review slot with its free/occupied
//!   state; the user selects one, then `[ Load ]` / `[ Cancel ]`.
//!
//! The dialog resolves to an [`Outcome`] — the chosen slot name on Load, or
//! Cancel. One implementation, two callers: the in-process TUI overlay
//! (removing-tmux Phase 4d) returns the [`Outcome`] to the review flow as a
//! value; the legacy tmux `shelbi __review-confirm` popup writes the slot name
//! to its `--out` temp file. [`Dialog::handle_key`] / [`Dialog::handle_click`]
//! fold the key/mouse decision and its application into one step so both callers
//! share the driver, not just the renderer.

use crossterm::event::KeyCode;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// One review-tagged slot as the dialog sees it: the workspace name and, when
/// occupied, the title of the task currently loaded on it (shown in quotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub name: String,
    /// The occupying task's title, or `None` when the slot is free.
    pub occupant: Option<String>,
}

/// Which control currently has focus. In the picker the list and the two
/// buttons all take focus; the confirm variant only uses the buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Load,
    Cancel,
}

/// What a key press means. Split from any event loop so the decision table is
/// unit-testable. `Activate` resolves against the focused control; `Up`/`Down`
/// move the list selection; `Focus*`/`Toggle*` move the highlight without
/// closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Load,
    Cancel,
    Activate,
    Up,
    Down,
    FocusLoad,
    FocusCancel,
    ToggleFocus,
    Ignore,
}

/// Map a key to a decision. With no slots (`has_slots` false) the popup is
/// informational, so every key dismisses (cancel) and nothing loads. `is_picker`
/// distinguishes the multi-slot picker (Up/Down navigate the list) from the
/// single-slot confirm.
fn decide(code: KeyCode, has_slots: bool, is_picker: bool) -> Decision {
    if !has_slots {
        return Decision::Cancel;
    }
    match code {
        KeyCode::Char('l') | KeyCode::Char('L') => Decision::Load,
        KeyCode::Char('c')
        | KeyCode::Char('C')
        | KeyCode::Char('n')
        | KeyCode::Char('N')
        | KeyCode::Char('q')
        | KeyCode::Esc => Decision::Cancel,
        // `y` is a confirm shortcut only in the yes/no variant; in the picker
        // it's ambiguous (which slot?), so it's ignored there.
        KeyCode::Char('y') | KeyCode::Char('Y') if !is_picker => Decision::Load,
        KeyCode::Enter | KeyCode::Char(' ') => Decision::Activate,
        KeyCode::Up | KeyCode::Char('k') if is_picker => Decision::Up,
        KeyCode::Down | KeyCode::Char('j') if is_picker => Decision::Down,
        KeyCode::Left => Decision::FocusLoad,
        KeyCode::Right => Decision::FocusCancel,
        KeyCode::Tab | KeyCode::BackTab => Decision::ToggleFocus,
        _ => Decision::Ignore,
    }
}

/// The dialog's resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Load the review task onto this slot (the workspace name).
    Load(String),
    Cancel,
}

/// Result of feeding one key/click into the dialog: keep it open, or resolve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Continue,
    Done(Outcome),
}

/// The interactive dialog state.
pub struct Dialog {
    title: String,
    slots: Vec<Slot>,
    /// Highlighted slot in the picker (always a valid index when `slots` is
    /// non-empty; ignored for the confirm/informational variants).
    selected: usize,
    focus: Focus,
}

impl Dialog {
    /// Build the dialog. Default focus: the picker starts on the list (so
    /// Up/Down/Enter act on slots immediately); the confirm starts on
    /// `[ Load ]`.
    pub fn new(title: impl Into<String>, slots: Vec<Slot>) -> Self {
        let is_picker = slots.len() > 1;
        Dialog {
            title: title.into(),
            slots,
            selected: 0,
            focus: if is_picker { Focus::List } else { Focus::Load },
        }
    }

    pub fn is_picker(&self) -> bool {
        self.slots.len() > 1
    }

    pub fn has_slots(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Feed one key press. Applies navigation/focus changes in place and
    /// returns [`Step::Done`] when the dialog resolves (Load / Cancel).
    pub fn handle_key(&mut self, code: KeyCode) -> Step {
        match decide(code, self.has_slots(), self.is_picker()) {
            Decision::Load => Step::Done(self.load_selected()),
            Decision::Cancel => Step::Done(Outcome::Cancel),
            Decision::Activate => Step::Done(self.activate()),
            Decision::Up => {
                self.move_selected(true);
                Step::Continue
            }
            Decision::Down => {
                self.move_selected(false);
                Step::Continue
            }
            Decision::FocusLoad => {
                self.focus = Focus::Load;
                Step::Continue
            }
            Decision::FocusCancel => {
                self.focus = Focus::Cancel;
                Step::Continue
            }
            Decision::ToggleFocus => {
                self.toggle_focus();
                Step::Continue
            }
            Decision::Ignore => Step::Continue,
        }
    }

    /// Feed a left-click at `(col, row)` within the popup `area`.
    pub fn handle_click(&mut self, area: Rect, col: u16, row: u16) -> Step {
        if !self.has_slots() {
            return Step::Done(Outcome::Cancel);
        }
        match hit_test(self, area, col, row) {
            Some(Hit::Slot(i)) => {
                self.selected = i;
                self.focus = Focus::List;
                Step::Continue
            }
            Some(Hit::Load) => Step::Done(self.load_selected()),
            Some(Hit::Cancel) => Step::Done(Outcome::Cancel),
            None => Step::Continue,
        }
    }

    /// Move the list highlight, saturating at the ends.
    fn move_selected(&mut self, up: bool) {
        if self.slots.is_empty() {
            return;
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        } else if self.selected + 1 < self.slots.len() {
            self.selected += 1;
        }
        self.focus = Focus::List;
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Load => Focus::Cancel,
            Focus::Cancel | Focus::List => Focus::Load,
        };
    }

    /// Resolve an `Activate` (Enter/Space) against the current focus. Focus on
    /// the list activates the highlighted slot (so Enter on a row loads it).
    fn activate(&self) -> Outcome {
        match self.focus {
            Focus::Cancel => Outcome::Cancel,
            Focus::Load | Focus::List => self.load_selected(),
        }
    }

    /// The chosen slot name, or Cancel when there are none.
    fn load_selected(&self) -> Outcome {
        match self.slots.get(self.selected) {
            Some(slot) => Outcome::Load(slot.name.clone()),
            None => Outcome::Cancel,
        }
    }

    /// Paint the dialog into `area` (the whole popup pane in the tmux runtime, a
    /// centered overlay rect in the single-process TUI).
    pub fn render(&self, f: &mut Frame, area: Rect) {
        render(f, area, self)
    }
}

/// A clickable region of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Slot(usize),
    Load,
    Cancel,
}

/// Deterministic geometry shared by [`render`] and [`hit_test`] so a click lands
/// on exactly the control drawn there.
struct DialogLayout {
    slot_rows: Vec<u16>,
    slot_x0: u16,
    slot_x1: u16,
    load: Rect,
    cancel: Rect,
}

/// Compute the dialog geometry for `inner` (the area inside the border).
fn layout(inner: Rect, dialog: &Dialog) -> DialogLayout {
    let mut y = inner.y + 2;
    let mut slot_rows = Vec::new();
    if dialog.is_picker() {
        for _ in &dialog.slots {
            slot_rows.push(y);
            y += 1;
        }
        y += 1; // blank line before the button row
    } else {
        y += 2;
    }
    let (load, cancel) = button_row(inner, y);
    DialogLayout {
        slot_rows,
        slot_x0: inner.x,
        slot_x1: inner.x + inner.width,
        load,
        cancel,
    }
}

/// Rects of the two buttons on row `y`, matching ratatui's integer centering of
/// the rendered `[ Load ]      [ Cancel ]` line so a click hit-tests the same
/// cells that were drawn.
fn button_row(inner: Rect, y: u16) -> (Rect, Rect) {
    const GAP: u16 = 6;
    let load_w = LOAD_LABEL.chars().count() as u16;
    let cancel_w = CANCEL_LABEL.chars().count() as u16;
    let total = load_w + GAP + cancel_w;
    let start = inner.x + inner.width.saturating_sub(total) / 2;
    (
        Rect {
            x: start,
            y,
            width: load_w,
            height: 1,
        },
        Rect {
            x: start + load_w + GAP,
            y,
            width: cancel_w,
            height: 1,
        },
    )
}

const LOAD_LABEL: &str = "[ Load ]";
const CANCEL_LABEL: &str = "[ Cancel ]";

/// Map a click at `(col, row)` to the control under it, or `None` for dead
/// space. `area` is the whole popup area (the border is drawn on it; geometry is
/// computed against its inner rect).
fn hit_test(dialog: &Dialog, area: Rect, col: u16, row: u16) -> Option<Hit> {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let l = layout(inner, dialog);
    if within(l.load, col, row) {
        return Some(Hit::Load);
    }
    if within(l.cancel, col, row) {
        return Some(Hit::Cancel);
    }
    for (i, &sy) in l.slot_rows.iter().enumerate() {
        if row == sy && col >= l.slot_x0 && col < l.slot_x1 {
            return Some(Hit::Slot(i));
        }
    }
    None
}

fn within(rect: Rect, col: u16, row: u16) -> bool {
    row == rect.y && col >= rect.x && col < rect.x + rect.width
}

fn render(f: &mut Frame, area: Rect, dialog: &Dialog) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Load for review ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let title_line = Line::from(Span::styled(
        dialog.title.clone(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    let mut body: Vec<Line> = vec![title_line, Line::raw("")];
    if !dialog.has_slots() {
        body.push(Line::from(Span::styled(
            "No review workspace is configured.",
            Style::default().fg(Color::Yellow),
        )));
        body.push(Line::raw(""));
        body.push(Line::from(button("[ Dismiss ]", true)).centered());
        f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
        return;
    }

    if dialog.is_picker() {
        for (i, slot) in dialog.slots.iter().enumerate() {
            body.push(slot_line(slot, i == dialog.selected));
        }
        body.push(Line::raw(""));
    } else {
        let slot = &dialog.slots[0];
        body.push(Line::from(vec![
            Span::styled("Load onto review workspace ", Style::default().fg(Color::Gray)),
            Span::styled(
                slot.name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(Color::Gray)),
        ]));
        if let Some(occ) = &slot.occupant {
            body.push(Line::from(Span::styled(
                format!("(returns \"{occ}\" to the queue)"),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            body.push(Line::raw(""));
        }
    }

    body.push(
        Line::from(vec![
            button(LOAD_LABEL, dialog.focus == Focus::Load),
            Span::raw("      "),
            button(CANCEL_LABEL, dialog.focus == Focus::Cancel),
        ])
        .centered(),
    );
    f.render_widget(Paragraph::new(body), inner);
}

/// One picker row: `> review-1   (free)` / `  review-2   (serving "title")`.
fn slot_line(slot: &Slot, selected: bool) -> Line<'static> {
    let marker = if selected { "> " } else { "  " };
    let name_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let state = match &slot.occupant {
        Some(title) => format!("  (serving \"{title}\")"),
        None => "  (free)".to_string(),
    };
    Line::from(vec![
        Span::raw(marker),
        Span::styled(slot.name.clone(), name_style),
        Span::styled(state, Style::default().fg(Color::DarkGray)),
    ])
}

/// A button span. The focused button is reverse-video + bold (a solid
/// highlighted block); an unfocused button is plain cyan text.
fn button(label: &str, focused: bool) -> Span<'_> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    Span::styled(label.to_string(), style)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(n: usize) -> Vec<Slot> {
        (0..n)
            .map(|i| Slot {
                name: format!("review-{}", i + 1),
                occupant: None,
            })
            .collect()
    }

    fn dialog(n: usize) -> Dialog {
        Dialog::new("t", slots(n))
    }

    #[test]
    fn slot_count_selects_the_confirm_or_picker_shape() {
        assert!(!dialog(0).is_picker());
        assert!(!dialog(0).has_slots());
        assert!(!dialog(1).is_picker(), "one slot is a confirm, not a picker");
        assert!(dialog(2).is_picker(), "two slots is a picker");
    }

    #[test]
    fn confirm_shortcut_keys_load_immediately_in_the_single_slot_variant() {
        for code in [KeyCode::Char('y'), KeyCode::Char('Y'), KeyCode::Char('l')] {
            assert_eq!(decide(code, true, false), Decision::Load, "{code:?}");
        }
    }

    #[test]
    fn y_is_ignored_in_the_picker_but_l_still_loads() {
        assert_eq!(decide(KeyCode::Char('y'), true, true), Decision::Ignore);
        assert_eq!(decide(KeyCode::Char('l'), true, true), Decision::Load);
    }

    #[test]
    fn cancel_keys_cancel_when_slots_exist() {
        for code in [
            KeyCode::Esc,
            KeyCode::Char('c'),
            KeyCode::Char('n'),
            KeyCode::Char('q'),
        ] {
            assert_eq!(decide(code, true, true), Decision::Cancel, "{code:?}");
        }
    }

    #[test]
    fn arrows_navigate_the_list_only_in_the_picker() {
        assert_eq!(decide(KeyCode::Up, true, true), Decision::Up);
        assert_eq!(decide(KeyCode::Down, true, true), Decision::Down);
        assert_eq!(decide(KeyCode::Up, true, false), Decision::Ignore);
        assert_eq!(decide(KeyCode::Down, true, false), Decision::Ignore);
    }

    #[test]
    fn enter_and_space_activate_the_focus() {
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            assert_eq!(decide(code, true, true), Decision::Activate, "{code:?}");
        }
    }

    #[test]
    fn every_key_dismisses_when_no_slot_exists() {
        for code in [
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('l'),
            KeyCode::Tab,
            KeyCode::Up,
            KeyCode::Esc,
            KeyCode::Char('x'),
        ] {
            assert_eq!(decide(code, false, false), Decision::Cancel, "{code:?}");
        }
    }

    #[test]
    fn handle_key_navigates_and_resolves() {
        let mut d = dialog(3);
        // Down moves the highlight without resolving.
        assert_eq!(d.handle_key(KeyCode::Down), Step::Continue);
        assert_eq!(d.selected, 1);
        // `l` loads the highlighted slot.
        assert_eq!(
            d.handle_key(KeyCode::Char('l')),
            Step::Done(Outcome::Load("review-2".into()))
        );
        // Esc cancels.
        let mut d = dialog(1);
        assert_eq!(d.handle_key(KeyCode::Esc), Step::Done(Outcome::Cancel));
    }

    #[test]
    fn handle_key_on_no_slots_dismisses() {
        let mut d = dialog(0);
        assert_eq!(d.handle_key(KeyCode::Char('x')), Step::Done(Outcome::Cancel));
    }

    #[test]
    fn enter_on_list_loads_the_selected_slot() {
        let mut d = dialog(3);
        d.handle_key(KeyCode::Down); // select review-2
        assert_eq!(
            d.handle_key(KeyCode::Enter),
            Step::Done(Outcome::Load("review-2".into()))
        );
    }

    #[test]
    fn enter_on_cancel_focus_cancels() {
        let mut d = dialog(2);
        // Tab from the list toggles to Load; a second resolves via Right→Cancel.
        d.handle_key(KeyCode::Right); // focus Cancel
        assert_eq!(d.handle_key(KeyCode::Enter), Step::Done(Outcome::Cancel));
    }

    #[test]
    fn handle_click_maps_clicks_to_slot_rows_and_buttons() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let l = layout(inner, &dialog(2));

        // Clicking the second slot row selects it (Continue).
        let mut d = dialog(2);
        assert_eq!(d.handle_click(area, inner.x + 1, l.slot_rows[1]), Step::Continue);
        assert_eq!(d.selected, 1);
        // Clicking Load resolves to loading the selected slot.
        assert_eq!(
            d.handle_click(area, l.load.x, l.load.y),
            Step::Done(Outcome::Load("review-2".into()))
        );
        // Clicking Cancel resolves to Cancel.
        let mut d = dialog(2);
        assert_eq!(
            d.handle_click(area, l.cancel.x, l.cancel.y),
            Step::Done(Outcome::Cancel)
        );
        // Dead space is inert.
        let mut d = dialog(2);
        assert_eq!(d.handle_click(area, inner.x + 1, l.load.y - 1), Step::Continue);
    }

    #[test]
    fn click_anywhere_dismisses_the_no_slots_dialog() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let mut d = dialog(0);
        assert_eq!(d.handle_click(area, 5, 5), Step::Done(Outcome::Cancel));
    }
}
