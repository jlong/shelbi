//! `shelbi __review-confirm --title T --out PATH [--slot NAME --occupant OCC]…`
//! — the "Load for review" dialog, meant to run inside a `tmux display-popup`
//! centered on the terminal (the same surface style as the palette popup).
//!
//! One dialog, three shapes, chosen by how many `review`-tagged slots the
//! caller passed:
//!
//! - **0 slots** — informational: "No review workspace configured." Any key
//!   dismisses; nothing is written.
//! - **1 slot** — a yes/no confirm ("Load onto review workspace `review`?")
//!   with `[ Load ]` / `[ Cancel ]`.
//! - **>1 slots** — a picker listing every review slot with its free/occupied
//!   state; the user selects one, then `[ Load ]` / `[ Cancel ]`.
//!
//! On Load the chosen slot name is written to `--out` and the process exits 0;
//! on Cancel/dismiss nothing is written and it exits non-zero. The caller (the
//! sidebar's Queued-for-Review activation) reads the file back to learn *which*
//! slot to load onto — occupancy and eviction are the caller's concern, so the
//! `--occupant` strings here are purely for display. Slots parallel occupants
//! positionally (an empty occupant means "free").

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
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

/// What a key press means. Split from the event loop so the decision table is
/// unit-testable without a terminal. `Activate` resolves against the focused
/// control; `Up`/`Down` move the list selection; `Focus*`/`Toggle*` move the
/// highlight without closing.
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
/// informational, so every key dismisses (cancel) and nothing loads — the same
/// guarantee the old none-free popup made. `is_picker` distinguishes the
/// multi-slot picker (Up/Down navigate the list) from the single-slot confirm.
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

/// The interactive dialog state.
struct Dialog {
    title: String,
    slots: Vec<Slot>,
    /// Highlighted slot in the picker (always a valid index when `slots` is
    /// non-empty; ignored for the confirm/informational variants).
    selected: usize,
    focus: Focus,
}

impl Dialog {
    fn is_picker(&self) -> bool {
        self.slots.len() > 1
    }

    fn has_slots(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Move the list highlight by `delta`, saturating at the ends.
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

    /// Resolve an `Activate` (Enter/Space) against the current focus, returning
    /// the chosen slot name on Load or `None` on Cancel. Focus on the list
    /// activates the highlighted slot (so Enter on a row loads it).
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
}

/// The dialog's resolution.
enum Outcome {
    Load(String),
    Cancel,
}

/// Render + drive the dialog, returning the chosen slot name (Load) or `None`
/// (cancel / dismiss / no slots). `out` receives the chosen name on Load.
pub fn run(title: String, out: String, slots: Vec<Slot>) -> Result<bool> {
    let mut term = setup_terminal()?;
    // Restore the terminal on any early return / panic — the caller reads our
    // exit code, so a stranded raw-mode popup pane would be worse than a normal
    // one that just closes.
    let _guard = TerminalGuard;

    let mut dialog = Dialog {
        title,
        slots,
        selected: 0,
        // Default focus: the picker starts on the list (so Up/Down/Enter act on
        // slots immediately); the confirm starts on `[ Load ]`.
        focus: Focus::Load,
    };
    if dialog.is_picker() {
        dialog.focus = Focus::List;
    }

    let outcome = loop {
        term.draw(|f| render(f, &dialog))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                match decide(k.code, dialog.has_slots(), dialog.is_picker()) {
                    Decision::Load => break dialog.load_selected(),
                    Decision::Cancel => break Outcome::Cancel,
                    Decision::Activate => break dialog.activate(),
                    Decision::Up => dialog.move_selected(true),
                    Decision::Down => dialog.move_selected(false),
                    Decision::FocusLoad => dialog.focus = Focus::Load,
                    Decision::FocusCancel => dialog.focus = Focus::Cancel,
                    Decision::ToggleFocus => dialog.toggle_focus(),
                    Decision::Ignore => {}
                }
            }
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                if !dialog.has_slots() {
                    break Outcome::Cancel;
                }
                match hit_test(&dialog, term.get_frame().area(), m.column, m.row) {
                    Some(Hit::Slot(i)) => {
                        dialog.selected = i;
                        dialog.focus = Focus::List;
                    }
                    Some(Hit::Load) => break dialog.load_selected(),
                    Some(Hit::Cancel) => break Outcome::Cancel,
                    None => {}
                }
            }
            _ => {}
        }
    };

    restore_terminal(&mut term)?;
    match outcome {
        Outcome::Load(name) => {
            std::fs::write(&out, name)?;
            Ok(true)
        }
        Outcome::Cancel => Ok(false),
    }
}

/// A clickable region of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Slot(usize),
    Load,
    Cancel,
}

/// Deterministic geometry shared by [`render`] and [`hit_test`] so a click
/// lands on exactly the control drawn there. Returns the y of each slot row
/// (picker only), and the rects of the two buttons.
struct DialogLayout {
    /// The y (absolute) of each slot row, indexed like `slots`. Empty for the
    /// confirm/informational variants (no per-slot rows).
    slot_rows: Vec<u16>,
    slot_x0: u16,
    slot_x1: u16,
    load: Rect,
    cancel: Rect,
}

/// Compute the dialog geometry for `inner` (the area inside the border).
fn layout(inner: Rect, dialog: &Dialog) -> DialogLayout {
    // Body starts under the title + a blank line.
    let mut y = inner.y + 2;
    let mut slot_rows = Vec::new();
    if dialog.is_picker() {
        for _ in &dialog.slots {
            slot_rows.push(y);
            y += 1;
        }
        y += 1; // blank line before the button row
    } else {
        // Confirm: one question line, then a blank, then the button row.
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
/// space. `area` is the whole popup area (the border is drawn on it; geometry
/// is computed against its inner rect).
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

fn render(f: &mut Frame, dialog: &Dialog) {
    // The popup pane is the whole "screen" from here; tmux has already centered
    // and sized it (and, with `-B`, drawn no border of its own). Draw the single
    // bordered modal that fills it.
    let area = f.area();
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
        // Informational: a single button, any key dismisses.
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
        // Single slot: a yes/no question naming the target (and its occupant,
        // if any, so the eviction is not a surprise).
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
/// The selected row is highlighted; occupancy is dimmed.
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

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture so the dialog is clickable; tmux forwards mouse events to
    // the popup pane when its `mouse` option is on.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    term.show_cursor()?;
    Ok(())
}

/// RAII backstop that leaves raw mode, mouse capture, and the alternate screen
/// on drop, so an error/panic path can't strand the popup pane in full-screen
/// raw mode. The happy path still calls [`restore_terminal`] explicitly;
/// re-issuing the escapes after a clean restore is harmless.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
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
        let slots = slots(n);
        let is_picker = slots.len() > 1;
        Dialog {
            title: "t".into(),
            slots,
            selected: 0,
            focus: if is_picker { Focus::List } else { Focus::Load },
        }
    }

    #[test]
    fn slot_count_selects_the_confirm_or_picker_shape() {
        // Exactly one review slot → confirm (yes/no); more than one → picker;
        // none → informational. This is the shape gate the whole dialog keys off.
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
        // `y` (yes) is ambiguous with more than one slot, so it's inert there;
        // the explicit `l` (load-selected) still works.
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
        // In the single-slot confirm there is no list to move through.
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
    fn move_selected_saturates_at_both_ends() {
        let mut d = dialog(3);
        assert_eq!(d.selected, 0);
        d.move_selected(true); // already at top
        assert_eq!(d.selected, 0);
        d.move_selected(false);
        d.move_selected(false);
        assert_eq!(d.selected, 2);
        d.move_selected(false); // already at bottom
        assert_eq!(d.selected, 2);
    }

    #[test]
    fn activate_on_a_selected_slot_loads_it() {
        let mut d = dialog(3);
        d.selected = 1;
        d.focus = Focus::List;
        match d.activate() {
            Outcome::Load(name) => assert_eq!(name, "review-2"),
            Outcome::Cancel => panic!("list activation should load the selected slot"),
        }
    }

    #[test]
    fn activate_on_cancel_focus_cancels() {
        let mut d = dialog(2);
        d.focus = Focus::Cancel;
        assert!(matches!(d.activate(), Outcome::Cancel));
    }

    #[test]
    fn hit_test_maps_clicks_to_slot_rows_and_buttons() {
        // A generous popup area so nothing clips.
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let d = dialog(2);
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let l = layout(inner, &d);

        // Each slot row hit-tests to its index anywhere along the row.
        for (i, &y) in l.slot_rows.iter().enumerate() {
            assert_eq!(
                hit_test(&d, area, inner.x + 1, y),
                Some(Hit::Slot(i)),
                "slot row {i}"
            );
        }
        // The button cells hit-test to their controls.
        assert_eq!(hit_test(&d, area, l.load.x, l.load.y), Some(Hit::Load));
        assert_eq!(
            hit_test(&d, area, l.cancel.x, l.cancel.y),
            Some(Hit::Cancel)
        );
        // Dead space between the last slot and the buttons resolves to nothing.
        assert_eq!(hit_test(&d, area, inner.x + 1, l.load.y - 1), None);
    }
}
