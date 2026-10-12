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

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use shelbi_core::ConfigMode;
use shelbi_orchestrator::project_create::{absolutize, validate_root, RootValidation};

/// The three focusable controls, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Root,
    Config,
}

/// What feeding one key into the form resolved to. The caller keeps the overlay
/// open on [`Step::Continue`], validates + creates on [`Step::Submit`] (reading
/// [`Form::name`] / [`Form::root`] / [`Form::mode`]), and discards on
/// [`Step::Cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Submit,
    Cancel,
}

/// Everything the dialog collects, plus transient UI state (focus, the last
/// inline validation error, and the cached git-repo detection).
#[derive(Debug, Clone)]
pub struct Form {
    name: String,
    root: String,
    mode: ConfigMode,
    focus: Field,
    error: Option<String>,
    /// The launch directory, used to absolutize a relative repo path for the
    /// detection line (and later for validation by the caller).
    cwd: PathBuf,
    /// Cached git-repo detection for the current `root`, recomputed only when
    /// the root text changes so we don't spawn `git rev-parse` per keystroke.
    detected: Detected,
    detect_root: String,
}

impl Form {
    /// Seed the form. Root prefills to `cwd` (the same default `shelbi init`'s
    /// "Project root?" prompt offers); config location defaults to `global`,
    /// the low-ceremony solo choice the user can flip to in-repo with one key.
    pub fn new(cwd: &Path) -> Self {
        let root = cwd.display().to_string();
        let detected = Detected::for_root(cwd, &root);
        Self {
            name: String::new(),
            root: root.clone(),
            mode: ConfigMode::Global,
            focus: Field::Name,
            error: None,
            cwd: cwd.to_path_buf(),
            detected,
            detect_root: root,
        }
    }

    /// The entered (raw, human-readable) project name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The entered repo-path text (not yet absolutized).
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The chosen config location.
    pub fn mode(&self) -> ConfigMode {
        self.mode
    }

    /// Stash an inline validation error (shown in red) after a failed submit,
    /// keeping the form open so the user can fix the input. Cleared the moment
    /// they edit anything.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
    }

    /// Feed one key event. Bindings (identical to the pre-move CLI dialog):
    /// - `Esc` cancels regardless of focus.
    /// - `Enter` requests submission regardless of focus.
    /// - `Tab` / `BackTab` cycle focus Name → Root → Config → Name.
    /// - On a text field: printable chars append, `Backspace` deletes.
    /// - On `Config`: `Space` / arrows toggle in-repo vs global.
    ///
    /// Any edit clears a stale inline error. Editing the root recomputes the
    /// cached git-repo detection.
    pub fn handle_key(&mut self, key: KeyEvent) -> Step {
        let step = match key.code {
            KeyCode::Esc => return Step::Cancel,
            KeyCode::Enter => return Step::Submit,
            KeyCode::Tab => {
                self.focus = next_field(self.focus);
                self.error = None;
                Step::Continue
            }
            KeyCode::BackTab => {
                self.focus = prev_field(self.focus);
                self.error = None;
                Step::Continue
            }
            _ => {
                match self.focus {
                    Field::Name => edit_text(&mut self.name, &mut self.error, key),
                    Field::Root => edit_text(&mut self.root, &mut self.error, key),
                    Field::Config => toggle_config(&mut self.mode, &mut self.error, key),
                }
                Step::Continue
            }
        };
        // Recompute detection only when the root text actually changed.
        if self.root != self.detect_root {
            self.detected = Detected::for_root(&self.cwd, &self.root);
            self.detect_root = self.root.clone();
        }
        step
    }

    /// Paint the dialog into `area`.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        render(f, area, self);
    }
}

fn next_field(f: Field) -> Field {
    match f {
        Field::Name => Field::Root,
        Field::Root => Field::Config,
        Field::Config => Field::Name,
    }
}

fn prev_field(f: Field) -> Field {
    match f {
        Field::Name => Field::Config,
        Field::Root => Field::Name,
        Field::Config => Field::Root,
    }
}

fn edit_text(buf: &mut String, error: &mut Option<String>, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c)
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
            buf.push(c);
            *error = None;
        }
        KeyCode::Backspace => {
            buf.pop();
            *error = None;
        }
        _ => {}
    }
}

fn toggle_config(mode: &mut ConfigMode, error: &mut Option<String>, key: KeyEvent) {
    match key.code {
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Char(' ') => {
            *mode = match *mode {
                ConfigMode::InRepo => ConfigMode::Global,
                ConfigMode::Global => ConfigMode::InRepo,
            };
            *error = None;
        }
        _ => {}
    }
}

/// Cheap, cached detection shown on the "Detected:" line — limited to the
/// git-repo check [`validate_root`] performs; the heavier default-branch /
/// runner / machine detection is deferred to the scaffold step so the dialog
/// stays responsive per keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Detected {
    exists: bool,
    is_git: bool,
}

impl Detected {
    fn for_root(cwd: &Path, root: &str) -> Self {
        let trimmed = root.trim();
        if trimmed.is_empty() {
            return Self {
                exists: false,
                is_git: false,
            };
        }
        let path = absolutize(cwd, Path::new(trimmed));
        match validate_root(&path) {
            RootValidation::Ok => Self {
                exists: true,
                is_git: true,
            },
            RootValidation::NotGitRepo => Self {
                exists: true,
                is_git: false,
            },
            RootValidation::NotExists | RootValidation::NotDirectory => Self {
                exists: false,
                is_git: false,
            },
        }
    }

    fn line(&self) -> Span<'static> {
        if !self.exists {
            return Span::styled("Detected: path not found", Style::default().fg(Color::Yellow));
        }
        if self.is_git {
            return Span::styled("Detected: git repo ✓", Style::default().fg(Color::Green));
        }
        Span::styled(
            "Detected: not a git repo (shelbi expects git, but will continue)",
            Style::default().fg(Color::Yellow),
        )
    }
}

/// Slugify the entered name the same way the scaffolder will, for the live
/// preview. `None` when the input has no `[a-z0-9]` to build an id from.
fn derived_slug(name: &str) -> Option<String> {
    shelbi_core::normalize_project_name(name.trim()).ok()
}

/// Paint the dialog. The card **fills** `area` (rather than a small centered
/// box), so the fields, the derived-slug preview, and long error/notice text
/// always have room and never truncate.
fn render(f: &mut Frame, area: Rect, form: &Form) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(
            " Add a project ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    lines.push(field_line("Name", &form.name, form.focus == Field::Name));
    lines.push(field_line("Repo path", &form.root, form.focus == Field::Root));
    lines.push(config_line(form.mode, form.focus == Field::Config));
    lines.push(Line::raw(""));
    lines.push(Line::from(form.detected.line()));
    lines.push(preview_line(&form.name, form.mode));
    lines.push(Line::raw(""));
    // Error row is reserved unconditionally so the hints don't reflow when a
    // validation message appears.
    lines.push(match &form.error {
        Some(msg) => Line::from(Span::styled(
            format!("✗ {msg}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        None => Line::raw(""),
    });
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        "[ Enter ] Create    [ Esc ] Cancel    [ Tab ] Next field",
        Style::default().fg(Color::DarkGray),
    )]));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The live "where this lands" preview: the derived slug and the path its
/// config will be written to.
fn preview_line(name: &str, mode: ConfigMode) -> Line<'static> {
    let label_style = Style::default().fg(Color::Gray);
    match derived_slug(name) {
        Some(slug) => {
            let path = match mode {
                ConfigMode::Global => format!("~/.shelbi/projects/{slug}.yaml"),
                ConfigMode::InRepo => "<repo>/.shelbi/project.yaml".to_string(),
            };
            Line::from(vec![
                Span::styled("Folder / id: ", label_style),
                Span::styled(
                    slug,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("   ({path})"), Style::default().fg(Color::DarkGray)),
            ])
        }
        None => Line::from(Span::styled(
            "Folder / id: (type a name to see the derived id)",
            Style::default().fg(Color::DarkGray),
        )),
    }
}

/// One labelled text field. The focused field shows a cyan cursor bar after its
/// value and an underlined label.
fn field_line(label: &str, value: &str, focused: bool) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut spans = vec![
        Span::styled(format!("{label:<11}"), label_style),
        Span::raw(value.to_string()),
    ];
    if focused {
        spans.push(Span::styled("▏", Style::default().fg(Color::Cyan)));
    }
    Line::from(spans)
}

/// The config-location radio row: `( ) In repo   (•) Global (~/.shelbi)`.
fn config_line(mode: ConfigMode, focused: bool) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::Gray)
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
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };
    Line::from(vec![
        Span::styled(format!("{:<11}", "Config"), label_style),
        Span::styled(in_repo.to_string(), opt_style(mode == ConfigMode::InRepo)),
        Span::raw("   "),
        Span::styled(global.to_string(), opt_style(mode == ConfigMode::Global)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(f.name, "");
        assert_eq!(f.root, "/tmp/here");
        assert_eq!(f.mode, ConfigMode::Global);
        assert_eq!(f.focus, Field::Name);
        assert!(f.error.is_none());
    }

    #[test]
    fn tab_and_back_tab_cycle_focus() {
        let mut f = form();
        assert_eq!(f.handle_key(key(KeyCode::Tab)), Step::Continue);
        assert_eq!(f.focus, Field::Root);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.focus, Field::Config);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.focus, Field::Name);
        f.handle_key(key(KeyCode::BackTab));
        assert_eq!(f.focus, Field::Config);
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
        f.focus = Field::Config;
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
        f.focus = Field::Config;
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
        assert!(f.error.is_some());
        f.handle_key(key(KeyCode::Char('a')));
        assert!(f.error.is_none());
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
