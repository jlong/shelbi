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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use crate::theme::palette;

// The dialog's state machine is shared with the desktop app
// (`shelbi_app::overlay::review_confirm`).
pub use shelbi_app::overlay::review_confirm::{ConfirmFocus as Focus, Outcome, Slot, Step};
use shelbi_app::overlay::review_confirm::{self as shared, CANCEL_LABEL, LOAD_LABEL};

/// The interactive dialog: the shared state machine plus the cell geometry a
/// click hit-tests against.
#[derive(Debug, Clone)]
pub struct Dialog {
    inner: shared::Dialog,
}

impl Dialog {
    /// Build the dialog. Default focus: the picker starts on the list (so
    /// Up/Down/Enter act on slots immediately); the confirm starts on
    /// `[ Load ]`.
    pub fn new(title: impl Into<String>, slots: Vec<Slot>) -> Self {
        Dialog {
            inner: shared::Dialog::new(title, slots),
        }
    }

    /// Build the no-slots informational variant with a custom line — "every
    /// review slot is busy", as opposed to the default "no review workspace is
    /// configured". Any key dismisses it and nothing loads (the all-busy report
    /// of `rt-tui-review-load-queued`).
    pub fn informational(title: impl Into<String>, message: impl Into<String>) -> Self {
        Dialog {
            inner: shared::Dialog::informational(title, message),
        }
    }

    pub fn is_picker(&self) -> bool {
        self.inner.is_picker()
    }

    pub fn has_slots(&self) -> bool {
        self.inner.has_slots()
    }

    /// Feed one key press. Applies navigation/focus changes in place and
    /// returns [`Step::Done`] when the dialog resolves (Load / Cancel).
    pub fn handle_key(&mut self, code: KeyCode) -> Step {
        match crate::keymap::chord_from_event(KeyEvent::new(code, KeyModifiers::NONE)) {
            Some(chord) => self.inner.handle_key(chord),
            // A key outside the chord vocabulary still dismisses the no-slots
            // report, as any key does.
            None => self.inner.dismiss_if_empty(),
        }
    }

    /// Feed a left-click at `(col, row)` within the popup `area`.
    pub fn handle_click(&mut self, area: Rect, col: u16, row: u16) -> Step {
        if !self.has_slots() {
            return Step::Done(Outcome::Cancel);
        }
        match hit_test(self, area, col, row) {
            Some(Hit::Slot(i)) => self.inner.click_slot(i),
            Some(Hit::Load) => self.inner.click_load(),
            Some(Hit::Cancel) => self.inner.click_cancel(),
            None => Step::Continue,
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
        for _ in dialog.inner.slots() {
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
        .border_style(Style::default().fg(palette().ansi_cyan))
        .title(format!(" {} ", shared::TITLE));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let title_line = Line::from(Span::styled(
        dialog.inner.title().to_string(),
        Style::default()
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD),
    ));

    let mut body: Vec<Line> = vec![title_line, Line::raw("")];
    if !dialog.has_slots() {
        body.push(Line::from(Span::styled(
            dialog.inner.empty_message().to_string(),
            Style::default().fg(palette().ansi_yellow),
        )));
        body.push(Line::raw(""));
        body.push(Line::from(button(shared::DISMISS_LABEL, true)).centered());
        f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
        return;
    }

    if dialog.is_picker() {
        for (i, slot) in dialog.inner.slots().iter().enumerate() {
            body.push(slot_line(slot, i == dialog.inner.selected()));
        }
        body.push(Line::raw(""));
    } else {
        let slot = &dialog.inner.slots()[0];
        body.push(Line::from(vec![
            Span::styled("Load onto review workspace ", Style::default().fg(palette().ansi_gray)),
            Span::styled(
                slot.name.clone(),
                Style::default()
                    .fg(palette().ansi_cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(palette().ansi_gray)),
        ]));
        if let Some(occ) = &slot.occupant {
            body.push(Line::from(Span::styled(
                format!("(returns \"{occ}\" to the queue)"),
                Style::default().fg(palette().ansi_dark_gray),
            )));
        } else {
            body.push(Line::raw(""));
        }
    }

    body.push(
        Line::from(vec![
            button(LOAD_LABEL, dialog.inner.focus() == Focus::Load),
            Span::raw("      "),
            button(CANCEL_LABEL, dialog.inner.focus() == Focus::Cancel),
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
            .fg(palette().ansi_cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().ansi_white)
    };
    let state = slot.state_note();
    Line::from(vec![
        Span::raw(marker),
        Span::styled(slot.name.clone(), name_style),
        Span::styled(state, Style::default().fg(palette().ansi_dark_gray)),
    ])
}

/// A button span. The focused button is reverse-video + bold (a solid
/// highlighted block); an unfocused button is plain cyan text.
fn button(label: &str, focused: bool) -> Span<'_> {
    let style = if focused {
        Style::default()
            .fg(palette().ansi_cyan)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(palette().ansi_cyan)
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

    /// The picker (slots, an occupied slot, the highlighted row, each
    /// button focused), the one-slot confirm, and the informational variant
    /// are readable on a light terminal.
    #[test]
    fn renders_readable_in_light() {
        use ratatui::{backend::TestBackend, Terminal};
        use shelbi_app::theme::Appearance;
        let picker_with = |focus: KeyCode| {
            let mut s = slots(3);
            s[1].occupant = Some("Fix login".into());
            let mut d = Dialog::new("t", s);
            d.handle_key(KeyCode::Down);
            d.handle_key(KeyCode::Down);
            d.handle_key(focus);
            d
        };
        // Up keeps the list focused (on the last slot after the two Downs);
        // Left / Right focus the buttons.
        let picker = {
            let mut d = picker_with(KeyCode::Up);
            d.handle_key(KeyCode::Down);
            d
        };
        let (load, cancel) = (picker_with(KeyCode::Left), picker_with(KeyCode::Right));
        let cases = [
            ("review-confirm-picker", picker),
            ("review-confirm-load", load),
            ("review-confirm-cancel", cancel),
            ("review-confirm-single", dialog(1)),
            ("review-confirm-busy", Dialog::informational("Fix login", "Every review slot is busy.")),
        ];
        crate::theme::with_appearance(Appearance::Light, || {
            for (name, d) in &cases {
                let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();
                term.draw(|f| d.render(f, f.area())).unwrap();
                crate::theme::assert_light_readable(name, term.backend().buffer());
            }
        });
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
            assert_eq!(
                dialog(1).handle_key(code),
                Step::Done(Outcome::Load("review-1".into())),
                "{code:?}"
            );
        }
    }

    #[test]
    fn y_is_ignored_in_the_picker_but_l_still_loads() {
        assert_eq!(dialog(2).handle_key(KeyCode::Char('y')), Step::Continue);
        assert_eq!(
            dialog(2).handle_key(KeyCode::Char('l')),
            Step::Done(Outcome::Load("review-1".into()))
        );
    }

    #[test]
    fn cancel_keys_cancel_when_slots_exist() {
        for code in [
            KeyCode::Esc,
            KeyCode::Char('c'),
            KeyCode::Char('n'),
            KeyCode::Char('q'),
        ] {
            assert_eq!(dialog(2).handle_key(code), Step::Done(Outcome::Cancel), "{code:?}");
        }
    }

    #[test]
    fn arrows_navigate_the_list_only_in_the_picker() {
        let mut d = dialog(1);
        assert_eq!(d.handle_key(KeyCode::Down), Step::Continue);
        assert_eq!(d.handle_key(KeyCode::Up), Step::Continue);
        assert_eq!(
            d.handle_key(KeyCode::Enter),
            Step::Done(Outcome::Load("review-1".into())),
            "the confirm's arrows leave focus on Load"
        );
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
            // Outside the chord vocabulary, still a dismiss.
            KeyCode::Null,
        ] {
            assert_eq!(dialog(0).handle_key(code), Step::Done(Outcome::Cancel), "{code:?}");
        }
    }

    #[test]
    fn handle_key_navigates_and_resolves() {
        let mut d = dialog(3);
        // Down moves the highlight without resolving; `l` loads it.
        assert_eq!(d.handle_key(KeyCode::Down), Step::Continue);
        assert_eq!(
            d.handle_key(KeyCode::Char('l')),
            Step::Done(Outcome::Load("review-2".into()))
        );
        let mut d = dialog(1);
        assert_eq!(d.handle_key(KeyCode::Esc), Step::Done(Outcome::Cancel));
    }

    #[test]
    fn informational_variant_has_no_slots_and_dismisses_on_any_key() {
        // The "every review slot is busy" report (rt-tui-review-load-queued):
        // no slots, so it's neither a picker nor a confirm, and any key just
        // dismisses it — nothing can be loaded.
        let d = Dialog::informational("Fix login", "Every review slot is busy.");
        assert!(!d.has_slots());
        assert!(!d.is_picker());
        for code in [KeyCode::Enter, KeyCode::Char('l'), KeyCode::Esc, KeyCode::Char('x')] {
            let mut d = Dialog::informational("t", "busy");
            assert_eq!(d.handle_key(code), Step::Done(Outcome::Cancel), "{code:?}");
        }
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

        // Clicking the second slot row selects it (Continue), so Load loads it.
        let mut d = dialog(2);
        assert_eq!(d.handle_click(area, inner.x + 1, l.slot_rows[1]), Step::Continue);
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
