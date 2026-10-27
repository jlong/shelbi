//! The General page's shared preferences (`tui-settings-general`): the
//! appearance (`theme:`) and the external editor (`editor:`) in
//! `~/.shelbi/config.yaml`, and where a save of them stands.
//!
//! Both are immediate-save controls, as the desktop app's are: picking a
//! theme saves it, and the editor's text is saved when the user commits it
//! (Enter), never per keystroke. A save is one narrow [`Change`] a client
//! writes off its UI thread through [`shelbi_state::update_user_config`],
//! which reads the file fresh and patches only that key. Until the write
//! lands the control shows the new value as pending ([`SaveStatus::Saving`]);
//! a failed write puts the control back on what the file holds and says why,
//! so a failed save never looks persisted.
//!
//! Toolkit-free: the TUI's General page draws [`GeneralForm`]; the desktop
//! app's Settings shares [`SaveStatus`], [`THEMES`] and [`theme_label`].

use shelbi_state::{EditorSource, ThemePreference, UserConfig};

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
    /// The `editor:` override, as written (it may carry arguments).
    pub editor: Option<String>,
}

impl Preferences {
    pub fn from_config(cfg: &UserConfig) -> Preferences {
        Preferences {
            theme: cfg.theme,
            editor: cfg.editor.clone(),
        }
    }
}

/// One setting to save: applied to the config as it is on disk, touching
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Theme(ThemePreference),
    /// The editor override; `None` clears it, so the fallback applies.
    Editor(Option<String>),
}

impl Change {
    /// Apply this change, and only it, to `cfg`.
    pub fn apply(&self, cfg: &mut UserConfig) {
        match self {
            Change::Theme(theme) => cfg.theme = *theme,
            Change::Editor(editor) => cfg.editor = editor.clone(),
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
    /// The change being written.
    pending: Option<Change>,
    status: SaveStatus,
    /// The editor command while it is being typed.
    draft: Option<LineInput>,
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

    /// The editor override the field shows: the one being saved, else the
    /// file's.
    pub fn editor(&self) -> Option<&str> {
        match &self.pending {
            Some(Change::Editor(e)) => e.as_deref(),
            _ => self.saved.as_ref().and_then(|p| p.editor.as_deref()),
        }
    }

    /// What Shelbi runs, and which layer named it.
    pub fn effective_editor(&self) -> (String, EditorSource) {
        shelbi_state::resolve_editor_source(self.editor(), self.env_editor.as_deref())
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
        if !self.editable() {
            return false;
        }
        self.draft = Some(LineInput::new(self.editor().unwrap_or("")));
        true
    }

    /// The field being typed, if it is.
    pub fn draft(&self) -> Option<&LineInput> {
        self.draft.as_ref()
    }

    pub fn draft_mut(&mut self) -> Option<&mut LineInput> {
        self.draft.as_mut()
    }

    /// Whether the typed command differs from the saved one (unsaved).
    pub fn dirty(&self) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|d| normalize_editor(&d.text()).as_deref() != self.editor())
    }

    /// Stop typing and keep the saved command.
    pub fn cancel_edit(&mut self) {
        self.draft = None;
    }

    /// Commit what was typed: the change to write, or `None` when it equals
    /// the saved command (typing stops either way). A blank command clears
    /// the override.
    pub fn commit_edit(&mut self) -> Option<Change> {
        let draft = self.draft.take()?;
        let editor = normalize_editor(&draft.text());
        if !self.editable() || editor.as_deref() == self.editor() {
            return None;
        }
        self.begin(Change::Editor(editor))
    }

    /// Clear the override, so the fallback applies.
    pub fn clear_editor(&mut self) -> Option<Change> {
        self.draft = None;
        if !self.editable() || self.editor().is_none() {
            return None;
        }
        self.begin(Change::Editor(None))
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
                editor: editor.map(str::to_string),
            }),
            env.map(str::to_string),
        );
        f
    }

    fn written(theme: ThemePreference, editor: Option<&str>) -> UserConfig {
        UserConfig {
            theme,
            editor: editor.map(str::to_string),
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
        f.load(Ok(Preferences { theme: ThemePreference::Light, editor: Some("zed".into()) }), None);
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
        assert_eq!((cfg.theme, cfg.editor.as_deref()), (ThemePreference::Light, Some("hx")));
        Change::Editor(None).apply(&mut cfg);
        assert_eq!((cfg.theme, cfg.editor), (ThemePreference::Light, None));
        assert_eq!(cfg.desktop.terminal_font_size, Some(15));
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
