//! Settings ▸ Keybindings, apart from any toolkit: the bindings listed by
//! context ([`shelbi_state::keymap::CONTEXTS`]) with their current chords,
//! the search and context filters, capturing a new chord, and the edits a
//! capture makes to `keys.yaml`. The desktop app and the TUI draw it.
//!
//! The source of truth is `keys.yaml`'s text. The listed chords are what the
//! shared loader merges from it for the shown project, so an override in the
//! file shows exactly as either client applies it. A finished capture is a
//! [`Change`]: the client saves it to the file as it is on disk
//! ([`Change::save`], through `shelbi_state::keymap::edit`, which keeps the
//! user's comments and other entries) off its UI thread and hands the result
//! back to [`KeybindingsState::saved`]. Only a saved file is shown, so the
//! rows always match the bindings the client is running with.
//!
//! One save at a time: while one is out, captures and resets don't start
//! ([`KeybindingsState::saving`]), so each change is made against the file
//! the last one left.
//!
//! Capturing: a chord picked (or the add slot) waits for a chord. Esc
//! cancels; Backspace or Delete removes the picked chord; any other chord
//! replaces it (or is added). A chord another action in the same context
//! already has is held as a [`Conflict`]: pressing it again moves it here
//! (off the other action), Esc keeps both as they were.
//!
//! How a chord is spelled on screen is the client's ([`Spell`]): the desktop
//! app draws keycaps (`⌃P`), the TUI its display style (`⌃P` or `Ctrl+P`).
//! The search finds a chord by that spelling and by every shared one.

use std::path::PathBuf;

use shelbi_state::keymap::edit::{self, builtin_chords, EditError};
use shelbi_state::keymap::{
    format_chord, validate_keymaps_yaml, Action, DisplayStyle, ErrorKind, Key, KeyChord,
    KeymapContext, KeymapDiagnostic, Keymaps, Mods, CONTEXTS,
};

/// How a client spells a chord on screen.
pub type Spell = fn(&KeyChord) -> String;

/// Which of an action's chords a capture replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The chord at this index in the action's list.
    Chord(usize),
    /// A new chord after the others.
    New,
}

/// A chord slot waiting for a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capture {
    pub action: Action,
    pub slot: Slot,
}

/// A captured chord another action in the same context already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    pub chord: KeyChord,
    pub other: Action,
}

/// One edit to `keys.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Bind each action to exactly these chords (an empty list unbinds it),
    /// in order.
    Set(Vec<(Action, Vec<KeyChord>)>),
    /// Back to the built-in chords for one action.
    Reset(Action),
    /// Back to the built-in chords for every action (Reset all).
    ResetAll,
}

impl Change {
    /// The change applied to `text`, a `keys.yaml`, for `project`. Each
    /// edit goes into the layer that already sets the action (the
    /// project's block when it has one, else `defaults`); Reset all clears
    /// `defaults` and `project`'s block, keeping other projects and entries
    /// Shelbi doesn't know.
    pub fn apply(&self, text: &str, project: Option<&str>) -> Result<String, EditError> {
        match self {
            Change::Set(sets) => {
                let mut text = text.to_string();
                for (action, chords) in sets {
                    text = edit::set_chords(&text, project, *action, chords)?;
                }
                Ok(text)
            }
            Change::Reset(action) => edit::clear(text, project, *action),
            Change::ResetAll => edit::clear_all(text, project),
        }
    }

    /// Apply the change to `$SHELBI_HOME/keys.yaml` as it is on disk now
    /// and write it. Returns the file's new text. Blocking: run it off the
    /// UI thread. A file that doesn't parse is left alone (the error says
    /// so).
    pub fn save(&self, project: Option<&str>) -> Result<String, String> {
        edit::update_keys_file(|text| self.apply(text, project))
    }
}

/// What a key did to a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    /// No capture is open: the key is someone else's.
    Ignored,
    /// The capture ended without a change (Esc, or a chord the action
    /// already has).
    Cancelled,
    /// The chord is another action's: the capture stays open, flagged.
    Conflict,
    /// The capture ended in this change: save it, then hand the result to
    /// [`KeybindingsState::saved`]. Nothing shows as changed until then.
    Save(Change),
    /// The change couldn't be made (`keys.yaml` doesn't take it); the
    /// capture ended and nothing changed.
    Failed(String),
}

/// One action's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub action: Action,
    pub label: &'static str,
    pub chords: Vec<KeyChord>,
    /// Its chords differ from the built-in ones.
    pub customized: bool,
}

impl Row {
    /// The chords in the order the row shows them, each with its index in
    /// [`Self::chords`] (a capture's [`Slot::Chord`]): named keys and
    /// modified chords before bare letters (`↑ or K`, `Home or G`),
    /// otherwise in `keys.yaml`'s order. Display only: the file's order
    /// (whose first chord the hints show) is kept.
    pub fn shown(&self) -> Vec<(usize, KeyChord)> {
        let letter = |c: &KeyChord| {
            matches!(c.code, Key::Char(ch) if ch.is_ascii_alphabetic())
                && !c.mods.intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER)
        };
        let mut shown: Vec<(usize, KeyChord)> = self.chords.iter().copied().enumerate().collect();
        shown.sort_by_key(|(_, c)| letter(c));
        shown
    }
}

/// One context's rows, after the filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub context: &'static KeymapContext,
    pub rows: Vec<Row>,
}

/// The editor's state.
pub struct KeybindingsState {
    /// The shown project, whose `projects.<name>` block applies.
    project: Option<String>,
    /// `keys.yaml` as last read or saved.
    text: String,
    keymaps: Keymaps,
    /// The search: rows whose label or chords contain it.
    pub query: String,
    /// The context filter: one context's mode, or every context.
    pub context: Option<&'static str>,
    capture: Option<Capture>,
    conflict: Option<Conflict>,
    /// A save is out; nothing else starts until it lands.
    saving: bool,
    spell: Spell,
}

impl KeybindingsState {
    /// The built-in bindings until `keys.yaml` is read ([`Self::load`]).
    pub fn new(project: Option<String>, spell: Spell) -> Self {
        let mut state = KeybindingsState {
            project,
            text: String::new(),
            keymaps: Keymaps::default(),
            query: String::new(),
            context: None,
            capture: None,
            conflict: None,
            saving: false,
            spell,
        };
        state.remerge();
        state
    }

    /// Show the bindings `text` (a `keys.yaml`) gives the project. A file
    /// that doesn't parse is refused, with why: the bindings on show (the
    /// ones in effect) stay. An open capture stays open (its row is still
    /// there), and a held conflict while the other action still has the
    /// chord.
    pub fn load(&mut self, text: String) -> Result<(), String> {
        let (keymaps, diags) = validate_keymaps_yaml(&text, self.project.as_deref());
        if let Some(e) = parse_error(&diags) {
            return Err(e);
        }
        self.keymaps = keymaps;
        self.text = text;
        self.conflict = self.conflict.filter(|c| self.keymaps.chords(c.other).contains(&c.chord));
        Ok(())
    }

    /// Show another project's bindings from the same file (the client
    /// switched project). Any capture ends.
    pub fn set_project(&mut self, project: Option<String>) {
        if self.project == project {
            return;
        }
        self.project = project;
        self.cancel();
        self.remerge();
    }

    fn remerge(&mut self) {
        // `text` always parsed when it was taken.
        self.keymaps = validate_keymaps_yaml(&self.text, self.project.as_deref()).0;
    }

    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// The `keys.yaml` text on show.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The merged keymaps on show.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// `action`'s chords on show.
    pub fn chords(&self, action: Action) -> Vec<KeyChord> {
        self.keymaps.chords(action)
    }

    /// The groups to list: every context (or the filtered one), each with
    /// its rows matching the search; a context with no match is left out.
    pub fn groups(&self) -> Vec<Group> {
        let query = self.query.trim().to_lowercase();
        CONTEXTS
            .iter()
            .filter(|c| self.context.is_none_or(|m| m == c.mode))
            .filter_map(|context| {
                let rows: Vec<Row> = context
                    .actions
                    .iter()
                    .map(|&action| self.row(action))
                    .filter(|row| self.matches(row, &query))
                    .collect();
                (!rows.is_empty()).then_some(Group { context, rows })
            })
            .collect()
    }

    /// `action`'s row.
    pub fn row(&self, action: Action) -> Row {
        let chords = self.chords(action);
        Row {
            action,
            label: action.label(),
            customized: chords != builtin_chords(action),
            chords,
        }
    }

    /// Whether any action's chords differ from the built-in ones (Reset all
    /// is enabled).
    pub fn any_customized(&self) -> bool {
        Action::all().any(|a| self.chords(a) != builtin_chords(a))
    }

    /// The context filter's label: the context's title, or `All contexts`.
    pub fn context_label(&self) -> &'static str {
        self.context
            .and_then(shelbi_state::keymap::context)
            .map_or("All contexts", |c| c.title)
    }

    /// Step the context filter through every context, then back to all of
    /// them (`forward`), or the other way.
    pub fn cycle_context(&mut self, forward: bool) {
        let modes: Vec<Option<&'static str>> =
            std::iter::once(None).chain(CONTEXTS.iter().map(|c| Some(c.mode))).collect();
        let at = modes.iter().position(|m| *m == self.context).unwrap_or(0);
        let n = modes.len();
        let next = if forward { (at + 1) % n } else { (at + n - 1) % n };
        self.context = modes[next];
    }

    /// The open capture.
    pub fn capture(&self) -> Option<Capture> {
        self.capture
    }

    /// The held conflict, while the capture is open.
    pub fn conflict(&self) -> Option<Conflict> {
        self.conflict
    }

    /// Whether a save is out.
    pub fn saving(&self) -> bool {
        self.saving
    }

    /// Wait for a chord for `action`'s `slot`. Refused (`false`) while a
    /// save is out.
    pub fn start_capture(&mut self, action: Action, slot: Slot) -> bool {
        if self.saving {
            return false;
        }
        self.capture = Some(Capture { action, slot });
        self.conflict = None;
        true
    }

    /// End the capture without a change.
    pub fn cancel(&mut self) {
        self.capture = None;
        self.conflict = None;
    }

    /// A key while capturing. See the module docs.
    pub fn key(&mut self, chord: KeyChord) -> KeyOutcome {
        let Some(Capture { action, slot }) = self.capture else {
            return KeyOutcome::Ignored;
        };
        let chord = normalize(chord);
        let current = self.chords(action);
        let plain = chord.mods == Mods::NONE;
        if plain && chord.code == Key::Esc {
            self.cancel();
            return KeyOutcome::Cancelled;
        }
        if plain && matches!(chord.code, Key::Backspace | Key::Delete) {
            let Slot::Chord(at) = slot else {
                self.cancel();
                return KeyOutcome::Cancelled;
            };
            let mut chords = current;
            if at < chords.len() {
                chords.remove(at);
            }
            return self.finish(Change::Set(vec![(action, chords)]));
        }
        if current.contains(&chord) {
            self.cancel();
            return KeyOutcome::Cancelled;
        }
        let mut chords = current;
        match slot {
            Slot::Chord(at) if at < chords.len() => chords[at] = chord,
            _ => chords.push(chord),
        }
        let Some(other) = self.holder(action, chord) else {
            return self.finish(Change::Set(vec![(action, chords)]));
        };
        // The same chord again confirms the move.
        if self.conflict.is_some_and(|c| c.chord == chord) {
            let others: Vec<KeyChord> = self.chords(other).into_iter().filter(|c| *c != chord).collect();
            return self.finish(Change::Set(vec![(other, others), (action, chords)]));
        }
        self.conflict = Some(Conflict { chord, other });
        KeyOutcome::Conflict
    }

    /// Remove `action`'s chord at `at` without a capture (a client's
    /// keyboard row action).
    pub fn remove(&mut self, action: Action, at: usize) -> KeyOutcome {
        if self.saving {
            return KeyOutcome::Ignored;
        }
        let mut chords = self.chords(action);
        if at >= chords.len() {
            return KeyOutcome::Ignored;
        }
        chords.remove(at);
        self.finish(Change::Set(vec![(action, chords)]))
    }

    /// Restore `action`'s built-in chords.
    pub fn reset(&mut self, action: Action) -> KeyOutcome {
        if self.saving {
            return KeyOutcome::Ignored;
        }
        self.finish(Change::Reset(action))
    }

    /// Restore every built-in chord.
    pub fn reset_all(&mut self) -> KeyOutcome {
        if self.saving {
            return KeyOutcome::Ignored;
        }
        self.finish(Change::ResetAll)
    }

    /// The save of the last [`KeyOutcome::Save`] landed: on success its
    /// `keys.yaml` shows (the bindings now in effect). Either way the next
    /// change may start. Returns why the save (or showing it) failed.
    pub fn saved(&mut self, result: Result<String, String>) -> Result<(), String> {
        self.saving = false;
        self.load(result?)
    }

    /// The other action in `action`'s context bound to `chord`, if any.
    fn holder(&self, action: Action, chord: KeyChord) -> Option<Action> {
        Action::all()
            .filter(|a| *a != action && a.mode() == action.mode())
            .find(|a| self.chords(*a).contains(&chord))
    }

    fn finish(&mut self, change: Change) -> KeyOutcome {
        self.cancel();
        // Made on the text on show first: a change it doesn't take is never
        // saved.
        match change.apply(&self.text, self.project.as_deref()) {
            Ok(_) => {
                self.saving = true;
                KeyOutcome::Save(change)
            }
            Err(e) => KeyOutcome::Failed(e.to_string()),
        }
    }

    /// The held conflict's line under its row.
    pub fn conflict_message(&self, conflict: &Conflict) -> String {
        let keys = (self.spell)(&conflict.chord);
        format!(
            "{keys} is already {}. Press {keys} again to move it here, or Esc to keep it there.",
            conflict.other.label()
        )
    }

    /// Every spelling a search finds `chord` by: the client's, the shared
    /// Mac and spelled-out forms (`⌃P`, `Ctrl+P`), `keys.yaml`'s (`ctrl-p`),
    /// and a few key names (`Return`, `Escape`), lowercased.
    pub fn search_text(&self, chord: &KeyChord) -> String {
        let alias = match chord.code {
            Key::Enter => "return",
            Key::Esc => "escape",
            Key::PageUp => "page up",
            Key::PageDown => "page down",
            Key::Char(' ') => "space",
            _ => "",
        };
        [
            (self.spell)(chord),
            format_chord(chord, DisplayStyle::Mac),
            format_chord(chord, DisplayStyle::Linux),
            chord.canonical(),
            alias.to_string(),
        ]
        .join(" ")
        .to_lowercase()
    }

    /// Whether `row` matches the lowercased `query`: its label, or any of
    /// its chords' spellings.
    fn matches(&self, row: &Row, query: &str) -> bool {
        query.is_empty()
            || row.label.to_lowercase().contains(query)
            || row.chords.iter().any(|c| self.search_text(c).contains(query))
    }
}

/// `chord` in the form `keys.yaml` parses to: a terminal's `K` or `⇧K`
/// becomes `shift-k`, so a captured chord compares equal to a bound one.
pub fn normalize(chord: KeyChord) -> KeyChord {
    KeyChord::parse(&chord.canonical()).unwrap_or(chord)
}

/// Why a `keys.yaml` with these diagnostics can't be shown: it doesn't
/// parse.
fn parse_error(diags: &[KeymapDiagnostic]) -> Option<String> {
    diags.iter().find_map(|d| match d {
        KeymapDiagnostic::Error {
            kind: ErrorKind::ParseError,
            message,
            ..
        } => Some(format!("keys.yaml doesn't parse: {message}")),
        _ => None,
    })
}

/// `$SHELBI_HOME/keys.yaml`, the shared key bindings.
pub fn keys_yaml_path() -> Option<PathBuf> {
    shelbi_state::shelbi_home()
        .ok()
        .map(|home| home.join(shelbi_state::keymap::KEYS_FILENAME))
}

/// The stub a missing `keys.yaml` starts as before an editor opens it:
/// comments only, so it changes no binding.
pub const KEYS_STUB: &str = "# Shelbi key bindings, shared by the TUI and the desktop app.\n\
# Override a built-in chord per mode; unlisted actions keep their defaults.\n\
# `shelbi config lint` checks this file.\n";

/// `keys.yaml`'s path, writing [`KEYS_STUB`] there first when there is no
/// file (an editor won't open a missing one). Blocking.
pub fn ensure_keys_file() -> Result<PathBuf, String> {
    let path = keys_yaml_path().ok_or("No Shelbi home directory.")?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, KEYS_STUB).map_err(|e| format!("Couldn't create keys.yaml: {e}"))?;
    }
    Ok(path)
}

/// Read `keys.yaml` for the editor, after the shared loader's one-shot
/// migrations (so the file is the one both clients read). Blocking.
pub fn read_keys(project: Option<&str>) -> Result<String, String> {
    shelbi_state::keymap::load_keymaps(project);
    edit::read_keys_file().map_err(|e| format!("Couldn't read keys.yaml: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::keymap::{GlobalAction, KanbanAction, SidebarAction};

    const NAV_UP: Action = Action::Sidebar(SidebarAction::NavUp);
    const REFRESH: Action = Action::Sidebar(SidebarAction::Refresh);
    const PALETTE: Action = Action::Global(GlobalAction::OpenPalette);

    fn chord(spec: &str) -> KeyChord {
        KeyChord::parse(spec).unwrap()
    }

    fn chords(specs: &[&str]) -> Vec<KeyChord> {
        specs.iter().map(|s| chord(s)).collect()
    }

    fn spell(c: &KeyChord) -> String {
        format_chord(c, DisplayStyle::Mac)
    }

    fn state(text: &str) -> KeybindingsState {
        let mut s = KeybindingsState::new(Some("shelbi".into()), spell);
        s.load(text.into()).unwrap();
        s
    }

    /// Finish what `outcome` asked to save, as a client does once the write
    /// lands (here, on the text on show).
    fn land(s: &mut KeybindingsState, outcome: KeyOutcome) -> KeyOutcome {
        if let KeyOutcome::Save(change) = &outcome {
            let text = change.apply(s.text(), s.project()).unwrap();
            s.saved(Ok(text)).unwrap();
        }
        outcome
    }

    fn press(s: &mut KeybindingsState, spec: &str) -> KeyOutcome {
        let out = s.key(chord(spec));
        land(s, out)
    }

    #[test]
    fn every_context_and_action_is_listed_with_its_chords() {
        let s = state("projects:\n  shelbi:\n    sidebar:\n      refresh: f5\n");
        let groups = s.groups();
        let titles: Vec<&str> = groups.iter().map(|g| g.context.title).collect();
        assert_eq!(
            titles,
            ["Global", "Sidebar", "Issues board", "Issue details", "Activity", "Command palette"]
        );
        let rows: usize = groups.iter().map(|g| g.rows.len()).sum();
        assert_eq!(rows, Action::all().count());
        let sidebar = &groups[1].rows;
        assert_eq!(sidebar[0].label, "Move selection up");
        assert_eq!(sidebar[0].chords, chords(&["k", "up"]));
        // The project's override shows, flagged as customized.
        let refresh = sidebar.iter().find(|r| r.action == REFRESH).unwrap();
        assert_eq!((refresh.chords.clone(), refresh.customized), (chords(&["f5"]), true));
        assert!(!sidebar[0].customized);
        assert!(s.any_customized());
        assert!(!state("").any_customized());
    }

    #[test]
    fn search_and_the_context_filter_narrow_live() {
        let mut s = state("");
        s.query = "selection up".into();
        let found: Vec<(&str, Action)> = s
            .groups()
            .iter()
            .flat_map(|g| g.rows.iter().map(move |r| (g.context.mode, r.action)))
            .collect();
        assert_eq!(
            found,
            [("sidebar", NAV_UP), ("kanban", Action::Kanban(KanbanAction::NavUp))]
        );
        // By chord, in any spelling.
        for query in ["⌃P", "ctrl+p", "ctrl-p"] {
            s.query = query.into();
            let actions: Vec<Action> = s.groups().iter().flat_map(|g| g.rows.iter().map(|r| r.action)).collect();
            assert!(actions.contains(&PALETTE), "{query}: {actions:?}");
            assert!(actions.iter().all(|a| s.chords(*a).contains(&chord("ctrl-p"))), "{query}");
        }
        s.query = "Return".into();
        assert!(!s.groups().is_empty());
        assert!(s.groups().iter().all(|g| g.rows.iter().all(|r| r.chords.contains(&chord("enter")))));
        // The filter narrows to one context, the search within it.
        s.query = "refresh".into();
        s.context = Some("kanban");
        let groups = s.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].rows.len(), 1);
        s.query = "no such action".into();
        assert!(s.groups().is_empty());
    }

    #[test]
    fn the_context_filter_cycles_through_every_context_and_back() {
        let mut s = state("");
        assert_eq!(s.context_label(), "All contexts");
        let mut seen = Vec::new();
        for _ in 0..=CONTEXTS.len() {
            s.cycle_context(true);
            seen.push(s.context_label());
        }
        assert_eq!(seen.first(), Some(&"Global"));
        assert_eq!(seen.last(), Some(&"All contexts"), "wraps back to all");
        s.cycle_context(false);
        assert_eq!(s.context_label(), "Command palette");
    }

    #[test]
    fn rows_show_named_keys_before_letters() {
        let s = state("");
        let shown = |action: Action| -> Vec<String> {
            s.row(action).shown().iter().map(|(_, c)| spell(c)).collect()
        };
        assert_eq!(shown(NAV_UP), ["↑", "k"]);
        assert_eq!(shown(Action::Sidebar(SidebarAction::Quit)), ["⌃C", "q"]);
        // Each keeps its place in the file's list.
        assert_eq!(s.row(NAV_UP).shown()[1], (0, chord("k")));
    }

    #[test]
    fn a_captured_chord_shows_only_once_saved() {
        let mut s = state("");
        assert!(s.start_capture(NAV_UP, Slot::Chord(0)));
        let out = s.key(chord("w"));
        assert_eq!(out, KeyOutcome::Save(Change::Set(vec![(NAV_UP, chords(&["w", "up"]))])));
        assert_eq!(s.capture(), None);
        assert!(s.saving());
        assert_eq!(s.chords(NAV_UP), chords(&["k", "up"]), "not until it's saved");
        // Nothing else starts while the save is out.
        assert!(!s.start_capture(NAV_UP, Slot::New));
        assert_eq!(s.reset_all(), KeyOutcome::Ignored);
        land(&mut s, out);
        assert!(!s.saving());
        assert_eq!(s.chords(NAV_UP), chords(&["w", "up"]));
        assert_eq!(s.text(), "defaults:\n  sidebar:\n    nav_up: [w, up]\n");
        // Another alternative.
        s.start_capture(NAV_UP, Slot::New);
        press(&mut s, "ctrl-k");
        assert_eq!(s.chords(NAV_UP), chords(&["w", "up", "ctrl-k"]));
    }

    #[test]
    fn a_failed_save_keeps_what_was_shown() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(0));
        assert!(matches!(s.key(chord("w")), KeyOutcome::Save(_)));
        assert_eq!(s.saved(Err("disk full".into())), Err("disk full".into()));
        assert!(!s.saving(), "the next change may start");
        assert_eq!(s.chords(NAV_UP), chords(&["k", "up"]));
    }

    #[test]
    fn a_terminals_shifted_letter_is_the_bound_chord() {
        let mut s = state("");
        // A terminal reports Shift+K as `K` with Shift, or a bare `K`.
        for raw in [KeyChord::new(Key::Char('K'), Mods::SHIFT), KeyChord::new(Key::Char('K'), Mods::NONE)] {
            s.start_capture(Action::Kanban(KanbanAction::ReorderUp), Slot::New);
            assert_eq!(s.key(raw), KeyOutcome::Cancelled, "already bound: {raw:?}");
        }
        assert_eq!(normalize(KeyChord::new(Key::Char('K'), Mods::SHIFT)), chord("shift-k"));
    }

    #[test]
    fn esc_cancels_and_backspace_removes_the_picked_chord() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(1));
        assert_eq!(s.key(chord("esc")), KeyOutcome::Cancelled);
        assert_eq!((s.capture(), s.chords(NAV_UP)), (None, chords(&["k", "up"])));
        assert_eq!(s.key(chord("j")), KeyOutcome::Ignored, "no capture, no change");

        s.start_capture(NAV_UP, Slot::Chord(1));
        assert!(matches!(press(&mut s, "backspace"), KeyOutcome::Save(_)));
        assert_eq!(s.chords(NAV_UP), chords(&["k"]));
        s.start_capture(NAV_UP, Slot::Chord(0));
        press(&mut s, "delete");
        assert!(s.chords(NAV_UP).is_empty());
        assert_eq!(s.text(), "defaults:\n  sidebar:\n    nav_up: []\n");
        // Backspace on the add slot just closes it.
        s.start_capture(NAV_UP, Slot::New);
        assert_eq!(s.key(chord("backspace")), KeyOutcome::Cancelled);
    }

    #[test]
    fn remove_drops_one_chord_without_a_capture() {
        let mut s = state("");
        let out = s.remove(NAV_UP, 0);
        land(&mut s, out);
        assert_eq!(s.chords(NAV_UP), chords(&["up"]));
        assert_eq!(s.remove(NAV_UP, 5), KeyOutcome::Ignored);
    }

    #[test]
    fn a_chord_in_use_in_the_context_is_flagged_until_confirmed() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(0));
        assert_eq!(s.key(chord("r")), KeyOutcome::Conflict);
        let conflict = s.conflict().unwrap();
        assert_eq!(conflict, Conflict { chord: chord("r"), other: REFRESH });
        assert_eq!(
            s.conflict_message(&conflict),
            "r is already Refresh. Press r again to move it here, or Esc to keep it there."
        );
        assert!(s.capture().is_some(), "still capturing");
        assert!(!s.saving(), "nothing saved yet");

        // A reload of the same file keeps it.
        s.load(String::new()).unwrap();
        assert_eq!(s.conflict(), Some(conflict));

        // Esc keeps both as they were.
        assert_eq!(s.key(chord("esc")), KeyOutcome::Cancelled);
        assert_eq!(s.conflict(), None);
        assert_eq!((s.chords(NAV_UP), s.chords(REFRESH)), (chords(&["k", "up"]), chords(&["r"])));

        // The same chord twice moves it.
        s.start_capture(NAV_UP, Slot::Chord(0));
        s.key(chord("r"));
        let out = press(&mut s, "r");
        assert_eq!(
            out,
            KeyOutcome::Save(Change::Set(vec![(REFRESH, vec![]), (NAV_UP, chords(&["r", "up"]))]))
        );
        assert_eq!((s.chords(NAV_UP), s.chords(REFRESH)), (chords(&["r", "up"]), vec![]));

        // A chord in another context isn't a conflict.
        s.start_capture(PALETTE, Slot::New);
        assert!(matches!(press(&mut s, "j"), KeyOutcome::Save(_)));
        assert_eq!(s.chords(PALETTE), chords(&["ctrl-p", "j"]));
    }

    #[test]
    fn a_different_chord_after_a_conflict_is_checked_afresh() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(0));
        s.key(chord("r"));
        // Not the conflicting chord: a fresh, free one saves at once.
        assert!(matches!(press(&mut s, "w"), KeyOutcome::Save(_)));
        assert_eq!(s.chords(REFRESH), chords(&["r"]), "Refresh untouched");
    }

    #[test]
    fn a_chord_the_action_has_already_changes_nothing() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(0));
        assert_eq!(s.key(chord("up")), KeyOutcome::Cancelled);
        assert_eq!(s.text(), "");
        assert!(!s.saving());
    }

    #[test]
    fn reset_and_reset_all_restore_the_defaults() {
        let text = "# mine\ndefaults:\n  sidebar:\n    nav_up: w\n    refresh: f5\n";
        let mut s = state(text);
        let out = s.reset(NAV_UP);
        assert_eq!(land(&mut s, out), KeyOutcome::Save(Change::Reset(NAV_UP)));
        assert_eq!(s.chords(NAV_UP), chords(&["k", "up"]));
        let out = s.reset_all();
        assert_eq!(land(&mut s, out), KeyOutcome::Save(Change::ResetAll));
        assert!(!s.any_customized());
        assert_eq!(s.text(), "# mine\n");
    }

    #[test]
    fn reset_all_keeps_other_projects_and_unknown_entries() {
        let text = "defaults:\n  sidebar:\n    nav_up: w\n  future_mode:\n    thing: x\nprojects:\n  shelbi:\n    sidebar:\n      refresh: f5\n  other:\n    sidebar:\n      refresh: f6\n";
        let out = Change::ResetAll.apply(text, Some("shelbi")).unwrap();
        assert!(out.contains("other:") && out.contains("f6"), "{out}");
        assert!(out.contains("future_mode"), "{out}");
        assert!(!out.contains("f5") && !out.contains("nav_up"), "{out}");
    }

    #[test]
    fn an_edit_goes_to_the_project_when_it_owns_the_action() {
        let text = "projects:\n  shelbi:\n    sidebar:\n      refresh: f5\n";
        let mut s = state(text);
        s.start_capture(REFRESH, Slot::Chord(0));
        press(&mut s, "f6");
        assert_eq!(s.text(), "projects:\n  shelbi:\n    sidebar:\n      refresh: f6\n");
        // An action the project doesn't set goes to the defaults.
        s.start_capture(NAV_UP, Slot::New);
        press(&mut s, "w");
        assert!(s.text().starts_with("projects:"));
        assert!(s.text().contains("defaults:\n  sidebar:\n    nav_up: [k, up, w]\n"), "{}", s.text());
    }

    #[test]
    fn another_project_shows_its_own_bindings_from_the_same_file() {
        let mut s = state("projects:\n  shelbi:\n    sidebar:\n      refresh: f5\n");
        s.start_capture(NAV_UP, Slot::New);
        s.set_project(Some("beta".into()));
        assert_eq!(s.capture(), None, "the capture ends");
        assert_eq!(s.chords(REFRESH), chords(&["r"]));
        s.set_project(Some("shelbi".into()));
        assert_eq!(s.chords(REFRESH), chords(&["f5"]));
    }

    #[test]
    fn rebinding_to_the_defaults_leaves_no_override() {
        let mut s = state("defaults:\n  sidebar:\n    nav_up: [w, up]\n");
        s.start_capture(NAV_UP, Slot::Chord(0));
        press(&mut s, "k");
        assert_eq!(s.text(), "");
        assert!(!s.any_customized());
    }

    #[test]
    fn a_broken_keys_yaml_is_refused_and_never_edited() {
        let mut s = state("defaults:\n  sidebar:\n    nav_up: w\n");
        let err = s.load("defaults: [unclosed\n".into()).unwrap_err();
        assert!(err.starts_with("keys.yaml doesn't parse"), "{err}");
        assert_eq!(s.chords(NAV_UP), chords(&["w"]), "the bindings in effect stay");
        // Saved onto the broken file on disk: refused, nothing written.
        let change = Change::Set(vec![(NAV_UP, chords(&["x"]))]);
        assert!(change.apply("defaults: [unclosed\n", Some("shelbi")).is_err());
    }

    #[test]
    fn the_change_applies_to_the_file_as_it_is_on_disk() {
        // Edited elsewhere since the page read it: the change lands on top.
        let disk = "# edited by hand\ndefaults:\n  kanban:\n    refresh: f5\n";
        let change = Change::Set(vec![(NAV_UP, chords(&["w"]))]);
        let out = change.apply(disk, Some("shelbi")).unwrap();
        assert_eq!(out, format!("{disk}  sidebar:\n    nav_up: w\n"));
    }

    #[test]
    fn search_text_has_every_spelling() {
        let s = state("");
        let text = s.search_text(&chord("ctrl-p"));
        for needle in ["⌃p", "ctrl+p", "ctrl-p"] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
        assert!(s.search_text(&chord("enter")).contains("return"));
    }
}
