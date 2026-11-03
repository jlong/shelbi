//! The TUI's new-project flow: three pages in one dialog, in the desktop
//! wizard's order ([`crate::add_project_wizard`]) with its copy:
//!
//! 1. **Folder** ("Add a project"): the project's **name** and **repo path**,
//!    with the "Detected:" line and the derived id.
//! 2. **Agent defaults**: the runner and model the project's agents use by
//!    default, on the shared [`HarnessPicker`] (the orchestrator onboarding's
//!    pickers).
//! 3. **Settings** ("Where should settings live?"): the **config location**
//!    (in the project vs outside it).
//!
//! Each page has Back and its primary button (Continue, then Create Project)
//! under a "Step N of 3" line. Continue on page 1 asks the host to validate
//! the name and path ([`Step::Validate`]; the host runs
//! `shelbi_orchestrator::project_create::validate_add_project` and answers
//! with [`Form::folder_valid`] or [`Form::set_error`]); Create Project hands
//! back [`Step::Submit`] and the host scaffolds the project through the same
//! engine `shelbi init` uses, with [`Form::defaults`]. The form never touches
//! the filesystem itself.
//!
//! The "Detected:" line reports whether the repo path is a git repo. Checking
//! means running git, so the form only says *when* a check is due
//! ([`Form::detection_due`]); the host runs it and hands the answer back with
//! [`Form::set_detected`].

use std::path::{Path, PathBuf};

use shelbi_core::{AgentDefaults, ConfigMode, RunnerKind};
use shelbi_state::keymap::{Key, KeyChord, Mods};
use shelbi_state::orchestrator_setup::RunnerSetupError;

use super::typed_char;
use crate::add_project_wizard as wiz;
use crate::harness_picker::{no_model_note, HarnessPicker};

/// Page 1's title.
pub const TITLE: &str = "Add a project";

/// The footer's buttons.
pub const BACK: &str = "Back";
pub const CONTINUE: &str = "Continue";
pub const CREATE: &str = "Create Project";

/// The pages, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Folder,
    Defaults,
    Settings,
}

impl Page {
    /// The footer's "Step N of 3".
    pub fn step(self) -> usize {
        match self {
            Page::Folder => 1,
            Page::Defaults => 2,
            Page::Settings => 3,
        }
    }

    /// "Step 2 of 3".
    pub fn step_label(self) -> String {
        format!("Step {} of {}", self.step(), wiz::STEPS)
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Folder => TITLE,
            Page::Defaults => wiz::DEFAULTS_TITLE,
            Page::Settings => wiz::SETTINGS_TITLE,
        }
    }

    /// The line under the title (page 1 has none).
    pub fn intro(self) -> Option<&'static str> {
        match self {
            Page::Folder => None,
            Page::Defaults => Some(wiz::DEFAULTS_INTRO),
            Page::Settings => Some(wiz::SETTINGS_INTRO),
        }
    }

    /// The primary button's label.
    pub fn primary(self) -> &'static str {
        match self {
            Page::Folder | Page::Defaults => CONTINUE,
            Page::Settings => CREATE,
        }
    }
}

/// The focusable controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Page 1's text fields.
    Name,
    Root,
    /// Page 2's pickers.
    Harness,
    Model,
    /// Page 3's location.
    Config,
    /// The footer's buttons: Back, and Continue / Create Project.
    Back,
    Primary,
}

/// What one key or click resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Keep editing.
    Continue,
    /// Page 1's Continue: validate the name and path, then answer with
    /// [`Form::folder_valid`] or [`Form::set_error`].
    Validate,
    /// Create Project: read [`Form::name`] / [`Form::root`] /
    /// [`Form::mode`] / [`Form::defaults`].
    Submit,
    Cancel,
    /// Open (or close) a picker's menu ([`Field::Harness`] /
    /// [`Field::Model`]); its choice goes to [`Form::select_runner`] /
    /// [`Form::select_model`].
    Menu(Field),
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
    page: Page,
    name: String,
    root: String,
    mode: ConfigMode,
    /// Page 2's choice.
    picker: HarnessPicker,
    focus: Field,
    error: Option<String>,
    /// The launch directory a relative repo path resolves against.
    cwd: PathBuf,
    /// The latest detection, and the root text it was computed for.
    detected: Option<(String, Detected)>,
}

impl Form {
    /// Seed the form: the repo path prefills to `cwd` (the default `shelbi
    /// init` offers), the agents default to Claude Code on its default
    /// model, and the config location to global.
    pub fn new(cwd: &Path) -> Self {
        Self {
            page: Page::Folder,
            name: String::new(),
            root: cwd.display().to_string(),
            mode: ConfigMode::Global,
            picker: HarnessPicker::default(),
            focus: Field::Name,
            error: None,
            cwd: cwd.to_path_buf(),
            detected: None,
        }
    }

    pub fn page(&self) -> Page {
        self.page
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

    /// Page 2's choice.
    pub fn picker(&self) -> &HarnessPicker {
        &self.picker
    }

    /// The agents' default runner and model, as chosen.
    pub fn defaults(&self) -> AgentDefaults {
        self.picker.defaults()
    }

    /// The line under Default model in place of its helper: why it's
    /// disabled.
    pub fn model_note(&self) -> Option<String> {
        (!self.picker.takes_model()).then(|| no_model_note(self.picker.runner()))
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

    /// The name or path didn't validate (page 1's Continue, or Create
    /// Project): page 1 shows `msg` until the next edit.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
        if self.page != Page::Folder {
            self.page = Page::Folder;
            self.focus = Field::Name;
        }
    }

    /// Page 1 validated: on to the Agent defaults.
    pub fn folder_valid(&mut self) {
        if self.page == Page::Folder {
            self.page = Page::Defaults;
            self.focus = Field::Harness;
            self.error = None;
        }
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

    /// The controls the page shows, in Tab order.
    pub fn fields(&self) -> Vec<Field> {
        let mut out = match self.page {
            Page::Folder => vec![Field::Name, Field::Root],
            Page::Defaults if self.picker.takes_model() => vec![Field::Harness, Field::Model],
            Page::Defaults => vec![Field::Harness],
            Page::Settings => vec![Field::Config],
        };
        out.extend([Field::Back, Field::Primary]);
        out
    }

    fn step_focus(&mut self, forward: bool) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = fields.len();
        self.focus = fields[if forward { (at + 1) % n } else { (at + n - 1) % n }];
    }

    /// Keep focus on a control the page shows.
    fn settle(&mut self) {
        let fields = self.fields();
        if !fields.contains(&self.focus) {
            self.focus = fields[0];
        }
    }

    // -----------------------------------------------------------------------
    // Page 2

    /// Choose the default runner. Its model starts on the runner's default.
    pub fn select_runner(&mut self, kind: RunnerKind) {
        if self.page == Page::Defaults {
            self.picker.set_runner(kind, None);
            self.error = None;
            self.settle();
        }
    }

    /// Choose the default model (`None` and blank are the runner's default).
    pub fn select_model(&mut self, model: Option<&str>) {
        if self.page == Page::Defaults {
            self.picker.set_model(model);
            self.error = None;
        }
    }

    /// ← → on a picker: its previous or next choice, in place.
    fn step_choice(&mut self, field: Field, forward: bool) {
        match field {
            Field::Harness => {
                let rows = self.picker.harnesses();
                let at = rows.iter().position(|h| h.selected).unwrap_or(0);
                let next = if forward { (at + 1).min(rows.len() - 1) } else { at.saturating_sub(1) };
                self.select_runner(rows[next].kind);
            }
            Field::Model => {
                let rows = self.picker.models();
                if rows.is_empty() {
                    return;
                }
                let at = rows.iter().position(|o| o.selected).unwrap_or(0);
                let next = if forward { (at + 1).min(rows.len() - 1) } else { at.saturating_sub(1) };
                let model = rows[next].model.clone();
                self.select_model(model.as_deref());
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Buttons

    /// Back: page 1 cancels the form; pages 2 and 3 go back a page, keeping
    /// every value.
    pub fn back(&mut self) -> Step {
        self.error = None;
        match self.page {
            Page::Folder => return Step::Cancel,
            Page::Defaults => self.page = Page::Folder,
            Page::Settings => self.page = Page::Defaults,
        }
        self.focus = Field::Primary;
        Step::Continue
    }

    /// The page's primary button: Continue (page 1 asks the host to
    /// validate first), then Create Project. A generic default runner is
    /// refused: a new project declares none (the orchestrator onboarding's
    /// refusal).
    pub fn primary(&mut self) -> Step {
        match self.page {
            Page::Folder => Step::Validate,
            Page::Defaults => {
                if self.picker.runner() == RunnerKind::Generic {
                    self.error = Some(RunnerSetupError::NotDeclared(RunnerKind::Generic).to_string());
                    return Step::Continue;
                }
                self.error = None;
                self.page = Page::Settings;
                self.focus = Field::Config;
                Step::Continue
            }
            Page::Settings => Step::Submit,
        }
    }

    /// Feed one key. Esc cancels and Enter presses the primary button from
    /// any control; Tab / BackTab cycle the page's controls; a text field
    /// takes typed characters and Backspace; Space presses the focused
    /// button or opens the focused picker; on a picker ← → step its choice,
    /// on the location Space or an arrow flips it. Any edit clears a stale
    /// error.
    pub fn handle_key(&mut self, chord: KeyChord) -> Step {
        self.settle();
        let bare = chord.mods == Mods::NONE;
        match chord.code {
            Key::Esc => Step::Cancel,
            Key::Enter => self.primary(),
            Key::Tab => {
                self.step_focus(true);
                Step::Continue
            }
            Key::BackTab => {
                self.step_focus(false);
                Step::Continue
            }
            _ => match self.focus {
                Field::Name => {
                    edit_text(&mut self.name, &mut self.error, chord);
                    Step::Continue
                }
                Field::Root => {
                    edit_text(&mut self.root, &mut self.error, chord);
                    Step::Continue
                }
                Field::Config => {
                    toggle_config(&mut self.mode, &mut self.error, chord);
                    Step::Continue
                }
                Field::Harness | Field::Model => match chord.code {
                    Key::Char(' ') if bare => self.click(self.focus),
                    Key::Left | Key::Up if bare => {
                        self.step_choice(self.focus, false);
                        Step::Continue
                    }
                    Key::Right | Key::Down if bare => {
                        self.step_choice(self.focus, true);
                        Step::Continue
                    }
                    _ => Step::Continue,
                },
                Field::Back | Field::Primary => match chord.code {
                    Key::Char(' ') if bare => self.click(self.focus),
                    Key::Left | Key::Up if bare => {
                        self.step_focus(false);
                        Step::Continue
                    }
                    Key::Right | Key::Down if bare => {
                        self.step_focus(true);
                        Step::Continue
                    }
                    _ => Step::Continue,
                },
            },
        }
    }

    /// Move focus to `field`.
    pub fn set_focus(&mut self, field: Field) {
        self.focus = field;
    }

    /// A click on a control (or Space on it): focus it, and press a button,
    /// open a picker's menu, or flip the location (a click on the location
    /// once it has focus flips it, as Space would).
    pub fn click(&mut self, field: Field) -> Step {
        if !self.fields().contains(&field) {
            return Step::Continue;
        }
        let refocus = self.focus == field;
        self.focus = field;
        match field {
            Field::Back => self.back(),
            Field::Primary => self.primary(),
            Field::Harness | Field::Model => Step::Menu(field),
            Field::Config => {
                if refocus {
                    self.mode = flip(self.mode);
                }
                self.error = None;
                Step::Continue
            }
            Field::Name | Field::Root => {
                self.error = None;
                Step::Continue
            }
        }
    }

    /// Pick where settings live (page 3's rows).
    pub fn select_mode(&mut self, mode: ConfigMode) {
        if self.page == Page::Settings {
            self.mode = mode;
            self.focus = Field::Config;
        }
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

    fn form() -> Form {
        Form::new(Path::new("/tmp/alpha"))
    }

    /// `f` past page 1, its name and path validated.
    fn on_defaults() -> Form {
        let mut f = form();
        for c in "acme".chars() {
            f.handle_key(ch(c));
        }
        assert_eq!(f.handle_key(key(Key::Enter)), Step::Validate);
        assert_eq!(f.page(), Page::Folder, "it waits for the host");
        f.folder_valid();
        f
    }

    #[test]
    fn new_prefills_the_root_and_defaults_to_claude_and_global() {
        let f = form();
        assert_eq!(f.page(), Page::Folder);
        assert_eq!(f.page().step_label(), "Step 1 of 3");
        assert_eq!(f.name(), "");
        assert_eq!(f.root(), "/tmp/alpha");
        assert_eq!(f.mode(), ConfigMode::Global);
        assert_eq!(f.defaults(), AgentDefaults::default());
        assert_eq!(f.focus(), Field::Name);
        assert_eq!(f.detection_due().as_deref(), Some("/tmp/alpha"));
    }

    #[test]
    fn the_pages_are_folder_then_agent_defaults_then_settings() {
        let mut f = on_defaults();
        assert_eq!(f.page(), Page::Defaults);
        assert_eq!(f.page().title(), "Agent defaults");
        assert_eq!(f.page().intro(), Some("Choose the provider and model that agents should use by default."));
        assert_eq!(f.page().step_label(), "Step 2 of 3");
        assert_eq!(f.focus(), Field::Harness);
        assert_eq!(f.fields(), [Field::Harness, Field::Model, Field::Back, Field::Primary]);
        assert_eq!(f.handle_key(key(Key::Enter)), Step::Continue);
        assert_eq!(f.page(), Page::Settings);
        assert_eq!(f.page().title(), "Where should settings live?");
        assert_eq!(f.page().step_label(), "Step 3 of 3");
        assert_eq!(f.page().primary(), "Create Project");
        assert_eq!(f.handle_key(key(Key::Enter)), Step::Submit);
    }

    #[test]
    fn back_and_continue_keep_every_choice() {
        let mut f = on_defaults();
        assert_eq!(f.click(Field::Harness), Step::Menu(Field::Harness));
        f.select_runner(RunnerKind::Codex);
        f.select_model(Some("gpt-5"));
        f.primary();
        f.select_mode(ConfigMode::InRepo);
        assert_eq!(f.back(), Step::Continue);
        assert_eq!(f.page(), Page::Defaults);
        assert_eq!(f.click(Field::Back), Step::Continue);
        assert_eq!(f.page(), Page::Folder);
        assert_eq!(f.name(), "acme");
        assert_eq!(f.back(), Step::Cancel, "page 1's Back cancels");
        f.folder_valid();
        assert_eq!(f.defaults(), AgentDefaults { runner: RunnerKind::Codex, model: Some("gpt-5".into()) });
        f.primary();
        assert_eq!(f.mode(), ConfigMode::InRepo);
    }

    #[test]
    fn arrows_step_a_focused_picker() {
        let mut f = on_defaults();
        f.handle_key(key(Key::Right));
        assert_eq!(f.picker().runner(), RunnerKind::Codex);
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Model);
        f.handle_key(key(Key::Right));
        assert_eq!(f.picker().model(), Some("gpt-5"));
        f.handle_key(key(Key::Left));
        assert_eq!(f.picker().model(), None);
        assert_eq!(f.handle_key(ch(' ')), Step::Menu(Field::Model));
        f.handle_key(key(Key::BackTab));
        f.handle_key(key(Key::Right));
        assert_eq!(f.picker().runner(), RunnerKind::Generic);
        assert_eq!(f.fields(), [Field::Harness, Field::Back, Field::Primary], "no model to pick");
        assert_eq!(f.model_note().as_deref(), Some("Generic runners take no model."));
    }

    #[test]
    fn a_generic_default_is_refused() {
        let mut f = on_defaults();
        f.select_runner(RunnerKind::Generic);
        assert_eq!(f.handle_key(key(Key::Enter)), Step::Continue);
        assert_eq!(f.page(), Page::Defaults);
        assert!(f.error().is_some_and(|e| e.contains("generic")));
        f.select_runner(RunnerKind::Claude);
        assert_eq!(f.error(), None);
    }

    #[test]
    fn a_validation_error_shows_on_page_one() {
        let mut f = on_defaults();
        f.primary();
        f.set_error("taken");
        assert_eq!((f.page(), f.focus(), f.error()), (Page::Folder, Field::Name, Some("taken")));
        f.handle_key(ch('x'));
        assert_eq!(f.error(), None, "an edit clears it");
    }

    #[test]
    fn tab_and_back_tab_cycle_the_pages_controls() {
        let mut f = form();
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Root);
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Back);
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Primary);
        f.handle_key(key(Key::Tab));
        assert_eq!(f.focus(), Field::Name);
        f.handle_key(key(Key::BackTab));
        assert_eq!(f.focus(), Field::Primary);
        assert_eq!(f.handle_key(ch(' ')), Step::Validate, "Space presses it");
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
        let mut f = on_defaults();
        f.primary();
        assert_eq!(f.focus(), Field::Config);
        f.handle_key(ch(' '));
        assert_eq!(f.mode(), ConfigMode::InRepo);
        f.handle_key(key(Key::Left));
        assert_eq!(f.mode(), ConfigMode::Global);
        f.handle_key(ch('x'));
        assert_eq!(f.name(), "acme");
        assert_eq!(f.root(), "/tmp/alpha");
    }

    #[test]
    fn esc_cancels_from_any_page() {
        let mut f = on_defaults();
        assert_eq!(f.handle_key(key(Key::Esc)), Step::Cancel);
        f.primary();
        assert_eq!(f.handle_key(key(Key::Esc)), Step::Cancel);
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
        f.folder_valid();
        f.primary();
        f.click(Field::Config);
        assert_eq!(f.mode(), ConfigMode::InRepo, "a click on the focused location flips it");
        assert_eq!(f.preview().unwrap().1, "<repo>/.shelbi/project.yaml");
    }
}
