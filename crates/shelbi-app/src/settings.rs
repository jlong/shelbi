//! Settings: its pages and how they group, the settings sidebar's
//! navigation, and the hooks a page uses to load, save, and guard unsaved
//! edits.
//!
//! Both clients draw Settings in place of their navigation column: a back
//! button, the Global pages, then the project section (the project switcher
//! over the shown project's pages), with the open page in the main area.
//! This module is the toolkit-free half they share:
//!
//! - [`SettingsPage`] and [`SECTIONS`]: every page, its label, its fixed
//!   number (⌘N in the desktop app, the digit key in the TUI), and the
//!   section it sits in.
//! - [`PageSet`]: the pages a client offers. A client registers a page once
//!   it can show it, so neither lists a page it can't open: the desktop app
//!   offers all of them, the TUI starts with General and adds the rest as
//!   their pages land.
//! - [`SettingsNav`]: the page shown and the row the keys moved to. It
//!   outlives a visit, so Settings reopens on the page last used.
//! - [`LeaveGuard`]: the save-or-discard prompt in front of leaving an
//!   editor with unsaved edits, and what to do once the save lands.
//! - [`Job`] and [`Ticket`]: a page's background load or save, keyed by the
//!   project it was for and a generation, so a completion that lands after
//!   the user moved on (another project, a newer request) is dropped.
//! - [`summary`]: what the TUI's General page summarizes.
//! - [`general`]: the General page's appearance and editor preferences, and
//!   where a save of them stands.
//! - [`terminal_preview`]: the Terminal page's canned preview session.
//! - [`workflows`]: the Workflows page's state (the project's workflows and
//!   status catalog, the edits it saves as they're made, and their guards).
//! - [`keybindings`]: the Keybindings page's bindings editor.
//! - [`agents`]: the Agents page's state (its rows, guards, and saves).
//! - [`workspaces`] and [`workspace_form`]: the Workspaces page (the pool,
//!   its machines and their reachability) and its New / Edit / Remove
//!   workspace forms, over the shared pool service.
//! - [`machine_form`]: the Add machine / Configure machine forms' draft,
//!   over the shared machine service, and [`machine_check`]: their
//!   connection check (readiness stages, Install CLI) over the readiness
//!   service.

pub mod agents;
pub mod general;
pub mod keybindings;
pub mod machine_check;
pub mod machine_form;
pub mod summary;
pub mod terminal_preview;
pub mod workflows;
pub mod workspace_form;
pub mod workspaces;

/// A page of Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsPage {
    /// Theme, the Shelbi CLI, and notifications (the desktop app); a summary
    /// of where the settings live (the TUI's first page).
    #[default]
    General,
    /// `keys.yaml`.
    Keybindings,
    /// The terminals' font, size, and Option key.
    Terminal,
    /// The project's workspace pool and the machines that host it.
    Workspaces,
    /// The project's workflows and status catalog.
    Workflows,
    /// The project's agents and their shared instructions.
    Agents,
}

/// Whose settings a page edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// This computer's: the same whichever project is shown.
    Global,
    /// The shown project's: switching project reloads it.
    Project,
}

impl SettingsPage {
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Keybindings => "Keybindings",
            SettingsPage::Terminal => "Terminal",
            SettingsPage::Workspaces => "Workspaces",
            SettingsPage::Workflows => "Workflows",
            SettingsPage::Agents => "Agents",
        }
    }

    /// Its number while Settings is open: ⌘N in the desktop app, the digit
    /// key in the TUI. Fixed per page rather than counted, so a page keeps
    /// its number whichever pages a client offers: the frames number
    /// Workflows 4 and Agents 6. They give Workspaces Terminal's 3 too; it
    /// takes the free 5 instead, so each number opens one page (the sidebar
    /// lists it before Workflows all the same).
    pub fn shortcut(self) -> u8 {
        match self {
            SettingsPage::General => 1,
            SettingsPage::Keybindings => 2,
            SettingsPage::Terminal => 3,
            SettingsPage::Workspaces => 5,
            SettingsPage::Workflows => 4,
            SettingsPage::Agents => 6,
        }
    }

    /// The page numbered `n`, offered or not ([`PageSet::for_shortcut`]
    /// asks a client's set).
    pub fn for_shortcut(n: u8) -> Option<SettingsPage> {
        pages().find(|p| p.shortcut() == n)
    }

    pub fn scope(self) -> Scope {
        match self {
            SettingsPage::General | SettingsPage::Keybindings | SettingsPage::Terminal => {
                Scope::Global
            }
            SettingsPage::Workspaces | SettingsPage::Workflows | SettingsPage::Agents => {
                Scope::Project
            }
        }
    }

    /// The page's bit in a [`PageSet`].
    const fn bit(self) -> u8 {
        match self {
            SettingsPage::General => 1,
            SettingsPage::Keybindings => 1 << 1,
            SettingsPage::Terminal => 1 << 2,
            SettingsPage::Workspaces => 1 << 3,
            SettingsPage::Workflows => 1 << 4,
            SettingsPage::Agents => 1 << 5,
        }
    }
}

/// What heads a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    /// A title.
    Title(&'static str),
    /// The project switcher, after a divider: the section's pages edit the
    /// shown project.
    Project,
}

/// A group of pages under a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    pub header: Header,
    pub pages: &'static [SettingsPage],
}

/// The sidebar's sections, in order.
pub const SECTIONS: &[Section] = &[
    Section {
        header: Header::Title("Global"),
        pages: &[
            SettingsPage::General,
            SettingsPage::Keybindings,
            SettingsPage::Terminal,
        ],
    },
    Section {
        header: Header::Project,
        pages: &[SettingsPage::Workspaces, SettingsPage::Workflows, SettingsPage::Agents],
    },
];

/// Every page, in the order the sidebar draws (and the keys walk) them.
pub fn pages() -> impl Iterator<Item = SettingsPage> {
    SECTIONS.iter().flat_map(|s| s.pages.iter().copied())
}

/// The pages a client offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageSet(u8);

impl PageSet {
    /// Every page (the desktop app).
    pub const ALL: PageSet = PageSet(
        SettingsPage::General.bit()
            | SettingsPage::Keybindings.bit()
            | SettingsPage::Terminal.bit()
            | SettingsPage::Workspaces.bit()
            | SettingsPage::Workflows.bit()
            | SettingsPage::Agents.bit(),
    );

    /// No page.
    pub const NONE: PageSet = PageSet(0);

    /// This set and `page`.
    pub const fn with(self, page: SettingsPage) -> PageSet {
        PageSet(self.0 | page.bit())
    }

    pub fn of(pages: &[SettingsPage]) -> PageSet {
        pages.iter().fold(PageSet::NONE, |set, p| set.with(*p))
    }

    pub fn contains(self, page: SettingsPage) -> bool {
        self.0 & page.bit() != 0
    }

    /// The offered pages, in sidebar order.
    pub fn pages(self) -> impl Iterator<Item = SettingsPage> {
        pages().filter(move |p| self.contains(*p))
    }

    /// The first offered page, where Settings opens the first time.
    pub fn first(self) -> Option<SettingsPage> {
        self.pages().next()
    }

    /// The offered page numbered `n`.
    pub fn for_shortcut(self, n: u8) -> Option<SettingsPage> {
        SettingsPage::for_shortcut(n).filter(|p| self.contains(*p))
    }

    /// The sections a client draws: each with the pages it offers there. A
    /// titled section with none is left out; the project section always
    /// stays, since its switcher picks the project the Settings shown are
    /// for even before a project page is offered.
    pub fn sections(self) -> Vec<ShownSection> {
        SECTIONS
            .iter()
            .map(|s| ShownSection {
                header: s.header,
                pages: s.pages.iter().copied().filter(|p| self.contains(*p)).collect(),
            })
            .filter(|s| s.header == Header::Project || !s.pages.is_empty())
            .collect()
    }
}

/// A section as a client draws it: [`PageSet::sections`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownSection {
    pub header: Header,
    pub pages: Vec<SettingsPage>,
}

/// Where the settings sidebar stands: the page shown, and the row the keys
/// moved the selection to (Enter opens it), over the pages its client
/// offers. The page outlives a visit, so Settings reopens on the page last
/// used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsNav {
    page: SettingsPage,
    cursor: Option<SettingsPage>,
    available: PageSet,
}

impl Default for SettingsNav {
    /// Every page offered (the desktop app).
    fn default() -> Self {
        SettingsNav::new(PageSet::ALL)
    }
}

impl SettingsNav {
    /// A sidebar over `available`, on its first page.
    pub fn new(available: PageSet) -> Self {
        SettingsNav {
            page: available.first().unwrap_or_default(),
            cursor: None,
            available,
        }
    }

    /// The pages this client offers.
    pub fn available(&self) -> PageSet {
        self.available
    }

    /// The page shown.
    pub fn page(&self) -> SettingsPage {
        self.page
    }

    /// The selected row: the keys' row, else the page's.
    pub fn selected(&self) -> SettingsPage {
        self.cursor.unwrap_or(self.page)
    }

    /// Show `page`; the selection goes back to its row. A page the client
    /// doesn't offer is refused (returns false) and nothing moves.
    pub fn show(&mut self, page: SettingsPage) -> bool {
        if !self.available.contains(page) {
            return false;
        }
        self.page = page;
        self.cursor = None;
        true
    }

    /// Settings opened: on `page` when one was asked for (and is offered),
    /// else the last.
    pub fn open(&mut self, page: Option<SettingsPage>) {
        let page = page.filter(|p| self.available.contains(*p)).unwrap_or(self.page);
        self.show(page);
    }

    /// Move the selection a row down or up, stopping at the ends.
    pub fn step(&mut self, down: bool) {
        let all: Vec<SettingsPage> = self.available.pages().collect();
        if all.is_empty() {
            return;
        }
        let at = all.iter().position(|p| *p == self.selected()).unwrap_or(0);
        let next = if down {
            (at + 1).min(all.len() - 1)
        } else {
            at.saturating_sub(1)
        };
        self.cursor = Some(all[next]);
    }

    /// Put the selection on `page`'s row without opening it.
    pub fn select(&mut self, page: SettingsPage) {
        if self.available.contains(page) {
            self.cursor = Some(page);
        }
    }

    /// Open the selected row. Returns whether the page changed.
    pub fn activate(&mut self) -> bool {
        let page = self.selected();
        let changed = page != self.page;
        self.show(page);
        changed
    }
}

/// The prompt in front of leaving an editor that has unsaved edits: Save,
/// Discard or Cancel. `T` is where the editor was going (its Back, another
/// page, another project). The guard only does the bookkeeping; the editor
/// decides what is dirty and runs the save.
///
/// ```text
/// leave(then, dirty) ── clean ──▶ Some(then): go now
///        │ dirty
///        ▼
///   prompt = then ── cancel() ──▶ stay
///        │ discard() ──▶ Some(then): drop the edits, go
///        │ save()    ──▶ save runs ── saved(true)  ──▶ Some(then): go
///        │                          └─ saved(false) ──▶ None: stay, error shows
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveGuard<T> {
    prompt: Option<T>,
    after_save: Option<T>,
}

impl<T> Default for LeaveGuard<T> {
    fn default() -> Self {
        LeaveGuard {
            prompt: None,
            after_save: None,
        }
    }
}

impl<T> LeaveGuard<T> {
    /// Ask to leave for `then`: `Some(then)` to go now, or `None` while the
    /// prompt asks to save or discard the unsaved edits first.
    pub fn leave(&mut self, then: T, dirty: bool) -> Option<T> {
        if dirty {
            self.prompt = Some(then);
            None
        } else {
            Some(then)
        }
    }

    /// Where the open prompt would go.
    pub fn prompt(&self) -> Option<&T> {
        self.prompt.as_ref()
    }

    /// The prompt's Cancel (and Esc): stay, edits kept.
    pub fn cancel(&mut self) {
        self.prompt = None;
    }

    /// The prompt's Discard: where to go now. The editor drops its edits.
    pub fn discard(&mut self) -> Option<T> {
        self.prompt.take()
    }

    /// The prompt's Save: the editor starts its save, and the guard goes
    /// once it lands ([`Self::saved`]).
    pub fn save(&mut self) {
        self.after_save = self.prompt.take();
    }

    /// Whether a save the prompt started is waiting to land.
    pub fn leaving_after_save(&self) -> bool {
        self.after_save.is_some()
    }

    /// The save didn't start (nothing to save after all, or a field refused
    /// its text): stay.
    pub fn forget_save(&mut self) {
        self.after_save = None;
    }

    /// A save landed: where to go next, when the prompt's Save started it
    /// and it worked. A failed save stays (its error shows).
    pub fn saved(&mut self, ok: bool) -> Option<T> {
        let then = self.after_save.take();
        if ok {
            then
        } else {
            None
        }
    }
}

/// One background request a page made: the project it was for (`None` for
/// global settings, or no project open) and its generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    project: Option<String>,
    generation: u64,
}

impl Ticket {
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }
}

/// Where a [`Job`] stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState<T> {
    /// Nothing asked for yet.
    Idle,
    /// Running (the first load, a reload, a save).
    Running,
    /// Done: its result.
    Done(T),
    /// It failed: why, as a sentence the page shows.
    Failed(String),
}

/// A page's background load or save. [`Job::begin`] hands out a [`Ticket`]
/// the work carries off the UI thread; [`Job::complete`] takes the result
/// back only if that ticket is still the latest, so a load that lands after
/// the user switched project (or asked again) never overwrites the newer
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job<T> {
    project: Option<String>,
    generation: u64,
    state: JobState<T>,
}

impl<T> Default for Job<T> {
    fn default() -> Self {
        Job {
            project: None,
            generation: 0,
            state: JobState::Idle,
        }
    }
}

impl<T> Job<T> {
    pub fn state(&self) -> &JobState<T> {
        &self.state
    }

    /// The project the latest request was for.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// The result, once done.
    pub fn done(&self) -> Option<&T> {
        match &self.state {
            JobState::Done(t) => Some(t),
            _ => None,
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.state, JobState::Running)
    }

    /// Whether the page should (re)load for `project`: nothing asked yet,
    /// or the latest request was for another project.
    pub fn stale_for(&self, project: Option<&str>) -> bool {
        matches!(self.state, JobState::Idle) || self.project.as_deref() != project
    }

    /// Start a request for `project`. Any request still in flight is
    /// superseded: its completion will be dropped.
    pub fn begin(&mut self, project: Option<&str>) -> Ticket {
        self.generation += 1;
        self.project = project.map(str::to_string);
        self.state = JobState::Running;
        Ticket {
            project: self.project.clone(),
            generation: self.generation,
        }
    }

    /// Whether `ticket` is the latest request.
    pub fn is_current(&self, ticket: &Ticket) -> bool {
        ticket.generation == self.generation && ticket.project == self.project
    }

    /// A request finished. Returns whether its result was taken: `false` for
    /// a superseded ticket, whose result is dropped.
    pub fn complete(&mut self, ticket: &Ticket, result: Result<T, String>) -> bool {
        if !self.is_current(ticket) || !self.running() {
            return false;
        }
        self.state = match result {
            Ok(t) => JobState::Done(t),
            Err(e) => JobState::Failed(e),
        };
        true
    }

    /// Forget the result and supersede anything in flight (the project the
    /// page is for changed, or it closed).
    pub fn reset(&mut self) {
        self.generation += 1;
        self.project = None;
        self.state = JobState::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pages_are_numbered_in_order() {
        let all: Vec<_> = pages().collect();
        assert_eq!(
            all,
            [
                SettingsPage::General,
                SettingsPage::Keybindings,
                SettingsPage::Terminal,
                SettingsPage::Workspaces,
                SettingsPage::Workflows,
                SettingsPage::Agents
            ]
        );
        assert_eq!(all.iter().map(|p| p.shortcut()).collect::<Vec<_>>(), [1, 2, 3, 5, 4, 6]);
        assert_eq!(SettingsPage::for_shortcut(3), Some(SettingsPage::Terminal));
        assert_eq!(SettingsPage::for_shortcut(4), Some(SettingsPage::Workflows));
        assert_eq!(SettingsPage::for_shortcut(5), Some(SettingsPage::Workspaces));
        assert_eq!(SettingsPage::for_shortcut(6), Some(SettingsPage::Agents));
        assert_eq!(SettingsPage::for_shortcut(7), None);
        assert_eq!(
            SECTIONS.iter().map(|s| (s.header, s.pages.len())).collect::<Vec<_>>(),
            [(Header::Title("Global"), 3), (Header::Project, 3)]
        );
        assert_eq!(SettingsPage::Terminal.scope(), Scope::Global);
        assert_eq!(SettingsPage::Workspaces.scope(), Scope::Project);
        assert_eq!(SettingsPage::Workflows.scope(), Scope::Project);
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.shortcut(), b.shortcut(), "{a:?} and {b:?} share a number");
                assert_ne!(a.label(), b.label());
            }
            assert_eq!(SettingsPage::for_shortcut(a.shortcut()), Some(*a), "its number opens it");
        }
    }

    #[test]
    fn a_page_set_offers_only_its_pages_in_sidebar_order() {
        let tui = PageSet::of(&[SettingsPage::General]);
        assert!(tui.contains(SettingsPage::General));
        assert!(!tui.contains(SettingsPage::Agents));
        assert_eq!(tui.pages().collect::<Vec<_>>(), [SettingsPage::General]);
        assert_eq!(tui.for_shortcut(1), Some(SettingsPage::General));
        assert_eq!(tui.for_shortcut(6), None, "Agents isn't offered");
        assert_eq!(PageSet::ALL.pages().count(), 6);
        assert_eq!(PageSet::NONE.first(), None);
        // Reversed input still walks in sidebar order.
        let set = PageSet::of(&[SettingsPage::Agents, SettingsPage::General]);
        assert_eq!(
            set.pages().collect::<Vec<_>>(),
            [SettingsPage::General, SettingsPage::Agents]
        );
    }

    #[test]
    fn the_shown_sections_keep_the_project_switcher_without_project_pages() {
        let tui = PageSet::of(&[SettingsPage::General]).sections();
        assert_eq!(
            tui,
            [
                ShownSection {
                    header: Header::Title("Global"),
                    pages: vec![SettingsPage::General]
                },
                ShownSection {
                    header: Header::Project,
                    pages: vec![]
                },
            ]
        );
        // A titled section with nothing offered is dropped.
        let project_only = PageSet::of(&[SettingsPage::Workflows]).sections();
        assert_eq!(project_only.len(), 1);
        assert_eq!(project_only[0].header, Header::Project);
        assert_eq!(PageSet::ALL.sections().len(), 2);
    }

    #[test]
    fn the_keys_move_the_selection_and_enter_opens_it() {
        let mut nav = SettingsNav::default();
        assert_eq!((nav.page(), nav.selected()), (SettingsPage::General, SettingsPage::General));
        nav.step(false);
        assert_eq!(nav.selected(), SettingsPage::General, "stops at the top");
        for _ in 0..6 {
            nav.step(true);
        }
        assert_eq!(
            nav.selected(),
            SettingsPage::Agents,
            "crosses into the project section and stops at the bottom"
        );
        assert_eq!(nav.page(), SettingsPage::General, "moving doesn't open");
        assert!(nav.activate());
        assert_eq!((nav.page(), nav.selected()), (SettingsPage::Agents, SettingsPage::Agents));
        assert!(!nav.activate(), "already shown");
    }

    #[test]
    fn the_keys_walk_only_the_offered_pages() {
        let mut nav = SettingsNav::new(PageSet::of(&[SettingsPage::General, SettingsPage::Agents]));
        nav.step(true);
        assert_eq!(nav.selected(), SettingsPage::Agents, "skips what isn't offered");
        nav.step(true);
        assert_eq!(nav.selected(), SettingsPage::Agents);
        assert!(!nav.show(SettingsPage::Terminal), "an unoffered page is refused");
        assert_eq!(nav.page(), SettingsPage::General);
        nav.select(SettingsPage::Terminal);
        assert_eq!(nav.selected(), SettingsPage::Agents, "nor can it be selected");
        let mut none = SettingsNav::new(PageSet::NONE);
        none.step(true);
        assert_eq!(none.selected(), SettingsPage::General, "an empty set doesn't panic");
    }

    #[test]
    fn settings_reopens_on_the_last_page_unless_one_is_asked_for() {
        let mut nav = SettingsNav::default();
        nav.show(SettingsPage::Keybindings);
        nav.step(true);
        nav.open(None);
        assert_eq!(
            (nav.page(), nav.selected()),
            (SettingsPage::Keybindings, SettingsPage::Keybindings)
        );
        nav.open(Some(SettingsPage::Terminal));
        assert_eq!(nav.page(), SettingsPage::Terminal);
        let mut tui = SettingsNav::new(PageSet::of(&[SettingsPage::General]));
        tui.open(Some(SettingsPage::Agents));
        assert_eq!(tui.page(), SettingsPage::General, "an unoffered page opens the last one");
    }

    #[test]
    fn a_clean_editor_leaves_at_once() {
        let mut guard = LeaveGuard::default();
        assert_eq!(guard.leave("back", false), Some("back"));
        assert_eq!(guard.prompt(), None);
    }

    #[test]
    fn a_dirty_editor_asks_then_cancel_stays_and_discard_goes() {
        let mut guard = LeaveGuard::default();
        assert_eq!(guard.leave("back", true), None);
        assert_eq!(guard.prompt(), Some(&"back"));
        guard.cancel();
        assert_eq!(guard.prompt(), None, "cancel stays");
        guard.leave("page", true);
        assert_eq!(guard.discard(), Some("page"));
        assert_eq!(guard.prompt(), None);
    }

    #[test]
    fn save_goes_once_the_save_lands_and_a_failed_save_stays() {
        let mut guard = LeaveGuard::default();
        guard.leave("project", true);
        guard.save();
        assert!(guard.prompt().is_none() && guard.leaving_after_save());
        assert_eq!(guard.saved(true), Some("project"));
        assert!(!guard.leaving_after_save());

        guard.leave("back", true);
        guard.save();
        assert_eq!(guard.saved(false), None, "a failed save stays");
        assert_eq!(guard.saved(true), None, "and forgets where it was going");

        guard.leave("back", true);
        guard.save();
        guard.forget_save();
        assert_eq!(guard.saved(true), None, "a save that never started goes nowhere");
        // A save the prompt didn't start (the editor's own Save button).
        assert_eq!(guard.saved(true), None);
    }

    #[test]
    fn a_job_takes_only_its_latest_completion() {
        let mut job: Job<u32> = Job::default();
        assert!(job.stale_for(Some("alpha")), "nothing loaded yet");
        let alpha = job.begin(Some("alpha"));
        assert!(job.running());
        assert_eq!(alpha.project(), Some("alpha"));
        // The user switches project before alpha's load lands.
        let beta = job.begin(Some("beta"));
        assert!(!job.complete(&alpha, Ok(1)), "alpha's late load is dropped");
        assert!(job.running(), "still waiting on beta");
        assert!(job.complete(&beta, Ok(2)));
        assert_eq!(job.done(), Some(&2));
        assert!(!job.stale_for(Some("beta")));
        assert!(job.stale_for(Some("alpha")));
        assert!(!job.complete(&beta, Ok(3)), "a ticket completes once");
        assert_eq!(job.done(), Some(&2));
    }

    #[test]
    fn a_failed_job_keeps_its_reason_and_reset_drops_what_is_in_flight() {
        let mut job: Job<u32> = Job::default();
        let t = job.begin(None);
        assert!(job.complete(&t, Err("unreadable".into())));
        assert_eq!(job.state(), &JobState::Failed("unreadable".into()));
        let t = job.begin(Some("alpha"));
        job.reset();
        assert_eq!(job.state(), &JobState::Idle);
        assert!(!job.complete(&t, Ok(1)), "reset supersedes the load in flight");
        assert_eq!(job.state(), &JobState::Idle);
    }
}
