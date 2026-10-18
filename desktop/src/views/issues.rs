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

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, relative, App, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Hsla, KeyDownEvent, ScrollHandle, SharedString, Subscription, Task,
    Window,
};
use shelbi_app::board::DropTarget;
use gpui_kit::component::input::{Input, InputEvent, InputState, TextareaState};
use shelbi_app::board::{self, BoardRead, ClosedPage};
use shelbi_app::{
    CommandKind, ExecError, ExpectedState, IssueCard, IssueColumn, TaskDetailModel,
};
use shelbi_state::keymap::{DisplayStyle, KanbanAction, Keymaps};

use super::board_scroll;
use super::board_state::{BoardState, Intent, Mode, MutationJob, Scroll};
use super::external_edit::{self, EditorEvent};
use super::task_detail;
use crate::assets::{heroicon, icons};
use crate::keys::chord_from_keystroke;
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
/// A collapsed column's width.
const COLLAPSED_WIDTH: f32 = 36.0;
/// The toolbar's height, its rule included (Figma: a 56px Board toolbar over
/// a 1px divider).
const TOOLBAR_H: f32 = 57.0;
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
    More(Result<ClosedPage, String>),
    Persisted(Result<(), String>),
}

pub struct IssuesView {
    /// The filter controls, measured so their menus open under them.
    workflow_anchor: style::Anchor,
    workspace_anchor: style::Anchor,
    pub(super) state: BoardState,
    pub(super) focus: FocusHandle,
    needs_focus: bool,
    open_refresher: shelbi_app::Refresher<BoardRead>,
    closed_refresher: shelbi_app::Refresher<Option<ClosedPage>>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    editor_tx: Sender<EditorEvent>,
    editor_rx: Receiver<EditorEvent>,
    last_refresh: Instant,
    load_more_in_flight: bool,
    board_scroll: ScrollHandle,
    column_scrolls: Vec<ScrollHandle>,
    pub(super) detail_scroll: ScrollHandle,
    /// The in-place body editor: the issue id and its text state.
    pub(super) editor: Option<(String, Entity<TextareaState>)>,
    add_input: Option<Entity<InputState>>,
    display_style: DisplayStyle,
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
        let open_refresher = shelbi_app::spawn_worker(move |_| board::read_open_board(&p));
        let p = project.clone();
        let closed_refresher = shelbi_app::spawn_worker(move |_| board::read_closed_page(&p));
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
            done_tx,
            done_rx,
            editor_tx,
            editor_rx,
            last_refresh: Instant::now(),
            load_more_in_flight: false,
            board_scroll: ScrollHandle::new(),
            column_scrolls: Vec::new(),
            workflow_anchor: Default::default(),
            workspace_anchor: Default::default(),
            detail_scroll: ScrollHandle::new(),
            editor: None,
            add_input: None,
            display_style: DisplayStyle::detect(),
            _subscriptions: Vec::new(),
            _poll: poll,
        }
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
    }

    /// Take keyboard focus on the next frame (the window calls this when the
    /// view is shown).
    pub fn request_focus(&mut self) {
        self.needs_focus = true;
    }

    /// The board's state, for the offscreen renders.
    #[cfg(feature = "visual-check")]
    pub fn state_mut(&mut self) -> &mut BoardState {
        &mut self.state
    }

    /// Drain the board's reads now, for the offscreen renders.
    #[cfg(feature = "visual-check")]
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        self.tick(cx);
    }

    /// Open task `id`'s detail (from an Activity row). A task the board hasn't
    /// loaded yet fills in when its read lands.
    pub fn open_task(&mut self, id: &str, cx: &mut Context<Self>) {
        self.state.open_detail(id);
        self.needs_focus = true;
        cx.notify();
    }

    /// Drain the refreshers and background results; re-read on the cadence.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(read) = self.open_refresher.latest() {
            self.state.apply_open(read);
            changed = true;
        }
        if let Some(page) = self.closed_refresher.latest() {
            self.state.apply_closed(page);
            changed = true;
        }
        while let Ok(done) = self.done_rx.try_recv() {
            changed = true;
            match done {
                Done::Mutation { id, result } => {
                    let intents = self.state.finish_mutation(&id, result);
                    self.apply_intents_bg(intents);
                }
                Done::More(page) => {
                    self.load_more_in_flight = false;
                    self.state.apply_more(page);
                }
                Done::Persisted(Err(e)) => self.state.status = Some(e),
                Done::Persisted(Ok(())) => {}
            }
        }
        while let Ok(ev) = self.editor_rx.try_recv() {
            changed = true;
            match ev {
                EditorEvent::Saved { id, body } => {
                    for intent in self.state.save_external_edit(&id, body) {
                        if let Intent::Command(kind) = intent {
                            cx.emit(IssuesEvent::Command(kind));
                        }
                    }
                }
                EditorEvent::Failed { id, error } => {
                    self.state.status = Some(format!("{id}: {error}"));
                }
            }
        }
        // A drag released off the board (or cancelled) never reaches a drop
        // handler; let go of its column styling.
        if self.state.dragging().is_some() && !cx.has_active_drag() {
            self.state.end_drag();
            changed = true;
        }
        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresh();
        }
        if changed {
            cx.notify();
        }
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

    /// Intents that need no window (from a background result).
    fn apply_intents_bg(&mut self, intents: Vec<Intent>) {
        for intent in intents {
            if intent == Intent::Refresh {
                self.refresh();
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
                Intent::OpenInEditor(id) => self.open_in_editor(&id),
                Intent::BeginAdd => self.begin_add(window, cx),
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

    fn load_more(&mut self) {
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
        let body = self.body_of(id).unwrap_or_default();
        let state = cx.new(|cx| TextareaState::new(window, cx).default_value(body));
        state.update(cx, |s, cx| s.focus(window, cx));
        self.editor = Some((id.to_string(), state));
    }

    /// Save the in-place editor's text through `EditIssue`.
    pub(super) fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some((id, state)) = self.editor.take() else {
            return;
        };
        let body = state.read(cx).value().to_string();
        self.needs_focus = true;
        for intent in self.state.save_edit(&id, body) {
            if let Intent::Command(kind) = intent {
                cx.emit(IssuesEvent::Command(kind));
            }
        }
        cx.notify();
    }

    pub(super) fn cancel_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        if let Mode::Detail { id, .. } = &self.state.mode {
            self.state.mode = Mode::Detail {
                id: id.clone(),
                editing: false,
            };
        }
        self.needs_focus = true;
        cx.notify();
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

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chord) = chord_from_keystroke(&ev.keystroke) else {
            return;
        };
        // The in-place editor saves on Cmd+S / Cmd+Enter.
        if self.editor.is_some() {
            let m = &ev.keystroke.modifiers;
            if m.platform && (ev.keystroke.key == "s" || ev.keystroke.key == "enter") {
                self.save_editor(cx);
                cx.stop_propagation();
                return;
            }
        }
        let before = (self.state.selected_column, self.state.selected_row);
        let Some(intents) = self.state.handle_key(chord) else {
            return;
        };
        cx.stop_propagation();
        // Leaving the editor / prompt with Escape drops its widget.
        if !matches!(self.state.mode, Mode::Detail { editing: true, .. }) && self.editor.is_some() {
            self.editor = None;
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
        cx.notify();
    }

    fn scroll_selection_into_view(&self) {
        self.board_scroll.scroll_to_item(self.state.selected_column);
        if let Some(h) = self.column_scrolls.get(self.state.selected_column) {
            h.scroll_to_item(self.state.selected_row);
        }
    }

    pub(super) fn hint(&self, chord: Option<&shelbi_state::keymap::KeyChord>) -> String {
        chord.map_or_else(|| "–".to_string(), |c| crate::keys::chord_text(c, self.display_style))
    }

    fn keymaps(&self) -> &Keymaps {
        self.state.keymaps()
    }

    // ---- rendering --------------------------------------------------------

    /// The toolbar: the two filters and, at the right, New issue, over the
    /// board's rule (the Figma Issues frame has no board title).
    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.state.model();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pl_3()
            .pr_4()
            .h(px(TOOLBAR_H))
            .flex_none()
            .border_b_1()
            .border_color(theme.divider_strong)
            .child(
                style::dropdown(
                    "workflow-chip",
                    icons::FUNNEL_BUTTON,
                    model.workflow_filter_label.clone(),
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
                style::dropdown(
                    "workspace-chip",
                    icons::FUNNEL_BUTTON,
                    model.workspace_filter_label.clone(),
                    matches!(self.state.mode, Mode::WorkspaceDropdown { .. }),
                    theme,
                )
                    .child(style::anchor_probe(&self.workspace_anchor))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.open_workspace_dropdown();
                        cx.notify();
                    })),
            )
            .when_some(model.banner.clone(), |row, banner| {
                row.child(div().type_style(Type::XSmall).text_color(theme.muted).child(banner))
            })
            .child(div().flex_1())
            .when(self.state.has_pending(), |row| {
                row.child(
                    heroicon(icons::ARROW_PATH).text_color(theme.icon),
                )
            })
            .child(
                style::toolbar_button("new-issue", icons::PLUS, "New issue", theme)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        let intents = this.state.begin_add();
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    })),
            )
    }

    fn render_board(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let columns = self.state.model().columns.clone();
        while self.column_scrolls.len() < columns.len() {
            self.column_scrolls.push(ScrollHandle::new());
        }
        let children: Vec<_> = columns
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
        board_scroll::with_shift_wheel(board, &self.board_scroll)
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
        let letters: Vec<_> = col
            .label
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| div().child(c.to_string()))
            .collect();
        let theme = *theme;
        // Figma draws no collapsed column: the strip keeps the column header's
        // pieces (the glyph, the count, the caps label run down the strip) on
        // the canvas, with a hover fill.
        div()
            .id(("collapsed", ci))
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .flex_none()
            .w(px(COLLAPSED_WIDTH))
            .h_full()
            .py(px(4.0))
            .rounded_lg()
            .type_style(Type::XSmallSemibold)
            .text_color(if focused { theme.text_loud } else { theme.muted })
            .cursor_pointer()
            .hover(move |s| s.bg(theme.background))
            .child(div().flex().items_center().h(px(16.0)).child(style::status_icon(col.category, &theme)))
            .child(
                div()
                    .type_style(Type::XSmall)
                    .text_color(theme.muted)
                    .child(col.cards.len().to_string()),
            )
            .children(letters)
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
        let model = self.state.model();
        let focused = ci == self.state.selected_column;
        let is_done = col.category == shelbi_core::StatusCategory::Done;
        let drop = self.state.dragging().map(|id| self.state.drop_target(id, ci));
        // In the dragged card's own column each card is a reorder target; the
        // row it was dragged from says which edge marks the landing spot.
        let reorder_from = match drop {
            Some(DropTarget::Reorder) => self
                .state
                .dragging()
                .and_then(|id| col.cards.iter().position(|c| c.id == id)),
            _ => None,
        };
        let cards: Vec<_> = col
            .cards
            .iter()
            .enumerate()
            .map(|(ri, card)| {
                // While the detail is open the keyboard is in it, so the
                // card under the scrim drops its focus ring (as the frame
                // draws it).
                let selected = focused && ri == self.state.selected_row && self.state.detail_id().is_none();
                let card_el = self.render_card(card, ci, ri, selected, theme, cx);
                match reorder_from {
                    Some(from) => reorder_target(card_el, card, ci, ri, from, theme, cx)
                        .into_any_element(),
                    None => card_el.into_any_element(),
                }
            })
            .collect();
        let refused = match &drop {
            Some(DropTarget::Refused(msg)) => Some(msg.clone()),
            _ => None,
        };
        let drop_hint = match &drop {
            Some(DropTarget::Move { merge: true }) => Some("Drop to run the gated merge"),
            Some(DropTarget::Move { merge: false }) => Some("Drop to move here"),
            _ => None,
        };
        let empty = col.cards.is_empty();
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
        let body = board_scroll::column_body(
            div()
                .id(("column", ci))
                .flex()
                .flex_col()
                .gap_2()
                .flex_1()
                .min_h_0()
                // The clip reaches GLOW_ROOM past the column on the sides and
                // top, so the focus glow shows while the cards keep the
                // column's width; the bottom padding is the room under the
                // last card once scrolled to the end.
                .mx(px(-GLOW_ROOM))
                .mt(px(-GLOW_ROOM))
                .px(px(GLOW_ROOM))
                .pt(px(GLOW_ROOM))
                .pb_3(),
            &self.column_scrolls[ci],
        )
        .children(cards)
        .when(empty && !(is_done && model.done_loading), |d| {
            d.child(if model.loading {
                muted_row(theme, "Loading board…").into_any_element()
            } else {
                // An empty column: what goes here and how to put it there.
                div()
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
                    .into_any_element()
            })
        })
        .when(is_done && model.done_loading, |d| {
            d.child(muted_row(theme, "Loading history…"))
        })
        .when(is_done && model.has_more_done, |d| {
            let hint = "m";
            d.child(
                div()
                    .id("load-more")
                    .px_2()
                    .py_1()
                    .type_style(Type::XSmall)
                    .text_color(theme.link)
                    .hover(|s| s.text_color(theme.link_hover))
                    .cursor_pointer()
                    .child(if self.load_more_in_flight {
                        "Loading…".to_string()
                    } else {
                        format!("Load more ({hint})")
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.load_more();
                        cx.notify();
                    })),
            )
        });
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
        let title_color = if card.blocked {
            theme.muted
        } else {
            theme.text_loud
        };
        div()
            .id(SharedString::from(format!("card-{}", card.id)))
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
        if self.needs_focus && self.editor.is_none() && self.add_input.is_none() {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        if self.state.dragging().is_some() && !cx.has_active_drag() {
            self.state.end_drag();
        }
        let theme = *cx.global::<Theme>();
        let detail = self
            .state
            .detail_id()
            .and_then(|id| TaskDetailModel::build(self.state.input(), id));
        let detail_missing = self.state.detail_id().is_some() && detail.is_none();
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
            .bg(theme.canvas)
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(self.render_header(&theme, cx))
            .child(board)
            .child(self.render_footer(&theme))
            .when_some(detail, |root, d| {
                root.child(task_detail::render(self, &d, &theme, cx))
            })
            .when(detail_missing, |root| {
                root.child(centered(
                    backdrop(&theme, cx),
                    panel(&theme)
                        .w(px(360.0))
                        .child(panel_title(&theme, "This issue is no longer on the board"))
                        .child(muted_row(&theme, "Press Esc to close.")),
                ))
            })
            .children(self.render_dropdown(&theme, cx))
            .children(self.render_assign(&theme, cx))
            .children(self.render_add(&theme, cx))
    }
}

// ---- small pieces ------------------------------------------------------------

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
/// in Figma: the detail and the workspace filter carry it.
fn card_meta(card: &IssueCard, theme: &Theme) -> impl IntoElement {
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
