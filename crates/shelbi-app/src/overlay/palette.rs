//! The command palette's state machine.
//!
//! The palette lists the registry's entries ([`crate::CommandRegistry::entries`])
//! fuzzy-filtered by [`shelbi_palette::search`], beside a Projects column. Its
//! focus model: typing filters, `↑`/`↓` move the selection, `→` moves focus
//! into the Projects column (and `←` back), Enter activates, Tab hands focus to
//! the sidebar (or, in a client that asks for it with
//! [`Palette::tab_switches_columns`], moves between the two columns), and
//! Escape (or the palette chord) closes. Activating a project
//! row yields its `action:switch-project:<slug>` entry (the trailing row,
//! `action:add-project`), so the host runs it through the registry like any
//! other entry.
//!
//! Typing also searches the project's issues and Changelog entries
//! ([`super::palette_search`]). While the query isn't blank the palette is
//! *searching*: the Projects column steps aside, a filter row (All / Issues /
//! Changelog Entries / Commands, each with its count) sits under the input,
//! and the results are [`Palette::rows`], grouped by kind under All. Tab and
//! Shift-Tab change the filter then (an empty query keeps Tab's old job), as
//! do ←/→ once ↑ from the first result has put focus on the filter row.

use shelbi_palette::{Entry, EntryKind};
use shelbi_state::keymap::{Key, KeyChord, Mods, PaletteAction};

use super::palette_search::{
    match_count, result_rows, FilterChip, PaletteSearch, ResultItem, ResultRow, SearchCorpus,
    SearchFilter,
};
use super::{is_bare, typed_char};

/// A project row's liveness indicator in the Projects column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectIndicator {
    /// The current project: a filled disc, pulsing.
    Active,
    /// Open but not the current project: a green ring.
    LoadedIdle,
    /// Not open: a gray ring.
    Unloaded,
}

impl ProjectIndicator {
    /// The row's glyph: a filled disc for the current project, a ring for the
    /// rest (green or gray by color, not glyph).
    pub fn glyph(self) -> &'static str {
        match self {
            ProjectIndicator::Active => "●",
            ProjectIndicator::LoadedIdle | ProjectIndicator::Unloaded => "○",
        }
    }
}

/// Resolve a project's indicator. Not-loaded dominates: a project with no live
/// session shows the gray ring even if it is the "active" one on disk.
pub fn project_indicator(loaded: bool, active: bool) -> ProjectIndicator {
    if !loaded {
        ProjectIndicator::Unloaded
    } else if active {
        ProjectIndicator::Active
    } else {
        ProjectIndicator::LoadedIdle
    }
}

/// A Projects-column row: its slug (for the switch entry id), display label,
/// and indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    pub slug: String,
    pub label: String,
    pub indicator: ProjectIndicator,
}

/// Which column holds focus inside the open palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteFocus {
    Commands,
    Projects,
}

/// What one key (or click) resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteStep {
    /// Keep the palette open (filtered, moved the selection, edited the query).
    Continue,
    /// Close the palette; focus returns where it was (Escape / the chord).
    Close,
    /// Close the palette and move focus to the sidebar (Tab).
    FocusSidebar,
    /// Run this entry's command, then close.
    Activate(Entry),
}

/// The palette's state: the query, the command selection, the switch-target
/// projects, and which column holds focus. The host supplies the entries and
/// the projects and turns an activated [`Entry`] into a command.
#[derive(Debug, Clone)]
pub struct Palette {
    query: String,
    /// The selected result: an index into the selectable rows
    /// ([`Self::rows`] without the headers).
    selected: usize,
    entries: Vec<Entry>,
    projects: Vec<ProjectEntry>,
    focus: PaletteFocus,
    project_selected: usize,
    tab_columns: bool,
    filter: SearchFilter,
    /// Whether the filter row has the keys (↑ from the first result).
    filter_focus: bool,
    search: PaletteSearch,
}

impl Palette {
    /// Open over `entries`, listing `projects` in the Projects column (empty
    /// hides the column).
    pub fn new(entries: Vec<Entry>, projects: Vec<ProjectEntry>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            entries,
            projects,
            focus: PaletteFocus::Commands,
            project_selected: 0,
            tab_columns: false,
            filter: SearchFilter::All,
            filter_focus: false,
            search: PaletteSearch::default(),
        }
    }

    /// Search issues and Changelog entries with `search` as well as the
    /// commands.
    pub fn with_search(mut self, search: PaletteSearch) -> Self {
        self.search = search;
        self
    }

    /// Search fresh issues and entries (a read landed while open).
    pub fn set_corpus(&mut self, corpus: SearchCorpus) {
        self.search.set_corpus(corpus);
    }

    /// The search's issues and entries.
    pub fn corpus(&self) -> &SearchCorpus {
        self.search.corpus()
    }

    /// Take search hits that landed off-thread. Returns whether the results
    /// changed (the host redraws).
    pub fn poll_search(&mut self) -> bool {
        self.search.poll()
    }

    /// Whether the current query's issue and Changelog search hasn't
    /// landed yet.
    pub fn search_pending(&self) -> bool {
        self.searching() && self.search.pending()
    }

    /// Whether the palette is searching: the query isn't blank. The filter
    /// row shows, and the Projects column doesn't.
    pub fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub fn filter(&self) -> SearchFilter {
        self.filter
    }

    /// Whether the filter row has the keys.
    pub fn filter_focused(&self) -> bool {
        self.searching() && self.filter_focus
    }

    /// Whether the Projects column shows: there are projects and the
    /// palette isn't searching.
    pub fn shows_projects(&self) -> bool {
        !self.projects.is_empty() && !self.searching()
    }

    /// The filter row's chips, left to right, each with its match count.
    pub fn chips(&self) -> Vec<FilterChip> {
        let commands = self.results().len();
        let hits = self.search.hits();
        SearchFilter::ROW
            .iter()
            .map(|&filter| FilterChip {
                filter,
                label: filter.label(),
                count: match_count(filter, commands, hits),
                selected: filter == self.filter,
            })
            .collect()
    }

    /// The results band's rows: the commands alone while not searching;
    /// grouped by kind under All, or one kind under its filter, while
    /// searching.
    pub fn rows(&self) -> Vec<ResultRow> {
        result_rows(&self.results(), self.search.hits(), self.filter, self.searching())
    }

    fn items(&self) -> Vec<ResultItem> {
        self.rows()
            .into_iter()
            .filter_map(|r| match r {
                ResultRow::Item(i) => Some(i),
                ResultRow::Header(_) => None,
            })
            .collect()
    }

    /// The row the selection is on (clamped to the rows there are), or
    /// `None` when there are none.
    pub fn selected_row(&self) -> Option<usize> {
        let rows = self.rows();
        let count = rows.iter().filter(|r| r.item().is_some()).count();
        let target = self.selected_in(count);
        rows.iter()
            .enumerate()
            .filter(|(_, r)| r.item().is_some())
            .nth(target)
            .map(|(i, _)| i)
    }

    /// Show `filter`'s results (a chip clicked, a "Show all" row taken), from
    /// the top.
    pub fn set_filter(&mut self, filter: SearchFilter) {
        self.filter = filter;
        self.selected = 0;
    }

    /// A click on a filter chip.
    pub fn click_filter(&mut self, filter: SearchFilter) {
        self.set_filter(filter);
        self.filter_focus = false;
        self.focus = PaletteFocus::Commands;
    }

    /// A click on the `index`-th row of [`Self::rows`]: select and activate
    /// it. A header, or a row that's gone, does nothing.
    pub fn click_row(&mut self, index: usize) -> PaletteStep {
        let rows = self.rows();
        if rows.get(index).and_then(ResultRow::item).is_none() {
            return PaletteStep::Continue;
        }
        let item = rows[..index].iter().filter(|r| r.item().is_some()).count();
        self.click_item(item)
    }

    /// Activate a result: run its entry, or take a "Show all" row's filter.
    fn activate(&mut self, item: ResultItem) -> PaletteStep {
        match item {
            ResultItem::ShowAll { kind, .. } => {
                self.set_filter(kind.filter());
                PaletteStep::Continue
            }
            other => match other.entry() {
                Some(entry) => PaletteStep::Activate(entry),
                None => PaletteStep::Continue,
            },
        }
    }

    /// Make Tab move focus between the commands and the Projects column
    /// (the desktop's palette) instead of handing it to the sidebar. A
    /// palette with no Projects column still hands Tab to the sidebar.
    pub fn tab_switches_columns(mut self) -> Self {
        self.tab_columns = true;
        self
    }

    /// Replace the entries (a background refresh landed), keeping the query.
    /// The selection is clamped by readers ([`Self::selected_in`]).
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
    }

    /// Replace the Projects column (a fresh read landed), keeping the focus
    /// on a row that still exists.
    pub fn set_projects(&mut self, projects: Vec<ProjectEntry>) {
        self.projects = projects;
        self.project_selected = self.project_selected.min(self.projects.len());
        if self.projects.is_empty() {
            self.focus = PaletteFocus::Commands;
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The selected command index (may exceed the current results after a
    /// refresh; see [`Self::selected_in`]).
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The selected index clamped to `count` results.
    pub fn selected_in(&self, count: usize) -> usize {
        self.selected.min(count.saturating_sub(1))
    }

    pub fn set_selected(&mut self, index: usize) {
        self.selected = index;
    }

    pub fn focus(&self) -> PaletteFocus {
        self.focus
    }

    /// Move focus to the Projects column's first row (the bare "Switch
    /// Project" lands here). A palette with no Projects column keeps the
    /// commands focused.
    pub fn focus_projects(&mut self) {
        if !self.projects.is_empty() {
            self.focus = PaletteFocus::Projects;
            self.project_selected = 0;
        }
    }

    pub fn projects(&self) -> &[ProjectEntry] {
        &self.projects
    }

    /// The focused Projects-column row (`projects().len()` is the trailing
    /// "Add project" row), or `None` while the commands column has focus.
    pub fn project_selection(&self) -> Option<usize> {
        (self.focus == PaletteFocus::Projects).then_some(self.project_selected)
    }

    /// The fuzzy-filtered results for the current query, best first.
    pub fn results(&self) -> Vec<(Entry, u16)> {
        shelbi_palette::search(&self.entries, &self.query)
    }

    /// The footer hint. `close` names the close chord (`Ctrl+P`).
    pub fn footer(&self, close: &str) -> String {
        if self.searching() {
            format!("↑↓ navigate · Tab filter · Enter open · Esc / {close} close")
        } else if self.projects.is_empty() {
            format!("↑↓ navigate · Enter activate · Esc / {close} close")
        } else if self.tab_columns {
            format!("↑↓ navigate · Enter activate · Tab projects · Esc / {close} close")
        } else {
            format!("↑↓ navigate · → projects · Enter activate · Esc / {close} close")
        }
    }

    /// The entry Enter on the focused project row activates: the project's
    /// `action:switch-project:<slug>`, or `action:add-project` for the
    /// trailing row.
    pub fn project_entry(&self) -> Entry {
        let (id, label) = match self.projects.get(self.project_selected) {
            Some(p) => (format!("action:switch-project:{}", p.slug), p.label.clone()),
            None => ("action:add-project".to_string(), "Add project".to_string()),
        };
        Entry {
            id,
            label,
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        }
    }

    /// Feed one key. `action` is the palette-mode binding the host resolved
    /// from `keys.yaml` (`keymaps.palette.dispatch(chord)`), so Close /
    /// Activate / NavUp / NavDown / Backspace honor the user's bindings; Tab,
    /// Space, the arrows and unbound printable characters are matched on the
    /// chord itself so they behave whatever is bound.
    pub fn handle_key(&mut self, chord: KeyChord, action: Option<PaletteAction>) -> PaletteStep {
        // Tab leaves for the sidebar (or crosses to the other column), ahead
        // of the keymap so no binding can shadow it.
        // While searching, Tab and Shift-Tab change the filter.
        let back_tab = chord.code == Key::BackTab || (chord.code == Key::Tab && chord.mods == Mods::SHIFT);
        if self.searching() && (is_bare(&chord, Key::Tab) || back_tab) {
            let filter = if back_tab { self.filter.prev() } else { self.filter.next() };
            self.set_filter(filter);
            return PaletteStep::Continue;
        }
        // ←/→ move along the filter row while it has the keys; with no
        // Projects column beside the results they do nothing else.
        if self.searching() && (is_bare(&chord, Key::Left) || is_bare(&chord, Key::Right)) {
            if self.filter_focus {
                let filter = if chord.code == Key::Left { self.filter.prev() } else { self.filter.next() };
                self.set_filter(filter);
            }
            return PaletteStep::Continue;
        }
        if is_bare(&chord, Key::Tab) {
            if !self.tab_columns || self.projects.is_empty() {
                return PaletteStep::FocusSidebar;
            }
            match self.focus {
                PaletteFocus::Commands => {
                    self.focus = PaletteFocus::Projects;
                    self.project_selected = 0;
                }
                PaletteFocus::Projects => self.focus = PaletteFocus::Commands,
            }
            return PaletteStep::Continue;
        }
        if is_bare(&chord, Key::Right) {
            if self.focus == PaletteFocus::Commands && !self.projects.is_empty() {
                self.focus = PaletteFocus::Projects;
                self.project_selected = 0;
            }
            return PaletteStep::Continue;
        }
        if is_bare(&chord, Key::Left) {
            self.focus = PaletteFocus::Commands;
            return PaletteStep::Continue;
        }
        // Space always types into the query, even if a binding claims it.
        if chord.code == Key::Char(' ') && (chord.mods.is_empty() || chord.mods == Mods::SHIFT) {
            self.type_char(' ');
            return PaletteStep::Continue;
        }

        match action {
            Some(PaletteAction::Close) => PaletteStep::Close,
            Some(PaletteAction::Activate) => {
                if self.focus == PaletteFocus::Projects {
                    return PaletteStep::Activate(self.project_entry());
                }
                // Enter on the filter row hands the keys back to the results.
                if self.filter_focused() {
                    self.filter_focus = false;
                    return PaletteStep::Continue;
                }
                let items = self.items();
                let ix = self.selected_in(items.len());
                match items.into_iter().nth(ix) {
                    Some(item) => self.activate(item),
                    None => PaletteStep::Continue,
                }
            }
            Some(PaletteAction::NavUp) => {
                if self.focus == PaletteFocus::Projects {
                    self.project_selected = self.project_selected.saturating_sub(1);
                } else if self.searching() && self.selected == 0 {
                    // Up from the first result reaches the filter row.
                    self.filter_focus = true;
                } else {
                    self.selected = self.selected.saturating_sub(1);
                }
                PaletteStep::Continue
            }
            Some(PaletteAction::NavDown) => {
                if self.focus == PaletteFocus::Projects {
                    // One past the last project is the "Add project" row.
                    if self.project_selected < self.projects.len() {
                        self.project_selected += 1;
                    }
                } else if self.filter_focused() {
                    self.filter_focus = false;
                    self.selected = 0;
                } else if self.selected + 1 < self.items().len() {
                    self.selected += 1;
                }
                PaletteStep::Continue
            }
            Some(PaletteAction::Backspace) => {
                self.query.pop();
                self.edited();
                PaletteStep::Continue
            }
            None => {
                if let Some(c) = typed_char(&chord) {
                    self.type_char(c);
                }
                PaletteStep::Continue
            }
        }
    }

    /// A click on the `index`-th result (the selectable rows, headers not
    /// counted): select it and activate it. Out of range (the list changed
    /// under the click) does nothing.
    pub fn click_item(&mut self, index: usize) -> PaletteStep {
        match self.items().into_iter().nth(index) {
            Some(item) => {
                self.selected = index;
                self.focus = PaletteFocus::Commands;
                self.filter_focus = false;
                self.activate(item)
            }
            None => PaletteStep::Continue,
        }
    }

    /// A click on the `index`-th Projects-column row (`projects().len()` is
    /// the "Add project" row): select and activate it.
    pub fn click_project(&mut self, index: usize) -> PaletteStep {
        self.focus = PaletteFocus::Projects;
        self.project_selected = index.min(self.projects.len());
        PaletteStep::Activate(self.project_entry())
    }

    fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.edited();
    }

    /// The query changed: back to the first result, and search again. A
    /// query cleared to blank leaves the search (and its filter) behind.
    fn edited(&mut self) {
        self.selected = 0;
        self.focus = PaletteFocus::Commands;
        self.filter_focus = false;
        if !self.searching() {
            self.filter = SearchFilter::All;
        }
        self.search.ask(&self.query);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::test_keys::{ch, key};

    fn entry(id: &str, label: &str) -> Entry {
        Entry {
            id: id.into(),
            label: label.into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        }
    }

    fn project(slug: &str) -> ProjectEntry {
        ProjectEntry {
            slug: slug.into(),
            label: slug.to_uppercase(),
            indicator: ProjectIndicator::LoadedIdle,
        }
    }

    fn keymaps() -> shelbi_state::keymap::Keymaps {
        shelbi_state::keymap::validate_keymaps_yaml("", None).0
    }

    /// Feed `chord` the way a host does: resolve the palette binding first.
    fn press(p: &mut Palette, chord: KeyChord) -> PaletteStep {
        let action = keymaps().palette.dispatch(chord);
        p.handle_key(chord, action)
    }

    #[test]
    fn typing_filters_and_resets_the_selection() {
        let mut p = Palette::new(
            vec![entry("view:tasks", "Issues"), entry("action:toggle-zen", "Turn Zen Mode on")],
            Vec::new(),
        );
        p.set_selected(1);
        for c in "zen".chars() {
            assert_eq!(press(&mut p, ch(c)), PaletteStep::Continue);
        }
        assert_eq!(p.query(), "zen");
        assert_eq!(p.selected(), 0);
        let ids: Vec<String> = p.results().into_iter().map(|(e, _)| e.id).collect();
        assert_eq!(ids, ["action:toggle-zen"]);
        assert_eq!(
            press(&mut p, key(Key::Enter)),
            PaletteStep::Activate(entry("action:toggle-zen", "Turn Zen Mode on"))
        );
    }

    #[test]
    fn arrows_move_the_selection_within_the_results() {
        let mut p = Palette::new(vec![entry("a", "Alpha"), entry("b", "Beta")], Vec::new());
        press(&mut p, key(Key::Up));
        assert_eq!(p.selected(), 0, "saturates at the top");
        press(&mut p, key(Key::Down));
        press(&mut p, key(Key::Down));
        assert_eq!(p.selected(), 1, "stops on the last result");
        assert_eq!(press(&mut p, key(Key::Enter)), PaletteStep::Activate(entry("b", "Beta")));
    }

    #[test]
    fn backspace_space_and_shifted_letters_edit_the_query() {
        let mut p = Palette::new(Vec::new(), Vec::new());
        press(&mut p, KeyChord::new(Key::Char('Q'), Mods::SHIFT));
        press(&mut p, ch(' '));
        press(&mut p, ch('p'));
        assert_eq!(p.query(), "Q p");
        press(&mut p, key(Key::Backspace));
        assert_eq!(p.query(), "Q ");
        // A Ctrl chord is a command, not text.
        press(&mut p, KeyChord::new(Key::Char('x'), Mods::CONTROL));
        assert_eq!(p.query(), "Q ");
    }

    #[test]
    fn tab_focuses_the_sidebar_and_esc_or_the_chord_closes() {
        let mut p = Palette::new(Vec::new(), Vec::new());
        assert_eq!(press(&mut p, key(Key::Tab)), PaletteStep::FocusSidebar);
        assert_eq!(press(&mut p, key(Key::Esc)), PaletteStep::Close);
        assert_eq!(
            press(&mut p, KeyChord::new(Key::Char('p'), Mods::CONTROL)),
            PaletteStep::Close
        );
    }

    #[test]
    fn right_enters_the_projects_column_and_enter_switches() {
        let mut p = Palette::new(vec![entry("a", "Alpha")], vec![project("web"), project("api")]);
        press(&mut p, key(Key::Right));
        assert_eq!(p.project_selection(), Some(0));
        press(&mut p, key(Key::Down));
        assert_eq!(p.project_selection(), Some(1));
        let PaletteStep::Activate(e) = press(&mut p, key(Key::Enter)) else {
            panic!("enter on a project activates it");
        };
        assert_eq!(e.id, "action:switch-project:api");
        // Down past the last project lands on (and stops at) "Add project".
        press(&mut p, key(Key::Down));
        press(&mut p, key(Key::Down));
        assert_eq!(p.project_selection(), Some(2));
        let PaletteStep::Activate(e) = press(&mut p, key(Key::Enter)) else {
            panic!("enter on the add row activates it");
        };
        assert_eq!(e.id, "action:add-project");
        // Left, or typing, returns focus to the commands.
        press(&mut p, key(Key::Left));
        assert_eq!(p.focus(), PaletteFocus::Commands);
        press(&mut p, key(Key::Right));
        press(&mut p, ch('a'));
        assert_eq!(p.focus(), PaletteFocus::Commands);
    }

    #[test]
    fn tab_can_switch_columns_instead() {
        let mut p =
            Palette::new(vec![entry("a", "Alpha")], vec![project("web")]).tab_switches_columns();
        assert_eq!(press(&mut p, key(Key::Tab)), PaletteStep::Continue);
        assert_eq!(p.project_selection(), Some(0));
        press(&mut p, key(Key::Down));
        assert_eq!(p.project_selection(), Some(1), "down reaches Add project");
        assert_eq!(press(&mut p, key(Key::Tab)), PaletteStep::Continue);
        assert_eq!(p.focus(), PaletteFocus::Commands);
        // Tab again lands on the first project, as Right does.
        press(&mut p, key(Key::Tab));
        assert_eq!(p.project_selection(), Some(0));
        assert_eq!(
            p.footer("⌃P"),
            "↑↓ navigate · Enter activate · Tab projects · Esc / ⌃P close"
        );
        // With no Projects column, Tab still hands focus to the sidebar.
        let mut bare = Palette::new(vec![entry("a", "Alpha")], Vec::new()).tab_switches_columns();
        assert_eq!(press(&mut bare, key(Key::Tab)), PaletteStep::FocusSidebar);
    }

    #[test]
    fn right_without_projects_stays_on_the_commands() {
        let mut p = Palette::new(vec![entry("a", "Alpha")], Vec::new());
        press(&mut p, key(Key::Right));
        assert_eq!(p.focus(), PaletteFocus::Commands);
    }

    #[test]
    fn clicks_activate_results_and_projects() {
        let mut p = Palette::new(vec![entry("a", "Alpha"), entry("b", "Beta")], vec![project("web")]);
        assert_eq!(p.click_item(1), PaletteStep::Activate(entry("b", "Beta")));
        assert_eq!(p.selected(), 1);
        assert_eq!(p.click_item(9), PaletteStep::Continue);
        let PaletteStep::Activate(e) = p.click_project(1) else {
            panic!("the add row activates");
        };
        assert_eq!(e.id, "action:add-project");
    }

    #[test]
    fn a_refresh_keeps_the_query_and_clamps_the_selection() {
        let mut p = Palette::new(vec![entry("a", "Alpha"), entry("b", "Beta")], Vec::new());
        p.set_selected(1);
        p.set_entries(vec![entry("a", "Alpha")]);
        assert_eq!(p.selected_in(p.results().len()), 0);
    }

    fn searchable() -> Palette {
        use crate::overlay::palette_search::{IssueDoc, SearchCorpus};
        use shelbi_core::StatusCategory;
        let issues = (0..5)
            .map(|i| IssueDoc::new(format!("{}", 1930 + i), format!("Login bug {i}"), "", "Backlog", StatusCategory::Backlog))
            .collect();
        Palette::new(
            vec![entry("action:log-out", "Log out of GitHub"), entry("view:tasks", "Issues")],
            vec![project("web")],
        )
        .with_search(PaletteSearch::inline(SearchCorpus::new(issues, vec![])))
    }

    fn type_query(p: &mut Palette, q: &str) {
        for c in q.chars() {
            press(p, ch(c));
        }
    }

    #[test]
    fn searching_groups_results_and_hides_the_projects_column() {
        let mut p = searchable();
        assert!(!p.searching() && p.shows_projects());
        assert_eq!(p.rows().len(), 2, "an empty query lists the commands, as ever");
        type_query(&mut p, "lo");
        assert!(p.searching() && !p.shows_projects());
        use crate::overlay::palette_search::ResultKind;
        let rows = p.rows();
        assert_eq!(rows[0], ResultRow::Header(ResultKind::Command), "commands lead");
        assert_eq!(rows[2], ResultRow::Header(ResultKind::Issue));
        assert_eq!(p.selected_row(), Some(1), "the selection skips the header");
        // Type-and-Enter still runs the command.
        assert_eq!(
            press(&mut p.clone(), key(Key::Enter)),
            PaletteStep::Activate(entry("action:log-out", "Log out of GitHub"))
        );
        press(&mut p, key(Key::Down));
        let counts: Vec<(&str, usize, bool)> = p.chips().iter().map(|c| (c.label, c.count, c.selected)).collect();
        assert_eq!(
            counts,
            [("All", 6, true), ("Issues", 5, false), ("Changelog Entries", 0, false), ("Commands", 1, false)]
        );
        // Enter on an issue opens it.
        let PaletteStep::Activate(e) = press(&mut p, key(Key::Enter)) else {
            panic!("enter on an issue activates it");
        };
        assert_eq!(e.id, "action:open-issue:1930");
        // The capped group's "Show all" row switches to its filter.
        for _ in 0..3 {
            press(&mut p, key(Key::Down));
        }
        assert!(matches!(p.rows()[p.selected_row().unwrap()], ResultRow::Item(ResultItem::ShowAll { .. })));
        assert_eq!(press(&mut p, key(Key::Enter)), PaletteStep::Continue);
        assert_eq!(p.filter(), SearchFilter::Issues);
        assert_eq!(p.rows().len(), 5);
        assert_eq!(p.selected(), 0);
        // Clearing the query leaves the search and its filter behind.
        press(&mut p, key(Key::Backspace));
        press(&mut p, key(Key::Backspace));
        assert_eq!(p.filter(), SearchFilter::All);
        assert!(p.shows_projects());
    }

    #[test]
    fn tab_and_shift_tab_change_the_filter_only_while_searching() {
        let mut p = searchable();
        type_query(&mut p, "lo");
        assert_eq!(press(&mut p, key(Key::Tab)), PaletteStep::Continue);
        assert_eq!(p.filter(), SearchFilter::Issues);
        press(&mut p, KeyChord::new(Key::BackTab, Mods::SHIFT));
        press(&mut p, KeyChord::new(Key::Tab, Mods::SHIFT));
        assert_eq!(p.filter(), SearchFilter::Commands, "Shift-Tab wraps left");
        let ids: Vec<String> = p.rows().iter().filter_map(|r| r.item()?.entry()).map(|e| e.id).collect();
        assert_eq!(ids, ["action:log-out"]);
        // An empty query keeps Tab's own job.
        let mut idle = searchable();
        assert_eq!(press(&mut idle, key(Key::Tab)), PaletteStep::FocusSidebar);
    }

    #[test]
    fn up_from_the_first_result_reaches_the_filter_row() {
        let mut p = searchable();
        type_query(&mut p, "lo");
        press(&mut p, key(Key::Up));
        assert!(p.filter_focused());
        press(&mut p, key(Key::Right));
        press(&mut p, key(Key::Right));
        assert_eq!(p.filter(), SearchFilter::Changelog);
        press(&mut p, key(Key::Left));
        assert_eq!(p.filter(), SearchFilter::Issues);
        // Down (or Enter) hands the keys back to the results.
        press(&mut p, key(Key::Down));
        assert!(!p.filter_focused());
        assert_eq!(p.selected(), 0);
        // ←/→ outside the filter row do nothing while searching.
        press(&mut p, key(Key::Right));
        assert_eq!((p.filter(), p.focus()), (SearchFilter::Issues, PaletteFocus::Commands));
        press(&mut p, key(Key::Up));
        assert_eq!(press(&mut p, key(Key::Enter)), PaletteStep::Continue);
        assert!(!p.filter_focused());
        // Typing takes the keys back too.
        press(&mut p, key(Key::Up));
        press(&mut p, ch('g'));
        assert!(!p.filter_focused());
        assert_eq!(p.footer("⌃P"), "↑↓ navigate · Tab filter · Enter open · Esc / ⌃P close");
    }

    #[test]
    fn clicks_pick_a_chip_or_a_row() {
        let mut p = searchable();
        type_query(&mut p, "lo");
        assert_eq!(p.click_row(0), PaletteStep::Continue, "a header does nothing");
        let PaletteStep::Activate(e) = p.click_row(4) else {
            panic!("a row activates");
        };
        assert_eq!(e.id, "action:open-issue:1931");
        p.click_filter(SearchFilter::Commands);
        assert_eq!(p.filter(), SearchFilter::Commands);
        assert_eq!(p.click_row(0), PaletteStep::Activate(entry("action:log-out", "Log out of GitHub")));
    }

    #[test]
    fn indicators_resolve_with_not_loaded_dominant() {
        assert_eq!(project_indicator(false, true), ProjectIndicator::Unloaded);
        assert_eq!(project_indicator(true, true), ProjectIndicator::Active);
        assert_eq!(project_indicator(true, false), ProjectIndicator::LoadedIdle);
        assert_eq!(ProjectIndicator::Active.glyph(), "●");
        assert_eq!(ProjectIndicator::Unloaded.glyph(), "○");
    }
}
