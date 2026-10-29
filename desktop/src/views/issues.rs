//! The Issues view: the kanban board, rendered from `shelbi_app::IssuesModel`.
//!
//! The view is a thin gpui shell over [`BoardState`] (`board_state.rs`): it
//! feeds the state from two background refreshers (the open board from the
//! daemon's index, and the done/canceled history on its own refresher so it
//! fills in after the open board without blocking it), turns keystrokes into
//! chords for the state's key map, draws the model, and carries out the
//! state's [`Intent`]s. Issue mutations never run on the UI thread: each goes
//! out on its own thread through `execute_mutation_expecting`, carrying the
//! state the card was read at, and its answer comes back over a channel the
//! view polls.
//!
//! Each expanded column's cards are a virtualized list (`board_list.rs`): only
//! the cards in view are built, so a board of a thousand done issues draws
//! as fast as a small one. A background read that finds the board unchanged
//! isn't folded in (the model isn't rebuilt every refresh).
//!
//! Commands leave the view as [`IssuesEvent::Command`] so the window runs them
//! through the one exhaustive dispatch (`crate::commands`), which hands the
//! issue mutations back to [`IssuesView::run_issue_command`].
//!
//! Drag and drop: a card body is a drag source ([`CardDrag`], drawn by a
//! [`CardGhost`] under the pointer); every column is a drop target, and the
//! cards of the dragged card's own column are reorder targets. Legality comes
//! from the state ([`BoardState::drop_target`]): a refused column is dimmed
//! with its reason and never highlights under the pointer; a drop on it sends
//! nothing and puts the reason in the status line. A legal drop becomes the
//! same registry command the keys send, so it goes through the window's
//! dispatch and the daemon like any other move.
//!
//! A card opens in the floating task pane (`task_detail.rs`), over the
//! board's right side: not modal, so the board keeps its layout and stays
//! usable beside it, and a click on another card shows that one. The pane
//! can also open over another screen ([`IssuesView::open_task_over`], the
//! Review and Workspace panels' More, `dt-more-opens-detail`): the view then
//! draws only the pane and what it opens, as a layer the window puts over
//! its main area, and keeps its own keys, editor and commands. The board
//! itself isn't drawn until the window shows it.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, relative, AnyElement, AnyWindowHandle, App, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, ScrollHandle, SharedString,
    Stateful, Subscription, Task, Window,
};
use shelbi_app::board::DropTarget;
use gpui_kit::component::input::{Enter, Escape, Input, InputEvent, InputState, TextareaState};
use shelbi_app::board::{self, BoardRead, ClosedPage};
use shelbi_app::board_search::SearchWorker;
use shelbi_app::{
    CommandKind, ExecError, ExpectedState, IssueCard, IssueColumn, TaskDetailModel,
};
use shelbi_app::view::{ReviewRow, WorkspaceRow};
use shelbi_state::keymap::{DisplayStyle, KanbanAction, Keymaps};

use super::board_list::{self, ColumnList, Item};
use super::board_scroll::{self, BoardScroll};
use super::board_state::{BoardState, Intent, Mode, MutationJob, Property, Scroll};
use shelbi_app::issue_edit::{Field, SaveJob, Splice};
use super::external_edit::{self, EditorEvent};
use super::pane_session::{MainShows, PaneSession};
use super::session_view::{SessionDeps, SessionView};
use super::task_detail;
use crate::assets::{heroicon, icons};
use crate::elements::rotated_text::rotated_text;
use crate::keys::chord_from_keystroke;
use crate::refresh::SessionSink;
use crate::theme::style;
use crate::theme::Theme;
use crate::theme::style::{Type, Typography};

/// The fallback refresh cadence (the TUI's).
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
/// How often the view drains its channels.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// An expanded column's width.
const COLUMN_WIDTH: f32 = 272.0;
/// How far a card's title runs past its content box into the right padding.
const TITLE_OVERHANG: f32 = 6.0;
/// How many lines a card's excerpt keeps (the Figma card's description).
const SUMMARY_LINES: usize = 4;
/// The excerpt's width: Figma's description box is 244px, 4px short of the
/// card's content.
const SUMMARY_WIDTH: f32 = 244.0;
/// A card's corner radius (the Figma card's: 12px, rounder than a button's
/// 6px).
const CARD_RADIUS: f32 = 12.0;
/// A collapsed column's width (Figma's turned column heading: 24pt).
const COLLAPSED_WIDTH: f32 = 24.0;
/// The toolbar's height (Figma: the 52px Header, no rule under it; its band
/// sets it off from the board).
const TOOLBAR_H: f32 = 52.0;
/// The toolbar's search field's width (Figma's Searchbox in the Header).
const SEARCH_W: f32 = 180.0;
/// The key-hint footer's height, its rule included (Figma: a 1px divider
/// over a 28px row).
const FOOTER_H: f32 = 29.0;
/// How far a column's scroll clip reaches past the column on every side, so
/// the selected card's focus glow (4px outside it) isn't cut off while the
/// cards keep the column's full width.
const GLOW_ROOM: f32 = 6.0;

/// What the view asks of the window.
#[derive(Debug, Clone)]
pub enum IssuesEvent {
    /// Run this registry command through `commands::dispatch_command`.
    Command(CommandKind),
    /// The pane's session terminal took keyboard focus: the window's focus
    /// model moves to the main area.
    Focused,
}

/// The value a card drag carries.
#[derive(Debug, Clone)]
pub struct CardDrag {
    pub id: String,
}

/// The card that follows the pointer during a drag.
pub struct CardGhost {
    title: SharedString,
    id: SharedString,
}

impl Render for CardGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        div()
            .w(px(COLUMN_WIDTH))
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded(px(CARD_RADIUS))
            .bg(theme.card)
            .shadow(style::popover_shadow(&theme))
            .opacity(0.95)
            .type_style(Type::Base)
            .child(
                div()
                    .type_style(Type::BaseMedium)
                    .text_color(theme.text_loud)
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(self.title.clone()),
            )
            .child(div().type_style(Type::XSmall).text_color(theme.muted).child(self.id.clone()))
    }
}

/// A background result.
enum Done {
    Mutation {
        id: String,
        /// The ack carries the issue's post-change state, when reported.
        result: Result<Option<ExpectedState>, ExecError>,
    },
    /// A save finished (its answer is already in the shared draft store).
    /// A test's saves are recorded, not run.
    #[cfg_attr(test, allow(dead_code))]
    Saved { id: String },
    More(Result<ClosedPage, String>),
    Persisted(Result<(), String>),
}

pub struct IssuesView {
    /// The filter controls, measured so their menus open under them.
    workflow_anchor: style::Anchor,
    workspace_anchor: style::Anchor,
    /// The task pane's More button, measured so its menu opens under it.
    pub(super) pane_menu_anchor: style::Anchor,
    /// The open property picker's value, measured so the picker opens
    /// under it (one picker is open at a time), and the picker's scroll.
    pub(super) property_anchor: style::Anchor,
    pub(super) picker_scroll: ScrollHandle,
    pub(super) state: BoardState,
    pub(super) focus: FocusHandle,
    needs_focus: bool,
    open_refresher: shelbi_app::Refresher<(BoardRead, u64)>,
    closed_refresher: shelbi_app::Refresher<(Option<ClosedPage>, u64)>,
    /// The fingerprints of the last reads folded in: a refresh that reads
    /// the same board again (most of them) is not folded, which would
    /// rebuild the whole model on the UI thread every 750 ms.
    open_print: Option<u64>,
    closed_print: Option<u64>,
    /// The open detail's model, by issue id and the board generation it was
    /// built at (building it merges every task on the board).
    detail_cache: Option<(String, u64, Option<TaskDetailModel>)>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    editor_tx: Sender<EditorEvent>,
    editor_rx: Receiver<EditorEvent>,
    last_refresh: Instant,
    load_more_in_flight: bool,
    board_scroll: BoardScroll,
    /// Each expanded column's virtualized card list, by status id (a column
    /// keeps its scroll position across a filter change or a collapse).
    column_lists: HashMap<String, ColumnList>,
    /// The column lists a search shows, kept apart so the board's own lists
    /// (and their scroll) are where the user left them when it's cleared.
    /// Emptied when a new search starts, so its results start at the top.
    search_lists: HashMap<String, ColumnList>,
    pub(super) detail_scroll: ScrollHandle,
    /// The in-place body editor: the issue id and its text state. It edits
    /// the issue's draft (`issue_edit`), which saves itself.
    pub(super) editor: Option<(String, Entity<TextareaState>)>,
    _editor_subscriptions: Vec<Subscription>,
    /// The draft store changed the editor's text (a read merged into it, a
    /// conflict resolved, the external editor): put it in at the next render
    /// (that needs the window).
    sync_editor: bool,
    /// The saves a test's view sent (a test never reaches the daemon).
    #[cfg(test)]
    sent_saves: Vec<SaveJob>,
    add_input: Option<Entity<InputState>>,
    /// The toolbar's search field, made on first draw (an input needs the
    /// window).
    search_input: Option<Entity<InputState>>,
    /// Runs the board's GitHub searches off the UI thread, started on the
    /// first one (a local board never starts it).
    search_worker: Option<SearchWorker>,
    display_style: DisplayStyle,
    /// Drawn over another screen: the detail alone, no board.
    floating: bool,
    /// The pane's live session, once the window hosts one
    /// ([`Self::host_sessions`]; offscreen boards have none).
    pub(super) pane_session: Option<PaneSession>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl EventEmitter<IssuesEvent> for IssuesView {}

impl IssuesView {
    pub fn new(project: String, cx: &mut Context<Self>) -> Self {
        let (keymaps, diagnostics) = shelbi_state::keymap::load_keymaps(Some(&project));
        for d in diagnostics {
            tracing::warn!("keys.yaml: {d:?}");
        }
        let p = project.clone();
        let open_refresher = shelbi_app::spawn_worker(move |_| {
            let read = board::read_open_board(&p);
            let print = fingerprint(&read);
            (read, print)
        });
        let p = project.clone();
        let closed_refresher = shelbi_app::spawn_worker(move |_| {
            let page = board::read_closed_page(&p);
            let print = fingerprint(&page);
            (page, print)
        });
        open_refresher.request();
        closed_refresher.request();
        let (done_tx, done_rx) = mpsc::channel();
        let (editor_tx, editor_rx) = mpsc::channel();
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                break;
            }
        });
        IssuesView {
            state: BoardState::new(project, keymaps),
            focus: cx.focus_handle(),
            needs_focus: true,
            open_refresher,
            closed_refresher,
            open_print: None,
            closed_print: None,
            detail_cache: None,
            done_tx,
            done_rx,
            editor_tx,
            editor_rx,
            last_refresh: Instant::now(),
            load_more_in_flight: false,
            board_scroll: BoardScroll::default(),
            column_lists: HashMap::new(),
            search_lists: HashMap::new(),
            workflow_anchor: Default::default(),
            workspace_anchor: Default::default(),
            pane_menu_anchor: Default::default(),
            property_anchor: Default::default(),
            picker_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            editor: None,
            _editor_subscriptions: Vec::new(),
            sync_editor: false,
            #[cfg(test)]
            sent_saves: Vec::new(),
            add_input: None,
            search_input: None,
            search_worker: None,
            display_style: DisplayStyle::detect(),
            floating: false,
            pane_session: None,
            _subscriptions: Vec::new(),
            _poll: poll,
        }
    }

    /// Give the pane a live session for the task it shows, in `window`, on
    /// `deps` (whose relauncher [`PaneSession`] replaces: the pane never
    /// launches anything).
    pub fn host_sessions(&mut self, window: AnyWindowHandle, deps: SessionDeps, sink: Option<SessionSink>, cx: &mut Context<Self>) {
        let project = self.state.project.clone();
        self.pane_session = Some(PaneSession::new(window, &project, deps, sink, cx));
    }

    /// The sidebar's refreshed workspace and review rows: which session works
    /// the pane's task.
    pub fn set_task_rows(&mut self, workspaces: Vec<WorkspaceRow>, reviews: Vec<ReviewRow>, cx: &mut Context<Self>) {
        let changed = self.pane_session.as_mut().is_some_and(|p| p.set_rows(workspaces, reviews));
        if changed {
            self.sync_session(true, cx);
            cx.notify();
        }
    }

    /// Whether the window shows the board, or this view floating over
    /// another screen, and what that screen shows. A hidden board's pane
    /// keeps its task but detaches its session.
    pub fn set_shown(&mut self, shown: bool, main_shows: MainShows, cx: &mut Context<Self>) {
        let changed = self.pane_session.as_mut().is_some_and(|p| p.set_shown(shown, main_shows));
        if changed {
            self.sync_session(false, cx);
            cx.notify();
        }
    }

    /// The project is being quit (`on`), so the pane session's exit is not
    /// restarted, or the quit failed (`!on`).
    pub fn note_deliberate_close(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(p) = &self.pane_session {
            p.view().update(cx, |v, _| if on { v.note_deliberate_close() } else { v.clear_deliberate_close() });
        }
    }

    /// Whether the pane's session terminal holds keyboard focus.
    pub fn session_focused(&self, window: &Window, cx: &App) -> bool {
        self.pane_session.as_ref().is_some_and(|p| p.contains_focus(window, cx))
    }

    /// The pane's session view, while it shows one.
    pub fn pane_session_view(&self) -> Option<&Entity<SessionView>> {
        self.pane_session.as_ref().filter(|p| p.attached().is_some()).map(PaneSession::view)
    }

    /// Attach the pane's session to what its task needs now.
    fn sync_session(&mut self, rows_changed: bool, cx: &mut Context<Self>) {
        if self.pane_session.is_none() {
            return;
        }
        // Only a task the board has: a deleted or missing one shows none.
        let task = self.detail_model().map(|d| d.id);
        if let Some(p) = self.pane_session.as_mut() {
            p.sync(task.as_deref(), rows_changed, cx);
        }
    }

    /// Open the pane's session in its own screen (the session header's Open
    /// Workspace): the same workspace or review, through the registry. The
    /// pane closes; its attach goes with it, the agent stays as it is.
    pub(super) fn open_session_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(command) = self.pane_session.as_ref().and_then(|p| p.attached()).map(|s| s.open_command()) else {
            return;
        };
        self.close_pane(cx);
        self.sync_session(false, cx);
        cx.emit(IssuesEvent::Command(command));
    }

    /// The keyboard's split resize (the handle's ↑ / ↓, the pane's ⌥↑ /
    /// ⌥↓): `grow` gives the session one step more.
    pub(super) fn step_split(&mut self, grow: bool, cx: &mut Context<Self>) -> bool {
        let Some(p) = self.pane_session.as_mut().filter(|p| p.attached().is_some()) else {
            return false;
        };
        let Some(bounds) = p.bounds else {
            return false;
        };
        p.split.step(grow, bounds.size.height.as_f32());
        cx.notify();
        true
    }

    /// The board's workflow filter (what a project switch saves).
    pub fn workflow_filter(&self) -> Option<String> {
        self.state.filters().workflow.clone()
    }

    /// Put back the workflow filter a project was left with, quietly (no
    /// status note: the user didn't just choose it).
    pub fn restore_workflow_filter(&mut self, filter: Option<String>, cx: &mut Context<Self>) {
        if self.state.filters().workflow != filter {
            self.state.set_workflow_filter(filter);
            self.state.status = None;
            cx.notify();
        }
    }

    /// The window is dropping this view (a project switch): stop its
    /// background reads without waiting on one in flight, so the UI thread
    /// never joins a slow read.
    pub fn stop_workers(&mut self) {
        self.open_refresher.stop_without_waiting();
        self.closed_refresher.stop_without_waiting();
        // Send what's typed. The answers land in the project's draft store,
        // which keeps a draft that fails for when the project is back.
        self.close_editor();
        for intent in self.state.flush_all_edits() {
            if let Intent::Save(job) = intent {
                self.spawn_save(job);
            }
        }
    }

    /// Take keyboard focus on the next frame (the window calls this when the
    /// view is shown).
    pub fn request_focus(&mut self) {
        self.needs_focus = true;
    }

    /// The board's state, for the window's tests.
    #[cfg(test)]
    pub fn state_for_test(&mut self) -> &mut BoardState {
        &mut self.state
    }

    /// The board's state, for the offscreen renders.
    #[cfg(feature = "visual-check")]
    pub fn state_mut(&mut self) -> &mut BoardState {
        &mut self.state
    }

    /// Whether the open detail has its issue to show (the board has loaded),
    /// for the offscreen renders.
    #[cfg(feature = "visual-check")]
    pub fn detail_loaded(&self) -> bool {
        self.state.detail_id().is_some() && !self.state.model().loading
    }

    /// Drain the board's reads now, for the offscreen renders.
    #[cfg(feature = "visual-check")]
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        self.tick(cx);
    }

    /// Each column's category and card count (`--bench-board`).
    #[cfg(feature = "visual-check")]
    pub fn bench_model_columns(&self) -> Vec<(shelbi_core::StatusCategory, usize)> {
        self.state.model().columns.iter().map(|c| (c.category, c.cards.len())).collect()
    }

    /// What the board says about its load (`--bench-board`).
    #[cfg(feature = "visual-check")]
    pub fn bench_status(&self) -> String {
        let m = self.state.model();
        format!("loading={} error={:?} status={:?} banner={:?}", m.loading, m.error, self.state.status, m.banner)
    }

    /// Column `ci`'s rectangle in the window as last drawn, scrolled
    /// (`--bench-board`).
    #[cfg(feature = "visual-check")]
    pub fn bench_column_rect(&self, ci: usize) -> Option<gpui::Bounds<gpui::Pixels>> {
        let b = self.board_scroll.handle.bounds_for_item(ci)?;
        Some(gpui::Bounds::new(b.origin + self.board_scroll.handle.offset(), b.size))
    }

    /// One refresh of the board's reads (`--bench-board`): waits for both
    /// reads, then times folding them in as the poll does. `changed` folds
    /// them as reads that differ from the last ones.
    #[cfg(feature = "visual-check")]
    pub fn bench_refresh(&mut self, changed: bool, cx: &mut Context<Self>) -> Duration {
        self.refresh();
        let wait = Duration::from_secs(30);
        let open = self.open_refresher.recv_timeout(wait);
        let closed = self.closed_refresher.recv_timeout(wait);
        if changed {
            self.open_print = None;
            self.closed_print = None;
        }
        let start = Instant::now();
        if self.fold_reads(open, closed) {
            cx.notify();
        }
        start.elapsed()
    }

    /// Put the keyboard's selection on row `ri` of column `ci`
    /// (`--bench-board`).
    #[cfg(feature = "visual-check")]
    pub fn bench_select(&mut self, ci: usize, ri: usize, cx: &mut Context<Self>) {
        self.state.select(ci, ri);
        self.scroll_selection_into_view();
        cx.notify();
    }

    /// Open task `id`'s detail (from an Activity row). A task the board hasn't
    /// loaded yet fills in when its read lands.
    pub fn open_task(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.editor.as_ref().is_some_and(|(open, _)| open != id) {
            self.close_editor();
        }
        self.state.open_detail(id);
        self.needs_focus = true;
        cx.notify();
    }

    /// Open task `id`'s detail over another screen (the Review or Workspace
    /// panel's More): the view draws the detail, and the assign picker it
    /// opens, without the board, for the window to lay over its main area
    /// until [`Self::overlay_open`] goes false.
    pub fn open_task_over(&mut self, id: &str, cx: &mut Context<Self>) {
        self.floating = true;
        self.open_task(id, cx);
    }

    /// Whether the view draws over another screen.
    pub fn is_floating(&self) -> bool {
        self.floating
    }

    /// Whether the detail, or a picker over the board, is up.
    pub fn overlay_open(&self) -> bool {
        self.state.mode != Mode::Board
    }

    /// Stop drawing over another screen. `close` closes the detail too (the
    /// screen it opened over went away); without it the detail stays open,
    /// for the board.
    pub fn land(&mut self, close: bool, cx: &mut Context<Self>) {
        self.floating = false;
        if close {
            self.close_editor();
            self.add_input = None;
            self.state.close_pane();
        }
        cx.notify();
    }

    /// Close the task pane (its Close, or Esc on it). An in-place edit with
    /// changes is saved rather than dropped; the keys go back to the board,
    /// whose selection, filters and scroll are as they were (over another
    /// screen, the window hands them back to it).
    pub(super) fn close_pane(&mut self, cx: &mut Context<Self>) {
        self.finish_editor(cx);
        self.add_input = None;
        self.state.close_pane();
        self.needs_focus = true;
        cx.notify();
    }

    /// Wire the task pane's pointer and keys: over another screen the pane is
    /// the view's focus target (the screen around it keeps its own clicks);
    /// on the board a press in it brings the keys back from, say, the search
    /// field, unless something inside took them (the body editor).
    pub(super) fn pane_focus(&self, pane: Stateful<gpui::Div>, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        if self.floating {
            return pane
                .track_focus(&self.focus)
                .key_context("IssuesBoard")
                .on_key_down(cx.listener(Self::on_key));
        }
        pane.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                if !window.default_prevented() && this.editor.is_none() {
                    window.focus(&this.focus, cx);
                }
            }),
        )
    }

    /// The open detail's task, title and body, as the board last read them.
    pub fn detail_text(&mut self) -> Option<(String, String, String)> {
        self.detail_model().map(|d| (d.id, d.title, d.body))
    }

    /// Whether the detail is open on `id` with its editor up (an assign
    /// picker opened from it keeps it).
    fn editing(&self, id: &str) -> bool {
        match &self.state.mode {
            Mode::Detail { id: open, editing } => open == id && *editing,
            Mode::AssignPicker { id: open, .. } => open == id,
            _ => false,
        }
    }

    /// Drain the refreshers and background results; re-read on the cadence.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let open = self.open_refresher.latest();
        let closed = self.closed_refresher.latest();
        let mut changed = self.fold_reads(open, closed);
        while let Ok(done) = self.done_rx.try_recv() {
            changed = true;
            match done {
                Done::Mutation { id, result } => {
                    let intents = self.state.finish_mutation(&id, result);
                    self.apply_intents_bg(intents, cx);
                }
                Done::Saved { id } => {
                    let intents = self.state.finish_save(&id);
                    self.apply_intents_bg(intents, cx);
                }
                Done::More(page) => {
                    self.load_more_in_flight = false;
                    self.state.apply_more(page);
                }
                Done::Persisted(result) => {
                    // The next read is folded either way: it carries what
                    // was persisted (or puts back what wasn't).
                    self.open_print = None;
                    if let Err(e) = result {
                        self.state.status = Some(e);
                    }
                }
            }
        }
        while let Ok(ev) = self.editor_rx.try_recv() {
            changed = true;
            match ev {
                EditorEvent::Saved { id, body } => {
                    let intents = self.state.save_external_edit(&id, body);
                    self.apply_intents_bg(intents, cx);
                }
                EditorEvent::Failed { id, error } => {
                    self.state.status = Some(format!("{id}: {error}"));
                }
            }
        }
        if self.pump_search() {
            self.scroll_selection_into_view();
            changed = true;
        }
        // A drag released off the board (or cancelled) never reaches a drop
        // handler; let go of its column styling.
        if self.state.dragging().is_some() && !cx.has_active_drag() {
            self.state.end_drag();
            changed = true;
        }
        // Saves whose pause in typing is over, and text the store changed
        // under the editor.
        let saves = self.state.poll_saves(Instant::now());
        changed |= !saves.is_empty();
        self.apply_intents_bg(saves, cx);
        // The editor's text is the draft's: catch it up when the store
        // changed it (compared, not queued: another window on the project may
        // have taken the store's updates).
        let updates = !self.state.take_text_updates().is_empty();
        if let Some((id, input)) = &self.editor {
            let draft = self.state.draft_text(id, Field::Body);
            if draft.is_some_and(|d| d != input.read(cx).value().as_ref()) {
                self.sync_editor = true;
                changed = true;
            }
        }
        changed |= updates;
        // Leaving the task (closing the detail, opening another card, the
        // board taking the keys) closes its editor, saving what's typed.
        if self.editor.as_ref().is_some_and(|(id, _)| !self.editing(id)) {
            self.close_editor();
            changed = true;
        }
        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresh();
        }
        if changed {
            cx.notify();
        }
    }

    /// Fold in the reads that arrived; true when the board changed. A read
    /// the same as the last one folded changes nothing, unless a change is
    /// in flight: a read is what settles it.
    fn fold_reads(
        &mut self,
        open: Option<(BoardRead, u64)>,
        closed: Option<(Option<ClosedPage>, u64)>,
    ) -> bool {
        let pending = self.state.has_pending();
        let open = to_fold(open, &mut self.open_print, pending);
        let closed = to_fold(closed, &mut self.closed_print, pending);
        let changed = open.is_some() || closed.is_some();
        self.state.apply_reads(open, closed);
        changed
    }

    /// A card was dropped: run what the state makes of it.
    fn handle_drop(&mut self, intents: Vec<Intent>, window: &mut Window, cx: &mut Context<Self>) {
        self.needs_focus = true;
        self.apply_intents(intents, window, cx);
        cx.notify();
    }

    fn refresh(&mut self) {
        self.open_refresher.request();
        self.closed_refresher.request();
        self.last_refresh = Instant::now();
    }

    /// The open detail's model, rebuilt only when the detail or the board
    /// changes.
    fn detail_model(&mut self) -> Option<TaskDetailModel> {
        let Some(id) = self.state.detail_id() else {
            self.detail_cache = None;
            return None;
        };
        let generation = self.state.generation();
        match &self.detail_cache {
            Some((cached, at, model)) if cached == id && *at == generation => model.clone(),
            _ => {
                let model = TaskDetailModel::build(self.state.input(), id);
                self.detail_cache = Some((id.to_string(), generation, model.clone()));
                model
            }
        }
    }

    /// The window hands an issue mutation back here after dispatch.
    pub fn run_issue_command(
        &mut self,
        kind: &CommandKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.state.begin_mutation(kind) {
            Ok(job) => self.spawn_mutation(job),
            Err(intents) => self.apply_intents(intents, window, cx),
        }
        cx.notify();
    }

    fn spawn_mutation(&self, job: MutationJob) {
        let project = self.state.project.clone();
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let result = match job.mutation(&project) {
                Some(m) => shelbi_app::execute_mutation_expecting(
                    &m,
                    job.expected.clone(),
                    &mut |_stream, _line| {},
                ),
                None => Err(ExecError::Backend("not an issue mutation".into())),
            };
            let _ = tx.send(Done::Mutation { id: job.id, result });
        });
    }

    /// Send a draft's save. Its answer goes straight into the project's
    /// shared draft store (so it lands even if this view is gone by then);
    /// the view is only told to catch up.
    fn spawn_save(&mut self, job: SaveJob) {
        #[cfg(test)]
        self.sent_saves.push(job);
        #[cfg(not(test))]
        self.run_save(job);
    }

    #[cfg(not(test))]
    fn run_save(&self, job: SaveJob) {
        let project = self.state.project.clone();
        let edits = self.state.edits();
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let result = shelbi_app::execute_mutation_expecting(
                &job.mutation(&project),
                job.expected.clone(),
                &mut |_stream, _line| {},
            );
            if let Err(e) = &result {
                tracing::warn!("saving {}/{} failed: {e}", project, job.id);
            }
            BoardState::record_save(&edits, &job, result);
            let _ = tx.send(Done::Saved { id: job.id });
        });
    }

    /// Intents that need no window (from a background result, or the tick).
    fn apply_intents_bg(&mut self, intents: Vec<Intent>, cx: &mut Context<Self>) {
        for intent in intents {
            match intent {
                Intent::Refresh => self.refresh(),
                Intent::Save(job) => self.spawn_save(job),
                // A change held for the issue's lane, now its turn.
                Intent::Command(kind) => cx.emit(IssuesEvent::Command(kind)),
                _ => {}
            }
        }
    }

    pub(super) fn apply_intents(
        &mut self,
        intents: Vec<Intent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for intent in intents {
            match intent {
                Intent::Command(kind) => cx.emit(IssuesEvent::Command(kind)),
                Intent::Refresh => self.refresh(),
                Intent::LoadMore => self.load_more(),
                Intent::PersistWorkspaceFilter(filter) => {
                    let project = self.state.project.clone();
                    let disk = filter.map(|f| f.to_disk());
                    self.persist(move || {
                        shelbi_state::set_workspace_filter(&project, disk.as_deref())
                            .map_err(|e| format!("filter persist failed: {e}"))
                    });
                }
                Intent::PersistColumnOverride {
                    workflow,
                    status_id,
                    value,
                } => {
                    let project = self.state.project.clone();
                    self.persist(move || {
                        shelbi_state::set_kanban_column_override(
                            &project,
                            &workflow,
                            &status_id,
                            Some(value),
                        )
                        .map_err(|e| format!("column override persist failed: {e}"))
                    });
                }
                Intent::ScrollDetail(s) => self.scroll_detail(s),
                Intent::BeginEdit(id) => self.begin_edit(&id, window, cx),
                Intent::Save(job) => self.spawn_save(job),
                Intent::OpenInEditor(id) => self.open_in_editor(&id),
                Intent::BeginAdd => self.begin_add(window, cx),
                Intent::FocusSearch => {
                    let input = self.search_field(window, cx);
                    input.update(cx, |s, cx| s.focus(window, cx));
                }
                Intent::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
                Intent::ClearSearchField => {
                    if let Some(input) = &self.search_input {
                        input.update(cx, |s, cx| s.set_value("", window, cx));
                    }
                }
            }
        }
    }

    /// Small `state.json` writes, off the UI thread.
    fn persist(&self, f: impl FnOnce() -> Result<(), String> + Send + 'static) {
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done::Persisted(f()));
        });
    }

    /// The search's worker, started on first use.
    fn search_worker(&mut self) -> &SearchWorker {
        let project = self.state.project.clone();
        self.search_worker.get_or_insert_with(|| SearchWorker::github(project))
    }

    /// Send the search that's due and fold in the answers; true when the
    /// board changed.
    fn pump_search(&mut self) -> bool {
        if self.state.search_due() {
            self.search_worker();
        }
        match &self.search_worker {
            Some(worker) => self.state.pump_search(worker, Instant::now()),
            None => false,
        }
    }

    fn load_more(&mut self) {
        // While the search's results show, "load more" pages them.
        if self.state.search_showing_results() {
            if let Some(req) = self.state.search_load_more() {
                self.search_worker().send(req);
            }
            return;
        }
        let Some(cursor) = self.state.closed_cursor().map(str::to_string) else {
            return;
        };
        if self.load_more_in_flight {
            return;
        }
        self.load_more_in_flight = true;
        self.state.status = Some("loading more history…".into());
        let project = self.state.project.clone();
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done::More(board::load_more_closed(&project, &cursor)));
        });
    }

    fn scroll_detail(&mut self, s: Scroll) {
        let step = px(40.0);
        let page = px(400.0);
        let mut offset = self.detail_scroll.offset();
        let max = self.detail_scroll.max_offset();
        offset.y = match s {
            Scroll::Up => offset.y + step,
            Scroll::Down => offset.y - step,
            Scroll::PageUp => offset.y + page,
            Scroll::PageDown => offset.y - page,
            Scroll::Top => px(0.0),
        };
        offset.y = offset.y.min(px(0.0)).max(-max.y);
        self.detail_scroll.set_offset(offset);
    }

    fn body_of(&self, id: &str) -> Option<String> {
        self.state
            .input()
            .tasks()
            .into_iter()
            .find(|tf| tf.task.id == id)
            .map(|tf| tf.body)
    }

    fn begin_edit(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.as_ref().is_some_and(|(open, _)| open == id) {
            if let Some((_, state)) = &self.editor {
                state.update(cx, |s, cx| s.focus(window, cx));
            }
            return;
        }
        self.close_editor();
        // The draft's text: what was left unsaved last time, if anything.
        let body = self
            .state
            .draft_text(id, Field::Body)
            .or_else(|| self.body_of(id))
            .unwrap_or_default();
        let state = cx.new(|cx| TextareaState::new(window, cx).default_value(body));
        state.update(cx, |s, cx| s.focus(window, cx));
        let issue = id.to_string();
        let sub = cx.subscribe_in(&state, window, move |this, input, ev: &InputEvent, _, cx| {
            match ev {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.state.edit_text(&issue, Field::Body, &text, Instant::now());
                    cx.notify();
                }
                // Leaving the field saves what's typed (the editor stays).
                InputEvent::Blur => {
                    for intent in this.state.flush_edit(&issue) {
                        if let Intent::Save(job) = intent {
                            this.spawn_save(job);
                        }
                    }
                    cx.notify();
                }
                _ => {}
            }
        });
        self._editor_subscriptions = vec![sub];
        self.sync_editor = false;
        self.editor = Some((id.to_string(), state));
    }

    /// Cmd+S: save the in-place editor's text now. Optional (the draft saves
    /// itself) and the editor stays open.
    pub(super) fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editor.as_ref().map(|(id, _)| id.clone()) else {
            return;
        };
        for intent in self.state.flush_edit(&id) {
            if let Intent::Save(job) = intent {
                self.spawn_save(job);
            }
        }
        cx.notify();
    }

    /// Done / Escape / Close: close the in-place editor, saving what's typed
    /// (an unchanged draft sends nothing; nothing is reverted).
    pub(super) fn finish_editor(&mut self, cx: &mut Context<Self>) {
        self.close_editor();
        self.needs_focus = true;
        cx.notify();
    }

    /// Drop the editor widget, flushing its draft. A draft that can't be
    /// saved stays in the store and comes back with the editor.
    fn close_editor(&mut self) {
        let Some((id, _)) = self.editor.take() else {
            return;
        };
        self._editor_subscriptions.clear();
        self.sync_editor = false;
        for intent in self.state.end_edit(&id) {
            if let Intent::Save(job) = intent {
                self.spawn_save(job);
            }
        }
    }

    /// Put the text the draft store changed (a read merged into the draft,
    /// a conflict resolved, the external editor) into the open editor, as
    /// one splice so the caret and the text around it stay put.
    fn sync_editor_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_editor = false;
        let Some((id, state)) = &self.editor else {
            return;
        };
        let Some(draft) = self.state.draft_text(id, Field::Body) else {
            return;
        };
        state.update(cx, |s, cx| {
            let current = s.value().to_string();
            let splice = Splice::between(&current, &draft);
            if splice.is_empty() {
                return;
            }
            let selection = s.selected_range();
            s.set_selected_range(splice.start..splice.end, cx);
            s.replace(splice.text.clone(), window, cx);
            s.set_selected_range(splice.map(selection.start)..splice.map(selection.end), cx);
        });
    }

    fn open_in_editor(&mut self, id: &str) {
        let Some(body) = self.body_of(id) else {
            return;
        };
        let editor = shelbi_state::resolve_editor();
        self.state.status = Some(format!(
            "editing {id} in {}; saves are sent as you write",
            shelbi_state::editor_display_name(&editor)
        ));
        external_edit::open(&self.state.project, id, &body, self.editor_tx.clone());
    }

    fn begin_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Issue title"));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let title = input.read(cx).value().to_string();
                this.add_input = None;
                this.needs_focus = true;
                let intents = this.state.submit_add(&title);
                this.apply_intents(intents, window, cx);
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        self.add_input = Some(input);
    }

    /// The toolbar's search field: typing narrows the board as it goes.
    fn search_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.search_input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                let query = input.read(cx).value().to_string();
                this.state.set_search(&query);
                this.scroll_selection_into_view();
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        self.search_input = Some(input.clone());
        input
    }

    fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_some_and(|s| s.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Leave the search field for the board: `clear` empties the search too
    /// (Esc), else it stays (Enter, to walk the cards it left).
    fn leave_search(&mut self, clear: bool, window: &mut Window, cx: &mut Context<Self>) {
        if clear {
            self.clear_search(window, cx);
        }
        self.needs_focus = true;
        cx.notify();
    }

    /// Empty the search (the board shows every card under the other
    /// filters again), leaving the keyboard where it is.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.set_search("");
        if let Some(input) = &self.search_input {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.scroll_selection_into_view();
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The search field owns its keys (its Esc and Enter come in as
        // actions, caught in `render_search`), and the pane's session its
        // own: a typed `j` or an Esc there is the agent's, never the board's.
        if self.search_focused(window, cx) || self.session_focused(window, cx) {
            return;
        }
        let Some(chord) = chord_from_keystroke(&ev.keystroke) else {
            return;
        };
        if self.pane_session_key(ev, window, cx) {
            cx.stop_propagation();
            return;
        }
        // Cmd+S / Cmd+Enter save the in-place editor now (it saves on its
        // own too) and leave it open; Esc leaves it, saving its draft (it
        // never closes the pane under it).
        if self.editor.is_some() {
            let m = &ev.keystroke.modifiers;
            if m.platform && (ev.keystroke.key == "s" || ev.keystroke.key == "enter") {
                self.save_editor(cx);
                cx.stop_propagation();
                return;
            }
            if ev.keystroke.key == "escape" && !m.modified() {
                self.finish_editor(cx);
                cx.stop_propagation();
                return;
            }
        }
        let before = (self.state.selected_column, self.state.selected_row);
        let Some(intents) = self.state.handle_key(chord) else {
            return;
        };
        cx.stop_propagation();
        // Leaving the editor / prompt with Escape drops its widget (the
        // editor's text is saved, not reverted).
        if !matches!(self.state.mode, Mode::Detail { editing: true, .. }) && self.editor.is_some() {
            self.close_editor();
            self.needs_focus = true;
        }
        if self.state.mode != Mode::AddPrompt && self.add_input.is_some() {
            self.add_input = None;
            self.needs_focus = true;
        }
        self.apply_intents(intents, window, cx);
        if before != (self.state.selected_column, self.state.selected_row) {
            self.scroll_selection_into_view();
        }
        self.scroll_picker_to_cursor();
        cx.notify();
    }

    /// The pane's keys for its session, while it shows one and has the keys
    /// (no menu or editor over it): ⌥↑ / ⌥↓ resize the split, and Tab past
    /// the last property (Status, Workflow, Priority, then the session) puts
    /// the keys in the terminal.
    fn pane_session_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.editor.is_some() || !matches!(self.state.mode, Mode::Detail { editing: false, .. }) {
            return false;
        }
        let Some(view) = self.pane_session_view().cloned() else {
            return false;
        };
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "up" | "down" if m.alt && !m.platform && !m.control && !m.shift => self.step_split(ks.key == "up", cx),
            "tab" if !m.modified() && self.state.pane_focus() == Some(Property::Priority) => {
                self.state.clear_pane_focus();
                let target = view.read(cx).focus_target(cx);
                window.focus(&target, cx);
                true
            }
            _ => false,
        }
    }

    /// Keep an open property picker's keyboard row in view.
    pub(super) fn scroll_picker_to_cursor(&self) {
        if let Mode::PropertyPicker { cursor, .. } = self.state.mode {
            self.picker_scroll.scroll_to_item(cursor);
        }
    }

    fn scroll_selection_into_view(&mut self) {
        self.board_scroll.handle.scroll_to_item(self.state.selected_column);
        // The list learns of a model change at render; catch it up first so
        // the row it reveals is the selected card's.
        self.sync_column_lists();
        let column = self.state.model().columns.get(self.state.selected_column);
        if let Some(list) = column.and_then(|c| self.lists().get(&c.status_id)) {
            list.reveal(self.state.selected_row);
        }
    }

    /// Bring each expanded column's list in line with the model (a no-op for
    /// a column that hasn't changed).
    fn sync_column_lists(&mut self) {
        let searching = self.state.model().search.is_some();
        if !searching {
            self.search_lists.clear();
        }
        let model = self.state.model();
        let lists = if searching { &mut self.search_lists } else { &mut self.column_lists };
        for col in model.columns.iter().filter(|c| !c.collapsed) {
            let rows = column_rows(col, model);
            match lists.get_mut(&col.status_id) {
                Some(list) => list.sync(col, &rows),
                None => {
                    let mut list = ColumnList::new();
                    list.sync(col, &rows);
                    lists.insert(col.status_id.clone(), list);
                }
            }
        }
    }

    /// The column lists on screen: the search's while one is active.
    fn lists(&self) -> &HashMap<String, ColumnList> {
        if self.state.model().search.is_some() {
            &self.search_lists
        } else {
            &self.column_lists
        }
    }

    pub(super) fn hint(&self, chord: Option<&shelbi_state::keymap::KeyChord>) -> String {
        chord.map_or_else(|| "–".to_string(), |c| crate::keys::chord_text(c, self.display_style))
    }

    fn keymaps(&self) -> &Keymaps {
        self.state.keymaps()
    }

    // ---- rendering --------------------------------------------------------

    /// The toolbar, the Figma Issues Header: `Filter:` and the workflow and
    /// workspace chips, then at the right the search field and New issue, on
    /// the Header's band (the Issues frame has no board title).
    fn render_header(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.state.model();
        let filters = self.state.filters();
        // Unfiltered, a chip reads as the Header draws it (`Workspaces`, in
        // the Default style); set, it names the filter in the Select style.
        let workflow_label = match filters.workflow {
            Some(_) => model.workflow_filter_label.clone(),
            None => "Workflows".to_string(),
        };
        let workspace_label = match filters.workspace {
            Some(_) => model.workspace_filter_label.clone(),
            None => "Workspaces".to_string(),
        };
        let (workflow_set, workspace_set) = (filters.workflow.is_some(), filters.workspace.is_some());
        let banner = model.banner.clone();
        // The search's note beside its field: the match count, or that the
        // board shows local results because GitHub's search is unavailable.
        let search_note = model.search.as_ref().and_then(|s| s.label());
        let search = self.render_search(theme, window, cx);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pl_3()
            .pr_4()
            .h(px(TOOLBAR_H))
            .flex_none()
            .bg(theme.toolbar)
            // Figma's Header: a 1px canvas edge on its left.
            .border_l_1()
            .border_color(theme.canvas)
            .child(
                div()
                    .flex_none()
                    .type_style(Type::SmallMedium)
                    .text_color(theme.form_hint)
                    .child("Filter:"),
            )
            .child(
                style::filter_chip(
                    "workflow-chip",
                    workflow_label,
                    workflow_set,
                    matches!(self.state.mode, Mode::WorkflowDropdown { .. }),
                    theme,
                )
                .child(style::anchor_probe(&self.workflow_anchor))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.state.open_workflow_dropdown();
                    cx.notify();
                })),
            )
            .child(
                style::filter_chip(
                    "workspace-chip",
                    workspace_label,
                    workspace_set,
                    matches!(self.state.mode, Mode::WorkspaceDropdown { .. }),
                    theme,
                )
                .child(style::anchor_probe(&self.workspace_anchor))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.state.open_workspace_dropdown();
                    cx.notify();
                })),
            )
            .when_some(banner, |row, banner| {
                row.child(div().type_style(Type::XSmall).text_color(theme.muted).child(banner))
            })
            .child(div().flex_1())
            .when(self.state.has_pending(), |row| {
                row.child(
                    heroicon(icons::ARROW_PATH).text_color(theme.icon),
                )
            })
            .when_some(search_note, |row, note| {
                row.child(
                    div()
                        .debug_selector(|| "board-search-note".into())
                        .flex_none()
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .child(note),
                )
            })
            .child(search)
            .child(
                style::toolbar_button("new-issue", icons::PLUS, "New issue", theme)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        let intents = this.state.begin_add();
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    })),
            )
    }

    /// The toolbar's search field (Figma's Searchbox): the input with its
    /// own look off inside [`style::search_box`], and `Search` in
    /// `form_hint` while it is empty. Esc clears it and Enter keeps it, both
    /// handing the keyboard back to the board.
    fn render_search(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let input = self.search_field(window, cx);
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let empty = self.state.search().is_empty();
        let searching = self.state.model().search.as_ref().is_some_and(|s| s.searching);
        let field = div()
            .debug_selector(|| "board-search-field".into())
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .type_style(Type::Small)
            .text_color(theme.foreground)
            .child(
                Input::new(&input)
                    .appearance(false)
                    .px(px(0.0))
                    .py(px(0.0))
                    .h_full()
                    .type_style(Type::Small),
            )
            .when(empty, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .text_color(theme.form_hint)
                        .child("Search"),
                )
            });
        let focus = input.read(cx).focus_handle(cx);
        let refocus = focus.clone();
        style::search_box("board-search", field, focused, theme)
            .w(px(SEARCH_W))
            .on_click(move |_, window, cx| window.focus(&focus, cx))
            // A GitHub search on its way: a quiet spinner glyph at the end
            // of the field.
            .when(searching, |d| {
                d.child(
                    div()
                        .debug_selector(|| "board-search-searching".into())
                        .flex_none()
                        .child(heroicon(icons::ARROW_PATH).text_color(theme.muted)),
                )
            })
            // The clear button keeps the keyboard in the field, for the
            // next search.
            .when(!empty, |d| {
                d.child(style::search_clear("board-search-clear", theme).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.clear_search(window, cx);
                        window.focus(&refocus, cx);
                        cx.stop_propagation();
                    },
                )))
            })
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.leave_search(true, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                this.leave_search(false, window, cx);
                cx.stop_propagation();
            }))
    }

    fn render_board(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_column_lists();
        let children: Vec<_> = self
            .state
            .model()
            .columns
            .iter()
            .enumerate()
            .map(|(ci, col)| {
                if col.collapsed {
                    self.render_collapsed(ci, col, theme, cx).into_any_element()
                } else {
                    self.render_column(ci, col, theme, cx).into_any_element()
                }
            })
            .collect();
        let board = board_scroll::board_row(
            div()
                .id("board-columns")
                .debug_selector(|| "board-columns".into())
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .gap_3()
                // No bottom padding: a long column runs under the footer's
                // rule, as Figma draws it.
                .pt_3()
                .px_3(),
            &self.board_scroll,
        )
        .children(children);
        board_scroll::with_board_wheel(board, &self.board_scroll)
    }

    fn render_collapsed(
        &self,
        ci: usize,
        col: &IssueColumn,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = ci == self.state.selected_column;
        let drop = self.state.dragging().map(|id| self.state.drop_target(id, ci));
        let refused = matches!(drop, Some(DropTarget::Refused(_)));
        let theme = *theme;
        // Figma's collapsed column (node 2165:94) is the column heading
        // turned 90° clockwise down a 24pt strip: the glyph at the top (kept
        // upright, as the expanded heading draws it), then the caps label
        // and the count reading down. The label takes the strip's height
        // and is cut with an ellipsis only where it would run past it; the
        // count is never cut.
        let label_color = if focused { theme.text_loud } else { theme.muted };
        div()
            .id(("collapsed", ci))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .flex_none()
            .w(px(COLLAPSED_WIDTH))
            .h_full()
            .py(px(4.0))
            .overflow_hidden()
            .rounded_lg()
            .cursor_pointer()
            .hover(move |s| s.bg(theme.background))
            .child(div().flex_none().size(px(16.0)).child(style::status_icon(col.category, &theme)))
            .child(
                rotated_text(col.label.to_uppercase(), Type::XSmallSemibold, label_color).truncate(),
            )
            .child(rotated_text(col.cards.len().to_string(), Type::XSmall, theme.muted))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                let intents = this.state.toggle_column(ci);
                this.apply_intents(intents, window, cx);
                cx.notify();
            }))
            .when(refused, |d| d.opacity(0.35))
            .when(drop.is_some() && !refused, |d| {
                d.drag_over::<CardDrag>(move |s, _, _, _| drop_highlight(s, &theme))
            })
            // A refused target still takes the drop, so the state can say why
            // (it sends nothing).
            .when(drop.is_some(), |d| {
                d.on_drop(cx.listener(move |this, drag: &CardDrag, window, cx| {
                    let intents = this.state.drop_on_column(&drag.id, ci);
                    this.handle_drop(intents, window, cx);
                }))
            })
    }

    fn render_column(
        &self,
        ci: usize,
        col: &IssueColumn,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = ci == self.state.selected_column;
        let drop = self.state.dragging().map(|id| self.state.drop_target(id, ci));
        let refused = match &drop {
            Some(DropTarget::Refused(msg)) => Some(msg.clone()),
            _ => None,
        };
        let drop_hint = match &drop {
            Some(DropTarget::Move { merge: true }) => Some("Drop to run the gated merge"),
            Some(DropTarget::Move { merge: false }) => Some("Drop to move here"),
            _ => None,
        };
        // The Figma column heading: the status glyph, the label in caps
        // (loud on the keyboard's column) and the count.
        let header = div()
            .id(("header", ci))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_1()
            .h(px(24.0))
            .flex_none()
            .cursor_pointer()
            .child(style::status_heading(col.category, col.label.clone(), focused, theme))
            .child(
                div()
                    .flex_none()
                    .type_style(Type::XSmall)
                    .text_color(theme.muted)
                    .child(col.cards.len().to_string()),
            )
            // While a card is dragged, what a drop here does (in the header
            // row, so the cards don't shift when the drag starts).
            .children(
                refused
                    .clone()
                    .map(|msg| format!("Can't drop: {msg}"))
                    .or(drop_hint.map(str::to_string))
                    .map(|text| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .child(text)
                    }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                let intents = this.state.toggle_column(ci);
                this.apply_intents(intents, window, cx);
                cx.notify();
            }));
        // Only the cards in view are built (`board_list.rs`); the list asks
        // for each as it scrolls in.
        let list_state = self.lists()[&col.status_id].state.clone();
        let cards = gpui::list(
            list_state.clone(),
            cx.processor(move |this, ix: usize, _window, cx| this.render_column_item(ci, ix, cx)),
        )
        // The room under the last card once scrolled to the end.
        .pb_3();
        // The clip reaches GLOW_ROOM past the column on the sides and top, so
        // the focus glow shows while the cards keep the column's width (each
        // item pads itself back in).
        let body = board_scroll::column_list(cards, &list_state, &self.board_scroll)
            .mx(px(-GLOW_ROOM))
            .mt(px(-GLOW_ROOM));
        let theme = *theme;
        // No well: the cards sit on the canvas under the heading. A legal
        // drop target lights up under the pointer.
        div()
            .id(("column-drop", ci))
            .flex()
            .flex_col()
            .gap_2()
            .flex_none()
            .w(px(COLUMN_WIDTH))
            .h_full()
            .rounded_lg()
            .child(header)
            .child(body)
            .when(refused.is_some(), |d| d.opacity(0.35))
            .when(drop.is_some() && refused.is_none(), |d| {
                d.drag_over::<CardDrag>(move |s, _, _, _| drop_highlight(s, &theme))
            })
            // A refused target still takes the drop, so the state can say why
            // (it sends nothing).
            .when(drop.is_some(), |d| {
                d.on_drop(cx.listener(move |this, drag: &CardDrag, window, cx| {
                    let intents = this.state.drop_on_column(&drag.id, ci);
                    this.handle_drop(intents, window, cx);
                }))
            })
    }

    /// Item `ix` of column `ci`'s list: a card (a reorder target while its
    /// column's card is dragged), or a row under the cards. Each is padded
    /// as the column's flex gap and glow room placed it: the glow's room
    /// above the first, the 8px gap above the rest, the glow's room beside.
    fn render_column_item(&self, ci: usize, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let model = self.state.model();
        let Some(col) = model.columns.get(ci) else {
            return div().into_any_element();
        };
        let el = match col.cards.get(ix) {
            Some(card) => {
                // The pane's card keeps its ring while the pane is up (the
                // Task pane frames draw it on the open card).
                let selected = ci == self.state.selected_column && ix == self.state.selected_row;
                let card_el = self.render_card(card, ci, ix, selected, &theme, cx);
                // In the dragged card's own column each card is a reorder
                // target; the row it was dragged from says which edge marks
                // the landing spot.
                let reorder_from = match self.state.dragging() {
                    Some(id) if self.state.drop_target(id, ci) == DropTarget::Reorder => {
                        col.cards.iter().position(|c| c.id == id)
                    }
                    _ => None,
                };
                match reorder_from {
                    Some(from) => reorder_target(card_el, card, ci, ix, from, &theme, cx).into_any_element(),
                    None => card_el.into_any_element(),
                }
            }
            None => match column_rows(col, model).get(ix - col.cards.len()) {
                Some(Item::Empty) if model.loading => muted_row(&theme, "Loading board…").into_any_element(),
                // An empty column: what goes here and how to put it there.
                Some(Item::Empty) => div()
                    .debug_selector(|| "column-empty".into())
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(72.0))
                    .px_3()
                    .rounded(px(CARD_RADIUS))
                    .border_1()
                    .border_dashed()
                    .border_color(theme.divider_strong)
                    .type_style(Type::XSmall)
                    .text_center()
                    .text_color(theme.muted)
                    .child("No issues. Drag a card here.")
                    .into_any_element(),
                Some(Item::LoadingHistory) => muted_row(&theme, "Loading history…").into_any_element(),
                Some(Item::LoadMore) => {
                    let hint = "m";
                    div()
                        .id("load-more")
                        .px_2()
                        .py_1()
                        .type_style(Type::XSmall)
                        .text_color(theme.link)
                        .hover(|s| s.text_color(theme.link_hover))
                        .cursor_pointer()
                        .child(if self.load_more_in_flight
                            || self.state.model().search.as_ref().is_some_and(|s| s.searching)
                        {
                            "Loading…".to_string()
                        } else {
                            format!("Load more ({hint})")
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.load_more();
                            cx.notify();
                        }))
                        .into_any_element()
                }
                Some(Item::Card(_)) | None => div().into_any_element(),
            },
        };
        div()
            .px(px(GLOW_ROOM))
            .pt(px(if ix == 0 { GLOW_ROOM } else { 8.0 }))
            .child(el)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card(
        &self,
        card: &IssueCard,
        ci: usize,
        ri: usize,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let pending = self.state.pending(&card.id).cloned();
        let notice = self.state.notice(&card.id).map(str::to_string);
        let dragged = self.state.dragging() == Some(card.id.as_str());
        let id = card.id.clone();
        let weak = cx.weak_entity();
        let drag = CardDrag {
            id: card.id.clone(),
        };
        let ghost_title = SharedString::from(card.title.clone());
        let selector = format!("card-{}", card.id);
        let title_color = if card.blocked {
            theme.muted
        } else {
            theme.text_loud
        };
        div()
            .id(SharedString::from(format!("card-{}", card.id)))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .p_3()
            .overflow_hidden()
            .rounded(px(CARD_RADIUS))
            .bg(theme.card)
            // The keyboard's card wears the focus ring and glow. The dragged
            // card's placeholder wears neither: a shadow is a filled box under
            // the card and would show through its dimmed fill.
            .shadow(if dragged {
                Vec::new()
            } else if selected {
                style::focus_shadow(theme)
            } else {
                style::card_shadow(theme)
            })
            .when(pending.is_some(), |d| d.opacity(0.65))
            // The card being dragged stays as a placeholder under its ghost.
            .when(dragged, |d| d.opacity(0.3))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.state.select(ci, ri);
                this.state.open_detail(&id);
                cx.notify();
            }))
            // A card with a change in flight can't be dragged (one mutation
            // per issue at a time).
            .when(pending.is_none(), |d| {
                d.on_drag(drag, move |drag: &CardDrag, _offset, _window, cx: &mut App| {
                    let id = drag.id.clone();
                    let _ = weak.update(cx, |view, cx| {
                        if !view.state.begin_drag(&id) {
                            view.state.status = Some(format!("{id}: a change is already in progress"));
                        }
                        cx.notify();
                    });
                    let title = ghost_title.clone();
                    cx.new(|_| CardGhost {
                        title,
                        id: SharedString::from(drag.id.clone()),
                    })
                })
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .items_start()
                    // Figma's title box is 254px, 6px wider than the card's
                    // content: it runs into the right padding, so a title
                    // wraps where Figma wraps it.
                    .mr(px(-TITLE_OVERHANG))
                    .when(card.blocked, |d| {
                        d.child(
                            heroicon(icons::LOCK_CLOSED)
                                .mt(px(1.0))
                                .text_color(theme.muted),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .type_style(Type::BaseMedium)
                            .text_color(title_color)
                            .line_clamp(2)
                            .text_ellipsis()
                            .child(card.title.clone()),
                    ),
            )
            // The body's first paragraph, X-Small on Figma's 18pt leading.
            .when(!card.summary.is_empty(), |d| {
                d.child(
                    div()
                        .w(px(SUMMARY_WIDTH))
                        .overflow_hidden()
                        .type_style(Type::XSmall)
                        .line_height(px(18.0))
                        .text_color(theme.foreground)
                        .line_clamp(SUMMARY_LINES)
                        .text_ellipsis()
                        .child(card.summary.clone()),
                )
            })
            .child(card_meta(card, theme))
            .when_some(pending, |d, p| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .child(heroicon(icons::ARROW_PATH).text_color(theme.icon))
                        .child(p.label),
                )
            })
            .when_some(notice, |d, message| {
                let id = card.id.clone();
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap_1()
                        .type_style(Type::XSmall)
                        .text_color(theme.error)
                        .child(
                            heroicon(icons::EXCLAMATION_TRIANGLE)
                                .text_color(theme.error),
                        )
                        // Wraps (a gated merge's refusal can be long); the
                        // status line carries it too.
                        .child(div().flex_1().min_w_0().line_clamp(4).child(message))
                        .child(
                            div()
                                .id(SharedString::from(format!("dismiss-{}", card.id)))
                                .flex_none()
                                .cursor_pointer()
                                .child(heroicon(icons::X_MARK).text_color(theme.icon))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.state.dismiss_notice(&id);
                                    cx.notify();
                                })),
                        ),
                )
            })
    }

    /// The key-hint footer along the bottom of the board, the Figma Board
    /// keyboard shortcuts row, with the status line at its right.
    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
        let km = self.keymaps();
        let h = |a| self.hint(km.kanban.first_chord_for(a));
        let hints = format!(
            "{}/{} column   {}/{} card   {} open   {}/{} move   {}/{} reorder   drag to move or reorder   s start   a assign   e edit   E editor   n new",
            h(KanbanAction::NavLeft),
            h(KanbanAction::NavRight),
            h(KanbanAction::NavUp),
            h(KanbanAction::NavDown),
            h(KanbanAction::OpenPopover),
            h(KanbanAction::MoveCardLeft),
            h(KanbanAction::MoveCardRight),
            h(KanbanAction::ReorderUp),
            h(KanbanAction::ReorderDown),
        );
        let status = self
            .state
            .status
            .clone()
            .or_else(|| self.state.model().error.clone());
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_4()
            .h(px(FOOTER_H))
            .px_4()
            .border_t_1()
            .border_color(theme.divider)
            .type_style(Type::XSmall)
            .text_color(theme.muted)
            .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().child(hints))
            .when_some(status, |d, s| {
                d.child(div().flex_none().max_w(relative(0.5)).truncate().text_color(theme.foreground).child(s))
            })
    }

    fn render_dropdown(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (title, rows, cursor, workflow) = match &self.state.mode {
            Mode::WorkflowDropdown { cursor } => (
                "Workflow",
                self.state
                    .workflow_options()
                    .into_iter()
                    .map(|o| {
                        (
                            o.filter.unwrap_or_else(|| "All workflows".to_string()),
                            o.count,
                        )
                    })
                    .collect::<Vec<_>>(),
                *cursor,
                true,
            ),
            Mode::WorkspaceDropdown { cursor } => (
                "Workspace",
                self.state
                    .workspace_options()
                    .into_iter()
                    .map(|o| {
                        (
                            o.filter.map_or_else(|| "All workspaces".to_string(), |f| f.label()),
                            o.count,
                        )
                    })
                    .collect(),
                *cursor,
                false,
            ),
            _ => return None,
        };
        let items: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(ix, (label, count))| {
                option_row(theme, ix == cursor, label, Some(count))
                    .id(("dropdown-option", ix))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if workflow {
                            this.state.choose_workflow_option(ix);
                        } else {
                            let intents = this.state.choose_workspace_option(ix);
                            this.apply_intents(intents, window, cx);
                        }
                        cx.notify();
                    }))
            })
            .collect();
        let anchor = if workflow {
            &self.workflow_anchor
        } else {
            &self.workspace_anchor
        };
        Some(
            // A dropdown dims nothing (a menu, not a dialog). It opens 6px
            // under the control that opened it.
            backdrop(theme, cx).bg(gpui::transparent_black()).child(style::below(
                anchor,
                gpui::point(px(16.0), px(48.0)),
                style::menu_entrance(
                    if workflow { "workflow-menu" } else { "workspace-menu" },
                    panel(theme)
                        .id("dropdown-panel")
                        .debug_selector(|| "dropdown-panel".into())
                        .min_w(px(240.0))
                        .max_w(px(360.0))
                        .child(panel_title(theme, title))
                        .children(items),
                ),
            )),
        )
    }

    fn render_assign(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let Mode::AssignPicker { id, cursor } = &self.state.mode else {
            return None;
        };
        let current = self.state.model().card(id).and_then(|c| c.assignee.clone());
        let items: Vec<_> = self
            .state
            .input()
            .workspaces
            .iter()
            .enumerate()
            .map(|(ix, ws)| {
                let mark = if current.as_deref() == Some(ws.as_str()) { " (assigned)" } else { "" };
                let id = id.clone();
                option_row(theme, ix == *cursor, format!("@{ws}{mark}"), None)
                    .id(("assign-option", ix))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let intents = this.state.choose_assignee(&id, ix);
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    }))
            })
            .collect();
        Some(centered(
            backdrop(theme, cx),
            panel(theme)
                .w(px(320.0))
                .child(panel_title(theme, &format!("Assign {id} to")))
                .children(items),
        ))
    }

    fn render_add(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let input = self.add_input.as_ref()?;
        Some(centered(
            backdrop(theme, cx),
            panel(theme)
                .w(px(420.0))
                .gap_2()
                .child(panel_title(theme, "New issue"))
                .child(div().px_2().child(Input::new(input)))
                .child(
                    div()
                        .px_2()
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .child("Enter adds it to Backlog · Esc cancels"),
                ),
        ))
    }
}

impl Render for IssuesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sync_editor {
            self.sync_editor_text(window, cx);
        }
        if self.needs_focus && self.editor.is_none() && self.add_input.is_none() {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        if self.state.dragging().is_some() && !cx.has_active_drag() {
            self.state.end_drag();
        }
        // The pane's session follows its task (opened, switched, closed, or
        // gone from the board) as the pane draws it.
        self.sync_session(false, cx);
        let theme = *cx.global::<Theme>();
        let detail = self.render_detail(&theme, cx);
        if self.floating {
            // Over another screen: the pane and its assign picker, on a layer
            // filling the main area the window lays this over. The layer
            // itself takes no pointer or focus, so the screen around the
            // pane stays usable; the pane carries the view's keys.
            return div()
                .id("issues-over")
                .debug_selector(|| "issues-over".into())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .type_style(Type::Base)
                .text_color(theme.foreground)
                .children(detail)
                .children(self.render_assign(&theme, cx))
                .into_any_element();
        }
        let board = self.render_board(&theme, cx);
        div()
            .id("issues")
            .track_focus(&self.focus)
            .key_context("IssuesBoard")
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.nested(theme.canvas))
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(self.render_header(&theme, window, cx))
            .child(board)
            .child(self.render_footer(&theme))
            .children(detail)
            .children(self.render_dropdown(&theme, cx))
            .children(self.render_assign(&theme, cx))
            .children(self.render_add(&theme, cx))
            .into_any_element()
    }
}

impl IssuesView {
    /// The task pane, while it is up: the task, or its loading, missing or
    /// failed state in the same shell.
    fn render_detail(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.state.detail_id()?.to_string();
        let model = self.detail_model();
        let content = match &model {
            Some(d) => task_detail::PaneContent::Task(d),
            None if self.state.model().loading => task_detail::PaneContent::Loading,
            None => match self.state.model().error.clone() {
                Some(e) => task_detail::PaneContent::Failed(e),
                None => task_detail::PaneContent::Missing,
            },
        };
        Some(task_detail::render(self, &id, content, theme, cx))
    }
}

// ---- small pieces ------------------------------------------------------------

/// A fingerprint of a background read (its debug rendering hashed, off the
/// UI thread), to tell a refresh that read the same board again.
fn fingerprint(value: &impl std::fmt::Debug) -> u64 {
    use std::fmt::Write;
    use std::hash::Hasher;
    struct Feed(std::collections::hash_map::DefaultHasher);
    impl Write for Feed {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.write(s.as_bytes());
            Ok(())
        }
    }
    let mut feed = Feed(Default::default());
    let _ = write!(feed, "{value:?}");
    feed.0.finish()
}

/// The read to fold of `arrived` (a read and its fingerprint): none when it
/// matches `last`, the last one folded, unless a change is `pending` (a read
/// is what settles it). Records the fingerprint of what it lets through.
fn to_fold<T>(arrived: Option<(T, u64)>, last: &mut Option<u64>, pending: bool) -> Option<T> {
    let (read, print) = arrived?;
    if *last == Some(print) && !pending {
        return None;
    }
    *last = Some(print);
    Some(read)
}

/// The rows column `col` shows under its cards.
fn column_rows(col: &IssueColumn, model: &shelbi_app::IssuesModel) -> Vec<Item> {
    board_list::trailing(
        col.cards.is_empty(),
        col.category == shelbi_core::StatusCategory::Done,
        model.done_loading,
        model.has_more_done,
    )
}

/// Make `card` (row `ri` of column `ci`, the dragged card's own column) a
/// reorder target: hovering it marks the edge the dragged card lands on (above
/// when dragging up from row `from`, below when dragging down) and a drop
/// takes its place.
fn reorder_target(
    card: gpui::Stateful<gpui::Div>,
    target: &IssueCard,
    ci: usize,
    ri: usize,
    from: usize,
    theme: &Theme,
    cx: &mut Context<IssuesView>,
) -> gpui::Stateful<gpui::Div> {
    let theme = *theme;
    let target_id = target.id.clone();
    card.drag_over::<CardDrag>(move |s, _, _, _| {
        let s = s.border_color(theme.focus_ring);
        if ri < from {
            s.border_t_4()
        } else if ri > from {
            s.border_b_4()
        } else {
            s
        }
    })
    .on_drop(cx.listener(move |this, drag: &CardDrag, window, cx| {
        let intents = this.state.drop_on_card(&drag.id, ci, &target_id);
        this.handle_drop(intents, window, cx);
    }))
}

/// A card's meta lines: the workflow badge and the branch, then its
/// dependencies when it has any. The badge is on every card, as Figma draws
/// it, even on a one-workflow board (where the TUI leaves it off, see
/// `IssuesModel::show_workflow_badges`). The assignee isn't on the card, as
/// in Figma: the detail and the workspace filter carry it. On a shared remote
/// tracker a card that isn't the user's leads with whose it is (`@alice`,
/// `unassigned`), since Shelbi won't start it without an explicit take.
fn card_meta(card: &IssueCard, theme: &Theme) -> impl IntoElement {
    let owner = card
        .tracker_owner
        .as_ref()
        .filter(|o| !o.is_mine())
        .map(|o| o.label());
    let deps = (!card.depends_on.is_empty()).then(|| {
        let list = card
            .depends_on
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let label = if card.blocked { "blocked by" } else { "depends on" };
        format!("{label} {list}")
    });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .type_style(Type::XSmall)
        .text_color(theme.muted)
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .overflow_hidden()
                .when_some(owner, |d, o| d.child(div().flex_none().text_color(theme.text_loud).child(o)))
                .child(workflow_badge(card.workflow_name.clone(), theme))
                .when_some(card.branch.clone(), |d, b| d.child(div().min_w_0().truncate().child(b))),
        )
        .when_some(deps, |d, deps| d.child(div().truncate().child(deps)))
}

/// Figma's Workflow badge: the workflow's name, X-Small, on
/// `theme.workflow_badge_bg` with 2px corners.
fn workflow_badge(name: String, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .px_1()
        .rounded(px(2.0))
        .bg(theme.workflow_badge)
        .text_color(theme.workflow_badge_text)
        .child(name)
}

/// A legal drop target under a dragged card: the overlay fill and the focus
/// ring's color drawn as a 1px ring just inside it (no border, so nothing
/// moves). The ring is an inset shadow, painted over the fill: a drop shadow
/// is a filled box under the element and would show through the
/// translucent overlay fill as a solid block of the ring's color.
fn drop_highlight(s: gpui::StyleRefinement, theme: &Theme) -> gpui::StyleRefinement {
    s.bg(theme.background).shadow(vec![gpui::BoxShadow {
        color: theme.focus_ring,
        offset: gpui::point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(1.0),
        inset: true,
    }])
}

fn muted_row(theme: &Theme, text: &'static str) -> gpui::Div {
    div().px_2().py_1().type_style(Type::XSmall).text_color(theme.muted).child(text)
}

/// A menu or popover: Taskpage's raised surface with its ring and shadow.
pub(super) fn panel(theme: &Theme) -> gpui::Div {
    style::popover(div().flex().flex_col().py_2(), theme)
}

pub(super) fn panel_title(theme: &Theme, title: &str) -> impl IntoElement {
    style::heading(title.to_string(), theme).px_3().pt_1().pb_2()
}

fn option_row(theme: &Theme, selected: bool, label: String, count: Option<usize>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .justify_between()
        .mx_1()
        .px_2()
        .py_1()
        .rounded_lg()
        .cursor_pointer()
        .when(selected, |d| d.bg(theme.row_selected).text_color(theme.text_loud))
        .when(!selected, |d| d.hover(|s| s.bg(theme.background)))
        .child(label)
        .when_some(count, |d, n| d.child(div().text_color(theme.muted).child(n.to_string())))
}

/// A full-size layer over the board that swallows clicks and closes the
/// overlay when clicked outside its panel.
pub(super) fn backdrop(theme: &Theme, cx: &mut Context<IssuesView>) -> gpui::Stateful<gpui::Div> {
    let dim: Hsla = theme.scrim;
    div()
        .id("backdrop")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .bg(dim)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            if this.editor.is_none() {
                this.state.close_overlay();
                this.add_input = None;
                this.needs_focus = true;
                cx.notify();
            }
        }))
}

/// Center `child` in `layer`; the child swallows clicks so they don't reach
/// the backdrop.
pub(super) fn centered(
    layer: gpui::Stateful<gpui::Div>,
    child: gpui::Div,
) -> gpui::Stateful<gpui::Div> {
    layer.items_center().justify_center().child(
        child
            .id("overlay-panel")
            .max_h(relative(0.9))
            .on_click(|_, _, cx: &mut App| cx.stop_propagation()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_like_the_last_is_not_folded_unless_a_change_is_in_flight() {
        let mut last = None;
        assert_eq!(to_fold(Some(("a", 1)), &mut last, false), Some("a"), "the first read");
        assert_eq!(last, Some(1));
        assert_eq!(to_fold(Some(("a", 1)), &mut last, false), None, "the same board again");
        assert_eq!(to_fold(Some(("a", 1)), &mut last, true), Some("a"), "it may settle a change");
        assert_eq!(to_fold(Some(("b", 2)), &mut last, false), Some("b"), "the board changed");
        assert_eq!(to_fold::<&str>(None, &mut last, true), None, "nothing arrived");
        assert_eq!(last, Some(2));
        // A persisted filter clears it: the next read is folded either way.
        let mut cleared = None;
        assert_eq!(to_fold(Some(("b", 2)), &mut cleared, false), Some("b"));
    }

    /// The board in a window of its own, with `tasks` loaded (its disk
    /// reads stopped, so nothing folds over them) and the toolbar drawn.
    fn open_board(cx: &mut gpui::TestAppContext, tasks: Vec<shelbi_state::IssueFile>) -> (Entity<IssuesView>, gpui::VisualTestContext) {
        open_board_sized(cx, tasks, None)
    }

    /// [`open_board`] in a window of `size` (the board fills it, as it does
    /// the main area).
    fn open_board_sized(
        cx: &mut gpui::TestAppContext,
        tasks: Vec<shelbi_state::IssueFile>,
        size: Option<gpui::Size<gpui::Pixels>>,
    ) -> (Entity<IssuesView>, gpui::VisualTestContext) {
        use crate::views::board_state::tests::read;
        cx.update(|cx| {
            cx.set_global(Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let options = gpui::WindowOptions {
            window_bounds: size.map(|size| gpui::WindowBounds::Windowed(gpui::Bounds::new(gpui::Point::default(), size))),
            ..Default::default()
        };
        let handle = cx.update(|cx| {
            cx.open_window(options, |_, cx| {
                cx.new(|cx| IssuesView::new("dt-search-clear-test".into(), cx))
            })
            .unwrap()
        });
        let mut vcx = gpui::VisualTestContext::from_window(handle.into(), cx);
        let view = handle.root(&mut vcx).unwrap();
        view.update(&mut vcx, |v, cx| {
            v.stop_workers();
            v.state.apply_reads(Some(read(tasks)), Some(None));
            cx.notify();
        });
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        vcx.run_until_parked();
        (view, vcx)
    }

    fn drawn(cx: &mut gpui::VisualTestContext, selector: &'static str) -> bool {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        cx.debug_bounds(selector).is_some()
    }

    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        assert!(drawn(cx, selector), "{selector} is drawn");
        let at = cx.debug_bounds(selector).unwrap().center();
        cx.simulate_click(at, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn cards(view: &Entity<IssuesView>, cx: &mut gpui::VisualTestContext) -> usize {
        view.read_with(cx, |v, _| v.state.model().columns.iter().map(|c| c.cards.len()).sum())
    }

    fn search_focused(view: &Entity<IssuesView>, cx: &mut gpui::VisualTestContext) -> bool {
        let view = view.clone();
        cx.update(|window, cx| view.read(cx).search_focused(window, cx))
    }

    #[gpui::test]
    fn the_clear_button_shows_with_a_query_and_clears_it(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        let mut tasks = vec![issue("A", "todo"), issue("B", "todo"), issue("C", "in-progress")];
        tasks[0].task.title = "Sidebar collapse".into();
        let (view, mut cx) = open_board(cx, tasks);
        assert!(!drawn(&mut cx, "board-search-clear"), "hidden while the box is empty");
        click(&mut cx, "board-search");
        assert!(search_focused(&view, &mut cx));
        assert!(!drawn(&mut cx, "board-search-clear"), "focus alone doesn't show it");

        cx.simulate_keystrokes("s i d e");
        assert_eq!(view.read_with(&cx, |v, _| v.state.search().to_string()), "side");
        assert_eq!(cards(&view, &mut cx), 1);
        assert!(drawn(&mut cx, "board-search-clear"), "shown with a query");
        // The text area ends before the button, inside the box.
        let field = cx.debug_bounds("board-search-field").unwrap();
        let clear = cx.debug_bounds("board-search-clear").unwrap();
        let bx = cx.debug_bounds("board-search").unwrap();
        assert!(field.right() <= clear.left(), "{field:?} runs under {clear:?}");
        assert!(clear.right() <= bx.right() && clear.top() >= bx.top() && clear.bottom() <= bx.bottom());

        click(&mut cx, "board-search-clear");
        assert_eq!(view.read_with(&cx, |v, _| v.state.search().to_string()), "");
        let value = view.read_with(&cx, |v, cx| v.search_input.as_ref().unwrap().read(cx).value().to_string());
        assert_eq!(value, "", "the field is emptied too");
        assert_eq!(cards(&view, &mut cx), 3, "every card is back");
        assert!(!drawn(&mut cx, "board-search-clear"), "hidden again");
        assert!(search_focused(&view, &mut cx), "the keyboard stays in the field");
        cx.simulate_keystrokes("b");
        cx.run_until_parked();
        assert_eq!(view.read_with(&cx, |v, _| v.state.search().to_string()), "b", "ready for the next search");
    }

    #[gpui::test]
    fn a_filter_chip_shows_a_chevron_and_anchors_its_menu(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        let (view, mut cx) = open_board(cx, vec![issue("A", "todo")]);
        for (chip, label, right) in [
            ("workflow-chip", "workflow-chip-label", "workflow-chip-right"),
            ("workspace-chip", "workspace-chip-label", "workspace-chip-right"),
        ] {
            assert!(drawn(&mut cx, right), "{chip} has a chevron");
            let bx = cx.debug_bounds(chip).unwrap();
            let label = cx.debug_bounds(label).unwrap();
            let chevron = cx.debug_bounds(right).unwrap();
            assert!(label.right() <= chevron.left(), "{label:?} runs under {chevron:?}");
            assert!(chevron.left() < bx.right() && chevron.top() >= bx.top() && chevron.bottom() <= bx.bottom());
        }

        click(&mut cx, "workflow-chip");
        assert!(view.read_with(&cx, |v, _| matches!(v.state.mode, Mode::WorkflowDropdown { .. })));
        assert!(drawn(&mut cx, "dropdown-panel"));
        let chip = cx.debug_bounds("workflow-chip").unwrap();
        let panel = cx.debug_bounds("dropdown-panel").unwrap();
        assert!(panel.top() >= chip.bottom(), "the menu opens under {chip:?}: {panel:?}");
        assert_eq!(panel.left(), chip.left(), "the menu lines up with its chip");
    }


    // ---- the task pane (`dt-floating-task-pane`) ---------------------------

    /// A card in each of the default workflow's columns, more than a 1160pt
    /// board shows at once, so it scrolls sideways.
    fn wide_board() -> Vec<shelbi_state::IssueFile> {
        use crate::views::board_state::tests::issue;
        ["backlog", "todo", "in-progress", "review", "done", "canceled"]
            .iter()
            .enumerate()
            .map(|(i, status)| issue(&format!("T{i}"), status))
            .collect()
    }

    fn bounds(cx: &mut gpui::VisualTestContext, selector: &'static str) -> gpui::Bounds<gpui::Pixels> {
        assert!(drawn(cx, selector), "{selector} is drawn");
        cx.debug_bounds(selector).unwrap()
    }

    fn pane_id(view: &Entity<IssuesView>, cx: &gpui::VisualTestContext) -> Option<String> {
        view.read_with(cx, |v, _| v.state.detail_id().map(str::to_string))
    }

    fn board_focused(view: &Entity<IssuesView>, cx: &mut gpui::VisualTestContext) -> bool {
        let view = view.clone();
        cx.update(|window, cx| view.read(cx).focus.is_focused(window))
    }

    /// Every `IssuesEvent` the view emits from here on.
    fn record(view: &Entity<IssuesView>, cx: &mut gpui::VisualTestContext) -> std::rc::Rc<std::cell::RefCell<Vec<CommandKind>>> {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(view, move |_, ev: &IssuesEvent, _| {
                if let IssuesEvent::Command(kind) = ev {
                    sink.borrow_mut().push(kind.clone());
                }
            })
            .detach()
        });
        events
    }

    #[gpui::test]
    fn the_pane_floats_at_its_insets_and_leaves_the_board_as_it_was(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        // The main area of the 1440 x 1024 frame, beside its 280pt sidebar.
        let (view, mut cx) = open_board_sized(cx, wide_board(), Some(gpui::size(px(1160.0), px(1024.0))));
        view.update(&mut cx, |v, cx| {
            v.board_scroll.handle.set_offset(gpui::point(px(-120.0), px(0.0)));
            cx.notify();
        });
        let columns = bounds(&mut cx, "board-columns");
        let offset = view.read_with(&cx, |v, _| v.board_scroll.handle.offset());
        assert_eq!(offset.x, px(-120.0), "the board scrolls sideways");
        let selected = view.read_with(&cx, |v, _| (v.state.selected_column, v.state.selected_row));

        click(&mut cx, "card-T2");
        assert_eq!(pane_id(&view, &cx).as_deref(), Some("T2"));
        let pane = bounds(&mut cx, "task-pane");
        // 640 x 1008, 8pt from the top, right and bottom: x 792 in the
        // frame's window, 512 in its main area.
        assert_eq!(pane, gpui::Bounds::new(gpui::point(px(512.0), px(8.0)), gpui::size(px(640.0), px(1008.0))));
        assert_eq!(bounds(&mut cx, "board-columns"), columns, "the board keeps its width and place");
        assert_eq!(view.read_with(&cx, |v, _| v.board_scroll.handle.offset()), offset, "no auto-scroll");
        assert!(!drawn(&mut cx, "backdrop"), "no scrim over the board");
        let now = view.read_with(&cx, |v, _| (v.state.selected_column, v.state.selected_row));
        assert_ne!(now, selected, "the clicked card is selected");

        // Closing it leaves the board where it was too.
        click(&mut cx, "pane-close");
        assert!(!drawn(&mut cx, "task-pane"));
        assert_eq!(bounds(&mut cx, "board-columns"), columns);
        assert_eq!(view.read_with(&cx, |v, _| v.board_scroll.handle.offset()), offset);
        assert_eq!(view.read_with(&cx, |v, _| (v.state.selected_column, v.state.selected_row)), now);
        assert!(board_focused(&view, &mut cx), "the keys are back on the board");
    }

    #[gpui::test]
    fn a_narrow_window_narrows_the_pane_inside_its_insets(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut cx) = open_board_sized(cx, vec![issue("A", "todo")], Some(gpui::size(px(600.0), px(500.0))));
        view.update(&mut cx, |v, cx| v.open_task("A", cx));
        let pane = bounds(&mut cx, "task-pane");
        assert_eq!(pane, gpui::Bounds::new(gpui::point(px(8.0), px(8.0)), gpui::size(px(584.0), px(484.0))));
        // The scroll inside it clips to it: nothing runs past its edges.
        let scroll = bounds(&mut cx, "pane-scroll");
        assert!(scroll.right() <= pane.right() && scroll.bottom() <= pane.bottom(), "{scroll:?} in {pane:?}");
        let title = bounds(&mut cx, "pane-title");
        assert!(title.right() <= pane.right(), "a long title wraps inside: {title:?}");
    }

    #[gpui::test]
    fn the_pane_slides_in_from_the_right(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        let (view, mut cx) = open_board_sized(cx, vec![issue("A", "todo")], Some(gpui::size(px(1160.0), px(1024.0))));
        view.update(&mut cx, |v, cx| v.open_task("A", cx));
        let first = bounds(&mut cx, "task-pane");
        assert!(first.left() > px(512.0), "it starts to the right of its place: {first:?}");
        std::thread::sleep(super::super::column_swap::SLIDE + Duration::from_millis(60));
        let settled = bounds(&mut cx, "task-pane");
        assert_eq!(settled.left(), px(512.0), "and lands 8pt in from the right");
        assert_eq!(settled.top(), first.top(), "sliding, not dropping");
    }

    #[gpui::test]
    fn another_card_shows_in_the_one_pane(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut cx) = open_board_sized(cx, wide_board(), Some(gpui::size(px(1160.0), px(1024.0))));
        click(&mut cx, "card-T1");
        assert_eq!(pane_id(&view, &cx).as_deref(), Some("T1"));
        // The board beside the pane takes clicks: another card switches it.
        click(&mut cx, "card-T0");
        assert_eq!(pane_id(&view, &cx).as_deref(), Some("T0"));
        assert!(drawn(&mut cx, "task-pane"));
        let heading = view.read_with(&cx, |v, _| v.state.mode.clone());
        assert_eq!(heading, Mode::Detail { id: "T0".into(), editing: false }, "and the keys are in it");
        // Esc closes it.
        cx.simulate_keystrokes("escape");
        assert_eq!(pane_id(&view, &cx), None);
        assert!(!drawn(&mut cx, "task-pane"));
        assert!(board_focused(&view, &mut cx));
    }

    #[gpui::test]
    fn a_board_menu_opens_over_the_pane_and_closes_back_to_it(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut cx) = open_board_sized(cx, wide_board(), Some(gpui::size(px(1160.0), px(1024.0))));
        click(&mut cx, "card-T1");
        click(&mut cx, "workflow-chip");
        assert!(view.read_with(&cx, |v, _| matches!(v.state.mode, Mode::WorkflowDropdown { .. })));
        assert!(drawn(&mut cx, "task-pane"), "the pane stays up under the board's menu");
        // Esc belongs to the menu first, then to the pane.
        cx.simulate_keystrokes("escape");
        assert_eq!(view.read_with(&cx, |v, _| v.state.mode.clone()), Mode::Detail { id: "T1".into(), editing: false });
        assert!(drawn(&mut cx, "task-pane"));
    }

    #[gpui::test]
    fn more_lists_the_task_operations_and_runs_them(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut cx) = open_board_sized(cx, vec![issue("A", "todo")], Some(gpui::size(px(1160.0), px(1024.0))));
        let events = record(&view, &mut cx);
        view.update(&mut cx, |v, cx| v.open_task("A", cx));
        assert!(!drawn(&mut cx, "pane-menu"));
        click(&mut cx, "pane-more");
        assert!(drawn(&mut cx, "pane-menu"));
        for row in ["pane-menu-move-left", "pane-menu-move-right", "pane-menu-start", "pane-menu-assign", "pane-menu-edit", "pane-menu-editor"] {
            assert!(drawn(&mut cx, row), "More lists {row}");
        }
        assert!(!drawn(&mut cx, "pane-menu-workspace"), "no workspace to open: unassigned");
        let pane = bounds(&mut cx, "task-pane");
        let menu = bounds(&mut cx, "pane-menu");
        assert!(menu.right() <= pane.right(), "the menu hangs inside the pane: {menu:?}");

        click(&mut cx, "pane-menu-start");
        assert_eq!(*events.borrow(), vec![CommandKind::StartIssue { id: "A".into() }]);
        assert!(!drawn(&mut cx, "pane-menu"), "a choice closes the menu");
        assert_eq!(pane_id(&view, &cx).as_deref(), Some("A"), "the pane stays");

        // Esc on the open menu closes the menu, not the pane.
        click(&mut cx, "pane-more");
        cx.simulate_keystrokes("escape");
        assert!(!drawn(&mut cx, "pane-menu"));
        assert!(drawn(&mut cx, "task-pane"));
        // Its keys: down to Move right, Enter.
        click(&mut cx, "pane-more");
        cx.simulate_keystrokes("down enter");
        assert!(matches!(events.borrow().last(), Some(CommandKind::MoveIssue { .. })));
    }

    #[gpui::test]
    fn a_property_value_opens_its_picker_under_it_and_a_choice_is_sent(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut cx) = open_board_sized(cx, vec![issue("A", "todo"), issue("B", "todo")], Some(gpui::size(px(1160.0), px(1024.0))));
        let events = record(&view, &mut cx);
        view.update(&mut cx, |v, cx| v.open_task("A", cx));
        for value in ["pane-status", "pane-workflow", "pane-priority"] {
            assert!(drawn(&mut cx, value), "{value} is a button");
        }
        assert!(!drawn(&mut cx, "property-picker"));
        // No edit mode: one click on the value opens its picker, under it.
        click(&mut cx, "pane-status");
        assert!(drawn(&mut cx, "property-picker"));
        let value = bounds(&mut cx, "pane-status");
        let menu = bounds(&mut cx, "property-picker");
        assert!(menu.top() >= value.bottom() && (menu.left() - value.left()).abs() < px(1.0), "{menu:?} under {value:?}");
        assert!(drawn(&mut cx, "picker-current-1"), "Todo is checked");
        // A press outside closes it, and the pane stays.
        let pane = bounds(&mut cx, "task-pane");
        cx.simulate_click(gpui::point(pane.left() + px(40.0), pane.bottom() - px(40.0)), gpui::Modifiers::none());
        assert!(!drawn(&mut cx, "property-picker"));
        assert!(drawn(&mut cx, "task-pane"));
        // A choice is the registry's command; the picker closes on the pane.
        click(&mut cx, "pane-status");
        click(&mut cx, "picker-status-2");
        assert_eq!(
            *events.borrow(),
            vec![CommandKind::MoveIssue { id: "A".into(), to_status: "in-progress".into() }]
        );
        assert!(!drawn(&mut cx, "property-picker"));
        assert_eq!(view.read_with(&cx, |v, _| v.state.mode.clone()), Mode::Detail { id: "A".into(), editing: false });
        // The keyboard: Tab to Priority, Enter opens it, End, Enter.
        cx.simulate_keystrokes("tab tab tab enter");
        assert!(drawn(&mut cx, "picker-priority-1"));
        cx.simulate_keystrokes("end enter");
        assert_eq!(
            events.borrow().last(),
            Some(&CommandKind::PrioritizeIssue { id: "A".into(), position: 1 })
        );
        assert_eq!(view.read_with(&cx, |v, _| v.state.pane_focus()), Some(crate::views::board_state::Property::Priority));
    }

    #[gpui::test]
    fn a_task_the_board_lacks_shows_in_the_pane_shell(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut cx) = open_board_sized(cx, vec![issue("A", "todo")], Some(gpui::size(px(1160.0), px(1024.0))));
        view.update(&mut cx, |v, cx| v.open_task("gone", cx));
        assert!(drawn(&mut cx, "task-pane"), "the floating shell, not a centered modal");
        assert!(drawn(&mut cx, "pane-state"));
        assert!(!drawn(&mut cx, "backdrop"));
        assert_eq!(bounds(&mut cx, "task-pane").left(), px(512.0));
        click(&mut cx, "pane-close");
        assert!(!drawn(&mut cx, "task-pane"));
    }

    #[gpui::test]
    fn esc_in_the_body_editor_keeps_the_draft(cx: &mut gpui::TestAppContext) {
        use crate::views::board_state::tests::issue;
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut cx) = open_board_sized(cx, vec![issue("A", "todo")], Some(gpui::size(px(1160.0), px(1024.0))));
        let sent = |cx: &gpui::VisualTestContext| {
            view.read_with(cx, |v, _| v.sent_saves.iter().map(|j| (j.id.clone(), j.body.clone())).collect::<Vec<_>>())
        };
        view.update(&mut cx, |v, cx| v.open_task("A", cx));
        assert!(drawn(&mut cx, "task-pane"));
        cx.simulate_keystrokes("e");
        assert!(view.read_with(&cx, |v, _| v.editor.is_some()), "e opens the body editor");
        cx.simulate_input("A new draft");
        // Esc leaves the editor with its draft saved, and the pane up.
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(&cx, |v, _| v.editor.is_none()));
        assert_eq!(sent(&cx), vec![("A".into(), Some("A new draftBody of A".into()))], "the draft is saved, not dropped");
        assert_eq!(pane_id(&view, &cx).as_deref(), Some("A"), "the pane stays");
        assert!(drawn(&mut cx, "pane-save-status"), "the pane says the save is out");

        // Reopened, the editor is on the draft; unchanged, Esc sends nothing
        // more (the first save is still out).
        cx.simulate_keystrokes("e");
        let text = view.read_with(&cx, |v, cx| v.editor.as_ref().map(|(_, s)| s.read(cx).value().to_string()));
        assert_eq!(text.as_deref(), Some("A new draftBody of A"));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(&cx, |v, _| v.editor.is_none()), "Esc in the editor reaches the pane");
        assert_eq!(sent(&cx).len(), 1, "nothing more to save");
        assert_eq!(pane_id(&view, &cx).as_deref(), Some("A"));

        // Close saves a changed draft too, and closes the pane.
        cx.simulate_keystrokes("e");
        cx.simulate_input(" again");
        view.update(&mut cx, |v, cx| v.close_pane(cx));
        assert!(view.read_with(&cx, |v, _| v.editor.is_none()));
        assert_eq!(pane_id(&view, &cx), None);
        // Typing during a save is kept and saved after it lands: still the
        // one save out, with the newer text queued as the draft.
        assert_eq!(sent(&cx).len(), 1);
        let draft = view.read_with(&cx, |v, _| v.state.draft_text("A", Field::Body));
        assert!(draft.as_ref().is_some_and(|d| d.contains(" again") && d.contains("A new draft")), "{draft:?}");
    }

    #[test]
    fn a_fingerprint_tells_reads_apart() {
        let read = |title: &str| vec![("id-1", title.to_string()), ("id-2", "Other".to_string())];
        assert_eq!(fingerprint(&read("Title")), fingerprint(&read("Title")));
        assert_ne!(fingerprint(&read("Title")), fingerprint(&read("Retitled")));
    }
}
