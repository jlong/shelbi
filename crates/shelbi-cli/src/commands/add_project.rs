//! "Add project" dialog.
//!
//! Reached from the command palette's `Add project` entry. Presents a
//! native ratatui form — name, repo/root path, and config location
//! (in-repo vs global) — inside the palette's own alt-screen, so the
//! flow reads as a seamless dialog rather than a terminal handoff (the
//! same in-alt-screen pattern the Zen-intro and Quit popovers use).
//!
//! On confirm the collected values are handed to the exact `shelbi init`
//! scaffolder ([`super::init::scaffold_with_prompt`] driven
//! non-interactively), so the result is byte-identical to what a
//! command-line `shelbi init --project … --root … --mode …` would
//! produce — config written, agents/workflows/statuses materialized, and
//! the project registered so it shows up in the Switch-Project picker.
//!
//! The state machine ([`step`]) + renderer ([`render`]) are pure data so
//! the focus-cycle, text editing, radio toggle, and submit paths are
//! unit-tested without a terminal. Validation (duplicate name, bad path)
//! touches the filesystem, so it lives in the loop ([`run`]) — a `Submit`
//! outcome from the pure step function is re-checked there and, on
//! failure, an inline error is stashed back on the state rather than
//! writing a partial config.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
};
use shelbi_state::keymap::{GlobalAction, KeyChord, Keymaps};

use super::init::InitMode;
use crate::project_root::{
    absolutize, project_name_collides, validate_root, RootValidation,
};

/// The three focusable controls, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Root,
    Config,
}

/// Everything the dialog collects, plus transient UI state (focus + the
/// last inline validation error). Cloned once per frame so the render is
/// a pure function of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormState {
    pub name: String,
    pub root: String,
    pub mode: InitMode,
    pub focus: Field,
    pub error: Option<String>,
}

impl FormState {
    /// Seed the form. Root prefills to the launch directory (the same
    /// default `shelbi init`'s "Project root?" prompt offers); config
    /// location defaults to `global`, the low-ceremony solo choice the
    /// user can flip to in-repo with one keystroke.
    pub fn new(cwd: &Path) -> Self {
        Self {
            name: String::new(),
            root: cwd.display().to_string(),
            mode: InitMode::Global,
            focus: Field::Name,
            error: None,
        }
    }
}

/// The validated result the loop returns on a successful confirm. Ready
/// to drive [`super::init::scaffold_with_prompt`] verbatim.
///
/// `name` is the **human-readable** name exactly as the user typed it (any
/// characters). The scaffolder slugifies it into the on-disk id and records
/// the original as `display_name` only when the two differ — the same path a
/// command-line `shelbi init --project "<name>"` takes — so both entry points
/// behave identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddProjectForm {
    pub name: String,
    pub root: PathBuf,
    pub mode: InitMode,
}

/// Result of feeding one key into the form state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Continue,
    Cancelled,
    /// The user asked to create (Enter). The caller validates the current
    /// values against disk before accepting — a `Submit` is a request,
    /// not a guarantee the input is good.
    Submit,
}

/// Advance the pure form state machine.
///
/// Bindings:
/// - `Esc` cancels regardless of focus.
/// - `Enter` requests submission regardless of focus.
/// - `Tab` / `BackTab` cycle focus Name → Root → Config → Name.
/// - On a text field (`Name`/`Root`): printable chars append, `Backspace`
///   deletes the last char.
/// - On `Config`: `Space` / `Left` / `Right` / `Up` / `Down` toggle
///   between in-repo and global.
///
/// Any edit clears a stale inline error so the message disappears the
/// moment the user starts fixing the input.
pub fn step(state: &mut FormState, key: KeyEvent) -> StepOutcome {
    match key.code {
        KeyCode::Esc => return StepOutcome::Cancelled,
        KeyCode::Enter => return StepOutcome::Submit,
        KeyCode::Tab => {
            state.focus = next_field(state.focus);
            state.error = None;
        }
        KeyCode::BackTab => {
            state.focus = prev_field(state.focus);
            state.error = None;
        }
        _ => match state.focus {
            Field::Name => edit_text(&mut state.name, &mut state.error, key),
            Field::Root => edit_text(&mut state.root, &mut state.error, key),
            Field::Config => toggle_config(state, key),
        },
    }
    StepOutcome::Continue
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

fn toggle_config(state: &mut FormState, key: KeyEvent) {
    match key.code {
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Char(' ') => {
            state.mode = match state.mode {
                InitMode::InRepo => InitMode::Global,
                InitMode::Global => InitMode::InRepo,
            };
            state.error = None;
        }
        _ => {}
    }
}

/// Cheap, cached detection shown on the "Detected:" line. Deliberately
/// limited to the git-repo check `validate_root` already performs — the
/// heavier default-branch / workspace-count / machine detection
/// `shelbi init` runs is deferred to the scaffold step so the dialog
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
            return Span::styled(
                "Detected: path not found",
                Style::default().fg(Color::Yellow),
            );
        }
        if self.is_git {
            return Span::styled(
                "Detected: git repo ✓",
                Style::default().fg(Color::Green),
            );
        }
        Span::styled(
            "Detected: not a git repo (shelbi expects git, but will continue)",
            Style::default().fg(Color::Yellow),
        )
    }
}

/// Slugify the entered name the same way the scaffolder will. Returns the
/// derived id (`ContextStore` → `contextstore`, `My App` → `my-app`), or
/// `None` when the input has no `[a-z0-9]` to build an id from (empty or
/// all-punctuation). The dialog uses this for both the live preview line and
/// the pre-submit collision check, so what the preview shows is exactly the
/// id the project lands under.
fn derived_slug(name: &str) -> Option<String> {
    shelbi_core::normalize_project_name(name.trim()).ok()
}

/// Validate the current form against disk. Returns the ready-to-scaffold
/// [`AddProjectForm`] on success, or a user-facing inline error string on
/// failure. Any human-readable name is accepted — it's slugified into the
/// on-disk id ([`derived_slug`]) and the collision / path guards run against
/// that slug, mirroring what `shelbi init` does before anything is written.
fn validate(state: &FormState, cwd: &Path) -> std::result::Result<AddProjectForm, String> {
    let name = state.name.trim();
    if name.is_empty() {
        return Err("Enter a project name.".to_string());
    }
    // Slugify for storage. Only an empty / all-punctuation name has nothing to
    // build an id from — every other human-readable name is accepted.
    let slug = derived_slug(name).ok_or_else(|| {
        format!("`{name}` has no letters or digits to build an id from — try a name like `my-app`.")
    })?;
    // Collision is checked on the SLUG (the folder / settings-file id), not the
    // display name, so two different labels that slugify the same still clash.
    match project_name_collides(&slug) {
        Ok(true) => {
            return Err(format!(
                "a project already lives at `{slug}` — pick a different name."
            ))
        }
        Ok(false) => {}
        Err(e) => return Err(e.to_string()),
    }

    let root_raw = state.root.trim();
    if root_raw.is_empty() {
        return Err("Enter the project's repo path.".to_string());
    }
    let root = absolutize(cwd, Path::new(root_raw));
    match validate_root(&root) {
        // A non-git dir is allowed (shelbi expects git but doesn't hard-reject),
        // matching `shelbi init --root`'s warn-and-continue behavior.
        RootValidation::Ok | RootValidation::NotGitRepo => {}
        RootValidation::NotExists => {
            return Err(format!("{} does not exist.", root.display()))
        }
        RootValidation::NotDirectory => {
            return Err(format!("{} is not a directory.", root.display()))
        }
    }

    Ok(AddProjectForm {
        name: name.to_string(),
        root,
        mode: state.mode,
    })
}

/// Run the dialog loop, sharing the palette's alt-screen. Returns
/// `Ok(Some(form))` when the user confirms with valid input, `Ok(None)`
/// when they cancel (Esc, or the opener chord as-close). Does not touch
/// the terminal's raw/alt-screen state — the caller owns setup/teardown.
pub fn run<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    keymaps: &Keymaps,
    cwd: &Path,
) -> Result<Option<AddProjectForm>> {
    // Opener-as-close: re-pressing the palette chord dismisses the dialog,
    // same convention the Quit / Zen-intro popovers follow.
    let opener_close = keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .copied();

    let mut state = FormState::new(cwd);
    // Cache the git-repo detection and only recompute when the root text
    // changes, so we don't spawn `git rev-parse` on every 150ms poll.
    let mut detect = Detected::for_root(cwd, &state.root);
    let mut detect_root = state.root.clone();

    loop {
        if state.root != detect_root {
            detect = Detected::for_root(cwd, &state.root);
            detect_root = state.root.clone();
        }
        let snapshot = state.clone();
        let detected = detect;
        term.draw(|f| render(f, f.area(), &snapshot, &detected))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(c) = opener_close {
                    if c == KeyChord::from_event(k) {
                        return Ok(None);
                    }
                }
                match step(&mut state, k) {
                    StepOutcome::Continue => {}
                    StepOutcome::Cancelled => return Ok(None),
                    StepOutcome::Submit => match validate(&state, cwd) {
                        Ok(form) => return Ok(Some(form)),
                        Err(msg) => state.error = Some(msg),
                    },
                }
            }
        }
    }
}

/// Paint the dialog. The card **fills the palette window** (the whole `area`
/// the palette occupies) rather than a small centered box, so the fields, the
/// derived-slug preview, and long error/notice text always have room and never
/// truncate. Holds the three fields, the derived id/path preview, the detected
/// line, an inline error row (blank when clean), and the key hints.
fn render(f: &mut Frame, area: Rect, state: &FormState, detected: &Detected) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(
            " Add a project ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    lines.push(field_line("Name", &state.name, state.focus == Field::Name));
    lines.push(field_line(
        "Repo path",
        &state.root,
        state.focus == Field::Root,
    ));
    lines.push(config_line(state.mode, state.focus == Field::Config));
    lines.push(Line::raw(""));
    lines.push(Line::from(detected.line()));
    lines.push(preview_line(&state.name, state.mode));
    lines.push(Line::raw(""));
    // Error row is reserved unconditionally so the hints don't reflow when
    // a validation message appears.
    lines.push(match &state.error {
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

    // Wrap long lines (a lengthy repo path or a long validation message) onto
    // the next row instead of clipping them — with the dialog now filling the
    // whole palette window there's room, and nothing should ever truncate.
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The live "where this lands" preview: the derived slug and the path its
/// config will be written to. Shows a dimmed hint while the name is still
/// empty / all-punctuation so the row never renders a bogus id.
fn preview_line(name: &str, mode: InitMode) -> Line<'static> {
    let label_style = Style::default().fg(Color::Gray);
    match derived_slug(name) {
        Some(slug) => {
            let path = match mode {
                InitMode::Global => format!("~/.shelbi/projects/{slug}.yaml"),
                InitMode::InRepo => "<repo>/.shelbi/project.yaml".to_string(),
            };
            Line::from(vec![
                Span::styled("Folder / id: ", label_style),
                Span::styled(
                    slug,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
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

/// One labelled text field. The focused field shows a cyan cursor bar
/// after its value and an underlined label so the active control is
/// unambiguous.
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
fn config_line(mode: InitMode, focused: bool) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::Gray)
    };
    let in_repo = if mode == InitMode::InRepo {
        "(•) In repo"
    } else {
        "( ) In repo"
    };
    let global = if mode == InitMode::Global {
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
        Span::styled(in_repo.to_string(), opt_style(mode == InitMode::InRepo)),
        Span::raw("   "),
        Span::styled(global.to_string(), opt_style(mode == InitMode::Global)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn state() -> FormState {
        FormState::new(Path::new("/tmp/here"))
    }

    #[test]
    fn new_prefills_root_and_defaults_to_global() {
        let s = FormState::new(Path::new("/tmp/here"));
        assert_eq!(s.name, "");
        assert_eq!(s.root, "/tmp/here");
        assert_eq!(s.mode, InitMode::Global);
        assert_eq!(s.focus, Field::Name);
        assert!(s.error.is_none());
    }

    #[test]
    fn tab_cycles_focus_name_root_config() {
        let mut s = state();
        assert_eq!(step(&mut s, key(KeyCode::Tab)), StepOutcome::Continue);
        assert_eq!(s.focus, Field::Root);
        assert_eq!(step(&mut s, key(KeyCode::Tab)), StepOutcome::Continue);
        assert_eq!(s.focus, Field::Config);
        assert_eq!(step(&mut s, key(KeyCode::Tab)), StepOutcome::Continue);
        assert_eq!(s.focus, Field::Name);
    }

    #[test]
    fn back_tab_walks_focus_in_reverse() {
        let mut s = state();
        assert_eq!(step(&mut s, key(KeyCode::BackTab)), StepOutcome::Continue);
        assert_eq!(s.focus, Field::Config);
        assert_eq!(step(&mut s, key(KeyCode::BackTab)), StepOutcome::Continue);
        assert_eq!(s.focus, Field::Root);
    }

    #[test]
    fn typing_edits_the_focused_text_field() {
        let mut s = state();
        for c in "acme".chars() {
            step(&mut s, key(KeyCode::Char(c)));
        }
        assert_eq!(s.name, "acme");
        // Backspace deletes the last char.
        step(&mut s, key(KeyCode::Backspace));
        assert_eq!(s.name, "acm");
        // Move to the root field and edit that instead.
        step(&mut s, key(KeyCode::Tab));
        step(&mut s, key(KeyCode::Char('/')));
        assert_eq!(s.root, "/tmp/here/");
        assert_eq!(s.name, "acm");
    }

    #[test]
    fn config_toggles_between_in_repo_and_global() {
        let mut s = state();
        s.focus = Field::Config;
        assert_eq!(s.mode, InitMode::Global);
        step(&mut s, key(KeyCode::Char(' ')));
        assert_eq!(s.mode, InitMode::InRepo);
        step(&mut s, key(KeyCode::Left));
        assert_eq!(s.mode, InitMode::Global);
        step(&mut s, key(KeyCode::Right));
        assert_eq!(s.mode, InitMode::InRepo);
    }

    #[test]
    fn typing_on_config_field_does_not_leak_into_a_text_buffer() {
        // A printable char while Config is focused toggles nothing but the
        // space bar; other chars are ignored and never touch name/root.
        let mut s = state();
        s.focus = Field::Config;
        step(&mut s, key(KeyCode::Char('x')));
        assert_eq!(s.mode, InitMode::Global);
        assert_eq!(s.name, "");
        assert_eq!(s.root, "/tmp/here");
    }

    #[test]
    fn esc_cancels_and_enter_submits() {
        let mut s = state();
        assert_eq!(step(&mut s, key(KeyCode::Esc)), StepOutcome::Cancelled);
        assert_eq!(step(&mut s, key(KeyCode::Enter)), StepOutcome::Submit);
    }

    #[test]
    fn editing_clears_a_stale_error() {
        let mut s = state();
        s.error = Some("boom".to_string());
        step(&mut s, key(KeyCode::Char('a')));
        assert!(s.error.is_none());
    }

    #[test]
    fn validate_rejects_empty_and_all_punctuation_names() {
        let cwd = Path::new("/tmp");
        let mut s = FormState::new(cwd);
        s.name = String::new();
        assert!(validate(&s, cwd).is_err());
        // An all-punctuation name has nothing to slugify → clear suggestion.
        s.name = "!!!".to_string();
        let err = validate(&s, cwd).unwrap_err();
        assert!(err.contains("my-app"), "expected a suggestion, got: {err}");
    }

    #[test]
    fn derived_slug_slugifies_human_readable_names() {
        assert_eq!(derived_slug("ContextStore").as_deref(), Some("contextstore"));
        assert_eq!(derived_slug("My App").as_deref(), Some("my-app"));
        assert_eq!(derived_slug("  Foo Bar!! ").as_deref(), Some("foo-bar"));
        // Already-clean slug passes through unchanged.
        assert_eq!(derived_slug("my-app").as_deref(), Some("my-app"));
        // Nothing to build an id from.
        assert_eq!(derived_slug(""), None);
        assert_eq!(derived_slug("!!!"), None);
    }

    /// A mixed-case / spaced name is accepted (not rejected as it once was):
    /// `validate` keeps the raw human-readable name on the form so the
    /// scaffolder can slugify it and record the original as `display_name`.
    #[test]
    fn validate_accepts_human_readable_name_and_keeps_it_raw() {
        let _g = crate::commands::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-add-project-accept-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // `/tmp` exists (non-git is allowed), so only the name path is exercised.
        let cwd = Path::new("/tmp");
        let mut s = FormState::new(cwd);
        s.name = "My App".to_string();
        let form = validate(&s, cwd).expect("human-readable name should be accepted");
        // The form carries the raw name; slugification happens downstream.
        assert_eq!(form.name, "My App");

        std::env::remove_var("SHELBI_HOME");
    }

    /// Collision is checked against the derived slug, so two different labels
    /// that slugify to the same id clash — and the error names the slug.
    #[test]
    fn validate_rejects_slug_collision() {
        let _g = crate::commands::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-add-project-collide-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(home.join("projects/contextstore.yaml"), "name: contextstore\n").unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let cwd = Path::new("/tmp");
        let mut s = FormState::new(cwd);
        s.name = "ContextStore".to_string();
        let err = validate(&s, cwd).unwrap_err();
        assert!(
            err.contains("contextstore"),
            "collision error should name the slug, got: {err}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn validate_rejects_missing_root() {
        let cwd = Path::new("/tmp");
        let mut s = FormState::new(cwd);
        s.name = "ok-name".to_string();
        s.root = "/definitely/not/a/real/path/xyzzy".to_string();
        let err = validate(&s, cwd).unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn render_paints_fields_detected_and_hints() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut s = FormState::new(Path::new("/tmp/here"));
        s.name = "acme".to_string();
        s.error = Some("A shelbi project named `acme` already exists".to_string());
        let detected = Detected {
            exists: true,
            is_git: true,
        };
        term.draw(|f| render(f, f.area(), &s, &detected)).unwrap();
        let buf = term.backend().buffer().clone();
        let dumped: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");

        for needle in [
            "Add a project",
            "Name",
            "Repo path",
            "Config",
            "In repo",
            "Global",
            "Detected: git repo",
            "already exists",
            "[ Enter ] Create",
            "[ Esc ] Cancel",
            "[ Tab ] Next field",
        ] {
            assert!(
                dumped.contains(needle),
                "missing {needle:?} in rendered dialog:\n{dumped}"
            );
        }
    }
}
