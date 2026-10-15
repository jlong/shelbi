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
    FocusHandle, FontWeight, Hsla, KeyDownEvent, ScrollHandle, SharedString, Subscription, Task,
    Window,
};
use shelbi_app::board::DropTarget;
use gpui_kit::component::input::{Input, InputEvent, InputState, TextareaState};
use shelbi_app::board::{self, BoardRead, ClosedPage};
use shelbi_app::{CommandKind, ExecError, IssueCard, IssueColumn, TaskDetailModel};
use shelbi_state::keymap::{format_chord, DisplayStyle, KanbanAction, Keymaps};

use super::board_scroll;
use super::board_state::{BoardState, Intent, Mode, MutationJob, Scroll};
use super::external_edit::{self, EditorEvent};
use super::task_detail;
use crate::assets::{heroicon, icons, Icon};
use crate::keys::chord_from_keystroke;
use crate::theme::Theme;

/// The fallback refresh cadence (the TUI's).
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
/// How often the view drains its channels.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// An expanded column's width.
const COLUMN_WIDTH: f32 = 272.0;
/// A collapsed column's width.
const COLLAPSED_WIDTH: f32 = 36.0;

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
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme.gray)
            .bg(theme.selection)
            .opacity(0.9)
            .shadow_lg()
            .text_sm()
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.white)
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(self.title.clone()),
            )
            .child(div().text_xs().text_color(theme.muted).child(self.id.clone()))
    }
}

/// A background result.
enum Done {
    Mutation {
        id: String,
        result: Result<(), ExecError>,
    },
    More(Result<ClosedPage, String>),
    Persisted(Result<(), String>),
}

pub struct IssuesView {
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
            detail_scroll: ScrollHandle::new(),
            editor: None,
            add_input: None,
            display_style: DisplayStyle::detect(),
            _subscriptions: Vec::new(),
            _poll: poll,
        }
    }

    /// Take keyboard focus on the next frame (the window calls this when the
    /// view is shown).
    pub fn request_focus(&mut self) {
        self.needs_focus = true;
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
        chord.map_or_else(|| "–".to_string(), |c| format_chord(c, self.display_style))
    }

    fn keymaps(&self) -> &Keymaps {
        self.state.keymaps()
    }

    // ---- rendering --------------------------------------------------------

    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.state.model();
        let title = if model.title.is_empty() {
            self.state.project.clone()
        } else {
            model.title.clone()
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.white)
                    .child(title),
            )
            .child(
                chip(theme, icons::FUNNEL, model.workflow_filter_label.clone())
                    .id("workflow-chip")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.open_workflow_dropdown();
                        cx.notify();
                    })),
            )
            .child(
                chip(theme, icons::USER, model.workspace_filter_label.clone())
                    .id("workspace-chip")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.open_workspace_dropdown();
                        cx.notify();
                    })),
            )
            .when_some(model.banner.clone(), |row, banner| {
                row.child(div().text_xs().text_color(theme.muted).child(banner))
            })
            .child(div().flex_1())
            .when(self.state.has_pending(), |row| {
                row.child(
                    heroicon(icons::ARROW_PATH).text_color(theme.muted),
                )
            })
            .child(
                chip(theme, icons::PLUS, "New issue".into())
                    .id("new-issue")
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
        let show_wf = self.state.model().show_workflow_badges;
        let children: Vec<_> = columns
            .iter()
            .enumerate()
            .map(|(ci, col)| {
                if col.collapsed {
                    self.render_collapsed(ci, col, theme, cx).into_any_element()
                } else {
                    self.render_column(ci, col, show_wf, theme, cx).into_any_element()
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
                .p_3(),
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
        div()
            .id(("collapsed", ci))
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .flex_none()
            .w(px(COLLAPSED_WIDTH))
            .h_full()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(if focused { theme.gray } else { theme.divider })
            .text_xs()
            .text_color(if focused { theme.white } else { theme.muted })
            .cursor_pointer()
            .child(div().font_weight(FontWeight::BOLD).child(col.cards.len().to_string()))
            .children(letters)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                let intents = this.state.toggle_column(ci);
                this.apply_intents(intents, window, cx);
                cx.notify();
            }))
            .when(refused, |d| d.opacity(0.35))
            .when(drop.is_some() && !refused, |d| {
                let theme = *theme;
                d.drag_over::<CardDrag>(move |s, _, _, _| {
                    s.bg(theme.selection).border_color(theme.white)
                })
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
        show_wf: bool,
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
                let selected = focused && ri == self.state.selected_row;
                let card_el = self.render_card(card, ci, ri, selected, show_wf, theme, cx);
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
        let header = div()
            .id(("header", ci))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_1()
            .pb_2()
            .cursor_pointer()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(if focused { theme.white } else { theme.muted })
                    .when(focused, |d| d.underline())
                    .child(col.label.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted)
                    .child(format!("{}", col.cards.len())),
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
                            .text_xs()
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
                .min_h_0(),
            &self.column_scrolls[ci],
        )
        .children(cards)
        .when(empty && !(is_done && model.done_loading), |d| {
            d.child(muted_row(theme, if model.loading { "Loading board…" } else { "(empty)" }))
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
                    .text_xs()
                    .text_color(theme.muted)
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
        div()
            .id(("column-drop", ci))
            .flex()
            .flex_col()
            .flex_none()
            .w(px(COLUMN_WIDTH))
            .h_full()
            .rounded_md()
            .border_1()
            .border_color(gpui::transparent_black())
            .child(header)
            .child(body)
            .when(refused.is_some(), |d| d.opacity(0.35))
            .when(drop.is_some() && refused.is_none(), |d| {
                d.drag_over::<CardDrag>(move |s, _, _, _| s.border_color(theme.gray))
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
        show_wf: bool,
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
        } else if selected {
            theme.white
        } else {
            theme.gray
        };
        div()
            .id(SharedString::from(format!("card-{}", card.id)))
            .flex()
            .flex_col()
            .flex_none()
            .gap_1()
            .p_2()
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(if selected { theme.gray } else { theme.divider })
            .bg(if selected { theme.selection } else { theme.search })
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
                            .font_weight(FontWeight::BOLD)
                            .text_color(title_color)
                            .line_clamp(2)
                            .text_ellipsis()
                            .child(card.title.clone()),
                    ),
            )
            .child(card_meta(card, show_wf, theme))
            .when_some(pending, |d, p| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.muted)
                        .child(heroicon(icons::ARROW_PATH).text_color(theme.muted))
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
                        .text_xs()
                        .text_color(theme.red)
                        .child(
                            heroicon(icons::EXCLAMATION_TRIANGLE)
                                .text_color(theme.red),
                        )
                        // Wraps (a gated merge's refusal can be long); the
                        // status line carries it too.
                        .child(div().flex_1().min_w_0().line_clamp(4).child(message))
                        .child(
                            div()
                                .id(SharedString::from(format!("dismiss-{}", card.id)))
                                .flex_none()
                                .cursor_pointer()
                                .child(heroicon(icons::X_MARK).text_color(theme.muted))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.state.dismiss_notice(&id);
                                    cx.notify();
                                })),
                        ),
                )
            })
    }

    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
        let km = self.keymaps();
        let h = |a| self.hint(km.kanban.first_chord_for(a));
        let hints = format!(
            "{}/{} column   {}/{} card   {} open   {}/{} move   {}/{} reorder   drag to move or reorder   s start   a assign   e edit   E editor   n new   {} workflow   f workspace   {} refresh",
            h(KanbanAction::NavLeft),
            h(KanbanAction::NavRight),
            h(KanbanAction::NavUp),
            h(KanbanAction::NavDown),
            h(KanbanAction::OpenPopover),
            h(KanbanAction::MoveCardLeft),
            h(KanbanAction::MoveCardRight),
            h(KanbanAction::ReorderUp),
            h(KanbanAction::ReorderDown),
            h(KanbanAction::CycleWorkflowFilter),
            h(KanbanAction::Refresh),
        );
        let status = self
            .state
            .status
            .clone()
            .or_else(|| self.state.model().error.clone());
        div()
            .flex()
            .flex_row()
            .gap_4()
            .px_4()
            .py_1()
            .border_t_1()
            .border_color(theme.divider)
            .text_xs()
            .text_color(theme.muted)
            .child(div().flex_1().truncate().child(hints))
            .when_some(status, |d, s| d.child(div().text_color(theme.gray).child(s)))
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
        Some(
            backdrop(theme, cx)
                .child(panel(theme).w(px(280.0)).mt(px(48.0)).ml(px(16.0)).child(panel_title(theme, title)).children(items)),
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
                        .text_xs()
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
            .text_sm()
            .text_color(theme.gray)
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
        let s = s.border_color(theme.white);
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

/// A card's meta lines: workflow badge + branch, then assignee and deps.
fn card_meta(card: &IssueCard, show_wf: bool, theme: &Theme) -> impl IntoElement {
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
        .text_xs()
        .text_color(theme.muted)
        .when(show_wf || card.branch.is_some(), |d| {
            d.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .items_center()
                    .overflow_hidden()
                    .when(show_wf, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .bg(theme.workflow_badge_bg)
                                .text_color(theme.workflow_badge_fg)
                                .child(card.workflow_name.clone()),
                        )
                    })
                    .when_some(card.branch.clone(), |d, b| {
                        d.child(div().truncate().child(format!("⎇ {b}")))
                    }),
            )
        })
        .when(card.assignee.is_some() || deps.is_some(), |d| {
            d.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .when_some(card.assignee.clone(), |d, ws| d.child(format!("@{ws}")))
                    .when_some(deps, |d, deps| d.child(div().truncate().child(deps))),
            )
        })
}

pub(super) fn chip(theme: &Theme, icon: Icon, label: String) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py(px(2.0))
        .rounded_md()
        .bg(theme.search)
        .text_xs()
        .text_color(theme.gray)
        .cursor_pointer()
        .child(heroicon(icon).text_color(theme.muted))
        .child(label)
}

fn muted_row(theme: &Theme, text: &'static str) -> gpui::Div {
    div().px_2().py_1().text_xs().text_color(theme.muted).child(text)
}

pub(super) fn panel(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme.divider)
        .bg(theme.search)
        .text_color(theme.foreground)
}

pub(super) fn panel_title(theme: &Theme, title: &str) -> impl IntoElement {
    div()
        .px_3()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(theme.muted)
        .child(title.to_string())
}

fn option_row(theme: &Theme, selected: bool, label: String, count: Option<usize>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .justify_between()
        .mx_1()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(theme.selection).text_color(theme.white))
        .child(label)
        .when_some(count, |d, n| d.child(div().text_color(theme.muted).child(n.to_string())))
}

/// A full-size layer over the board that swallows clicks and closes the
/// overlay when clicked outside its panel.
pub(super) fn backdrop(theme: &Theme, cx: &mut Context<IssuesView>) -> gpui::Stateful<gpui::Div> {
    let dim: Hsla = theme.background.opacity(0.55);
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
