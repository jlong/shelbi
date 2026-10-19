//! The Settings ▸ Keybindings editor's state, apart from gpui: the bindings
//! listed by context ([`shelbi_state::keymap::CONTEXTS`]) with their current
//! chords, the search and context filters, and capturing a new chord.
//!
//! The source of truth is `keys.yaml`'s text. The listed chords are what the
//! shared loader merges from it for the window's project, so an override in
//! the file shows exactly as the TUI would apply it. A finished capture is a
//! [`Change`]: the view applies it to the file as it is on disk (through
//! `shelbi_state::keymap::edit`, which keeps the user's comments and other
//! entries), writes it, and reloads this state from the result. The state
//! applies the same change to its own copy at once, so the row shows the new
//! chord before the write lands.
//!
//! Capturing: a keycap clicked (or the add button) waits for a chord. Esc
//! cancels; Backspace or Delete removes the clicked chord; any other chord
//! replaces it (or is added). A chord another action in the same context
//! already has is held as a [`Conflict`] and shown on the row: pressing it
//! again moves it here (off the other action), Esc keeps both as they were.

use shelbi_state::keymap::edit::{self, builtin_chords, EditError};
use shelbi_state::keymap::{
    validate_keymaps_yaml, Action, Key, KeyChord, KeymapContext, Keymaps, Mods, CONTEXTS,
};

use crate::keys::{chord_search_text, keycap_keys};

/// Which of an action's chords a capture replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The chord at this index in the action's list.
    Chord(usize),
    /// A new chord after the others.
    New,
}

/// A keycap waiting for a chord.
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
    /// The change applied to `text`, a `keys.yaml`, for `project`.
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
    /// The capture ended in this change, already applied here; save it.
    Save(Change),
    /// The change couldn't be made (`keys.yaml` doesn't parse); the capture
    /// ended and nothing changed.
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
    /// modified chords before bare letters, as the frame lists them (`↑ or
    /// K`, `Home or G`), otherwise in `keys.yaml`'s order. Display only: the
    /// file's order (whose first chord the TUI's hints show) is kept.
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
    /// The window's project, whose `projects.<name>` block applies.
    project: Option<String>,
    /// `keys.yaml` as last read (or as the last change left it).
    text: String,
    keymaps: Keymaps,
    /// The search: rows whose label or chords contain it.
    pub query: String,
    /// The context picker: one context's mode, or every context.
    pub context: Option<&'static str>,
    capture: Option<Capture>,
    conflict: Option<Conflict>,
}

impl KeybindingsState {
    /// The built-in bindings until `keys.yaml` is read ([`Self::load`]).
    pub fn new(project: Option<String>) -> Self {
        let mut state = KeybindingsState {
            project,
            text: String::new(),
            keymaps: Keymaps::default(),
            query: String::new(),
            context: None,
            capture: None,
            conflict: None,
        };
        state.load(String::new());
        state
    }

    /// Show the bindings `text` (a `keys.yaml`) gives the project. An open
    /// capture stays open (its row is still there), and a held conflict
    /// while the other action still has the chord.
    pub fn load(&mut self, text: String) {
        self.keymaps = validate_keymaps_yaml(&text, self.project.as_deref()).0;
        self.text = text;
        self.conflict = self.conflict.filter(|c| self.keymaps.chords(c.other).contains(&c.chord));
    }

    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// The merged keymaps on show.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// `action`'s chords on show.
    pub fn chords(&self, action: Action) -> Vec<KeyChord> {
        self.keymaps.chords(action)
    }

    /// The groups to list: every context (or the picked one), each with its
    /// rows matching the search; a context with no match is left out.
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
                    .filter(|row| matches(row, &query))
                    .collect();
                (!rows.is_empty()).then_some(Group { context, rows })
            })
            .collect()
    }

    fn row(&self, action: Action) -> Row {
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

    /// The open capture.
    pub fn capture(&self) -> Option<Capture> {
        self.capture
    }

    /// The held conflict, while the capture is open.
    pub fn conflict(&self) -> Option<Conflict> {
        self.conflict
    }

    /// Wait for a chord for `action`'s `slot`.
    pub fn start_capture(&mut self, action: Action, slot: Slot) {
        self.capture = Some(Capture { action, slot });
        self.conflict = None;
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

    /// Restore `action`'s built-in chords.
    pub fn reset(&mut self, action: Action) -> KeyOutcome {
        self.finish(Change::Reset(action))
    }

    /// Restore every built-in chord.
    pub fn reset_all(&mut self) -> KeyOutcome {
        self.finish(Change::ResetAll)
    }

    /// The other action in `action`'s context bound to `chord`, if any.
    fn holder(&self, action: Action, chord: KeyChord) -> Option<Action> {
        Action::all()
            .filter(|a| *a != action && a.mode() == action.mode())
            .find(|a| self.chords(*a).contains(&chord))
    }

    fn finish(&mut self, change: Change) -> KeyOutcome {
        self.cancel();
        match change.apply(&self.text, self.project.as_deref()) {
            Ok(text) => {
                self.load(text);
                KeyOutcome::Save(change)
            }
            Err(e) => KeyOutcome::Failed(e.to_string()),
        }
    }

    /// The `keys.yaml` text the editor shows.
    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The held conflict's line under its row.
pub fn conflict_message(conflict: &Conflict) -> String {
    let keys = keycap_keys(&conflict.chord).concat();
    format!(
        "{keys} is already {}. Press {keys} again to move it here, or Esc to keep it there.",
        conflict.other.label()
    )
}

/// Whether `row` matches the lowercased `query`: its label, or any of its
/// chords' spellings ([`chord_search_text`]).
fn matches(row: &Row, query: &str) -> bool {
    query.is_empty()
        || row.label.to_lowercase().contains(query)
        || row.chords.iter().any(|c| chord_search_text(c).contains(query))
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

    fn state(text: &str) -> KeybindingsState {
        let mut s = KeybindingsState::new(Some("shelbi".into()));
        s.load(text.into());
        s
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
    fn search_and_the_context_picker_filter_live() {
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
        assert!(s.groups().iter().all(|g| g.rows.iter().all(|r| r.chords.contains(&chord("enter")))));
        // The picker narrows to one context, the search within it.
        s.query = "refresh".into();
        s.context = Some("kanban");
        let groups = s.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].rows.len(), 1);
        s.query = "no such action".into();
        assert!(s.groups().is_empty());
    }

    #[test]
    fn rows_show_named_keys_before_letters_as_the_frame_does() {
        let s = state("");
        let shown = |action: Action| -> Vec<String> {
            s.row(action).shown().iter().map(|(_, c)| keycap_keys(c).concat()).collect()
        };
        assert_eq!(shown(NAV_UP), ["↑", "K"]);
        assert_eq!(shown(Action::Kanban(KanbanAction::ReorderUp)), ["⇧↑", "⇧K"]);
        assert_eq!(shown(Action::Sidebar(SidebarAction::Quit)), ["⌃C", "Q"]);
        assert_eq!(shown(Action::Global(GlobalAction::OpenPalette)), ["⌃P"]);
        // Each keeps its place in the file's list.
        assert_eq!(s.row(NAV_UP).shown()[1], (0, chord("k")));
    }

    #[test]
    fn a_captured_chord_replaces_the_clicked_one_and_saves() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(0));
        let out = s.key(chord("w"));
        assert_eq!(out, KeyOutcome::Save(Change::Set(vec![(NAV_UP, chords(&["w", "up"]))])));
        assert_eq!(s.chords(NAV_UP), chords(&["w", "up"]));
        assert_eq!(s.capture(), None);
        assert_eq!(s.text(), "defaults:\n  sidebar:\n    nav_up: [w, up]\n");
        // Another alternative.
        s.start_capture(NAV_UP, Slot::New);
        s.key(chord("ctrl-k"));
        assert_eq!(s.chords(NAV_UP), chords(&["w", "up", "ctrl-k"]));
    }

    #[test]
    fn esc_cancels_and_backspace_removes_the_clicked_chord() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(1));
        assert_eq!(s.key(chord("esc")), KeyOutcome::Cancelled);
        assert_eq!((s.capture(), s.chords(NAV_UP)), (None, chords(&["k", "up"])));
        assert_eq!(s.key(chord("j")), KeyOutcome::Ignored, "no capture, no change");

        s.start_capture(NAV_UP, Slot::Chord(1));
        assert!(matches!(s.key(chord("backspace")), KeyOutcome::Save(_)));
        assert_eq!(s.chords(NAV_UP), chords(&["k"]));
        s.start_capture(NAV_UP, Slot::Chord(0));
        s.key(chord("delete"));
        assert!(s.chords(NAV_UP).is_empty());
        assert_eq!(s.text(), "defaults:\n  sidebar:\n    nav_up: []\n");
        // Backspace on the add button just closes it.
        s.start_capture(NAV_UP, Slot::New);
        assert_eq!(s.key(chord("backspace")), KeyOutcome::Cancelled);
    }

    #[test]
    fn a_chord_in_use_in_the_context_is_flagged_until_confirmed() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(0));
        assert_eq!(s.key(chord("r")), KeyOutcome::Conflict);
        let conflict = s.conflict().unwrap();
        assert_eq!(conflict, Conflict { chord: chord("r"), other: REFRESH });
        assert_eq!(
            conflict_message(&conflict),
            "R is already Refresh. Press R again to move it here, or Esc to keep it there."
        );
        assert!(s.capture().is_some(), "still capturing");
        assert_eq!(s.chords(REFRESH), chords(&["r"]), "nothing saved yet");

        // A reload of the same file keeps it.
        s.load(String::new());
        assert_eq!(s.conflict(), Some(conflict));

        // Esc keeps both as they were.
        assert_eq!(s.key(chord("esc")), KeyOutcome::Cancelled);
        assert_eq!(s.conflict(), None);
        assert_eq!((s.chords(NAV_UP), s.chords(REFRESH)), (chords(&["k", "up"]), chords(&["r"])));

        // The same chord twice moves it.
        s.start_capture(NAV_UP, Slot::Chord(0));
        s.key(chord("r"));
        let out = s.key(chord("r"));
        assert_eq!(
            out,
            KeyOutcome::Save(Change::Set(vec![(REFRESH, vec![]), (NAV_UP, chords(&["r", "up"]))]))
        );
        assert_eq!((s.chords(NAV_UP), s.chords(REFRESH)), (chords(&["r", "up"]), vec![]));

        // A chord in another context isn't a conflict.
        s.start_capture(PALETTE, Slot::New);
        assert!(matches!(s.key(chord("j")), KeyOutcome::Save(_)));
        assert_eq!(s.chords(PALETTE), chords(&["ctrl-p", "j"]));
    }

    #[test]
    fn a_chord_the_action_has_already_changes_nothing() {
        let mut s = state("");
        s.start_capture(NAV_UP, Slot::Chord(0));
        assert_eq!(s.key(chord("up")), KeyOutcome::Cancelled);
        assert_eq!(s.text(), "");
    }

    #[test]
    fn reset_and_reset_all_restore_the_defaults() {
        let text = "# mine\ndefaults:\n  sidebar:\n    nav_up: w\n    refresh: f5\n";
        let mut s = state(text);
        assert_eq!(s.reset(NAV_UP), KeyOutcome::Save(Change::Reset(NAV_UP)));
        assert_eq!(s.chords(NAV_UP), chords(&["k", "up"]));
        assert_eq!(s.reset_all(), KeyOutcome::Save(Change::ResetAll));
        assert!(!s.any_customized());
        assert_eq!(s.text(), "# mine\n");
    }

    #[test]
    fn rebinding_to_the_defaults_leaves_no_override() {
        let mut s = state("defaults:\n  sidebar:\n    nav_up: [w, up]\n");
        s.start_capture(NAV_UP, Slot::Chord(0));
        s.key(chord("k"));
        assert_eq!(s.text(), "");
        assert!(!s.any_customized());
    }

    #[test]
    fn a_broken_keys_yaml_is_left_alone() {
        let mut s = state("defaults: [unclosed\n");
        s.start_capture(NAV_UP, Slot::Chord(0));
        assert!(matches!(s.key(chord("w")), KeyOutcome::Failed(_)));
        assert_eq!(s.text(), "defaults: [unclosed\n");
        assert_eq!(s.capture(), None);
    }

    #[test]
    fn the_change_applies_to_the_file_as_it_is_on_disk() {
        // Edited elsewhere since the page read it: the change lands on top.
        let disk = "# edited by hand\ndefaults:\n  kanban:\n    refresh: f5\n";
        let change = Change::Set(vec![(NAV_UP, chords(&["w"]))]);
        let out = change.apply(disk, Some("shelbi")).unwrap();
        assert_eq!(out, format!("{disk}  sidebar:\n    nav_up: w\n"));
    }
}
