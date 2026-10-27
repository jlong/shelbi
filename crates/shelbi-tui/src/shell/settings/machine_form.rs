//! The Add machine and Configure machine forms in the TUI
//! (`tui-settings-machine-forms`), over the desktop's shared draft
//! (`shelbi_app::settings::machine_form`): a modal layer the Workspaces page
//! owns ([`super::workspaces`]), drawn over the main area.
//!
//! The fields are the machine's real schema: Name, Kind (read-only: Add is
//! always SSH, Configure keeps the machine's own), SSH host (SSH only: a host
//! or an alias from the user's SSH config, which `ssh` resolves with the
//! user's own keys; nothing asks for or stores a password), Project
//! directory (`Machine::work_dir`, typed as a path: a remote's is the
//! remote's own, the local one's `~` is the user's home) and Machine tags.
//! Forwarding and whatever else the form doesn't own is kept by the service.
//!
//! Fields edit at the end of the text: type or paste (a dropped path too),
//! Backspace, Ctrl+W / Alt+Backspace deletes back to the last `/` or space,
//! Ctrl+U clears. Tab / Shift-Tab and ↑ / ↓ move between the controls, ← / →
//! between the buttons, Enter submits from a field (or presses the focused
//! button), Space presses a button, Esc cancels. Nothing is written until
//! Add machine / Save changes; while the save runs the form is busy (one
//! save at a time, Cancel waits for it).
//!
//! An SSH machine's connection region says the draft is Not checked: the
//! readiness check (Test connection, Install CLI) is its own task. On
//! Configure, what the page's reachability probe last saw of the saved host
//! shows under it, labeled as the probe's (only SSH answering), and goes as
//! soon as the host is edited.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use shelbi_app::settings::machine_form::{self as mform, Action, Field, Focus, MachineForm, NOT_CHECKED};
use shelbi_app::settings::workspace_form::sentence;
use shelbi_app::settings::workspaces::{MachineStatus, LOCAL_LABEL};
use shelbi_core::ForwardMode;
use shelbi_state::keymap::{Key, KeyChord, Mods};

use super::workspaces::{button_style, field_line, text_box};
use crate::panel;
use crate::theme::palette;

/// The SSH host field's helper.
pub(crate) const HOST_HELP: &str = "A host name or an alias from your SSH config. Shelbi connects with your \
                                    existing SSH keys and config and never asks for a password. Empty uses the \
                                    name.";

/// What the connection region says under Not checked.
pub(crate) const NOT_CHECKED_HELP: &str = "Nothing has checked this draft yet: SSH, the Shelbi CLI, Git and the \
                                           project directory aren’t verified until a readiness check runs.";

/// The form's key: the TUI's text editing (at the end of the text) and its
/// arrows, over the shared form's keys.
pub(super) fn key(form: &mut MachineForm, k: KeyEvent) -> Action {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let field = match form.focus() {
        Focus::Field(f) => Some(f),
        _ => None,
    };
    let shared = |form: &mut MachineForm, code: Key| {
        let action = form.key(KeyChord::new(code, Mods::NONE), false).unwrap_or(Action::Stay);
        // Cancel waits for a save in flight to land.
        if action == Action::Cancel && form.saving() {
            Action::Stay
        } else {
            action
        }
    };
    match k.code {
        KeyCode::Tab => shared(form, Key::Tab),
        KeyCode::BackTab => shared(form, Key::BackTab),
        KeyCode::Enter => shared(form, Key::Enter),
        KeyCode::Esc => shared(form, Key::Esc),
        KeyCode::Down => shared(form, Key::Tab),
        KeyCode::Up => shared(form, Key::BackTab),
        KeyCode::Right if field.is_none() => shared(form, Key::Tab),
        KeyCode::Left if field.is_none() => shared(form, Key::BackTab),
        _ if form.saving() => Action::Stay,
        KeyCode::Char(' ') if field.is_none() && !ctrl && !alt => shared(form, Key::Char(' ')),
        _ => {
            if let Some(field) = field {
                edit(form, field, k.code, ctrl, alt);
            }
            Action::Stay
        }
    }
}

/// A text edit of `field`, at the end of its text.
fn edit(form: &mut MachineForm, field: Field, code: KeyCode, ctrl: bool, alt: bool) {
    let mut text = form.text(field).to_string();
    match code {
        KeyCode::Backspace if alt => delete_word(&mut text),
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char('w') if ctrl => delete_word(&mut text),
        KeyCode::Char('u') if ctrl => text.clear(),
        KeyCode::Char(c) if !ctrl && !alt => text.push(c),
        _ => return,
    }
    form.set(field, &text);
}

/// Delete back to (not past) the last `/` or space before the end: a path's
/// last component, a tag list's last word.
fn delete_word(text: &mut String) {
    let trimmed = text.trim_end_matches(['/', ' ', ',']).len();
    let cut = text[..trimmed].rfind(['/', ' ', ',']).map_or(0, |i| i + 1);
    text.truncate(cut);
}

/// A paste (or a dropped file's path) onto the focused field, as one line.
/// Returns whether a field took it.
pub(super) fn paste(form: &mut MachineForm, pasted: &str) -> bool {
    let Focus::Field(field) = form.focus() else {
        return false;
    };
    if form.saving() {
        return false;
    }
    let line: String = pasted.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
    let mut text = form.text(field).to_string();
    text.push_str(&line);
    form.set(field, &text);
    true
}

/// A click on `focus`: a field takes the keyboard, a button is pressed.
pub(super) fn click(form: &mut MachineForm, focus: Focus) -> Action {
    if form.saving() {
        return Action::Stay;
    }
    match focus {
        Focus::Field(_) => {
            form.set_focus(focus);
            Action::Stay
        }
        other => form.press(other),
    }
}

/// Draw `form` over `main`, centred, recording each control's rect in
/// `hits`. `probe` is what the page's reachability probe last saw of the
/// configured machine. Below 40 columns the labels stack over their fields;
/// a short terminal scrolls to keep the focused control (and what's said
/// under it) in view.
pub(super) fn render(
    f: &mut Frame,
    main: Rect,
    form: &MachineForm,
    probe: Option<&MachineStatus>,
    hits: &mut Vec<(Rect, Focus)>,
) {
    let width = 78.min(main.width.saturating_sub(2));
    if width < 16 || main.height < 5 {
        return;
    }
    let inner_w = width.saturating_sub(4) as usize;
    let stacked = inner_w < 40;
    let label_w = if stacked { 0 } else { 19usize.min(inner_w / 3) };
    let value_w = inner_w.saturating_sub(label_w).max(1);
    let wrap = |s: &str, w: usize| shelbi_app::overlay::error_log::wrap(s, w.max(1));
    let muted = Style::default().fg(palette().muted);
    let red = Style::default().fg(palette().red);

    // Lay the content out as lines, recording which lines each control
    // spans (its own and what's said under it) and where it sits.
    let mut lines: Vec<Line<'static>> = Vec::new();
    // (first line, last line + 1, line of the control, x, width, focus)
    let mut controls: Vec<(usize, usize, usize, u16, u16, Focus)> = Vec::new();
    for l in wrap(&form.description(), inner_w) {
        lines.push(Line::from(Span::styled(l, muted)));
    }
    lines.push(Line::raw(""));
    let under = |lines: &mut Vec<Line<'static>>, text: &str, style: Style| {
        for l in wrap(text, value_w) {
            lines.push(Line::from(vec![Span::raw(" ".repeat(label_w)), Span::styled(l, style)]));
        }
    };
    let labeled = |lines: &mut Vec<Line<'static>>, label: &str, value: Vec<Span<'static>>| {
        if stacked {
            lines.push(Line::from(Span::styled(label.to_string(), Style::default().fg(palette().gray))));
            lines.push(Line::from(value));
        } else {
            lines.push(field_line(label, label_w, value));
        }
    };
    let box_w = value_w.saturating_sub(2).clamp(4, 48);

    // Kind: read-only.
    let kind = if form.remote() { "SSH".to_string() } else { format!("Local ({LOCAL_LABEL})") };
    labeled(&mut lines, "Kind", vec![Span::styled(kind, Style::default().fg(palette().foreground))]);
    for field in form.fields_shown() {
        let first = lines.len();
        let focused = form.focus() == Focus::Field(field);
        let (label, help) = match field {
            Field::Name => ("Name", None),
            Field::Host => ("SSH host", Some(HOST_HELP.to_string())),
            Field::WorkDir => ("Project directory", Some(form.work_dir_help())),
            Field::Tags => ("Machine tags", Some(mform::TAGS_HELP.to_string())),
        };
        labeled(&mut lines, label, text_box(form.text(field), box_w, focused, false));
        let at = lines.len() - 1;
        if let Some(e) = form.field_error(field) {
            under(&mut lines, &sentence(e), red);
        }
        if let Some(help) = help {
            under(&mut lines, &help, muted);
        }
        controls.push((first, lines.len(), at, label_w as u16, box_w as u16 + 2, Focus::Field(field)));
    }
    // Forwarding: the config's, kept as it is.
    if let Some(forward) = form.configured().and_then(|m| m.forward) {
        let mode = match forward {
            ForwardMode::Unix => "unix",
            ForwardMode::Tcp => "tcp",
        };
        labeled(
            &mut lines,
            "Forwarding",
            vec![
                Span::styled(mode.to_string(), Style::default().fg(palette().foreground)),
                Span::styled("  from the config, kept as is".to_string(), muted),
            ],
        );
    }
    // Connection: a draft is Not checked; the probe's last word is its own.
    if form.remote() {
        lines.push(Line::raw(""));
        labeled(
            &mut lines,
            "Connection",
            vec![Span::styled(NOT_CHECKED.to_string(), Style::default().fg(palette().yellow))],
        );
        under(&mut lines, NOT_CHECKED_HELP, muted);
        if let Some(note) = probe.and_then(|s| form.probe_note(s)) {
            let style = match probe {
                Some(MachineStatus::Unreachable(_)) => red,
                _ => Style::default().fg(palette().gray),
            };
            under(&mut lines, &note, style);
        }
    }
    if let Some(e) = form.error() {
        lines.push(Line::raw(""));
        for l in wrap(&sentence(e), inner_w) {
            lines.push(Line::from(Span::styled(l, red)));
        }
    }
    lines.push(Line::raw(""));
    // The buttons, at the right.
    let button_row = lines.len();
    let buttons = [
        (Focus::Cancel, " Cancel ".to_string(), button_style(form.focus() == Focus::Cancel, !form.saving(), false)),
        (
            Focus::Submit,
            format!(" {} ", if form.saving() { "Saving…" } else { form.submit_label() }),
            button_style(form.focus() == Focus::Submit, !form.saving(), true),
        ),
    ];
    let total: usize = buttons.iter().map(|(_, t, _)| panel::display_width(t) + 1).sum();
    let mut x = inner_w.saturating_sub(total);
    let mut spans = vec![Span::raw(" ".repeat(x))];
    for (focus, label, style) in buttons {
        let w = panel::display_width(&label);
        controls.push((button_row, button_row + 1, button_row, x as u16, w as u16, focus));
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
        x += w + 1;
    }
    lines.push(Line::from(spans));
    let hint = if form.saving() {
        "Saving… (nothing else runs until this lands)".to_string()
    } else {
        format!("Tab next · ⏎ {} · Esc cancel", form.submit_label().to_lowercase())
    };
    lines.push(Line::from(Span::styled(panel::truncate_right(&hint, inner_w), muted)));

    let height = (lines.len() as u16 + 2).min(main.height);
    let rect = Rect {
        x: main.x + (main.width - width) / 2,
        y: main.y + (main.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette().divider_active))
        .title(Span::styled(
            format!(" {} ", form.title()),
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let content = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let skip = scroll(&controls, form.focus(), content.height as usize);
    f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), content);
    for (_, _, line, x, w, focus) in controls {
        let Some(y) = line.checked_sub(skip) else {
            continue;
        };
        if y < content.height as usize {
            hits.push((Rect { x: content.x + x, y: content.y + y as u16, width: w.max(1), height: 1 }, focus));
        }
    }
}

/// The lines to skip so the focused control's block (its line and what's
/// said under it) shows in `height` rows: its end when it fits, else its
/// start.
fn scroll(controls: &[(usize, usize, usize, u16, u16, Focus)], focus: Focus, height: usize) -> usize {
    let Some(&(first, end, ..)) = controls.iter().find(|c| c.5 == focus) else {
        return 0;
    };
    if end <= height {
        0
    } else if end - first <= height {
        end - height
    } else {
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_word_takes_a_path_component_or_a_word() {
        let cases = [
            ("/home/me/src", "/home/me/"),
            ("/home/me/", "/home/"),
            ("linux, gpu", "linux, "),
            ("gpu", ""),
            ("", ""),
        ];
        for (from, to) in cases {
            let mut text = from.to_string();
            delete_word(&mut text);
            assert_eq!(text, to, "from {from:?}");
        }
    }

    #[test]
    fn scrolling_keeps_the_focused_block_in_view() {
        let controls = [
            (0, 3, 0, 0, 1, Focus::Field(Field::Name)),
            (3, 9, 3, 0, 1, Focus::Field(Field::WorkDir)),
            (12, 13, 12, 0, 1, Focus::Submit),
        ];
        assert_eq!(scroll(&controls, Focus::Field(Field::Name), 5), 0);
        assert_eq!(scroll(&controls, Focus::Field(Field::WorkDir), 6), 3, "the block's end shows");
        assert_eq!(scroll(&controls, Focus::Field(Field::WorkDir), 4), 3, "too tall: its start");
        assert_eq!(scroll(&controls, Focus::Submit, 5), 8);
    }
}
