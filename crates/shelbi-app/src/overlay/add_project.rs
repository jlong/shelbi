//! The add-project form's state machine.
//!
//! Collects a project **name**, a **repo path**, and a **config location**
//! (in-repo vs global). On submit the host validates and scaffolds through
//! `shelbi_orchestrator::project_create` (the same engine `shelbi init` uses)
//! and, on a validation failure, stashes the message back with
//! [`Form::set_error`]; the form never touches the filesystem itself.
//!
//! The "Detected:" line reports whether the repo path is a git repo. Checking
//! means running git, so the form only says *when* a check is due
//! ([`Form::detection_due`]); the host runs it (the TUI inline, the desktop
//! off the UI thread) and hands the answer back with [`Form::set_detected`].

use std::path::{Path, PathBuf};

use shelbi_core::ConfigMode;
use shelbi_state::keymap::{Key, KeyChord};

use super::typed_char;

/// The dialog's title.
pub const TITLE: &str = "Add a project";

/// The hint line.
pub const HINT: &str = "[ Enter ] Create    [ Esc ] Cancel    [ Tab ] Next field";

/// The focusable controls, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Root,
    Config,
}

impl Field {
    fn next(self) -> Field {
        match self {
            Field::Name => Field::Root,
            Field::Root => Field::Config,
            Field::Config => Field::Name,
        }
    }

    fn prev(self) -> Field {
        match self {
            Field::Name => Field::Config,
            Field::Root => Field::Name,
            Field::Config => Field::Root,
        }
    }
}

/// What one key resolved to: keep editing, submit (read [`Form::name`] /
/// [`Form::root`] / [`Form::mode`]), or cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Submit,
    Cancel,
}

/// What the repo path looks like, for the "Detected:" line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detected {
    NotFound,
    GitRepo,
    NotGitRepo,
}

impl Detected {
    /// The line's text.
    pub fn message(self) -> &'static str {
        match self {
            Detected::NotFound => "Detected: path not found",
            Detected::GitRepo => "Detected: git repo ✓",
            Detected::NotGitRepo => {
                "Detected: not a git repo (shelbi expects git, but will continue)"
            }
        }
    }

    /// Whether the line reads as good news (green) rather than a warning.
    pub fn is_ok(self) -> bool {
        self == Detected::GitRepo
    }
}

/// The form's values and transient UI state.
#[derive(Debug, Clone)]
pub struct Form {
    name: String,
    root: String,
    mode: ConfigMode,
    focus: Field,
    error: Option<String>,
    /// The launch directory a relative repo path resolves against.
    cwd: PathBuf,
    /// The latest detection, and the root text it was computed for.
    detected: Option<(String, Detected)>,
}

impl Form {
    /// Seed the form: the repo path prefills to `cwd` (the default `shelbi
    /// init` offers), the config location defaults to global.
    pub fn new(cwd: &Path) -> Self {
        Self {
            name: String::new(),
            root: cwd.display().to_string(),
            mode: ConfigMode::Global,
            focus: Field::Name,
            error: None,
            cwd: cwd.to_path_buf(),
            detected: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn mode(&self) -> ConfigMode {
        self.mode
    }

    pub fn focus(&self) -> Field {
        self.focus
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Show an inline validation error after a failed submit; cleared by the
    /// next edit.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
    }

    /// The latest detection, possibly for an earlier root text while a new
    /// check is in flight.
    pub fn detected(&self) -> Option<Detected> {
        self.detected.as_ref().map(|(_, d)| *d)
    }

    /// The root text a detection is due for: `Some` when the last answer (if
    /// any) was for different text. The host checks it and calls
    /// [`Self::set_detected`].
    pub fn detection_due(&self) -> Option<String> {
        match &self.detected {
            Some((root, _)) if *root == self.root => None,
            _ => Some(self.root.clone()),
        }
    }

    /// Record a detection for `root`. An answer for text the user has since
    /// changed is still shown until a fresher one lands, and
    /// [`Self::detection_due`] keeps asking for the current text.
    pub fn set_detected(&mut self, root: &str, detected: Detected) {
        self.detected = Some((root.to_string(), detected));
    }

    /// Feed one key. Esc cancels and Enter submits from any field; Tab /
    /// BackTab cycle focus; on a text field typed characters append and
    /// Backspace deletes; on the config row Space or an arrow flips the
    /// location. Any edit clears a stale error.
    pub fn handle_key(&mut self, chord: KeyChord) -> Step {
        match chord.code {
            Key::Esc => Step::Cancel,
            Key::Enter => Step::Submit,
            Key::Tab => {
                self.focus = self.focus.next();
                self.error = None;
                Step::Continue
            }
            Key::BackTab => {
                self.focus = self.focus.prev();
                self.error = None;
                Step::Continue
            }
            _ => {
                match self.focus {
                    Field::Name => edit_text(&mut self.name, &mut self.error, chord),
                    Field::Root => edit_text(&mut self.root, &mut self.error, chord),
                    Field::Config => toggle_config(&mut self.mode, &mut self.error, chord),
                }
                Step::Continue
            }
        }
    }

    /// Move focus to `field`.
    pub fn set_focus(&mut self, field: Field) {
        self.focus = field;
    }

    /// A click on a field: focus it (a click on the config row also flips it,
    /// as Space would).
    pub fn click_field(&mut self, field: Field) {
        if field == Field::Config && self.focus == Field::Config {
            self.mode = flip(self.mode);
        }
        self.focus = field;
        self.error = None;
    }

    /// The "Folder / id:" preview: the derived slug and where its config will
    /// be written, or `None` while the name has nothing to build an id from.
    pub fn preview(&self) -> Option<(String, String)> {
        let slug = derived_slug(&self.name)?;
        let path = match self.mode {
            ConfigMode::Global => format!("~/.shelbi/projects/{slug}.yaml"),
            ConfigMode::InRepo => "<repo>/.shelbi/project.yaml".to_string(),
        };
        Some((slug, path))
    }
}

/// Slugify the entered name the way the scaffolder will. `None` when it has
/// no `[a-z0-9]` to build an id from.
pub fn derived_slug(name: &str) -> Option<String> {
    shelbi_core::normalize_project_name(name.trim()).ok()
}

fn edit_text(buf: &mut String, error: &mut Option<String>, chord: KeyChord) {
    if let Some(c) = typed_char(&chord) {
        buf.push(c);
        *error = None;
    } else if chord.code == Key::Backspace {
        buf.pop();
        *error = None;
    }
}

fn toggle_config(mode: &mut ConfigMode, error: &mut Option<String>, chord: KeyChord) {
    if matches!(
        chord.code,
        Key::Left | Key::Right | Key::Up | Key::Down | Key::Char(' ')
    ) {
        *mode = flip(*mode);
        *error = None;
    }
}

fn flip(mode: ConfigMode) -> ConfigMode {
    match mode {
        ConfigMode::InRepo => ConfigMode::Global,
        ConfigMode::Global => ConfigMode::InRepo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::test_keys::{ch, ctrl, key};
    use shelbi_state::keymap::Mods;

    fn form() -> Form {
        Form::new(Path::new("/tmp/alpha"))
    }

    #[test]
    fn new_prefills_the_root_and_defaults_to_global() {
        let f = form();
        assert_eq!(f.name(), "");
        assert_eq!(f.root(), "/tmp/alpha");
        assert_eq!(f.mode(), ConfigMode::Global);
        assert_eq!(f.focus(), Field::Name);
        assert_eq!(f.detection_due().as_deref(), Some("/tmp/alpha"));
    }

    #[test]
    fn tab_and_back_tab_cycle_focus() {
        let mut f = form();
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Root);
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Config);
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Name);
        f.handle_key(key(Key::BackTab));
        assert_eq!(f.focus(), Field::Config);
    }

    #[test]
    fn typing_edits_the_focused_field_only() {
        let mut f = form();
        for c in "My App".chars() {
            let mods = if c.is_ascii_uppercase() { Mods::SHIFT } else { Mods::NONE };
            f.handle_key(KeyChord::new(Key::Char(c), mods));
        }
        f.handle_key(ctrl('a'));
        assert_eq!(f.name(), "My App");
        f.handle_key(key(Key::Backspace));
        assert_eq!(f.name(), "My Ap");
        f.handle_key(key(Key::Tab));
        f.handle_key(ch('/'));
        assert_eq!(f.root(), "/tmp/alpha/");
        assert_eq!(f.detection_due().as_deref(), Some("/tmp/alpha/"));
    }

    #[test]
    fn config_toggles_and_never_types() {
        let mut f = form();
        f.handle_key(key(Key::BackTab));
        f.handle_key(ch(' '));
        assert_eq!(f.mode(), ConfigMode::InRepo);
        f.handle_key(key(Key::Left));
        assert_eq!(f.mode(), ConfigMode::Global);
        f.handle_key(ch('x'));
        assert_eq!(f.name(), "");
        assert_eq!(f.root(), "/tmp/alpha");
    }

    #[test]
    fn esc_cancels_and_enter_submits_from_any_field() {
        let mut f = form();
        assert_eq!(f.handle_key(key(Key::Enter)), Step::Submit);
        f.handle_key(key(Key::Tab));
        assert_eq!(f.handle_key(key(Key::Esc)), Step::Cancel);
    }

    #[test]
    fn editing_clears_a_stale_error() {
        let mut f = form();
        f.set_error("taken");
        assert_eq!(f.error(), Some("taken"));
        f.handle_key(ch('a'));
        assert_eq!(f.error(), None);
    }

    #[test]
    fn detection_is_due_until_answered_for_the_current_root() {
        let mut f = form();
        f.set_detected("/tmp/alpha", Detected::GitRepo);
        assert_eq!(f.detection_due(), None);
        assert_eq!(f.detected(), Some(Detected::GitRepo));
        f.handle_key(key(Key::Tab));
        f.handle_key(ch('x'));
        // The stale answer still shows while the new one is due.
        assert_eq!(f.detected(), Some(Detected::GitRepo));
        assert_eq!(f.detection_due().as_deref(), Some("/tmp/alphax"));
        f.set_detected("/tmp/alphax", Detected::NotFound);
        assert_eq!(f.detection_due(), None);
        assert_eq!(f.detected().map(Detected::message), Some("Detected: path not found"));
    }

    #[test]
    fn the_preview_follows_the_name_and_location() {
        let mut f = form();
        assert_eq!(f.preview(), None);
        for c in "web app".chars() {
            f.handle_key(ch(c));
        }
        let (slug, path) = f.preview().unwrap();
        assert_eq!(slug, "web-app");
        assert_eq!(path, "~/.shelbi/projects/web-app.yaml");
        f.click_field(Field::Config);
        f.click_field(Field::Config);
        assert_eq!(f.mode(), ConfigMode::InRepo, "a second click flips the location");
        assert_eq!(f.preview().unwrap().1, "<repo>/.shelbi/project.yaml");
    }
}
