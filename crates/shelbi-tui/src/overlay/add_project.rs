//! The "Add project" form — shared overlay logic + rendering (removing-tmux
//! `rt-tui-add-project`).
//!
//! The form collects a project **name**, a **repo path**, and a **config
//! location** (in-repo vs global), then hands those values to the shared
//! [`shelbi_orchestrator::project_create`] engine (validation + scaffold). Its
//! fields, labels, and keys match the dialog `shelbi-cli` shipped before this
//! moved in-process, so the UX is unchanged.
//!
//! One implementation, two callers (the same split the other overlays use):
//!
//! - the in-process TUI overlay (`crate::shell`) holds a [`Form`] in its model,
//!   feeds it keys from the one event loop, and on [`Step::Submit`] validates +
//!   creates the project off the UI thread; and
//! - the legacy `shelbi __palette` dialog in `shelbi-cli`, which keeps its own
//!   terminal/event loop but renders and decides through this same [`Form`].
//!
//! The state machine ([`Form::handle_key`]) and renderer ([`Form::render`]) are
//! pure over a `Form` snapshot, so focus-cycle, text editing, the radio toggle,
//! and the submit/cancel paths are unit-tested without a terminal. Validation
//! (duplicate name, bad path — it touches the filesystem) is **not** done per
//! keystroke: the caller runs [`shelbi_orchestrator::project_create::validate_add_project`]
//! on submit and, on failure, stashes the message back via [`Form::set_error`].

use std::path::Path;

use crossterm::event::KeyEvent;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

use shelbi_core::ConfigMode;
use crate::theme::palette;

// The form's state machine is shared with the desktop app
// (`shelbi_app::overlay::add_project`).
pub use shelbi_app::overlay::add_project::{Detected, Field, Step};
use shelbi_app::overlay::add_project as shared;

/// The add-project form: the shared state machine, with the "Detected:" check
/// run inline after each key (it is cached per root text, so it only runs git
/// when the repo path changes).
#[derive(Debug, Clone)]
pub struct Form {
    inner: shared::Form,
}

impl Form {
    /// Seed the form. Root prefills to `cwd` (the same default `shelbi init`'s
    /// "Project root?" prompt offers); config location defaults to `global`,
    /// the low-ceremony solo choice the user can flip to in-repo with one key.
    pub fn new(cwd: &Path) -> Self {
        let mut form = Self {
            inner: shared::Form::new(cwd),
        };
        form.detect();
        form
    }

    /// The entered (raw, human-readable) project name.
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// The entered repo-path text (not yet absolutized).
    pub fn root(&self) -> &str {
        self.inner.root()
    }

    /// The chosen config location.
    pub fn mode(&self) -> ConfigMode {
        self.inner.mode()
    }

    /// The focused control.
    pub fn focus(&self) -> Field {
        self.inner.focus()
    }

    /// Stash an inline validation error (shown in red) after a failed submit,
    /// keeping the form open so the user can fix the input. Cleared the moment
    /// they edit anything.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.inner.set_error(msg);
    }

    /// Feed one key event ([`shared::Form::handle_key`]): Esc cancels and
    /// Enter submits from any field, Tab / BackTab cycle focus, a text field
    /// takes typed characters and Backspace, and the config row flips on Space
    /// or an arrow. Editing the root re-runs the cached git-repo detection.
    pub fn handle_key(&mut self, key: KeyEvent) -> Step {
        let step = match crate::keymap::chord_from_event(key) {
            Some(chord) => self.inner.handle_key(chord),
            None => Step::Continue,
        };
        self.detect();
        step
    }

    /// Paint the dialog into `area`.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        render(f, area, self);
    }

    /// Run the git-repo check when the root text changed since the last one.
    fn detect(&mut self) {
        if let Some(root) = self.inner.detection_due() {
            let detected =
                shelbi_orchestrator::project_create::detect_root(self.inner.cwd(), &root);
            self.inner.set_detected(&root, detected);
        }
    }
}

/// The "Detected:" line for the latest detection.
fn detected_line(detected: Option<Detected>) -> Span<'static> {
    match detected {
        Some(d) => {
            let color = if d.is_ok() {
                palette().ansi_green
            } else {
                palette().ansi_yellow
            };
            Span::styled(d.message(), Style::default().fg(color))
        }
        None => Span::raw(""),
    }
}

/// Paint the dialog. The card **fills** `area` (rather than a small centered
/// box), so the fields, the derived-slug preview, and long error/notice text
/// always have room and never truncate.
fn render(f: &mut Frame, area: Rect, form: &Form) {
    f.render_widget(Clear, area);
    let inner = crate::overlay::dialog_frame(f.buffer_mut(), area, None, Some(shared::TITLE));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    lines.push(field_line("Name", form.name(), form.focus() == Field::Name));
    lines.push(field_line("Repo path", form.root(), form.focus() == Field::Root));
    lines.push(config_line(form.mode(), form.focus() == Field::Config));
    lines.push(Line::raw(""));
    lines.push(Line::from(detected_line(form.inner.detected())));
    lines.push(preview_line(&form.inner));
    lines.push(Line::raw(""));
    // Error row is reserved unconditionally so the hints don't reflow when a
    // validation message appears.
    lines.push(match form.inner.error() {
        Some(msg) => Line::from(Span::styled(
            format!("✗ {msg}"),
            Style::default().fg(palette().ansi_red).add_modifier(Modifier::BOLD),
        )),
        None => Line::raw(""),
    });
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        shared::HINT,
        Style::default().fg(palette().ansi_dark_gray),
    )]));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The live "where this lands" preview: the derived slug and the path its
/// config will be written to.
fn preview_line(form: &shared::Form) -> Line<'static> {
    let label_style = Style::default().fg(palette().ansi_gray);
    match form.preview() {
        Some((slug, path)) => Line::from(vec![
            Span::styled("Folder / id: ", label_style),
            Span::styled(
                slug,
                Style::default().fg(palette().ansi_cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   ({path})"), Style::default().fg(palette().ansi_dark_gray)),
        ]),
        None => Line::from(Span::styled(
            "Folder / id: (type a name to see the derived id)",
            Style::default().fg(palette().ansi_dark_gray),
        )),
    }
}

/// One labelled text field. The focused field shows a cyan cursor bar after its
/// value and an underlined label.
fn field_line(label: &str, value: &str, focused: bool) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(palette().ansi_gray)
    };
    let mut spans = label_spans(label, label_style).to_vec();
    spans.push(Span::raw(value.to_string()));
    if focused {
        spans.push(Span::styled("▏", Style::default().fg(palette().ansi_cyan)));
    }
    Line::from(spans)
}

/// The config-location radio row: `( ) In repo   (•) Global (~/.shelbi)`.
fn config_line(mode: ConfigMode, focused: bool) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(palette().ansi_gray)
    };
    let in_repo = if mode == ConfigMode::InRepo {
        "(•) In repo"
    } else {
        "( ) In repo"
    };
    let global = if mode == ConfigMode::Global {
        "(•) Global (~/.shelbi)"
    } else {
        "( ) Global (~/.shelbi)"
    };
    let opt_style = |selected: bool| {
        if selected {
            Style::default().fg(palette().ansi_white)
        } else {
            Style::default().fg(palette().ansi_dark_gray)
        }
    };
    let mut spans = label_spans("Config", label_style).to_vec();
    spans.extend([
        Span::styled(in_repo.to_string(), opt_style(mode == ConfigMode::InRepo)),
        Span::raw("   "),
        Span::styled(global.to_string(), opt_style(mode == ConfigMode::Global)),
    ]);
    Line::from(spans)
}

/// A field's label in its 11-column gutter, the focused one's underline
/// under the label's own letters only.
fn label_spans(label: &str, style: Style) -> [Span<'static>; 3] {
    crate::panel::padded_label(label, 0, 11usize.saturating_sub(label.chars().count()), style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn form() -> Form {
        Form::new(Path::new("/tmp/here"))
    }

    #[test]
    fn new_prefills_root_and_defaults_to_global() {
        let f = Form::new(Path::new("/tmp/here"));
        assert_eq!(f.name(), "");
        assert_eq!(f.root(), "/tmp/here");
        assert_eq!(f.mode(), ConfigMode::Global);
        assert_eq!(f.focus(), Field::Name);
        assert!(f.inner.error().is_none());
    }

    #[test]
    fn tab_and_back_tab_cycle_focus() {
        let mut f = form();
        assert_eq!(f.handle_key(key(KeyCode::Tab)), Step::Continue);
        assert_eq!(f.focus(), Field::Root);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.focus(), Field::Config);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.focus(), Field::Name);
        f.handle_key(key(KeyCode::BackTab));
        assert_eq!(f.focus(), Field::Config);
    }

    #[test]
    fn typing_edits_the_focused_field() {
        let mut f = form();
        for c in "acme".chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(f.name(), "acme");
        f.handle_key(key(KeyCode::Backspace));
        assert_eq!(f.name(), "acm");
        f.handle_key(key(KeyCode::Tab));
        f.handle_key(key(KeyCode::Char('/')));
        assert_eq!(f.root(), "/tmp/here/");
        assert_eq!(f.name(), "acm");
    }

    #[test]
    fn config_toggles_between_in_repo_and_global() {
        let mut f = form();
        f.inner.set_focus(Field::Config);
        assert_eq!(f.mode(), ConfigMode::Global);
        f.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(f.mode(), ConfigMode::InRepo);
        f.handle_key(key(KeyCode::Left));
        assert_eq!(f.mode(), ConfigMode::Global);
        f.handle_key(key(KeyCode::Right));
        assert_eq!(f.mode(), ConfigMode::InRepo);
    }

    #[test]
    fn typing_on_config_field_does_not_leak_into_a_text_buffer() {
        let mut f = form();
        f.inner.set_focus(Field::Config);
        f.handle_key(key(KeyCode::Char('x')));
        assert_eq!(f.mode(), ConfigMode::Global);
        assert_eq!(f.name(), "");
        assert_eq!(f.root(), "/tmp/here");
    }

    #[test]
    fn esc_cancels_and_enter_submits() {
        let mut f = form();
        assert_eq!(f.handle_key(key(KeyCode::Esc)), Step::Cancel);
        assert_eq!(f.handle_key(key(KeyCode::Enter)), Step::Submit);
    }

    #[test]
    fn editing_clears_a_stale_error() {
        let mut f = form();
        f.set_error("boom");
        assert!(f.inner.error().is_some());
        f.handle_key(key(KeyCode::Char('a')));
        assert!(f.inner.error().is_none());
    }

    fn render_to_string(f: &Form, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|frame| render(frame, frame.area(), f)).unwrap();
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

    /// The form (title, labels, the focused field and cursor, the radio,
    /// the detection line, an inline error, the key hints) is readable on a
    /// light terminal with each control focused.
    #[test]
    fn a_focused_label_underlines_its_letters_not_its_gutter() {
        for line in [field_line("Name", "alpha", true), config_line(ConfigMode::InRepo, true)] {
            let underlined: String = line
                .spans
                .iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
                .map(|s| s.content.as_ref())
                .collect();
            assert!(["Name", "Config"].contains(&underlined.as_str()), "{underlined:?}");
            let gutter = &line.spans[2];
            assert!(gutter.content.trim().is_empty() && !gutter.content.is_empty());
            assert!(gutter.style.sub_modifier.contains(Modifier::UNDERLINED), "the gutter clears it");
        }
    }

    #[test]
    fn renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            for (name, focus) in [
                ("add-project-name", Field::Name),
                ("add-project-root", Field::Root),
                ("add-project-config", Field::Config),
            ] {
                let mut f = form();
                for c in "acme".chars() {
                    f.handle_key(key(KeyCode::Char(c)));
                }
                f.set_error("a project already lives at `acme`".to_string());
                f.inner.set_focus(focus);
                let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
                term.draw(|frame| f.render(frame, frame.area())).unwrap();
                crate::theme::assert_light_readable(name, term.backend().buffer());
            }
        });
    }

    #[test]
    fn render_paints_fields_detected_error_and_hints() {
        let mut f = Form::new(Path::new("/tmp/here"));
        for c in "acme".chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
        f.set_error("a project already lives at `acme`".to_string());
        let out = render_to_string(&f, 80, 24);
        for needle in [
            "Add a project",
            "Name",
            "Repo path",
            "Config",
            "In repo",
            "Global",
            "Detected:",
            "already lives at",
            "[ Enter ] Create",
            "[ Esc ] Cancel",
            "[ Tab ] Next field",
        ] {
            assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
        }
    }
}
