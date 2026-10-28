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
//! An SSH machine's form has the connection check
//! (`shelbi_app::settings::machine_check`, the desktop's): Test connection /
//! Test again checks the draft as typed (saved or not) and lists what it
//! found per stage (SSH, Platform, Shelbi CLI, Git, Project directory) with
//! what to do about anything that needs attention; Install CLI shows when the
//! check found no compatible CLI on a platform with a release, runs the
//! shared remote setup and checks again. While a check runs the region says
//! so with a spinner and how long it's been, and Test becomes Stop check
//! (an install can't be stopped: it runs to its end). On Configure, what the
//! page's reachability probe last saw of the saved host shows under it,
//! labeled as the probe's (only SSH answering), and goes as soon as the host
//! is edited.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    Frame,
};
use std::time::Duration;

use shelbi_app::settings::machine_check::{Action, Field, Focus, ReadinessForm, StageState, Tone};
use shelbi_app::settings::machine_form as mform;
use shelbi_app::settings::workspace_form::sentence;
use shelbi_app::settings::workspaces::MachineStatus;
use shelbi_core::ForwardMode;
use shelbi_state::keymap::{Key, KeyChord, Mods};

use crate::modal_form::{Field as Field2, FormLines};
use crate::panel;
use crate::button::{Align, Button};
use crate::textbox::{self, Textbox};
use crate::theme::palette;

/// What's said under the Name and SSH host pair: the desktop's footnote,
/// and what an empty host does.
pub(crate) const HOST_HELP: &str = "Use a hostname or an alias from your SSH config. Your existing SSH keys and \
                                    settings are observed. An empty SSH host uses the name.";

/// What's said under the connection check, as the desktop says it.
pub(crate) const CHECK_NOTE: &str = "Shelbi checks the remote CLI and offers to install a compatible version when needed.";

/// The spinner's frames, a tenth of a second each.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Whether pressing Test stops the check running now (an install can't be
/// stopped).
fn stops(form: &ReadinessForm, focus: Focus) -> bool {
    focus == Focus::Test && form.running().is_some() && !form.installing()
}

/// The form's key: the TUI's text editing (at the end of the text) and its
/// arrows, over the shared form's keys. Enter or Space on Stop check stops
/// the check (the page trips its flag).
pub(super) fn key(form: &mut ReadinessForm, k: KeyEvent) -> Action {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let field = match form.focus() {
        Focus::Field(f) => Some(f),
        _ => None,
    };
    let pressing = matches!(k.code, KeyCode::Enter) || (k.code == KeyCode::Char(' ') && !ctrl && !alt);
    if pressing && stops(form, form.focus()) {
        form.stop_check();
        return Action::Stay;
    }
    let shared = |form: &mut ReadinessForm, code: Key| {
        let action = form.key(KeyChord::new(code, Mods::NONE)).unwrap_or(Action::Stay);
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
fn edit(form: &mut ReadinessForm, field: Field, code: KeyCode, ctrl: bool, alt: bool) {
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
pub(super) fn paste(form: &mut ReadinessForm, pasted: &str) -> bool {
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
pub(super) fn click(form: &mut ReadinessForm, focus: Focus) -> Action {
    if form.saving() {
        return Action::Stay;
    }
    if stops(form, focus) {
        form.set_focus(focus);
        form.stop_check();
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

/// What the page knows of a form beyond its state: how long its check has
/// been running, and what an install whose answer the form dropped did.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Extra<'a> {
    pub elapsed: Option<Duration>,
    pub aside: Option<&'a str>,
}

/// Draw `form` over `main`, centred, recording each control's rect in
/// `hits`. `probe` is what the page's reachability probe last saw of the
/// configured machine. Below 40 columns the labels stack over their fields;
/// a short terminal scrolls to keep the focused control (and what's said
/// under it) in view.
pub(super) fn render(
    f: &mut Frame,
    main: Rect,
    form: &ReadinessForm,
    probe: Option<&MachineStatus>,
    extra: Extra<'_>,
    hits: &mut Vec<(Rect, Focus)>,
) {
    let width = 78.min(main.width.saturating_sub(2));
    if width < 16 || main.height < 5 {
        return;
    }
    let inner_w = width.saturating_sub(4) as usize;
    let muted = Style::default().fg(palette().muted);
    let red = Style::default().fg(palette().red);
    let gray = Style::default().fg(palette().gray);
    let wrap = |s: &str, w: usize| shelbi_app::overlay::error_log::wrap(s, w.max(1));

    // Lay the content out as lines, each field its label over its box,
    // recording each control's block (its own lines and what's said under
    // it) and where it sits.
    let mut out: FormLines<Focus> = FormLines::new(inner_w).with_rest(palette().input_well);
    out.heading(form.title(), &form.description());
    // What's said under each shown field: its refusal, then its help.
    let shown = form.fields_shown();
    let notes: Vec<(Option<String>, Option<String>)> = shown
        .iter()
        .map(|f| {
            let help = match f {
                Field::Name => None,
                // Said under the pair (below).
                Field::Host => None,
                Field::WorkDir => Some(form.work_dir_help()),
                Field::Tags => Some(mform::TAGS_HELP.to_string()),
            };
            (form.field_error(*f).map(sentence), help)
        })
        .collect();
    let spec = |field: Field| {
        let at = shown.iter().position(|f| *f == field).expect("a shown field");
        let (error, help) = &notes[at];
        let focused = form.focus() == Focus::Field(field);
        let (label, placeholder) = match field {
            Field::Name => ("Name", "Machine name"),
            Field::Host => ("SSH host", "Hostname or SSH alias"),
            Field::WorkDir => ("Project directory", "Path on the machine"),
            Field::Tags => ("Machine tags", "Comma-separated tags"),
        };
        let mut under: Vec<(&str, Style)> = Vec::new();
        under.extend(error.as_deref().map(|e| (e, red)));
        under.extend(help.as_deref().map(|h| (h, muted)));
        let textbox = Textbox::new(form.text(field))
            .placeholder(placeholder)
            .focused(focused)
            .invalid(error.is_some())
            .caret(focused.then(|| form.text(field).chars().count()));
        Field2 { label, textbox, focused, disabled: false, under, hit: Focus::Field(field) }
    };
    let mut fields = shown.iter().copied().peekable();
    let mut first_field = true;
    while let Some(field) = fields.next() {
        if !std::mem::take(&mut first_field) {
            out.blank();
        }
        // Name and SSH host side by side, as the desktop sets them, its
        // footnote under both.
        if field == Field::Name && fields.peek() == Some(&Field::Host) {
            fields.next();
            out.pair(spec(Field::Name), spec(Field::Host));
            out.text(HOST_HELP, muted);
            continue;
        }
        let f = spec(field);
        out.field(f.label, f.textbox, f.focused, f.disabled, &f.under, f.hit);
    }
    // Forwarding: the config's, kept as it is.
    if let Some(forward) = form.configured().and_then(|m| m.forward) {
        let mode = match forward {
            ForwardMode::Unix => "unix",
            ForwardMode::Tcp => "tcp",
        };
        out.blank();
        out.lines.push(textbox::label("Forwarding", false, false, inner_w));
        out.lines.push(Line::from(vec![
            Span::styled(mode.to_string(), Style::default().fg(palette().foreground)),
            Span::styled("  from the config, kept as is".to_string(), muted),
        ]));
    }
    // Connection: what the readiness check found, its buttons, and the
    // probe's last word (its own, never readiness).
    if form.remote() {
        out.blank();
        let panel = form.panel();
        let (glyph, tone) = match panel.tone {
            Tone::Idle => ("○".to_string(), palette().gray),
            Tone::Busy => (spinner(extra.elapsed).to_string(), palette().yellow),
            Tone::Success => ("✓".to_string(), palette().green),
            Tone::Warning => ("!".to_string(), palette().yellow),
            Tone::Error => ("✗".to_string(), palette().red),
        };
        let mut title = vec![
            Span::styled(format!("{glyph} "), Style::default().fg(tone)),
            Span::styled(panel.title.clone(), Style::default().fg(tone).add_modifier(Modifier::BOLD)),
        ];
        if let (Tone::Busy, Some(elapsed)) = (panel.tone, extra.elapsed) {
            title.push(Span::styled(format!("  {}s", elapsed.as_secs()), muted));
        }
        out.lines.push(textbox::label("Connection", false, false, inner_w));
        out.lines.push(Line::from(title));
        // The stages: what each found (or that it waits).
        let stage_w = form.stages().iter().map(|s| s.kind.label().chars().count()).max().unwrap_or(0) + 1;
        for stage in form.stages() {
            let (mark, color) = match stage.state {
                StageState::Waiting => ("·".to_string(), palette().muted),
                StageState::Running => (spinner(extra.elapsed).to_string(), palette().yellow),
                StageState::Passed => ("✓".to_string(), palette().green),
                StageState::Attention => ("!".to_string(), palette().yellow),
                StageState::Failed => ("✗".to_string(), palette().red),
                StageState::Skipped => ("–".to_string(), palette().muted),
            };
            let label = format!("{:<stage_w$}", stage.kind.label());
            let room = inner_w.saturating_sub(2 + stage_w).max(8);
            let result_style = match stage.state {
                StageState::Waiting | StageState::Skipped => muted,
                _ => Style::default().fg(palette().foreground),
            };
            // A long result (a path) wraps under itself.
            for (i, part) in wrap(&stage.result, room).into_iter().enumerate() {
                let mut spans = if i == 0 {
                    vec![
                        Span::styled(format!("{mark} "), Style::default().fg(color)),
                        Span::styled(label.clone(), gray),
                    ]
                } else {
                    vec![Span::raw(" ".repeat(2 + stage_w))]
                };
                spans.push(Span::styled(part, result_style));
                out.lines.push(Line::from(spans));
            }
        }
        let detail_style = match panel.tone {
            Tone::Error => red,
            Tone::Warning => Style::default().fg(palette().yellow),
            _ => muted,
        };
        out.text(&panel.detail, detail_style);
        if let Some(aside) = extra.aside {
            out.text(aside, gray);
        }
        // Install CLI when it can help, then Test connection / Test again
        // (Stop check while one runs), as the desktop's.
        let running = form.running().is_some();
        let mut buttons = Vec::new();
        if form.offers_install() {
            buttons.push((Button::primary("Install CLI").focused(form.focus() == Focus::InstallCli).enabled(!form.saving()), Focus::InstallCli));
        }
        let test = if stops(form, Focus::Test) { "Stop check" } else { form.test_label() };
        let test_on = !form.saving() && (!running || stops(form, Focus::Test));
        buttons.push((Button::secondary(test).focused(form.focus() == Focus::Test).enabled(test_on), Focus::Test));
        out.blank();
        out.buttons(&buttons, Align::Left);
        out.text(CHECK_NOTE, muted);
        if let Some(note) = probe.and_then(|s| form.probe_note(s)) {
            let style = match probe {
                Some(MachineStatus::Unreachable(_)) => red,
                _ => gray,
            };
            out.text(&note, style);
        }
    }
    // The footer: a refusal over the buttons, then the key hint.
    out.footer();
    if let Some(e) = form.error() {
        out.blank();
        out.text(&sentence(e), red);
    }
    out.blank();
    out.rule();
    let submit = if form.saving() { "Saving…" } else { form.submit_label() };
    out.buttons(
        &[
            (Button::secondary("Cancel").focused(form.focus() == Focus::Cancel).enabled(!form.saving()), Focus::Cancel),
            (Button::primary(submit).focused(form.focus() == Focus::Submit).enabled(!form.saving()), Focus::Submit),
        ],
        Align::Right,
    );
    let hint = if form.saving() {
        "Saving… (nothing else runs until this lands)".to_string()
    } else if form.installing() {
        "Installing… it runs to its end · Esc closes the form".to_string()
    } else if form.running().is_some() {
        "Checking… Stop check or Esc stops it · nothing is saved until you save".to_string()
    } else {
        format!("Tab next · ⏎ {} · Esc cancel", form.submit_label().to_lowercase())
    };
    out.lines.push(Line::from(Span::styled(panel::truncate_right(&hint, inner_w), muted)));

    let height = (out.lines.len() as u16 + 2).min(main.height);
    let rect = Rect {
        x: main.x + (main.width - width) / 2,
        y: main.y + (main.height - height) / 2,
        width,
        height,
    };
    let content = super::overlay_panel(f, rect, main, None);
    out.render(f, content, form.focus(), hits);
}

/// The spinner's frame `elapsed` into a check.
fn spinner(elapsed: Option<Duration>) -> char {
    let tenths = elapsed.map_or(0, |e| e.as_millis() / 100) as usize;
    SPINNER[tenths % SPINNER.len()]
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
}
