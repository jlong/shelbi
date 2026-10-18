//! The Activity feed's toolkit-free state: what `activity.rs` draws and what
//! its controls change, kept apart from gpui so it can be tested directly.
//!
//! It holds the latest read ([`ActivityData`], from the shared activity
//! refresher), the filter, the paging window, and a keyboard selection, and
//! rebuilds the shared [`Feed`] (`shelbi_app::activity::build_feed`) whenever
//! any of them change. Keys come from the user's `activity:` key map, as in the
//! TUI; a feed row is opened with Enter or a click.

use chrono::{DateTime, Utc};
use shelbi_app::activity::{
    build_feed, filter_choices, ActivityData, ActivityFilter, ActivityKind, Feed, FeedItem,
    FeedRow, RowTarget, ACTIVITY_PAGE,
};
use shelbi_state::keymap::{ActivityAction, Key, KeyChord, Keymaps};

/// Rows a page-up / page-down moves the selection.
const PAGE_ROWS: usize = 10;

/// Which filter picker is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Project,
    Workspace,
}

/// What the view should do after a key or a click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Open a row's task (task detail) or workspace.
    Open(RowTarget),
    /// Re-read the log now.
    Refresh,
    /// Re-read with a window of this many lines (load older).
    LoadWindow(usize),
    /// Bring feed item `ix` into view.
    Reveal(usize),
}

pub struct ActivityState {
    pub project: String,
    pub filter: ActivityFilter,
    data: ActivityData,
    /// Whether a read has landed yet.
    loaded: bool,
    feed: Feed,
    projects: Vec<String>,
    workspaces: Vec<String>,
    /// The line window asked of the refresher; a read with a smaller window
    /// means "load older" is still in flight.
    window: usize,
    /// The selected feed item (always a row, never a day header).
    pub selected: Option<usize>,
    pub picker: Option<Picker>,
    keymaps: Keymaps,
}

impl ActivityState {
    pub fn new(project: impl Into<String>, keymaps: Keymaps) -> Self {
        ActivityState {
            project: project.into(),
            filter: ActivityFilter::default(),
            data: ActivityData::default(),
            loaded: false,
            feed: Feed::default(),
            projects: Vec::new(),
            workspaces: Vec::new(),
            window: ACTIVITY_PAGE,
            selected: None,
            picker: None,
            keymaps,
        }
    }

    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// The window the refresher should read with.
    pub fn window(&self) -> usize {
        self.window
    }

    /// Fold in a fresh read and rebuild the feed.
    pub fn apply(&mut self, data: ActivityData, now: DateTime<Utc>) {
        self.data = data;
        self.loaded = true;
        self.rebuild(now);
    }

    /// Rebuild the feed from the latest read: on a new read, a filter change,
    /// and on the clock (the relative times).
    pub fn rebuild(&mut self, now: DateTime<Utc>) {
        let selected_row = self.selected_row().cloned();
        self.feed = build_feed(&self.data.events, &self.data.task_cache, &self.filter, now);
        let (projects, workspaces) = filter_choices(&self.data.events, &self.data.task_cache);
        self.projects = projects;
        self.workspaces = workspaces;
        // Keep the selection on the same row as the feed grows above it; drop
        // it when that row is gone.
        self.selected = selected_row.and_then(|old| {
            self.feed.items.iter().position(|item| match item {
                FeedItem::Row(r) => {
                    r.title == old.title
                        && r.verb == old.verb
                        && r.chip == old.chip
                        && r.target == old.target
                }
                FeedItem::Day(_) => false,
            })
        });
    }

    pub fn feed(&self) -> &Feed {
        &self.feed
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The read's error, if it failed.
    pub fn status(&self) -> Option<&str> {
        self.data.status.as_deref()
    }

    /// Whether the log holds older events than the ones loaded.
    pub fn has_older(&self) -> bool {
        self.data.has_older
    }

    /// Whether a "load older" read is still in flight.
    pub fn is_loading_older(&self) -> bool {
        self.loaded && self.data.window.is_some_and(|w| w < self.window)
    }

    /// How many log lines are loaded.
    pub fn loaded_events(&self) -> usize {
        self.data.events.len()
    }

    /// Ask for one more page of older lines. `None` when there is nothing
    /// older or a page is already on its way.
    pub fn load_older(&mut self) -> Option<Intent> {
        if !self.has_older() || self.is_loading_older() {
            return None;
        }
        self.window += ACTIVITY_PAGE;
        Some(Intent::LoadWindow(self.window))
    }

    pub fn projects(&self) -> &[String] {
        &self.projects
    }

    pub fn workspaces(&self) -> &[String] {
        &self.workspaces
    }

    // ---- filters ----------------------------------------------------------

    pub fn toggle_kind(&mut self, kind: ActivityKind, now: DateTime<Utc>) {
        self.filter.toggle_kind(kind);
        self.filter_changed(now);
    }

    /// Back to every kind (the `All` pill).
    pub fn clear_kinds(&mut self, now: DateTime<Utc>) {
        self.filter.clear_kinds();
        self.filter_changed(now);
    }

    pub fn set_project(&mut self, project: Option<String>, now: DateTime<Utc>) {
        self.filter.project = project;
        self.picker = None;
        self.filter_changed(now);
    }

    pub fn set_workspace(&mut self, workspace: Option<String>, now: DateTime<Utc>) {
        self.filter.workspace = workspace;
        self.picker = None;
        self.filter_changed(now);
    }

    pub fn toggle_picker(&mut self, picker: Picker) {
        self.picker = if self.picker == Some(picker) {
            None
        } else {
            Some(picker)
        };
    }

    fn filter_changed(&mut self, now: DateTime<Utc>) {
        // Like the TUI's pills: a filter change snaps back to the newest.
        self.selected = None;
        self.rebuild(now);
    }

    // ---- selection --------------------------------------------------------

    fn row_indices(&self) -> Vec<usize> {
        self.feed
            .items
            .iter()
            .enumerate()
            .filter_map(|(ix, item)| matches!(item, FeedItem::Row(_)).then_some(ix))
            .collect()
    }

    pub fn selected_row(&self) -> Option<&FeedRow> {
        match self.feed.items.get(self.selected?)? {
            FeedItem::Row(r) => Some(r),
            FeedItem::Day(_) => None,
        }
    }

    /// Move the selection `delta` rows (down is positive), clamped; the first
    /// move selects the newest row.
    fn step(&mut self, delta: isize) -> Vec<Intent> {
        let rows = self.row_indices();
        if rows.is_empty() {
            return Vec::new();
        }
        let pos = match self
            .selected
            .and_then(|s| rows.iter().position(|&r| r == s))
        {
            Some(p) => (p as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(rows[pos]);
        vec![Intent::Reveal(rows[pos])]
    }

    fn select_edge(&mut self, last: bool) -> Vec<Intent> {
        let rows = self.row_indices();
        let pick = if last { rows.last() } else { rows.first() };
        match pick {
            Some(&ix) => {
                self.selected = Some(ix);
                vec![Intent::Reveal(ix)]
            }
            None => Vec::new(),
        }
    }

    /// Select feed item `ix` without opening it (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn select_item(&mut self, ix: usize) {
        if matches!(self.feed.items.get(ix), Some(FeedItem::Row(_))) {
            self.selected = Some(ix);
        }
    }

    /// Select feed item `ix` (a click) and open it.
    pub fn click(&mut self, ix: usize) -> Vec<Intent> {
        if !matches!(self.feed.items.get(ix), Some(FeedItem::Row(_))) {
            return Vec::new();
        }
        self.selected = Some(ix);
        self.open_selected()
    }

    /// Open the selected row's task or workspace, if it has one.
    pub fn open_selected(&mut self) -> Vec<Intent> {
        self.selected_row()
            .and_then(|r| r.target.clone())
            .map(Intent::Open)
            .into_iter()
            .collect()
    }

    /// A key on the focused feed. `Some` when the feed handled it (the view
    /// stops propagation), `None` to let the window's global keys run.
    pub fn handle_key(&mut self, chord: KeyChord, now: DateTime<Utc>) -> Option<Vec<Intent>> {
        if self.picker.is_some() && chord.code == Key::Esc {
            self.picker = None;
            return Some(Vec::new());
        }
        if chord.code == Key::Enter && chord.mods.is_empty() {
            return Some(self.open_selected());
        }
        let action = self.keymaps.activity.dispatch(chord)?;
        Some(match action {
            ActivityAction::ScrollDown => self.step(1),
            ActivityAction::ScrollUp => self.step(-1),
            ActivityAction::PageDown => self.step(PAGE_ROWS as isize),
            ActivityAction::PageUp => self.step(-(PAGE_ROWS as isize)),
            ActivityAction::ScrollHome => self.select_edge(false),
            ActivityAction::ScrollEnd => self.select_edge(true),
            ActivityAction::Refresh => vec![Intent::Refresh],
            ActivityAction::ResetFilter => {
                self.clear_kinds(now);
                Vec::new()
            }
            ActivityAction::ToggleZenFilter => {
                self.toggle_kind(ActivityKind::Zen, now);
                Vec::new()
            }
            ActivityAction::ToggleWorkspacesFilter => {
                self.toggle_kind(ActivityKind::Workspaces, now);
                Vec::new()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use shelbi_app::activity::{parse_event_line, Event, EMPTY_FILTER};
    use shelbi_state::keymap::validate_keymaps_yaml;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 22, 15, 0, 0).unwrap()
    }

    fn events() -> Vec<Event> {
        [
            "2026-07-22T14:00:00+00:00 project=demo task=t1 workflow=default todo -> in_progress reason=user:cli",
            "2026-07-22T14:01:00+00:00 project=demo workspace=bravo working -> awaiting_input",
            "2026-07-22T14:02:00+00:00 project=web task=w9 workflow=default backlog -> todo reason=user:board",
            "2026-07-22T14:03:00+00:00 project=demo task=t1 workflow=default in_progress -> review reason=orchestrator:zen-merge_pr=7",
        ]
        .iter()
        .map(|l| parse_event_line(l))
        .collect()
    }

    fn data(events: Vec<Event>, has_older: bool, window: usize) -> ActivityData {
        ActivityData {
            events,
            has_older,
            window: Some(window),
            ..ActivityData::default()
        }
    }

    fn state() -> ActivityState {
        let keymaps = validate_keymaps_yaml("{}", None).0;
        let mut s = ActivityState::new("demo", keymaps);
        s.apply(data(events(), true, ACTIVITY_PAGE), now());
        s
    }

    fn chord(spec: &str) -> KeyChord {
        KeyChord::parse(spec).expect("chord")
    }

    fn verbs(s: &ActivityState) -> Vec<String> {
        s.feed().rows().map(|r| r.verb.clone()).collect()
    }

    #[test]
    fn renders_the_shared_feed_newest_first() {
        let s = state();
        assert!(s.is_loaded());
        assert_eq!(
            verbs(&s),
            ["merged", "promoted", "awaiting input", "is building"]
        );
        assert!(matches!(s.feed().items[0], FeedItem::Day(_)));
    }

    #[test]
    fn filters_apply_and_clear() {
        let mut s = state();
        s.toggle_kind(ActivityKind::Zen, now());
        assert_eq!(verbs(&s), ["merged"]);
        s.toggle_kind(ActivityKind::Workspaces, now());
        assert_eq!(verbs(&s), ["merged", "awaiting input"]);
        s.clear_kinds(now());
        s.set_project(Some("web".into()), now());
        assert_eq!(verbs(&s), ["promoted"]);
        assert_eq!(s.projects(), ["demo".to_string(), "web".to_string()]);
        s.set_project(None, now());
        s.set_workspace(Some("bravo".into()), now());
        assert_eq!(verbs(&s), ["awaiting input"]);
        s.set_workspace(Some("nobody".into()), now());
        assert_eq!(s.feed().empty, Some(EMPTY_FILTER));
    }

    #[test]
    fn the_tui_filter_keys_work_here() {
        let mut s = state();
        // The built-in `activity:` keys: z / w toggle the pills, a resets.
        assert!(s.handle_key(chord("z"), now()).is_some());
        assert_eq!(s.filter.kinds, vec![ActivityKind::Zen]);
        s.handle_key(chord("w"), now());
        assert_eq!(
            s.filter.kinds,
            vec![ActivityKind::Zen, ActivityKind::Workspaces]
        );
        s.handle_key(chord("a"), now());
        assert!(s.filter.is_all());
        assert_eq!(s.handle_key(chord("r"), now()), Some(vec![Intent::Refresh]));
    }

    #[test]
    fn paging_asks_for_one_more_page_and_waits_for_it() {
        let mut s = state();
        assert!(s.has_older());
        assert_eq!(s.load_older(), Some(Intent::LoadWindow(ACTIVITY_PAGE * 2)));
        assert!(s.is_loading_older());
        assert_eq!(s.load_older(), None, "one page at a time");
        // The bigger read lands: older events join the feed below.
        let mut more = vec![parse_event_line(
            "2026-07-22T13:00:00+00:00 project=demo workspace=alpha working -> paused",
        )];
        more.extend(events());
        s.apply(data(more, false, ACTIVITY_PAGE * 2), now());
        assert!(!s.is_loading_older());
        assert!(!s.has_older());
        assert_eq!(verbs(&s).last().map(String::as_str), Some("paused"));
        assert_eq!(s.load_older(), None, "nothing older left");
        assert_eq!(s.window(), ACTIVITY_PAGE * 2);
    }

    #[test]
    fn keys_move_the_selection_and_enter_opens_its_target() {
        let mut s = state();
        assert_eq!(
            s.handle_key(chord("j"), now()),
            Some(vec![Intent::Reveal(1)])
        );
        assert_eq!(
            s.handle_key(chord("enter"), now()),
            Some(vec![Intent::Open(RowTarget::Task("t1".into()))])
        );
        s.handle_key(chord("j"), now());
        s.handle_key(chord("j"), now());
        assert_eq!(
            s.open_selected(),
            vec![Intent::Open(RowTarget::Workspace("bravo".into()))]
        );
        // End selects the oldest row, Home the newest.
        s.handle_key(chord("G"), now());
        assert_eq!(
            s.selected_row().map(|r| r.verb.as_str()),
            Some("is building")
        );
        s.handle_key(chord("g"), now());
        assert_eq!(s.selected_row().map(|r| r.verb.as_str()), Some("merged"));
    }

    #[test]
    fn clicking_a_row_opens_it_and_headers_do_nothing() {
        let mut s = state();
        assert!(s.click(0).is_empty(), "the day header");
        assert_eq!(s.click(2), vec![Intent::Open(RowTarget::Task("w9".into()))]);
        assert_eq!(s.selected, Some(2));
    }

    #[test]
    fn the_selection_follows_its_row_when_new_events_arrive() {
        let mut s = state();
        s.click(3); // awaiting input
        let mut grown = events();
        grown.push(parse_event_line(
            "2026-07-22T14:30:00+00:00 project=demo workspace=charlie working -> blocked",
        ));
        s.apply(data(grown, true, ACTIVITY_PAGE), now());
        assert_eq!(
            s.selected_row().map(|r| r.verb.as_str()),
            Some("awaiting input")
        );
        assert_eq!(s.selected, Some(4));
    }
}
