//! The General page's shared preferences (`tui-settings-general`): the
//! appearance (`theme:`), the editor (`editor:`) and the diff tool
//! (`diff_tool:`) in `~/.shelbi/config.yaml`, and where a save of them
//! stands.
//!
//! All are immediate-save controls, as the desktop app's are: picking a
//! theme or a tool saves it, and an "Other command…" is saved when the user
//! commits it (Enter), never per keystroke. A tool is picked from a
//! [`ToolPicker`]: the default (the fallback), the installed tools Shelbi
//! detected (CLI tools, then apps; [`shelbi_state::tools::detected_tools`],
//! fed in with [`GeneralForm::set_detected`]), the current setting when it
//! is none of those, Browse (a picked `.app` or executable,
//! [`GeneralForm::browse`]) and "Other command…" (a typed command line with
//! flags, `code --wait`). A save is one narrow [`Change`] a client
//! writes off its UI thread through [`shelbi_state::update_user_config`],
//! which reads the file fresh and patches only that key. Until the write
//! lands the control shows the new value as pending ([`SaveStatus::Saving`]);
//! a failed write puts the control back on what the file holds and says why,
//! so a failed save never looks persisted.
//!
//! Toolkit-free: the TUI's General page draws [`GeneralForm`]; the desktop
//! app's Settings shares [`SaveStatus`], [`THEMES`] and [`theme_label`].

use std::path::Path;

use shelbi_state::tools::{DetectedTools, ToolOption};
use shelbi_state::{EditorSource, ThemePreference, ToolChoice, ToolKind, UserConfig};

/// The theme choices, in the order a segmented control shows them.
pub const THEMES: [ThemePreference; 3] = [ThemePreference::Auto, ThemePreference::Dark, ThemePreference::Light];

/// A theme choice's label.
pub fn theme_label(theme: ThemePreference) -> &'static str {
    match theme {
        ThemePreference::Auto => "Auto",
        ThemePreference::Dark => "Dark",
        ThemePreference::Light => "Light",
    }
}

/// Where the last save of a page's settings stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SaveStatus {
    /// Nothing saved since the page loaded.
    #[default]
    Idle,
    /// A write is in flight.
    Saving,
    /// The last write landed.
    Saved,
    /// The last write (or the read the page needs) failed: why.
    Failed(String),
}

impl SaveStatus {
    /// The line a page shows for it, if any.
    pub fn message(&self) -> Option<String> {
        match self {
            SaveStatus::Idle => None,
            SaveStatus::Saving => Some("Saving…".into()),
            SaveStatus::Saved => Some("Saved to ~/.shelbi/config.yaml".into()),
            SaveStatus::Failed(e) => Some(e.clone()),
        }
    }
}

/// What `config.yaml` says about the General page's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preferences {
    pub theme: ThemePreference,
    /// The `editor:` setting, as written (a command may carry arguments).
    pub editor: Option<ToolChoice>,
    /// The `diff_tool:` setting.
    pub diff_tool: Option<ToolChoice>,
}

impl Preferences {
    pub fn from_config(cfg: &UserConfig) -> Preferences {
        Preferences {
            theme: cfg.theme,
            editor: cfg.editor.clone(),
            diff_tool: cfg.diff_tool.clone(),
        }
    }
}

/// One of the tool settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolSetting {
    /// `editor:`, what "Edit in <name>" opens.
    Editor,
    /// `diff_tool:`, what View Diff runs.
    DiffTool,
}

impl ToolSetting {
    /// The setting's label on the page.
    pub fn label(self) -> &'static str {
        match self {
            ToolSetting::Editor => "Editor",
            ToolSetting::DiffTool => "Diff tool",
        }
    }

    /// The picker's default entry: no setting, so the fallback applies.
    pub fn default_label(self) -> &'static str {
        match self {
            ToolSetting::Editor => DEFAULT_EDITOR_LABEL,
            ToolSetting::DiffTool => GIT_DIFFTOOL_LABEL,
        }
    }
}

/// The editor picker's default entry (`editor:` unset).
pub const DEFAULT_EDITOR_LABEL: &str = "Default ($EDITOR or vim)";
/// The diff tool picker's default entry (`diff_tool:` unset).
pub const GIT_DIFFTOOL_LABEL: &str = "Git's difftool";
/// The picker entry that picks an app or a program from disk.
pub const BROWSE_LABEL: &str = "Browse…";
/// The picker entry that takes a typed command line.
pub const OTHER_COMMAND_LABEL: &str = "Other command…";

/// One setting to save: applied to the config as it is on disk, touching
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Theme(ThemePreference),
    /// The editor; `None` clears it, so the fallback applies.
    Editor(Option<ToolChoice>),
    /// The diff tool; `None` clears it, so git's difftool applies.
    DiffTool(Option<ToolChoice>),
}

impl Change {
    /// The change that sets `setting` to `choice`.
    pub fn tool(setting: ToolSetting, choice: Option<ToolChoice>) -> Change {
        match setting {
            ToolSetting::Editor => Change::Editor(choice),
            ToolSetting::DiffTool => Change::DiffTool(choice),
        }
    }

    /// Apply this change, and only it, to `cfg`.
    pub fn apply(&self, cfg: &mut UserConfig) {
        match self {
            Change::Theme(theme) => cfg.theme = *theme,
            Change::Editor(editor) => cfg.editor = editor.clone(),
            Change::DiffTool(diff_tool) => cfg.diff_tool = diff_tool.clone(),
        }
    }

    /// Write this change to `config.yaml` (blocking): a fresh read, this one
    /// key patched in. Returns the config as written.
    pub fn write(&self) -> Result<UserConfig, String> {
        shelbi_state::update_user_config(|cfg| self.apply(cfg))
            .map_err(|e| format!("Couldn't save config.yaml: {e}"))
    }
}

/// The editor override as typed: trimmed, and blank means none.
pub fn normalize_editor(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// One entry of a [`ToolPicker`], in the order [`ToolPicker::entries`]
/// lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerEntry {
    /// No setting: the fallback ([`ToolSetting::default_label`]).
    Default,
    /// An installed tool, or the current setting when it is none of them.
    Tool(ToolOption),
    /// Pick an `.app` or an executable from disk ([`GeneralForm::browse`]).
    Browse,
    /// Type a command line ([`GeneralForm::begin_edit_tool`]).
    OtherCommand,
}

impl PickerEntry {
    /// The entry's label, for `setting`'s picker.
    pub fn label(&self, setting: ToolSetting) -> String {
        match self {
            PickerEntry::Default => setting.default_label().to_string(),
            PickerEntry::Tool(t) => t.name.clone(),
            PickerEntry::Browse => BROWSE_LABEL.to_string(),
            PickerEntry::OtherCommand => OTHER_COMMAND_LABEL.to_string(),
        }
    }
}

/// A tool setting's choices: the default, the installed tools grouped CLI
/// and Apps, the current setting when it is none of those, Browse and Other
/// command, with which one is current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPicker {
    pub setting: ToolSetting,
    /// What the default runs now, in words (`nvim, from $EDITOR`, `vimdiff,
    /// from git`), when known.
    pub default_detail: Option<String>,
    /// Installed CLI tools (opened in a pane).
    pub cli: Vec<ToolOption>,
    /// Installed apps (opened in the app).
    pub apps: Vec<ToolOption>,
    /// The current setting when no installed entry is it: a typed command
    /// (`code --wait`) or a browsed app.
    pub custom: Option<ToolOption>,
    /// The current setting; `None` is the default.
    pub current: Option<ToolChoice>,
}

impl ToolPicker {
    /// Every entry, top to bottom: Default, the CLI tools, the apps, the
    /// custom setting, Browse, Other command.
    pub fn entries(&self) -> Vec<PickerEntry> {
        let mut entries = vec![PickerEntry::Default];
        entries.extend(self.cli.iter().chain(&self.apps).chain(&self.custom).cloned().map(PickerEntry::Tool));
        entries.push(PickerEntry::Browse);
        entries.push(PickerEntry::OtherCommand);
        entries
    }

    /// The index in [`Self::entries`] of the current setting.
    pub fn selected(&self) -> usize {
        let Some(current) = &self.current else {
            return 0;
        };
        self.entries()
            .iter()
            .position(|e| matches!(e, PickerEntry::Tool(t) if &t.choice == current))
            .unwrap_or(0)
    }

    /// The label of the current setting (the closed picker's text).
    pub fn current_label(&self) -> String {
        self.entries()[self.selected()].label(self.setting)
    }
}

/// What a source of the resolved editor is called on the page.
pub fn source_label(source: EditorSource) -> &'static str {
    match source {
        EditorSource::Config => "your setting",
        EditorSource::Env => "$EDITOR",
        EditorSource::Default => "the default",
    }
}

/// A one-line text field: the editor command while it is being typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    chars: Vec<char>,
    caret: usize,
}

impl LineInput {
    /// A field holding `text`, the caret at its end.
    pub fn new(text: &str) -> LineInput {
        let chars: Vec<char> = text.chars().collect();
        LineInput { caret: chars.len(), chars }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The caret, in characters from the start.
    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn insert(&mut self, c: char) {
        if !c.is_control() {
            self.chars.insert(self.caret, c);
            self.caret += 1;
        }
    }

    /// Insert pasted text (line breaks become spaces: it is one line).
    pub fn paste(&mut self, text: &str) {
        for c in text.chars() {
            self.insert(if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c });
        }
    }

    pub fn backspace(&mut self) {
        if self.caret > 0 {
            self.caret -= 1;
            self.chars.remove(self.caret);
        }
    }

    pub fn delete(&mut self) {
        if self.caret < self.chars.len() {
            self.chars.remove(self.caret);
        }
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.chars.len());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.chars.len();
    }

    /// Empty the field.
    pub fn clear(&mut self) {
        self.chars.clear();
        self.caret = 0;
    }
}

/// The General page's form: what the file holds, what is being typed, and
/// the save in flight.
#[derive(Debug, Clone, Default)]
pub struct GeneralForm {
    /// What `config.yaml` holds; `None` until it is read, or while it
    /// doesn't read (then nothing can be saved over it).
    saved: Option<Preferences>,
    /// Why `config.yaml` doesn't read.
    unreadable: Option<String>,
    /// `$EDITOR`, for the fallback.
    env_editor: Option<String>,
    /// The installed tools the pickers offer.
    detected: DetectedTools,
    /// The change being written.
    pending: Option<Change>,
    status: SaveStatus,
    /// A command line while it is being typed ("Other command…").
    draft: Option<LineInput>,
    /// The setting the draft is for.
    draft_setting: Option<ToolSetting>,
}

impl GeneralForm {
    /// The file was read: `Ok` with what it holds, or why it doesn't read.
    /// Ignored while a save is in flight (its result is newer). A draft
    /// being typed is kept.
    pub fn load(&mut self, read: Result<Preferences, String>, env_editor: Option<String>) {
        self.env_editor = env_editor;
        if self.pending.is_some() {
            return;
        }
        match read {
            Ok(prefs) => {
                self.saved = Some(prefs);
                self.unreadable = None;
            }
            Err(e) => {
                self.saved = None;
                self.unreadable = Some(e);
            }
        }
    }

    /// What `config.yaml` holds, once read.
    pub fn saved(&self) -> Option<&Preferences> {
        self.saved.as_ref()
    }

    /// Why `config.yaml` doesn't read, if it doesn't.
    pub fn unreadable(&self) -> Option<&str> {
        self.unreadable.as_deref()
    }

    pub fn status(&self) -> &SaveStatus {
        &self.status
    }

    /// Whether a save is in flight.
    pub fn saving(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether the controls take a change: the file read, and no save in
    /// flight.
    pub fn editable(&self) -> bool {
        self.saved.is_some() && self.pending.is_none()
    }

    /// The theme the control shows: the one being saved, else the file's.
    pub fn theme(&self) -> ThemePreference {
        match &self.pending {
            Some(Change::Theme(t)) => *t,
            _ => self.saved.as_ref().map(|p| p.theme).unwrap_or_default(),
        }
    }

    /// The installed tools arrived (detected off the UI thread).
    pub fn set_detected(&mut self, detected: DetectedTools) {
        self.detected = detected;
    }

    pub fn detected(&self) -> &DetectedTools {
        &self.detected
    }

    /// A tool setting as shown: the one being saved, else the file's.
    pub fn tool(&self, setting: ToolSetting) -> Option<&ToolChoice> {
        match (&self.pending, setting) {
            (Some(Change::Editor(e)), ToolSetting::Editor) => e.as_ref(),
            (Some(Change::DiffTool(d)), ToolSetting::DiffTool) => d.as_ref(),
            _ => self.saved.as_ref().and_then(|p| match setting {
                ToolSetting::Editor => p.editor.as_ref(),
                ToolSetting::DiffTool => p.diff_tool.as_ref(),
            }),
        }
    }

    /// The editor setting as shown (a command or an app).
    pub fn editor_choice(&self) -> Option<&ToolChoice> {
        self.tool(ToolSetting::Editor)
    }

    /// The diff tool setting as shown.
    pub fn diff_tool_choice(&self) -> Option<&ToolChoice> {
        self.tool(ToolSetting::DiffTool)
    }

    /// The editor setting's command line, as an "Other command" field shows
    /// it: `None` when unset or an app.
    pub fn editor(&self) -> Option<&str> {
        self.editor_choice().and_then(ToolChoice::command)
    }

    /// What Shelbi runs (a command line, or an app's name), and which layer
    /// named it.
    pub fn effective_editor(&self) -> (String, EditorSource) {
        let (choice, source) = shelbi_state::resolve_editor_choice_from(self.editor_choice(), self.env_editor.as_deref());
        (choice.summary(), source)
    }

    /// What View Diff runs, in words: the setting (a command line or an
    /// app's name), else git's difftool and the tool git names.
    pub fn effective_diff_tool(&self) -> String {
        match self.diff_tool_choice() {
            Some(choice) => choice.summary(),
            None => self.git_difftool_detail(),
        }
    }

    fn git_difftool_detail(&self) -> String {
        match &self.detected.git_difftool {
            Some(tool) => format!("{GIT_DIFFTOOL_LABEL} ({tool})"),
            None => format!("{GIT_DIFFTOOL_LABEL} (none configured)"),
        }
    }

    /// `setting`'s picker: its entries and the current one.
    pub fn picker(&self, setting: ToolSetting) -> ToolPicker {
        let detected = match setting {
            ToolSetting::Editor => &self.detected.editors,
            ToolSetting::DiffTool => &self.detected.diff_tools,
        };
        let of_kind = |kind| detected.iter().filter(|t| t.kind == kind).cloned().collect::<Vec<_>>();
        let current = self.tool(setting).cloned().and_then(ToolChoice::normalized);
        let custom = current
            .as_ref()
            .filter(|c| !detected.iter().any(|t| &t.choice == *c))
            .map(ToolOption::custom);
        let default_detail = match setting {
            ToolSetting::Editor => {
                let (command, source) = self.fallback_editor();
                Some(format!("{command}, {}", source_label(source)))
            }
            ToolSetting::DiffTool => self.detected.git_difftool.as_ref().map(|t| format!("{t}, from git")),
        };
        ToolPicker {
            setting,
            default_detail,
            cli: of_kind(ToolKind::Cli),
            apps: of_kind(ToolKind::App),
            custom,
            current,
        }
    }

    /// Pick `choice` for `setting` (`None`: the default): the change to
    /// write, or `None` when it is already the setting or the form can't
    /// save. A blank command is the default.
    pub fn choose_tool(&mut self, setting: ToolSetting, choice: Option<ToolChoice>) -> Option<Change> {
        let choice = choice.and_then(ToolChoice::normalized);
        if !self.editable() || self.tool(setting) == choice.as_ref() {
            return None;
        }
        self.draft = None;
        self.draft_setting = None;
        self.begin(Change::tool(setting, choice))
    }

    /// Pick a picker entry: Default and a tool save at once; Browse and
    /// Other command need more from the user, so they change nothing here
    /// (the client opens its file dialog, or [`Self::begin_edit_tool`]).
    pub fn choose_entry(&mut self, setting: ToolSetting, entry: &PickerEntry) -> Option<Change> {
        match entry {
            PickerEntry::Default => self.choose_tool(setting, None),
            PickerEntry::Tool(t) => self.choose_tool(setting, Some(t.choice.clone())),
            PickerEntry::Browse | PickerEntry::OtherCommand => None,
        }
    }

    /// Browse picked `path` for `setting`: an `.app` bundle becomes an app,
    /// an executable a command. `Err` says why the path can't be a tool (it
    /// doesn't exist, isn't executable, isn't a real bundle); `Ok(None)`
    /// when it is already the setting or the form can't save. Reads the
    /// file's metadata.
    pub fn browse(&mut self, setting: ToolSetting, path: &Path) -> Result<Option<Change>, String> {
        let choice = shelbi_state::tools::choice_from_path(path)?;
        Ok(self.choose_tool(setting, Some(choice)))
    }

    /// What Shelbi runs without the override.
    pub fn fallback_editor(&self) -> (String, EditorSource) {
        shelbi_state::resolve_editor_source(None, self.env_editor.as_deref())
    }

    /// Pick a theme: the change to write, or `None` when it is already the
    /// file's or the form can't save.
    pub fn choose_theme(&mut self, theme: ThemePreference) -> Option<Change> {
        if !self.editable() || self.theme() == theme {
            return None;
        }
        self.begin(Change::Theme(theme))
    }

    /// The theme `steps` places from the shown one, wrapping (a segmented
    /// control's ←/→).
    pub fn step_theme(&mut self, steps: isize) -> Option<Change> {
        let at = THEMES.iter().position(|t| *t == self.theme()).unwrap_or(0) as isize;
        let next = THEMES[(at + steps).rem_euclid(THEMES.len() as isize) as usize];
        self.choose_theme(next)
    }

    /// Start typing the editor command, from the shown override.
    pub fn begin_edit(&mut self) -> bool {
        self.begin_edit_tool(ToolSetting::Editor)
    }

    /// Start typing a command line for `setting` ("Other command…"), from
    /// its command (empty for an app or the default).
    pub fn begin_edit_tool(&mut self, setting: ToolSetting) -> bool {
        if !self.editable() {
            return false;
        }
        let text = self.tool(setting).and_then(ToolChoice::command).unwrap_or("");
        self.draft = Some(LineInput::new(text));
        self.draft_setting = Some(setting);
        true
    }

    /// The setting the field being typed is for.
    pub fn draft_setting(&self) -> Option<ToolSetting> {
        self.draft.as_ref().and(self.draft_setting)
    }

    /// The field being typed, if it is.
    pub fn draft(&self) -> Option<&LineInput> {
        self.draft.as_ref()
    }

    pub fn draft_mut(&mut self) -> Option<&mut LineInput> {
        self.draft.as_mut()
    }

    /// The typed command as a choice, for the draft's setting.
    fn typed(&self) -> Option<(ToolSetting, Option<ToolChoice>)> {
        let draft = self.draft.as_ref()?;
        let setting = self.draft_setting.unwrap_or(ToolSetting::Editor);
        Some((setting, normalize_editor(&draft.text()).map(ToolChoice::Command)))
    }

    /// Whether the typed command differs from the saved setting (unsaved).
    pub fn dirty(&self) -> bool {
        self.typed()
            .is_some_and(|(setting, typed)| typed.as_ref() != self.tool(setting))
    }

    /// Stop typing and keep the saved command.
    pub fn cancel_edit(&mut self) {
        self.draft = None;
        self.draft_setting = None;
    }

    /// Commit what was typed: the change to write, or `None` when it equals
    /// the saved setting (typing stops either way). A blank command clears
    /// the setting.
    pub fn commit_edit(&mut self) -> Option<Change> {
        let (setting, typed) = self.typed()?;
        self.cancel_edit();
        if !self.editable() || typed.as_ref() == self.tool(setting) {
            return None;
        }
        self.begin(Change::tool(setting, typed))
    }

    /// Clear the editor setting, so the fallback applies.
    pub fn clear_editor(&mut self) -> Option<Change> {
        self.clear_tool(ToolSetting::Editor)
    }

    /// Clear `setting`, so its fallback applies.
    pub fn clear_tool(&mut self, setting: ToolSetting) -> Option<Change> {
        self.cancel_edit();
        if !self.editable() || self.tool(setting).is_none() {
            return None;
        }
        self.begin(Change::tool(setting, None))
    }

    fn begin(&mut self, change: Change) -> Option<Change> {
        self.pending = Some(change.clone());
        self.status = SaveStatus::Saving;
        Some(change)
    }

    /// The write landed. On success the form holds what was written and
    /// returns the theme now in effect (for the client to apply); on failure
    /// the controls go back to what the file holds and the status says why.
    pub fn saved_result(&mut self, result: Result<UserConfig, String>) -> Option<ThemePreference> {
        self.pending.take()?;
        match result {
            Ok(cfg) => {
                let prefs = Preferences::from_config(&cfg);
                let theme = prefs.theme;
                self.saved = Some(prefs);
                self.status = SaveStatus::Saved;
                Some(theme)
            }
            Err(e) => {
                self.status = SaveStatus::Failed(e);
                None
            }
        }
    }

    /// Forget the save status (a new visit to the page).
    pub fn clear_status(&mut self) {
        if self.pending.is_none() {
            self.status = SaveStatus::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(theme: ThemePreference, editor: Option<&str>, env: Option<&str>) -> GeneralForm {
        let mut f = GeneralForm::default();
        f.load(
            Ok(Preferences {
                theme,
                editor: editor.map(ToolChoice::from),
                diff_tool: None,
            }),
            env.map(str::to_string),
        );
        f
    }

    fn written(theme: ThemePreference, editor: Option<&str>) -> UserConfig {
        UserConfig {
            theme,
            editor: editor.map(ToolChoice::from),
            ..UserConfig::default()
        }
    }

    #[test]
    fn a_theme_shows_pending_then_saved_and_a_failure_goes_back() {
        let mut f = loaded(ThemePreference::Auto, None, None);
        assert_eq!(f.choose_theme(ThemePreference::Auto), None, "already the file's");
        assert_eq!(f.choose_theme(ThemePreference::Light), Some(Change::Theme(ThemePreference::Light)));
        assert_eq!((f.theme(), f.status()), (ThemePreference::Light, &SaveStatus::Saving));
        assert_eq!(f.step_theme(1), None, "one save at a time");
        assert_eq!(
            f.saved_result(Ok(written(ThemePreference::Light, None))),
            Some(ThemePreference::Light)
        );
        assert_eq!((f.theme(), f.status()), (ThemePreference::Light, &SaveStatus::Saved));

        assert!(f.step_theme(1).is_some(), "Light wraps to Auto");
        assert_eq!(f.theme(), ThemePreference::Auto);
        assert_eq!(f.saved_result(Err("disk full".into())), None);
        assert_eq!(f.theme(), ThemePreference::Light, "a failed save doesn't look saved");
        assert_eq!(f.status(), &SaveStatus::Failed("disk full".into()));
    }

    #[test]
    fn the_editor_commits_on_purpose_and_clearing_restores_the_fallback() {
        let mut f = loaded(ThemePreference::Dark, None, Some("nvim"));
        assert_eq!(f.effective_editor(), ("nvim".to_string(), EditorSource::Env));
        assert!(f.begin_edit());
        for c in "  code --wait ".chars() {
            f.draft_mut().unwrap().insert(c);
        }
        assert!(f.dirty(), "typed, not saved");
        assert_eq!(f.editor(), None, "typing doesn't save");
        assert_eq!(f.commit_edit(), Some(Change::Editor(Some("code --wait".into()))));
        assert!(!f.dirty());
        f.saved_result(Ok(written(ThemePreference::Dark, Some("code --wait"))));
        assert_eq!(f.effective_editor(), ("code --wait".to_string(), EditorSource::Config));
        assert_eq!(f.fallback_editor(), ("nvim".to_string(), EditorSource::Env));

        // The same text again saves nothing; blank clears.
        f.begin_edit();
        assert_eq!(f.commit_edit(), None);
        f.begin_edit();
        f.draft_mut().unwrap().clear();
        assert_eq!(f.commit_edit(), Some(Change::Editor(None)));
        f.saved_result(Ok(written(ThemePreference::Dark, None)));
        assert_eq!(f.clear_editor(), None, "nothing to clear");

        let mut f = loaded(ThemePreference::Dark, Some("hx"), None);
        assert_eq!(f.clear_editor(), Some(Change::Editor(None)));
        assert_eq!(f.effective_editor(), ("vim".to_string(), EditorSource::Default), "pending shows the fallback");
        f.saved_result(Err("read-only".into()));
        assert_eq!(f.editor(), Some("hx"), "the failed clear didn't happen");
    }

    #[test]
    fn an_unreadable_file_takes_no_edits_and_a_load_waits_for_the_save() {
        let mut f = GeneralForm::default();
        assert!(!f.editable(), "not read yet");
        f.load(Err("bad indent".into()), None);
        assert_eq!(f.unreadable(), Some("bad indent"));
        assert_eq!(f.choose_theme(ThemePreference::Light), None);
        assert!(!f.begin_edit());

        let mut f = loaded(ThemePreference::Auto, None, None);
        f.choose_theme(ThemePreference::Dark);
        // A read that started before the write lands after it: ignored.
        f.load(Ok(Preferences::default()), None);
        assert_eq!(f.theme(), ThemePreference::Dark);
        f.saved_result(Ok(written(ThemePreference::Dark, None)));
        // An external edit read later is taken.
        f.load(Ok(Preferences { theme: ThemePreference::Light, editor: Some("zed".into()), diff_tool: None }), None);
        assert_eq!((f.theme(), f.editor()), (ThemePreference::Light, Some("zed")));
    }

    #[test]
    fn a_change_touches_only_its_key() {
        let mut cfg = UserConfig {
            editor: Some("hx".into()),
            ..UserConfig::default()
        };
        cfg.desktop.terminal_font_size = Some(15);
        Change::Theme(ThemePreference::Light).apply(&mut cfg);
        assert_eq!((cfg.theme, cfg.editor.as_ref()), (ThemePreference::Light, Some(&"hx".into())));
        Change::Editor(None).apply(&mut cfg);
        assert_eq!((cfg.theme, cfg.editor.as_ref()), (ThemePreference::Light, None));
        Change::DiffTool(Some("difft".into())).apply(&mut cfg);
        assert_eq!((cfg.editor.as_ref(), cfg.diff_tool.as_ref()), (None, Some(&"difft".into())));
        assert_eq!(cfg.desktop.terminal_font_size, Some(15));
    }

    fn detected() -> DetectedTools {
        let cli = |name: &str, command: &str| ToolOption {
            name: name.into(),
            kind: ToolKind::Cli,
            choice: command.into(),
            app_path: None,
        };
        let app = |name: &str, path: &str| ToolOption {
            name: name.into(),
            kind: ToolKind::App,
            choice: ToolChoice::app(path),
            app_path: Some(path.into()),
        };
        DetectedTools {
            editors: vec![cli("Neovim", "nvim"), cli("Helix", "hx"), app("Zed", "/Applications/Zed.app")],
            diff_tools: vec![cli("Difftastic", "difft"), app("Kaleidoscope", "/Applications/Kaleidoscope.app")],
            git_difftool: Some("vimdiff".into()),
        }
    }

    fn entry_labels(p: &ToolPicker) -> Vec<String> {
        p.entries().iter().map(|e| e.label(p.setting)).collect()
    }

    #[test]
    fn a_picker_groups_the_installed_tools_and_marks_the_current_one() {
        let mut f = loaded(ThemePreference::Dark, None, Some("nvim"));
        f.set_detected(detected());
        let p = f.picker(ToolSetting::Editor);
        assert_eq!(
            entry_labels(&p),
            ["Default ($EDITOR or vim)", "Neovim", "Helix", "Zed", "Browse…", "Other command…"]
        );
        assert_eq!((p.cli.len(), p.apps.len(), p.custom.as_ref()), (2, 1, None));
        assert_eq!((p.selected(), p.current_label().as_str()), (0, "Default ($EDITOR or vim)"));
        assert_eq!(p.default_detail.as_deref(), Some("nvim, $EDITOR"));

        // Picking an app saves it at once and the picker shows it.
        let zed = p.entries()[3].clone();
        assert_eq!(f.choose_entry(ToolSetting::Editor, &zed), Some(Change::Editor(Some(ToolChoice::app("/Applications/Zed.app")))));
        assert_eq!(f.picker(ToolSetting::Editor).current_label(), "Zed", "pending shows the pick");
        f.saved_result(Ok(UserConfig { editor: Some(ToolChoice::app("/Applications/Zed.app")), ..UserConfig::default() }));
        assert_eq!(f.effective_editor(), ("Zed".to_string(), EditorSource::Config));
        assert_eq!(f.editor(), None, "an app has no command to type");
        assert_eq!(f.choose_entry(ToolSetting::Editor, &zed), None, "already the setting");
        assert_eq!(f.choose_entry(ToolSetting::Editor, &PickerEntry::Browse), None, "Browse asks for a path first");

        // A command the detection doesn't list is the custom entry.
        f.load(Ok(Preferences { editor: Some("code --wait".into()), ..Preferences::default() }), None);
        let p = f.picker(ToolSetting::Editor);
        assert_eq!(p.custom.as_ref().map(|c| c.name.as_str()), Some("Visual Studio Code"));
        assert_eq!(p.selected(), 4);
        assert_eq!(f.choose_entry(ToolSetting::Editor, &PickerEntry::Default), Some(Change::Editor(None)));
    }

    #[test]
    fn the_diff_tool_saves_like_the_editor_and_defaults_to_git() {
        let mut f = loaded(ThemePreference::Dark, Some("hx"), None);
        f.set_detected(detected());
        assert_eq!(f.effective_diff_tool(), "Git's difftool (vimdiff)");
        let p = f.picker(ToolSetting::DiffTool);
        assert_eq!(entry_labels(&p), ["Git's difftool", "Difftastic", "Kaleidoscope", "Browse…", "Other command…"]);
        assert_eq!(p.default_detail.as_deref(), Some("vimdiff, from git"));

        assert_eq!(f.choose_tool(ToolSetting::DiffTool, Some("difft".into())), Some(Change::DiffTool(Some("difft".into()))));
        assert_eq!(f.editor(), Some("hx"), "the editor is untouched");
        assert_eq!(f.choose_tool(ToolSetting::DiffTool, None), None, "one save at a time");
        f.saved_result(Err("read-only".into()));
        assert_eq!(f.diff_tool_choice(), None, "the failed save didn't happen");

        // Other command…: typed with flags, committed on purpose.
        assert!(f.begin_edit_tool(ToolSetting::DiffTool));
        assert_eq!(f.draft_setting(), Some(ToolSetting::DiffTool));
        f.draft_mut().unwrap().paste("nvim -d -R");
        assert!(f.dirty());
        assert_eq!(f.commit_edit(), Some(Change::DiffTool(Some("nvim -d -R".into()))));
        assert_eq!(f.draft_setting(), None);
        f.saved_result(Ok(UserConfig { diff_tool: Some("nvim -d -R".into()), ..UserConfig::default() }));
        assert_eq!(f.effective_diff_tool(), "nvim -d -R");
        assert_eq!(f.clear_tool(ToolSetting::DiffTool), Some(Change::DiffTool(None)));
    }

    #[test]
    fn browse_picks_an_app_or_a_program_and_says_why_not() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shelbi-general-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = dir.join("Fancy.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), "").unwrap();
        let exe = dir.join("mydiff");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut f = loaded(ThemePreference::Dark, None, None);
        assert_eq!(
            f.browse(ToolSetting::Editor, &app),
            Ok(Some(Change::Editor(Some(ToolChoice::app(&app)))))
        );
        f.saved_result(Ok(UserConfig { editor: Some(ToolChoice::app(&app)), ..UserConfig::default() }));
        assert_eq!(f.picker(ToolSetting::Editor).current_label(), "Fancy");
        let exe_choice = ToolChoice::Command(exe.to_string_lossy().into_owned());
        assert_eq!(f.browse(ToolSetting::DiffTool, &exe), Ok(Some(Change::DiffTool(Some(exe_choice)))));
        f.saved_result(Err("nope".into()));
        assert!(f.browse(ToolSetting::DiffTool, &dir.join("missing")).unwrap_err().contains("doesn't exist"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_line_input_edits_at_its_caret() {
        let mut l = LineInput::new("vim");
        l.home();
        l.insert('n');
        l.end();
        l.paste(" -p\nx");
        assert_eq!(l.text(), "nvim -p x");
        l.left();
        l.backspace();
        l.delete();
        assert_eq!((l.text().as_str(), l.caret()), ("nvim -p", 7));
        l.insert('\u{7}');
        assert_eq!(l.text(), "nvim -p", "control characters aren't typed");
    }

    #[test]
    fn labels() {
        assert_eq!(THEMES.map(theme_label), ["Auto", "Dark", "Light"]);
        assert_eq!(SaveStatus::Saving.message().as_deref(), Some("Saving…"));
        assert_eq!(SaveStatus::Idle.message(), None);
        assert_eq!(source_label(EditorSource::Env), "$EDITOR");
        assert_eq!(normalize_editor("   "), None);
    }
}
