//! The issues board's client state, with no gpui in it.
//!
//! [`BoardState`] is what the desktop board *is* between frames: the board
//! input folded from the refreshers, the filters, the selection, which overlay
//! is open, and the in-flight / refused mutations per card. Everything the
//! board decides comes from `shelbi_app` ([`IssuesModel::build`],
//! [`shelbi_app::board`]); this type only holds client state and maps keys to
//! [`Intent`]s. The gpui view ([`super::issues`]) renders it and carries out the
//! intents, so the keyboard-to-command mapping and the refusal path are tested
//! here without a window.
//!
//! Keys are the user's TUI keys: board keys dispatch through
//! `Keymaps::kanban`, the detail through `Keymaps::popover`, and the dropdowns
//! use the TUI's fixed keys (`crates/shelbi-tui/src/handlers/kanban.rs`). The
//! TUI has no start, assign, add or edit keys; the desktop adds `s`, `a`, `n`,
//! `e` (edit in place) and `E` (open in the editor) outside the keymap, the way
//! the TUI routes `f` / `w` / `m`, so the shipped `keys.yaml` is untouched.
//!
//! Drag and drop (`dt-board-dnd`) is additive: the view reports a drag's start
//! and its drop ([`BoardState::begin_drag`], [`BoardState::drop_on_column`],
//! [`BoardState::drop_on_card`]) and gets back the same registry commands the
//! keys produce. Whether a column takes a card is `shelbi_app::board`'s
//! [`board::drop_target`], so a refused target renders as not droppable before
//! the drop. While a move or reorder is in flight the board shows the card
//! where it is headed, dimmed with a pending row; a refusal drops that overlay,
//! so the card snaps back to where it really is, with the daemon's message.
//!
//! The board's reads lag its own changes (they come from the daemon's board
//! index), so an acked mutation keeps the `(status, revision)` the daemon
//! reported it produced ([`Pending::landed`]). A follow-up on the same card
//! sends that state rather than the read's pre-change one, which the daemon
//! would refuse as changed elsewhere (`dt-board-stale-after-own-mutation`).

use std::collections::BTreeMap;

use shelbi_app::board::{
    self, BoardInput, BoardRead, ClosedPage, DropTarget, WorkflowOption, WorkspaceFilter,
    WorkspaceOption,
};
use shelbi_app::{
    BoardFilters, CommandKind, Effect, ExecError, ExpectedState, IssueCard, IssuesModel, Mutation,
};
use shelbi_state::keymap::{
    Key, KanbanAction, KeyChord, Keymaps, Mods, PopoverAction,
};
use shelbi_state::KanbanColumnOverride;

/// Which overlay owns the keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Board,
    /// The task detail for `id`. `editing` while the in-place body editor is
    /// open (the editor then owns the keys; see [`BoardState::handle_key`]).
    Detail { id: String, editing: bool },
    WorkspaceDropdown { cursor: usize },
    WorkflowDropdown { cursor: usize },
    /// Pick the workspace to assign `id` to.
    AssignPicker { id: String, cursor: usize },
    /// The new-issue title prompt (its text input owns the keys).
    AddPrompt,
}

/// Something the view must do. The board never performs IO itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Run a registry command through `commands::dispatch_command`.
    Command(CommandKind),
    /// Re-read the board now.
    Refresh,
    /// Fetch the next page of done history.
    LoadMore,
    /// Persist the workspace filter (`state.json`).
    PersistWorkspaceFilter(Option<WorkspaceFilter>),
    /// Persist a column's collapse override (`state.json`).
    PersistColumnOverride {
        workflow: String,
        status_id: String,
        value: KanbanColumnOverride,
    },
    /// Scroll the open detail.
    ScrollDetail(Scroll),
    /// Open the in-place body editor for `id` (the view seeds it with the body).
    BeginEdit(String),
    /// Open `id`'s body in the user's configured editor.
    OpenInEditor(String),
    /// Show the new-issue title prompt.
    BeginAdd,
    /// Put the keyboard in the toolbar's search field (`/`).
    FocusSearch,
    /// The search was cleared from the board (Esc): empty the field to match.
    ClearSearchField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
}

/// A mutation the board has sent and not yet seen settle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// What the card says while it waits (`Moving to REVIEW…`).
    pub label: String,
    /// The state the mutation was sent against: the card's read state, or the
    /// state this client's own previous change landed at.
    pub sent_from: ExpectedState,
    /// The card's read state when the first of this client's unsettled
    /// changes to it was sent. Reads that still show it (or anything between
    /// it and [`Pending::landed`]) predate this client's own changes.
    pub read_from: ExpectedState,
    /// The `(status, revision)` the daemon reported the mutation produced,
    /// once acked (`None` before the ack, or from a daemon that doesn't
    /// report it). A follow-up mutation expects this state, and a read at or
    /// past its revision settles the change.
    pub landed: Option<ExpectedState>,
    /// For a move, the target status: the board shows the move settled once a
    /// read has the card there.
    pub to_status: Option<String>,
    /// For a reorder, the slot the card is headed for in its stored column:
    /// settled once a read has it there.
    pub to_slot: Option<u32>,
    /// The stored column this mutation renumbers (a move's or start's source,
    /// a reorder's column). Every card whose priority shifts gets a new
    /// revision, so the column's other cards wait for this one to land rather
    /// than send a state the daemon would refuse as changed elsewhere.
    /// A follow-up keeps its predecessor's columns until the reads catch up.
    pub renumbers: Vec<String>,
    /// The daemon acknowledged it; the card stays pending until a read shows
    /// the change (so a read already in flight can't flash the old state).
    pub acked: bool,
    /// The acked change this one follows, put back if this one fails, so the
    /// card keeps the state its landed change produced.
    prev: Option<Box<Pending>>,
}

/// A mutation ready to send, from [`BoardState::begin_mutation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationJob {
    pub id: String,
    pub kind: CommandKind,
    /// The state the user was looking at. `None` only for `AddIssue`.
    pub expected: Option<ExpectedState>,
    /// The new body, for an `EditIssue`.
    pub body: Option<String>,
}

impl MutationJob {
    /// The daemon mutation this job sends: the registry's effect for the
    /// command (`CommandKind::effect_with_project`), with an edit's body
    /// filled in. `None` for a command that isn't an issue mutation.
    pub fn mutation(&self, project: &str) -> Option<Mutation> {
        match self.kind.effect_with_project(project) {
            Effect::Mutate(Mutation::EditIssue {
                project, id, title, ..
            }) => Some(Mutation::EditIssue {
                project,
                id,
                title,
                body: self.body.clone(),
            }),
            Effect::Mutate(m @ (Mutation::MoveIssue { .. }
            | Mutation::StartIssue { .. }
            | Mutation::AssignIssue { .. }
            | Mutation::AddIssue { .. }
            | Mutation::PrioritizeIssue { .. })) => Some(m),
            _ => None,
        }
    }
}

/// An edit waiting to be sent: the body, and the card state it was based on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StagedEdit {
    body: String,
    expected: Option<ExpectedState>,
}

/// A letter key outside the keymap: the desktop-only board keys and the TUI's
/// fixed `f` / `w` / `m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopKey {
    Plain(char),
    Shifted(char),
}

fn desktop_key(chord: KeyChord) -> Option<DesktopKey> {
    let Key::Char(c) = chord.code else {
        return None;
    };
    if chord.mods.is_empty() {
        // A terminal-style uppercase char with no shift bit is a shifted key.
        if c.is_ascii_uppercase() {
            return Some(DesktopKey::Shifted(c.to_ascii_lowercase()));
        }
        return Some(DesktopKey::Plain(c));
    }
    (chord.mods == Mods::SHIFT).then(|| DesktopKey::Shifted(c.to_ascii_lowercase()))
}

/// The pending-id key `AddIssue` uses (an add has no issue id yet).
pub const ADD_KEY: &str = "\u{0}add";

pub struct BoardState {
    pub project: String,
    keymaps: Keymaps,
    input: BoardInput,
    filters: BoardFilters,
    model: IssuesModel,
    pub selected_column: usize,
    pub selected_row: usize,
    pub mode: Mode,
    pending: BTreeMap<String, Pending>,
    /// A daemon refusal or failure, shown on the card until dismissed or the
    /// card's next action.
    notices: BTreeMap<String, String>,
    staged_edits: BTreeMap<String, StagedEdit>,
    /// The base state of an in-place edit, captured when the editor opened, so
    /// a save refuses if someone else edited the issue meanwhile.
    edit_base: BTreeMap<String, ExpectedState>,
    /// The card being dragged, while a drag is on.
    dragging: Option<String>,
    /// One line of feedback under the board.
    pub status: Option<String>,
    /// Bumped on every model rebuild, so the view can keep what it derives
    /// from the board (the detail's model) until the board changes.
    generation: u64,
}

impl BoardState {
    pub fn new(project: impl Into<String>, keymaps: Keymaps) -> Self {
        let input = BoardInput::default();
        let filters = BoardFilters::default();
        let model = IssuesModel::build(&input, &filters);
        BoardState {
            project: project.into(),
            keymaps,
            input,
            filters,
            model,
            selected_column: 0,
            selected_row: 0,
            mode: Mode::Board,
            pending: BTreeMap::new(),
            notices: BTreeMap::new(),
            staged_edits: BTreeMap::new(),
            edit_base: BTreeMap::new(),
            dragging: None,
            status: None,
            generation: 0,
        }
    }

    pub fn model(&self) -> &IssuesModel {
        &self.model
    }

    pub fn input(&self) -> &BoardInput {
        &self.input
    }

    /// Which rebuild the model is from: it changes whenever the board's
    /// reads or overlays do.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn filters(&self) -> &BoardFilters {
        &self.filters
    }

    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    pub fn pending(&self, id: &str) -> Option<&Pending> {
        self.pending.get(id)
    }

    pub fn notice(&self, id: &str) -> Option<&str> {
        self.notices.get(id).map(String::as_str)
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn dismiss_notice(&mut self, id: &str) {
        self.notices.remove(id);
    }

    // ---- folding reads ----------------------------------------------------

    /// Fold an open-board read alone.
    #[cfg(test)]
    pub fn apply_open(&mut self, read: BoardRead) {
        self.apply_reads(Some(read), None);
    }

    #[cfg(test)]
    pub fn apply_closed(&mut self, page: Option<ClosedPage>) {
        self.apply_reads(None, Some(page));
    }

    /// Fold the reads of one refresh (either may be absent) with one rebuild
    /// of the model, not one per read. The persisted workspace filter is
    /// reloaded from the open read, so another client's (or the TUI's)
    /// filter change shows up.
    pub fn apply_reads(&mut self, open: Option<BoardRead>, closed: Option<Option<ClosedPage>>) {
        if open.is_none() && closed.is_none() {
            return;
        }
        if let Some(read) = open {
            self.input.apply_open(read);
            self.filters.workspace = self.input.workspace_filter.clone();
        }
        if let Some(page) = closed {
            self.input.apply_closed(page);
        }
        self.settle_pending();
        self.rebuild();
    }

    pub fn apply_more(&mut self, page: Result<ClosedPage, String>) {
        match page {
            Ok(page) => {
                self.input.apply_more(page);
                self.rebuild();
            }
            Err(e) => self.status = Some(e),
        }
    }

    fn rebuild(&mut self) {
        // Keep the cursor on the same card across a rebuild when it still
        // exists (a read that reorders cards shouldn't move the selection).
        let selected = self.selected_card().map(|c| c.id.clone());
        self.generation += 1;
        self.model = IssuesModel::build(&self.input, &self.filters);
        self.overlay_pending();
        if let Some(id) = selected {
            if let Some((c, r)) = self.model.position(&id) {
                self.selected_column = c;
                self.selected_row = r;
            }
        }
        self.clamp_selection();
    }

    /// Show in-flight moves and reorders where they are headed: a moving card
    /// sits in its target column, a reordering card at its target slot. The
    /// overlay is rebuilt from `pending` on every rebuild, so dropping a
    /// pending entry (a refusal) puts the card back where the read has it.
    fn overlay_pending(&mut self) {
        // The merged board is a copy of every task: only made when there is
        // something to overlay.
        let tasks = if self.pending.is_empty() { Vec::new() } else { self.input.tasks() };
        for (id, p) in &self.pending {
            if let Some(to) = &p.to_status {
                let Some((from, row)) = self.model.position(id) else {
                    continue;
                };
                let Some(to_col) = self.model.columns.iter().position(|c| &c.status_id == to)
                else {
                    continue;
                };
                if from == to_col {
                    continue;
                }
                let mut card = self.model.columns[from].cards.remove(row);
                card.status_id = to.clone();
                let target = &mut self.model.columns[to_col];
                // Where the daemon puts it: the bottom of a priority column,
                // the top of a done column (newest first).
                if target.category == shelbi_core::StatusCategory::Done {
                    target.cards.insert(0, card);
                } else {
                    target.cards.push(card);
                }
            } else if let Some(slot) = p.to_slot {
                let Some((col, _)) = self.model.position(id) else {
                    continue;
                };
                let order = reordered_column(&tasks, id, slot);
                self.model.columns[col]
                    .cards
                    .sort_by_key(|c| order.iter().position(|o| *o == c.id).unwrap_or(usize::MAX));
            }
        }
        let wf_filter = self.filters.workflow.clone();
        for col in &mut self.model.columns {
            col.collapsed = self
                .input
                .expansion(wf_filter.as_deref(), &col.status_id)
                .is_collapsed(col.cards.len());
        }
    }

    /// Drop acked mutations a read now confirms: one whose landed state the
    /// daemon reported once the card's read revision reaches it; otherwise a
    /// move once the card sits in its target status, a reorder once it sits at
    /// its slot (or its revision moved on), anything else once the card's
    /// revision moved on. A card gone from the read (e.g. filtered out) drops
    /// it too.
    fn settle_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let tasks = self.input.tasks();
        self.pending.retain(|id, p| {
            if !p.acked {
                return true;
            }
            if id == ADD_KEY {
                return false;
            }
            let Some(tf) = tasks.iter().find(|tf| &tf.task.id == id) else {
                return false;
            };
            let read_rev = tf.task.updated_at.to_rfc3339();
            if let Some(landed) = &p.landed {
                return !revision_at_or_after(&read_rev, &landed.updated_at);
            }
            let revised = read_rev != p.read_from.updated_at;
            match (&p.to_status, p.to_slot) {
                (Some(to), _) => tf.task.column.as_str() != to,
                (None, Some(slot)) => !revised && board::current_slot(&tasks, id) != Some(slot),
                (None, None) => !revised,
            }
        });
    }

    // ---- selection --------------------------------------------------------

    fn column_len(&self, col: usize) -> usize {
        self.model.columns.get(col).map_or(0, |c| c.cards.len())
    }

    pub fn clamp_selection(&mut self) {
        let last = self.model.columns.len().saturating_sub(1);
        if self.selected_column > last {
            self.selected_column = last;
        }
        let n = self.column_len(self.selected_column);
        if n == 0 {
            self.selected_row = 0;
        } else if self.selected_row >= n {
            self.selected_row = n - 1;
        }
    }

    pub fn selected_card(&self) -> Option<&IssueCard> {
        self.model
            .columns
            .get(self.selected_column)?
            .cards
            .get(self.selected_row)
    }

    pub fn select(&mut self, col: usize, row: usize) {
        self.selected_column = col;
        self.selected_row = row;
        self.clamp_selection();
    }

    fn nav_left(&mut self) {
        let n = self.model.columns.len().max(1);
        self.selected_column = if self.selected_column == 0 {
            n - 1
        } else {
            self.selected_column - 1
        };
        self.clamp_selection();
    }

    fn nav_right(&mut self) {
        let n = self.model.columns.len().max(1);
        self.selected_column = (self.selected_column + 1) % n;
        self.clamp_selection();
    }

    fn nav_up(&mut self) {
        let n = self.column_len(self.selected_column);
        if n > 0 {
            self.selected_row = if self.selected_row == 0 {
                n - 1
            } else {
                self.selected_row - 1
            };
        }
    }

    fn nav_down(&mut self) {
        let n = self.column_len(self.selected_column);
        if n > 0 {
            self.selected_row = (self.selected_row + 1) % n;
        }
    }

    /// The detail's card, when the detail is open.
    pub fn detail_id(&self) -> Option<&str> {
        match &self.mode {
            Mode::Detail { id, .. } => Some(id),
            _ => None,
        }
    }

    pub fn open_detail(&mut self, id: &str) {
        if let Some((c, r)) = self.model.position(id) {
            self.select(c, r);
        }
        self.mode = Mode::Detail {
            id: id.to_string(),
            editing: false,
        };
    }

    pub fn close_overlay(&mut self) {
        self.mode = Mode::Board;
    }

    // ---- moves ------------------------------------------------------------

    /// The status `id` moves to one legal step left/right in its workflow
    /// (`wrap` on the board, clamped in the detail), or why it can't.
    fn move_target(&self, id: &str, forward: bool, wrap: bool) -> Result<String, String> {
        let tasks = self.input.tasks();
        let tf = tasks
            .iter()
            .find(|tf| tf.task.id == id)
            .ok_or_else(|| format!("{id} is no longer on the board"))?;
        let rules = self.input.rules();
        let wf = rules
            .workflow_for(&tf.task)
            .ok_or_else(|| format!("{id}: its workflow isn't loaded"))?;
        let columns = self.input.columns(self.filters.workflow.as_deref());
        let current = rules.resolved_status_id(&tf.task);
        board::adjacent_column(&columns, &current, wf, forward, wrap)
            .map(|i| columns[i].status_id.clone())
            .ok_or_else(|| {
                let dir = if forward { "right" } else { "left" };
                format!("{id}: no column to the {dir} in its workflow")
            })
    }

    fn move_command(&mut self, id: &str, forward: bool, wrap: bool) -> Vec<Intent> {
        match self.move_target(id, forward, wrap) {
            Ok(to_status) => vec![Intent::Command(CommandKind::MoveIssue {
                id: id.to_string(),
                to_status,
            })],
            Err(msg) => {
                self.status = Some(msg);
                Vec::new()
            }
        }
    }

    /// The detail's move buttons: one clamped step, like its `H` / `L`.
    pub fn detail_move(&mut self, forward: bool) -> Vec<Intent> {
        match self.detail_id().map(str::to_string) {
            Some(id) => self.move_command(&id, forward, false),
            None => Vec::new(),
        }
    }

    // ---- filters ----------------------------------------------------------

    pub fn workspace_options(&self) -> Vec<WorkspaceOption> {
        board::workspace_options(&self.input.tasks(), &self.input.workspaces)
    }

    pub fn workflow_options(&self) -> Vec<WorkflowOption> {
        board::workflow_options(&self.input.tasks(), self.input.rules())
    }

    pub fn open_workspace_dropdown(&mut self) {
        let cursor = self
            .workspace_options()
            .iter()
            .position(|o| o.filter == self.filters.workspace)
            .unwrap_or(0);
        self.mode = Mode::WorkspaceDropdown { cursor };
    }

    pub fn open_workflow_dropdown(&mut self) {
        let cursor = self
            .workflow_options()
            .iter()
            .position(|o| o.filter == self.filters.workflow)
            .unwrap_or(0);
        self.mode = Mode::WorkflowDropdown { cursor };
    }

    pub fn set_workspace_filter(&mut self, filter: Option<WorkspaceFilter>) -> Vec<Intent> {
        self.filters.workspace = filter.clone();
        self.input.workspace_filter = filter.clone();
        self.status = Some(match &filter {
            None => "filter: all workspaces".to_string(),
            Some(f) => format!("filter: {}", f.label()),
        });
        self.rebuild();
        vec![Intent::PersistWorkspaceFilter(filter)]
    }

    /// The board's text search, as typed.
    pub fn search(&self) -> &str {
        &self.filters.search
    }

    /// Narrow the cards to those matching `query` (the toolbar's search
    /// field, as the user types). Quiet: no status note per keystroke.
    /// When the selected card drops out, the keyboard moves to the first
    /// card left, so `j`/`k` walk the matches.
    pub fn set_search(&mut self, query: &str) {
        if self.filters.search != query {
            self.filters.search = query.to_string();
            self.rebuild();
            if self.selected_card().is_none() {
                if let Some(ci) = self.model.columns.iter().position(|c| !c.cards.is_empty()) {
                    self.select(ci, 0);
                }
            }
        }
    }

    pub fn set_workflow_filter(&mut self, filter: Option<String>) {
        self.status = Some(match &filter {
            None => "workflow: all".to_string(),
            Some(w) => format!("workflow: {w}"),
        });
        self.filters.workflow = filter;
        self.rebuild();
    }

    /// Flip a column between collapsed and expanded (a header click).
    pub fn toggle_column(&mut self, col: usize) -> Vec<Intent> {
        let Some(column) = self.model.columns.get(col) else {
            return Vec::new();
        };
        let status_id = column.status_id.clone();
        let count = column.cards.len();
        let wf_filter = self.filters.workflow.clone();
        let value = self
            .input
            .expansion(wf_filter.as_deref(), &status_id)
            .toggled(count);
        let workflow =
            board::override_workflow(wf_filter.as_deref(), &self.input.default_workflow_name)
                .to_string();
        self.input.column_overrides.insert(
            shelbi_state::kanban_column_override_key(&workflow, &status_id),
            value,
        );
        self.rebuild();
        vec![Intent::PersistColumnOverride {
            workflow,
            status_id,
            value,
        }]
    }

    // ---- keys -------------------------------------------------------------

    /// Map a key to intents. Returns `None` when the board doesn't handle the
    /// key (the window may), `Some(intents)` when it consumed it. The in-place
    /// editor and the add prompt own their keys, so in those modes only Escape
    /// reaches here (to cancel).
    pub fn handle_key(&mut self, chord: KeyChord) -> Option<Vec<Intent>> {
        match self.mode.clone() {
            Mode::Board => self.board_key(chord),
            Mode::Detail { id, editing } => {
                if editing {
                    if chord.code == Key::Esc {
                        self.mode = Mode::Detail { id, editing: false };
                        return Some(Vec::new());
                    }
                    return None;
                }
                Some(self.detail_key(&id, chord))
            }
            Mode::WorkspaceDropdown { cursor } => Some(self.workspace_dropdown_key(cursor, chord)),
            Mode::WorkflowDropdown { cursor } => Some(self.workflow_dropdown_key(cursor, chord)),
            Mode::AssignPicker { id, cursor } => Some(self.assign_key(&id, cursor, chord)),
            Mode::AddPrompt => {
                if chord.code == Key::Esc {
                    self.mode = Mode::Board;
                    return Some(Vec::new());
                }
                None
            }
        }
    }

    fn board_key(&mut self, chord: KeyChord) -> Option<Vec<Intent>> {
        // Esc on the board clears an active search (the field's own Esc does
        // the same while it has the keyboard).
        if chord.code == Key::Esc && chord.mods.is_empty() && !self.filters.search.is_empty() {
            self.set_search("");
            return Some(vec![Intent::ClearSearchField]);
        }
        if let Some(action) = self.keymaps.kanban.dispatch(chord) {
            let selected = self.selected_card().map(|c| (c.id.clone(), c.assignee.clone()));
            return Some(match action {
                KanbanAction::NavLeft => {
                    self.nav_left();
                    Vec::new()
                }
                KanbanAction::NavRight => {
                    self.nav_right();
                    Vec::new()
                }
                KanbanAction::NavUp => {
                    self.nav_up();
                    Vec::new()
                }
                KanbanAction::NavDown => {
                    self.nav_down();
                    Vec::new()
                }
                KanbanAction::MoveCardLeft | KanbanAction::MoveCardRight => match selected {
                    Some((id, _)) => {
                        let forward = action == KanbanAction::MoveCardRight;
                        self.move_command(&id, forward, true)
                    }
                    None => Vec::new(),
                },
                KanbanAction::ReorderUp | KanbanAction::ReorderDown => {
                    match selected {
                        Some((id, _)) => self.reorder_key(&id, action == KanbanAction::ReorderDown),
                        None => Vec::new(),
                    }
                }
                KanbanAction::OpenPopover => {
                    if let Some((id, _)) = selected {
                        self.open_detail(&id);
                    }
                    Vec::new()
                }
                KanbanAction::Refresh => vec![Intent::Refresh],
                KanbanAction::CycleWorkflowFilter => {
                    let next = board::next_workflow_filter(
                        &self.input.workflows,
                        self.filters.workflow.as_deref(),
                    );
                    if !self.input.workflows.is_empty() {
                        self.set_workflow_filter(next);
                    }
                    Vec::new()
                }
                KanbanAction::OpenWorkspace => self.open_workspace(selected.and_then(|s| s.1)),
            });
        }
        let selected = self.selected_card().map(|c| c.id.clone());
        Some(match desktop_key(chord)? {
            DesktopKey::Plain('f') => {
                self.open_workspace_dropdown();
                Vec::new()
            }
            DesktopKey::Plain('w') => {
                self.open_workflow_dropdown();
                Vec::new()
            }
            DesktopKey::Plain('/') | DesktopKey::Shifted('/') => vec![Intent::FocusSearch],
            DesktopKey::Plain('m') => self.load_more(),
            DesktopKey::Plain('s') => selected.map_or_else(Vec::new, |id| self.start(&id)),
            DesktopKey::Plain('a') => selected.map_or_else(Vec::new, |id| self.begin_assign(&id)),
            DesktopKey::Plain('n') => self.begin_add(),
            DesktopKey::Plain('e') => selected.map_or_else(Vec::new, |id| self.begin_edit(&id)),
            DesktopKey::Shifted('e') => {
                selected.map_or_else(Vec::new, |id| vec![Intent::OpenInEditor(id)])
            }
            _ => return None,
        })
    }

    fn detail_key(&mut self, id: &str, chord: KeyChord) -> Vec<Intent> {
        if let Some(action) = self.keymaps.popover.dispatch(chord) {
            return match action {
                PopoverAction::Close => {
                    self.mode = Mode::Board;
                    Vec::new()
                }
                PopoverAction::ScrollUp => vec![Intent::ScrollDetail(Scroll::Up)],
                PopoverAction::ScrollDown => vec![Intent::ScrollDetail(Scroll::Down)],
                PopoverAction::PageUp => vec![Intent::ScrollDetail(Scroll::PageUp)],
                PopoverAction::PageDown => vec![Intent::ScrollDetail(Scroll::PageDown)],
                PopoverAction::ScrollHome => vec![Intent::ScrollDetail(Scroll::Top)],
                PopoverAction::MoveLeft => self.move_command(id, false, false),
                PopoverAction::MoveRight => self.move_command(id, true, false),
                PopoverAction::OpenWorkspace => {
                    let ws = self.model.card(id).and_then(|c| c.assignee.clone());
                    let out = self.open_workspace(ws);
                    if !out.is_empty() {
                        self.mode = Mode::Board;
                    }
                    out
                }
            };
        }
        match desktop_key(chord) {
            Some(DesktopKey::Plain('s')) => self.start(id),
            Some(DesktopKey::Plain('a')) => self.begin_assign(id),
            Some(DesktopKey::Plain('e')) => self.begin_edit(id),
            Some(DesktopKey::Shifted('e')) => vec![Intent::OpenInEditor(id.to_string())],
            _ => Vec::new(),
        }
    }

    fn workspace_dropdown_key(&mut self, cursor: usize, chord: KeyChord) -> Vec<Intent> {
        let n = self.workspace_options().len();
        match chord.code {
            Key::Esc | Key::Char('q') | Key::Char('f') => self.mode = Mode::Board,
            Key::Up | Key::Char('k') if n > 0 => {
                self.mode = Mode::WorkspaceDropdown {
                    cursor: if cursor == 0 { n - 1 } else { cursor - 1 },
                }
            }
            Key::Down | Key::Char('j') if n > 0 => {
                self.mode = Mode::WorkspaceDropdown {
                    cursor: (cursor + 1) % n,
                }
            }
            Key::Enter | Key::Char(' ') => return self.choose_workspace_option(cursor),
            Key::Char('c') => {
                self.mode = Mode::Board;
                return self.set_workspace_filter(None);
            }
            _ => {}
        }
        Vec::new()
    }

    /// Commit workspace option `ix` (a click or Enter) and close the dropdown.
    pub fn choose_workspace_option(&mut self, ix: usize) -> Vec<Intent> {
        self.mode = Mode::Board;
        match self.workspace_options().get(ix) {
            Some(opt) => self.set_workspace_filter(opt.filter.clone()),
            None => Vec::new(),
        }
    }

    fn workflow_dropdown_key(&mut self, cursor: usize, chord: KeyChord) -> Vec<Intent> {
        let n = self.workflow_options().len();
        match chord.code {
            Key::Esc | Key::Char('q') | Key::Char('w') => self.mode = Mode::Board,
            Key::Up | Key::Char('k') if n > 0 => {
                self.mode = Mode::WorkflowDropdown {
                    cursor: if cursor == 0 { n - 1 } else { cursor - 1 },
                }
            }
            Key::Down | Key::Char('j') if n > 0 => {
                self.mode = Mode::WorkflowDropdown {
                    cursor: (cursor + 1) % n,
                }
            }
            Key::Enter | Key::Char(' ') => self.choose_workflow_option(cursor),
            Key::Char('c') => {
                self.mode = Mode::Board;
                self.set_workflow_filter(None);
            }
            _ => {}
        }
        Vec::new()
    }

    /// Commit workflow option `ix` and close the dropdown.
    pub fn choose_workflow_option(&mut self, ix: usize) {
        self.mode = Mode::Board;
        if let Some(opt) = self.workflow_options().get(ix) {
            self.set_workflow_filter(opt.filter.clone());
        }
    }

    fn assign_key(&mut self, id: &str, cursor: usize, chord: KeyChord) -> Vec<Intent> {
        let n = self.input.workspaces.len();
        match chord.code {
            Key::Esc | Key::Char('q') => self.mode = Mode::Board,
            Key::Up | Key::Char('k') if n > 0 => {
                self.mode = Mode::AssignPicker {
                    id: id.to_string(),
                    cursor: if cursor == 0 { n - 1 } else { cursor - 1 },
                }
            }
            Key::Down | Key::Char('j') if n > 0 => {
                self.mode = Mode::AssignPicker {
                    id: id.to_string(),
                    cursor: (cursor + 1) % n,
                }
            }
            Key::Enter | Key::Char(' ') => return self.choose_assignee(id, cursor),
            _ => {}
        }
        Vec::new()
    }

    /// Assign `id` to declared workspace `ix` (Enter or a click in the picker).
    pub fn choose_assignee(&mut self, id: &str, ix: usize) -> Vec<Intent> {
        self.mode = Mode::Board;
        match self.input.workspaces.get(ix) {
            Some(ws) => vec![Intent::Command(CommandKind::AssignIssue {
                id: id.to_string(),
                workspace: ws.clone(),
            })],
            None => Vec::new(),
        }
    }

    // ---- actions ----------------------------------------------------------

    pub fn open_workspace(&mut self, assignee: Option<String>) -> Vec<Intent> {
        match assignee {
            Some(workspace) => vec![Intent::Command(CommandKind::FocusWorkspace { workspace })],
            None => {
                self.status = Some("no workspace assigned to this card".into());
                Vec::new()
            }
        }
    }

    pub fn start(&mut self, id: &str) -> Vec<Intent> {
        vec![Intent::Command(CommandKind::StartIssue { id: id.to_string() })]
    }

    pub fn begin_assign(&mut self, id: &str) -> Vec<Intent> {
        if self.input.workspaces.is_empty() {
            self.status = Some("this project declares no workspaces".into());
            return Vec::new();
        }
        let current = self.model.card(id).and_then(|c| c.assignee.clone());
        let cursor = current
            .and_then(|ws| self.input.workspaces.iter().position(|w| *w == ws))
            .unwrap_or(0);
        self.mode = Mode::AssignPicker {
            id: id.to_string(),
            cursor,
        };
        Vec::new()
    }

    pub fn begin_add(&mut self) -> Vec<Intent> {
        self.mode = Mode::AddPrompt;
        vec![Intent::BeginAdd]
    }

    /// Open the in-place editor on `id`'s body, remembering the state it is
    /// based on.
    pub fn begin_edit(&mut self, id: &str) -> Vec<Intent> {
        let Some(card) = self.model.card(id) else {
            return Vec::new();
        };
        self.edit_base.insert(id.to_string(), card.expected());
        self.open_detail(id);
        self.mode = Mode::Detail {
            id: id.to_string(),
            editing: true,
        };
        vec![Intent::BeginEdit(id.to_string())]
    }

    /// Save an in-place edit: stage the body against the state the editor
    /// opened on and emit the registry's `EditIssue`.
    pub fn save_edit(&mut self, id: &str, body: String) -> Vec<Intent> {
        let expected = self
            .edit_base
            .remove(id)
            .or_else(|| self.model.card(id).map(IssueCard::expected));
        self.staged_edits
            .insert(id.to_string(), StagedEdit { body, expected });
        if let Mode::Detail { id: open, .. } = &self.mode {
            if open == id {
                self.mode = Mode::Detail {
                    id: id.to_string(),
                    editing: false,
                };
            }
        }
        vec![Intent::Command(CommandKind::EditIssue { id: id.to_string() })]
    }

    /// A save from the external editor: based on what the board shows now.
    pub fn save_external_edit(&mut self, id: &str, body: String) -> Vec<Intent> {
        let expected = self.model.card(id).map(IssueCard::expected);
        self.staged_edits
            .insert(id.to_string(), StagedEdit { body, expected });
        vec![Intent::Command(CommandKind::EditIssue { id: id.to_string() })]
    }

    pub fn submit_add(&mut self, title: &str) -> Vec<Intent> {
        self.mode = Mode::Board;
        let title = title.trim();
        if title.is_empty() {
            return Vec::new();
        }
        vec![Intent::Command(CommandKind::AddIssue {
            title: title.to_string(),
        })]
    }

    fn load_more(&mut self) -> Vec<Intent> {
        if self.input.closed_next_cursor.is_some() {
            vec![Intent::LoadMore]
        } else {
            Vec::new()
        }
    }

    pub fn closed_cursor(&self) -> Option<&str> {
        self.input.closed_next_cursor.as_deref()
    }

    // ---- mutations --------------------------------------------------------

    /// Turn an issue command into a job to send, capturing the state the user
    /// is looking at and marking the card pending. One mutation per issue at a
    /// time: a second one while the first is in flight is refused here.
    ///
    /// An `EditIssue` with no staged body (e.g. from the palette) opens the
    /// in-place editor instead.
    pub fn begin_mutation(&mut self, kind: &CommandKind) -> Result<MutationJob, Vec<Intent>> {
        let (id, label, to_status, to_slot) = match kind {
            CommandKind::MoveIssue { id, to_status } => {
                let column = self.model.columns.iter().find(|c| &c.status_id == to_status);
                let label = column.map_or(to_status.clone(), |c| c.label.clone());
                let merge = column.is_some_and(|c| {
                    self.drop_target_status(id, &c.status_id, c.category)
                        == DropTarget::Move { merge: true }
                });
                let label = if merge {
                    format!("Moving to {label} (gated merge)…")
                } else {
                    format!("Moving to {label}…")
                };
                (id.clone(), label, Some(to_status.clone()), None)
            }
            CommandKind::StartIssue { id } => (id.clone(), "Starting…".to_string(), None, None),
            CommandKind::AssignIssue { id, workspace } => {
                (id.clone(), format!("Assigning to @{workspace}…"), None, None)
            }
            CommandKind::EditIssue { id } => {
                if !self.staged_edits.contains_key(id) {
                    return Err(self.begin_edit(id));
                }
                (id.clone(), "Saving…".to_string(), None, None)
            }
            CommandKind::PrioritizeIssue { id, position } => {
                (id.clone(), "Reordering…".to_string(), None, Some(*position))
            }
            CommandKind::AddIssue { title } => {
                if self.pending.contains_key(ADD_KEY) {
                    self.status = Some("an issue is already being added".into());
                    return Err(Vec::new());
                }
                self.pending.insert(
                    ADD_KEY.to_string(),
                    Pending {
                        label: "Adding…".into(),
                        sent_from: ExpectedState {
                            status: String::new(),
                            updated_at: String::new(),
                        },
                        read_from: ExpectedState {
                            status: String::new(),
                            updated_at: String::new(),
                        },
                        landed: None,
                        to_status: None,
                        to_slot: None,
                        renumbers: Vec::new(),
                        acked: false,
                        prev: None,
                    },
                );
                self.status = Some(format!("adding “{title}”…"));
                return Ok(MutationJob {
                    id: ADD_KEY.to_string(),
                    kind: kind.clone(),
                    expected: None,
                    body: None,
                });
            }
            _ => return Err(Vec::new()),
        };
        // A change the daemon hasn't answered yet blocks a second one; an
        // acked one whose landed state we know is followed on from that state.
        if self
            .pending
            .get(&id)
            .is_some_and(|p| !p.acked || p.landed.is_none())
        {
            self.status = Some(format!("{id}: a change is already in progress"));
            return Err(Vec::new());
        }
        let staged = match kind {
            CommandKind::EditIssue { .. } => self.staged_edits.remove(&id),
            _ => None,
        };
        let seen = match &staged {
            Some(s) => s.expected.clone(),
            None => self.model.card(&id).map(IssueCard::expected),
        };
        let expected = seen.map(|seen| self.own_expected(&id, seen));
        let Some(sent_from) = expected.clone() else {
            self.status = Some(format!("{id} is no longer on the board"));
            return Err(vec![Intent::Refresh]);
        };
        // The card's priority may have shifted under a sibling's in-flight
        // change; wait for it to land so the state sent is the real one.
        if let Some(column) = self.renumbering(&id, &sent_from.status) {
            self.status = Some(format!(
                "{id}: waiting for the last change in {column} to land; try again in a moment"
            ));
            return Err(Vec::new());
        }
        let mut renumbers = match kind {
            CommandKind::MoveIssue { .. }
            | CommandKind::StartIssue { .. }
            | CommandKind::PrioritizeIssue { .. } => vec![sent_from.status.clone()],
            _ => Vec::new(),
        };
        // A follow-up on this client's own landed change: until the reads
        // catch up, the card still shows where that change put it, its
        // columns still wait, and a failure puts it back.
        let prev = self.pending.remove(&id).map(Box::new);
        let (read_from, to_status, to_slot) = match &prev {
            Some(p) => {
                for column in &p.renumbers {
                    if !renumbers.contains(column) {
                        renumbers.push(column.clone());
                    }
                }
                let to_slot = if to_status.is_none() { to_slot.or(p.to_slot) } else { None };
                (p.read_from.clone(), to_status.or_else(|| p.to_status.clone()), to_slot)
            }
            None => (sent_from.clone(), to_status, to_slot),
        };
        self.notices.remove(&id);
        self.pending.insert(
            id.clone(),
            Pending {
                label,
                sent_from,
                read_from,
                landed: None,
                to_status,
                to_slot,
                renumbers,
                acked: false,
                prev,
            },
        );
        self.rebuild();
        Ok(MutationJob {
            id,
            kind: kind.clone(),
            expected,
            body: staged.map(|s| s.body),
        })
    }

    /// The daemon answered `id`'s mutation. An ack leaves the card pending
    /// until a read shows the change, holding the state the daemon says the
    /// change landed at, so the card's next mutation can be sent against it
    /// at once (the card shows in the landed status meanwhile). A refusal
    /// (another client changed it first) or failure clears the pending state
    /// (back to the acked change it followed, if any), pins the daemon's
    /// message on the card, and re-reads so the board shows the real state.
    pub fn finish_mutation(
        &mut self,
        id: &str,
        result: Result<Option<ExpectedState>, ExecError>,
    ) -> Vec<Intent> {
        match result {
            Ok(landed) => {
                if let Some(p) = self.pending.get_mut(id) {
                    p.acked = true;
                    p.prev = None;
                    if let Some(landed) = landed.filter(|_| id != ADD_KEY) {
                        // A reorder stays where its slot overlay put it.
                        if p.to_slot.is_none() {
                            p.to_status = Some(landed.status.clone());
                        }
                        p.landed = Some(landed);
                        self.rebuild();
                    }
                }
                if id == ADD_KEY {
                    self.status = Some("issue added".into());
                }
            }
            Err(e) => {
                if let Some(prev) = self.pending.remove(id).and_then(|p| p.prev) {
                    self.pending.insert(id.to_string(), *prev);
                }
                let message = match &e {
                    ExecError::Conflict(detail) => {
                        format!("Not applied: it changed elsewhere ({detail})")
                    }
                    ExecError::NotFound(_) => "Not applied: the issue no longer exists".to_string(),
                    ExecError::Backend(msg) => msg.clone(),
                };
                if id == ADD_KEY {
                    self.status = Some(format!("add failed: {message}"));
                } else {
                    self.status = Some(format!("{id}: {message}"));
                    self.notices.insert(id.to_string(), message);
                }
                // Drop the overlay: the card snaps back to where it is.
                self.rebuild();
            }
        }
        vec![Intent::Refresh]
    }

    /// The state a mutation of `id` should expect, given the state the user
    /// acted on (`seen`). When `seen` predates this client's own acked change
    /// to the card (it lies between the read that change was sent from and
    /// the state it landed at), the daemon already holds the landed state, so
    /// that is what to send. Anything else, such as an editor opened on a
    /// state from before another client's change, is sent as seen, and a
    /// change made elsewhere is still refused.
    fn own_expected(&self, id: &str, seen: ExpectedState) -> ExpectedState {
        match self.pending.get(id) {
            Some(Pending {
                acked: true,
                landed: Some(landed),
                read_from,
                ..
            }) if revision_at_or_after(&seen.updated_at, &read_from.updated_at)
                && revision_at_or_after(&landed.updated_at, &seen.updated_at) =>
            {
                landed.clone()
            }
            _ => seen,
        }
    }

    /// The label of the stored column `status` when another card's in-flight
    /// change is renumbering it.
    fn renumbering(&self, id: &str, status: &str) -> Option<String> {
        self.pending
            .iter()
            .any(|(other, p)| other != id && p.renumbers.iter().any(|c| c == status))
            .then(|| {
                self.model
                    .columns
                    .iter()
                    .find(|c| c.status_id == status)
                    .map_or_else(|| status.to_string(), |c| c.label.clone())
            })
    }

    // ---- reorder ----------------------------------------------------------

    /// `Shift+k` / `Shift+j`: trade places with the visible neighbour, through
    /// the registry's `PrioritizeIssue` (the TUI's reorder keys, same slot).
    fn reorder_key(&mut self, id: &str, down: bool) -> Vec<Intent> {
        let Some((col, row)) = self.model.position(id) else {
            return Vec::new();
        };
        let cards = &self.model.columns[col].cards;
        let neighbour = if down {
            cards.get(row + 1)
        } else {
            row.checked_sub(1).and_then(|r| cards.get(r))
        };
        let Some(target) = neighbour.map(|c| c.id.clone()) else {
            return Vec::new();
        };
        match self.drop_target(id, col) {
            DropTarget::Refused(msg) => {
                self.status = Some(format!("{id}: can't reorder here: {msg}"));
                Vec::new()
            }
            _ => self.reorder_to(id, Some(&target)),
        }
    }

    /// A `PrioritizeIssue` landing `id` where `target` sits (the bottom for
    /// `None`), or nothing when that is where it already is.
    fn reorder_to(&mut self, id: &str, target: Option<&str>) -> Vec<Intent> {
        let tasks = self.input.tasks();
        let Some(position) = board::reorder_slot(&tasks, id, target) else {
            self.status = Some(format!("{id}: can't reorder against that card"));
            return Vec::new();
        };
        if board::current_slot(&tasks, id) == Some(position) {
            return Vec::new();
        }
        vec![Intent::Command(CommandKind::PrioritizeIssue {
            id: id.to_string(),
            position,
        })]
    }

    // ---- drag and drop ----------------------------------------------------

    /// A drag of `id` started. Returns false (and the view starts no drag) for
    /// a card with a change in flight.
    pub fn begin_drag(&mut self, id: &str) -> bool {
        if self.pending.contains_key(id) {
            return false;
        }
        if let Some((c, r)) = self.model.position(id) {
            self.select(c, r);
        }
        self.dragging = Some(id.to_string());
        true
    }

    pub fn dragging(&self) -> Option<&str> {
        self.dragging.as_deref()
    }

    /// The drag ended without a drop on the board.
    pub fn end_drag(&mut self) {
        self.dragging = None;
    }

    /// What dropping `id` on column `col` does ([`board::drop_target`]).
    pub fn drop_target(&self, id: &str, col: usize) -> DropTarget {
        match self.model.columns.get(col) {
            Some(c) => self.drop_target_status(id, &c.status_id, c.category),
            None => DropTarget::Refused("no such column".to_string()),
        }
    }

    fn drop_target_status(
        &self,
        id: &str,
        status_id: &str,
        category: shelbi_core::StatusCategory,
    ) -> DropTarget {
        if self.pending.contains_key(id) {
            return DropTarget::Refused("a change is already in progress".to_string());
        }
        let tasks = self.input.tasks();
        match tasks.iter().find(|tf| tf.task.id == id) {
            Some(tf) => board::drop_target(self.input.rules(), &tf.task, status_id, category),
            None => DropTarget::Refused("it is no longer on the board".to_string()),
        }
    }

    /// `id` was dropped on column `col`'s body (not on a card): a move, or in
    /// its own column a reorder to the bottom.
    pub fn drop_on_column(&mut self, id: &str, col: usize) -> Vec<Intent> {
        self.dragging = None;
        self.status = None;
        match self.drop_target(id, col) {
            DropTarget::Move { .. } => vec![Intent::Command(CommandKind::MoveIssue {
                id: id.to_string(),
                to_status: self.model.columns[col].status_id.clone(),
            })],
            DropTarget::Reorder => self.reorder_to(id, None),
            DropTarget::Refused(msg) => {
                let label = self
                    .model
                    .columns
                    .get(col)
                    .map_or_else(String::new, |c| c.label.clone());
                self.status = Some(format!("{id} can't move to {label}: {msg}"));
                Vec::new()
            }
        }
    }

    /// `id` was dropped on the card `target` in column `col`: in its own
    /// column it takes `target`'s place; elsewhere it is a column drop.
    pub fn drop_on_card(&mut self, id: &str, col: usize, target: &str) -> Vec<Intent> {
        if self.drop_target(id, col) != DropTarget::Reorder {
            return self.drop_on_column(id, col);
        }
        self.dragging = None;
        self.status = None;
        self.reorder_to(id, Some(target))
    }
}

/// `id`'s stored column, in priority order, with `id` moved to `slot`: the
/// order a reorder lands (the store's `set_task_priority`).
/// Is revision `a` (an RFC3339 `updated_at`) the same as or later than `b`?
/// Unparseable revisions compare only as equal.
fn revision_at_or_after(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a >= b,
        _ => a == b,
    }
}

fn reordered_column(tasks: &[shelbi_state::IssueFile], id: &str, slot: u32) -> Vec<String> {
    let Some(card) = tasks.iter().find(|tf| tf.task.id == id) else {
        return Vec::new();
    };
    let mut column: Vec<&shelbi_state::IssueFile> = tasks
        .iter()
        .filter(|tf| tf.task.column == card.task.column)
        .collect();
    column.sort_by(|a, b| {
        a.task
            .priority
            .cmp(&b.task.priority)
            .then_with(|| a.task.id.cmp(&b.task.id))
    });
    let mut ids: Vec<String> = column.into_iter().map(|tf| tf.task.id.clone()).collect();
    if let Some(from) = ids.iter().position(|o| o == id) {
        let moved = ids.remove(from);
        let at = (slot as usize).min(ids.len());
        ids.insert(at, moved);
    }
    ids
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use shelbi_app::board::{BoardOutcome, BoardProject};
    use shelbi_core::{Column, Issue};
    use shelbi_state::IssueFile;

    pub(crate) fn issue(id: &str, status: &str) -> IssueFile {
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        IssueFile {
            task: Issue {
                id: id.into(),
                title: format!("Issue {id}"),
                column: Column::from_status_id(status),
                priority: 0,
                assigned_to: None,
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: at,
                updated_at: at,
                params: Default::default(),
            },
            body: format!("Body of {id}"),
            tracker_assignees: Vec::new(),
        }
    }

    pub(crate) fn read(tasks: Vec<IssueFile>) -> BoardRead {
        BoardRead {
            project: Some(BoardProject {
                default_workflow_name: "default".into(),
                display_name: Some("Demo".into()),
                workspaces: vec!["alpha".into(), "bravo".into()],
            }),
            state_snapshot: None,
            workflows: Ok(vec![shelbi_core::default_workflow()]),
            project_statuses: Ok(shelbi_core::default_project_statuses()),
            board: BoardOutcome::Warm {
                banner: None,
                base_tasks: tasks,
            },
        }
    }

    /// The shipped keymaps (no `keys.yaml`), so the test sees the TUI defaults.
    fn default_keymaps() -> Keymaps {
        // A buffered, empty `keys.yaml`: the defaults, without reading the
        // machine's live configuration.
        let (km, diags) = shelbi_state::keymap::validate_keymaps_yaml("{}", None);
        assert!(diags.is_empty(), "{diags:?}");
        km
    }

    fn board(tasks: Vec<IssueFile>) -> BoardState {
        let mut b = BoardState::new("demo", default_keymaps());
        b.apply_open(read(tasks));
        b.apply_closed(None);
        b
    }

    fn key(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    fn press(b: &mut BoardState, s: &str) -> Vec<Intent> {
        b.handle_key(key(s)).unwrap_or_default()
    }

    #[test]
    fn one_refresh_folds_both_reads_with_one_rebuild() {
        let mut b = board(vec![issue("A", "todo")]);
        let before = b.generation();
        let done = ClosedPage {
            issues: vec![issue("D", "done")],
            next_cursor: None,
        };
        b.apply_reads(Some(read(vec![issue("A", "todo"), issue("B", "todo")])), Some(Some(done)));
        assert_eq!(b.generation(), before + 1, "one rebuild");
        assert!(b.model().card("B").is_some() && b.model().card("D").is_some());
        b.apply_reads(None, None);
        assert_eq!(b.generation(), before + 1, "nothing read, nothing rebuilt");
    }

    #[test]
    fn search_narrows_the_cards_and_combines_with_the_filters() {
        let mut tasks = vec![issue("A", "todo"), issue("B", "todo"), issue("C", "in-progress")];
        tasks[0].task.title = "Restyle the command palette".into();
        tasks[1].task.assigned_to = Some("alpha".into());
        tasks[1].task.branch = Some("jlong/palette-keys".into());
        tasks[2].task.title = "Palette search".into();
        tasks[2].task.assigned_to = Some("alpha".into());
        let mut b = board(tasks);
        let visible = |b: &BoardState| -> Vec<String> {
            b.model().columns.iter().flat_map(|c| c.cards.iter().map(|k| k.id.clone())).collect()
        };
        b.set_search("palette");
        assert_eq!(visible(&b), ["A", "B", "C"], "title or branch, any case");
        assert!(b.status.is_none(), "typing leaves no status note");
        b.set_search("command");
        assert_eq!(visible(&b), ["A"]);
        // The selection follows the board: a card outside the search can't
        // stay selected, so it lands on the first match.
        assert_eq!(b.selected_card().map(|c| c.id.as_str()), Some("A"));
        b.set_search("palette");
        let _ = b.set_workspace_filter(Some(WorkspaceFilter::Workspace("alpha".into())));
        assert_eq!(visible(&b), ["B", "C"], "search and workspace filter combine");
        let _ = b.set_workspace_filter(None);
        b.set_workflow_filter(Some("default".into()));
        assert_eq!(visible(&b), ["A", "B", "C"], "and the workflow filter");
    }

    #[test]
    fn slash_focuses_the_search_and_esc_clears_it() {
        let mut b = board(vec![issue("A", "todo"), issue("B", "todo")]);
        assert_eq!(press(&mut b, "/"), [Intent::FocusSearch]);
        // Esc with no search leaves the key to the window.
        assert_eq!(b.handle_key(key("esc")), None);
        b.set_search("issue a");
        assert_eq!(b.model().columns.iter().map(|c| c.cards.len()).sum::<usize>(), 1);
        assert_eq!(press(&mut b, "esc"), [Intent::ClearSearchField]);
        assert_eq!(b.search(), "");
        assert_eq!(b.model().columns.iter().map(|c| c.cards.len()).sum::<usize>(), 2);
    }

    #[test]
    fn hjkl_and_arrows_navigate_like_the_tui() {
        let mut b = board(vec![issue("A", "todo"), issue("B", "todo"), issue("C", "in-progress")]);
        // Column 0 (backlog) is empty; `l` steps right into todo.
        press(&mut b, "l");
        assert_eq!((b.selected_column, b.selected_row), (1, 0));
        press(&mut b, "j");
        assert_eq!(b.selected_card().unwrap().id, "B");
        press(&mut b, "down");
        assert_eq!(b.selected_card().unwrap().id, "A", "j wraps within the column");
        press(&mut b, "right");
        assert_eq!(b.selected_card().unwrap().id, "C");
        press(&mut b, "h");
        assert_eq!(b.selected_column, 1);
        press(&mut b, "h");
        press(&mut b, "h");
        assert_eq!(b.selected_column, b.model().columns.len() - 1, "h wraps to the last column");
    }

    #[test]
    fn move_start_and_assign_keys_emit_registry_commands() {
        let mut b = board(vec![issue("A", "todo")]);
        b.select(1, 0);
        assert_eq!(
            press(&mut b, "L"),
            vec![Intent::Command(CommandKind::MoveIssue {
                id: "A".into(),
                to_status: "in-progress".into()
            })]
        );
        assert_eq!(
            press(&mut b, "H"),
            vec![Intent::Command(CommandKind::MoveIssue {
                id: "A".into(),
                to_status: "backlog".into()
            })]
        );
        assert_eq!(
            press(&mut b, "s"),
            vec![Intent::Command(CommandKind::StartIssue { id: "A".into() })]
        );
        // `a` opens the picker; j + enter picks the second workspace.
        assert!(press(&mut b, "a").is_empty());
        assert!(matches!(b.mode, Mode::AssignPicker { .. }));
        press(&mut b, "j");
        assert_eq!(
            press(&mut b, "enter"),
            vec![Intent::Command(CommandKind::AssignIssue {
                id: "A".into(),
                workspace: "bravo".into()
            })]
        );
        assert_eq!(b.mode, Mode::Board);
    }

    #[test]
    fn board_move_wraps_and_detail_move_clamps() {
        let mut b = board(vec![issue("A", "canceled")]);
        let (c, r) = b.model().position("A").unwrap();
        b.select(c, r);
        // The default workflow's last status is canceled: L wraps to backlog.
        assert_eq!(
            press(&mut b, "L"),
            vec![Intent::Command(CommandKind::MoveIssue {
                id: "A".into(),
                to_status: "backlog".into()
            })]
        );
        press(&mut b, "enter");
        assert_eq!(b.detail_id(), Some("A"));
        assert!(press(&mut b, "L").is_empty(), "the detail never wraps");
        assert!(b.status.as_deref().unwrap().contains("no column to the right"));
        press(&mut b, "esc");
        assert_eq!(b.mode, Mode::Board);
    }

    #[test]
    fn o_without_a_workspace_reports_and_with_one_focuses_it() {
        let mut assigned = issue("A", "todo");
        assigned.task.assigned_to = Some("alpha".into());
        let mut b = board(vec![assigned, issue("B", "todo")]);
        b.select(1, 0);
        assert_eq!(
            press(&mut b, "o"),
            vec![Intent::Command(CommandKind::FocusWorkspace {
                workspace: "alpha".into()
            })]
        );
        b.select(1, 1);
        assert!(press(&mut b, "o").is_empty());
        assert_eq!(b.status.as_deref(), Some("no workspace assigned to this card"));
    }

    #[test]
    fn filters_follow_the_tui_keys() {
        let mut assigned = issue("A", "todo");
        assigned.task.assigned_to = Some("alpha".into());
        let mut b = board(vec![assigned, issue("B", "todo")]);
        press(&mut b, "f");
        assert!(matches!(b.mode, Mode::WorkspaceDropdown { cursor: 0 }));
        press(&mut b, "j");
        let out = press(&mut b, "enter");
        assert_eq!(
            out,
            vec![Intent::PersistWorkspaceFilter(Some(WorkspaceFilter::Workspace(
                "alpha".into()
            )))]
        );
        assert_eq!(b.model().columns[1].cards.len(), 1);
        // `tab` cycles the workflow filter (one workflow: All -> default -> All).
        press(&mut b, "tab");
        assert_eq!(b.filters().workflow.as_deref(), Some("default"));
        press(&mut b, "tab");
        assert_eq!(b.filters().workflow, None);
    }

    #[test]
    fn a_refused_move_pins_the_daemons_message_and_rereads() {
        let mut b = board(vec![issue("A", "todo")]);
        b.select(1, 0);
        let Intent::Command(kind) = press(&mut b, "L").remove(0) else {
            panic!("a command");
        };
        let job = b.begin_mutation(&kind).expect("a job");
        // The job carries the state the user saw, not one re-read at send time.
        assert_eq!(job.expected.as_ref().unwrap().status, "todo");
        assert!(b.pending("A").is_some());
        // A second mutation of the same issue waits for the first.
        assert!(b.begin_mutation(&kind).is_err());

        // Meanwhile another client moved it to review; the daemon refuses.
        let out = b.finish_mutation(
            "A",
            Err(ExecError::Conflict(
                "you saw `todo` rev 1, it is now `review` rev 2".into(),
            )),
        );
        assert_eq!(out, vec![Intent::Refresh], "a refusal re-reads the board");
        assert!(b.pending("A").is_none());
        assert!(b.notice("A").unwrap().contains("it is now `review`"));
        // The re-read lands; the card shows where it really is, with the notice.
        let mut moved = issue("A", "review");
        moved.task.updated_at += chrono::Duration::seconds(5);
        b.apply_open(read(vec![moved]));
        let (c, _) = b.model().position("A").unwrap();
        assert_eq!(b.model().columns[c].status_id, "review");
        assert!(b.notice("A").is_some());
        // The card's next action clears it.
        let job = b
            .begin_mutation(&CommandKind::StartIssue { id: "A".into() })
            .unwrap();
        assert_eq!(job.expected.unwrap().status, "review");
        assert!(b.notice("A").is_none());
    }

    #[test]
    fn an_acked_move_stays_pending_until_a_read_shows_it() {
        let mut b = board(vec![issue("A", "todo")]);
        let kind = CommandKind::MoveIssue {
            id: "A".into(),
            to_status: "in-progress".into(),
        };
        b.begin_mutation(&kind).unwrap();
        b.finish_mutation("A", Ok(None));
        assert!(b.pending("A").unwrap().acked);
        // A read from before the daemon applied it: still pending.
        b.apply_open(read(vec![issue("A", "todo")]));
        assert!(b.pending("A").is_some());
        b.apply_open(read(vec![issue("A", "in-progress")]));
        assert!(b.pending("A").is_none());
    }

    /// A revision `secs` seconds after [`issue`]'s, as a card reports it.
    fn rev(secs: i64) -> String {
        (issue("A", "todo").task.updated_at + chrono::Duration::seconds(secs)).to_rfc3339()
    }

    fn at(status: &str, secs: i64) -> ExpectedState {
        ExpectedState {
            status: status.into(),
            updated_at: rev(secs),
        }
    }

    /// `issue(id, status)` as a read `secs` seconds after [`issue`]'s.
    fn issue_at(id: &str, status: &str, secs: i64) -> IssueFile {
        let mut tf = issue(id, status);
        tf.task.updated_at += chrono::Duration::seconds(secs);
        tf
    }

    #[test]
    fn an_own_mutation_updates_the_cards_expected_revision() {
        let mut b = board(vec![issue("A", "todo")]);
        let start = CommandKind::StartIssue { id: "A".into() };
        let job = b
            .begin_mutation(&CommandKind::AssignIssue {
                id: "A".into(),
                workspace: "alpha".into(),
            })
            .unwrap();
        assert_eq!(job.expected, Some(at("todo", 0)));
        // Until the daemon answers, a second change still waits.
        assert!(b.begin_mutation(&start).is_err());

        // The assign lands at a new revision; no read has seen it yet.
        b.finish_mutation("A", Ok(Some(at("todo", 5))));
        // Start goes at once, against the state the assign produced rather
        // than the read's (which the daemon would refuse as changed elsewhere).
        let job = b.begin_mutation(&start).unwrap();
        assert_eq!(job.expected, Some(at("todo", 5)));
        // A read from before the assign landed settles nothing.
        b.apply_open(read(vec![issue("A", "todo")]));
        assert!(b.pending("A").is_some());

        // Start lands too; the card shows in its landed status before a read.
        b.finish_mutation("A", Ok(Some(at("in-progress", 6))));
        assert_eq!(b.model().card("A").unwrap().status_id, "in-progress");
        // A read that shows only the assign still predates the start: the
        // card keeps the start's state for its next change.
        b.apply_open(read(vec![issue_at("A", "todo", 5)]));
        assert!(b.pending("A").is_some());
        assert_eq!(b.model().card("A").unwrap().status_id, "in-progress");
        let seen = b.model().card("A").unwrap().expected();
        assert_eq!(b.own_expected("A", seen), at("in-progress", 6));

        // The read catches up: settled, and the card is the read's again.
        b.apply_open(read(vec![issue_at("A", "in-progress", 6)]));
        assert!(b.pending("A").is_none());
        assert_eq!(b.model().card("A").unwrap().expected(), at("in-progress", 6));
    }

    #[test]
    fn a_follow_up_keeps_its_predecessors_column_waiting() {
        let mut b = board(vec![issue("A", "todo"), issue("B", "todo")]);
        b.begin_mutation(&CommandKind::StartIssue { id: "A".into() })
            .unwrap();
        b.finish_mutation("A", Ok(Some(at("in-progress", 5))));
        // A's follow-up rides on the landed start...
        let job = b
            .begin_mutation(&CommandKind::AssignIssue {
                id: "A".into(),
                workspace: "alpha".into(),
            })
            .unwrap();
        assert_eq!(job.expected, Some(at("in-progress", 5)));
        // ...and B, renumbered by the start, still waits for a read.
        assert!(b
            .begin_mutation(&CommandKind::StartIssue { id: "B".into() })
            .is_err());
        assert!(b.status.as_deref().unwrap().contains("waiting for the last change"));
        b.finish_mutation("A", Ok(Some(at("in-progress", 6))));
        b.apply_open(read(vec![issue_at("A", "in-progress", 6), issue_at("B", "todo", 5)]));
        assert!(b.pending("A").is_none());
        let job = b
            .begin_mutation(&CommandKind::StartIssue { id: "B".into() })
            .unwrap();
        assert_eq!(job.expected.unwrap().updated_at, rev(5));
    }

    #[test]
    fn a_change_made_elsewhere_after_an_own_change_is_still_refused() {
        let mut b = board(vec![issue("A", "todo")]);
        // The editor opens on the card as read.
        b.begin_edit("A");
        let assign = CommandKind::AssignIssue {
            id: "A".into(),
            workspace: "alpha".into(),
        };
        b.begin_mutation(&assign).unwrap();
        b.finish_mutation("A", Ok(Some(at("todo", 5))));

        // Another client moved it after the assign: the daemon refuses the
        // start sent against the assign's landed state.
        let job = b
            .begin_mutation(&CommandKind::StartIssue { id: "A".into() })
            .unwrap();
        assert_eq!(job.expected, Some(at("todo", 5)));
        let out = b.finish_mutation(
            "A",
            Err(ExecError::Conflict(format!(
                "you saw `todo` rev {}, it is now `review` rev {}",
                rev(5),
                rev(7)
            ))),
        );
        assert_eq!(out, vec![Intent::Refresh]);
        assert!(b.notice("A").unwrap().starts_with("Not applied: it changed elsewhere"));
        // The card keeps the assign it did land.
        let p = b.pending("A").unwrap();
        assert!(p.acked);
        assert_eq!(p.landed, Some(at("todo", 5)));

        // The re-read shows the other client's change: it settles the assign,
        // and the card's next change is sent against what the read shows.
        b.apply_open(read(vec![issue_at("A", "review", 7)]));
        assert!(b.pending("A").is_none());
        let job = b
            .begin_mutation(&CommandKind::StartIssue { id: "A".into() })
            .unwrap();
        assert_eq!(job.expected, Some(at("review", 7)));
        b.finish_mutation("A", Ok(Some(at("in-progress", 8))));

        // An edit staged against the state from before the other client's
        // change isn't taken for this client's own: it goes as the editor saw
        // it, and the daemon refuses it.
        b.save_edit("A", "new body".into());
        let job = b
            .begin_mutation(&CommandKind::EditIssue { id: "A".into() })
            .unwrap();
        assert_eq!(job.expected, Some(at("todo", 0)));
    }

    #[test]
    fn edits_stage_the_body_against_the_state_the_editor_opened_on() {
        let mut b = board(vec![issue("A", "todo")]);
        b.select(1, 0);
        assert_eq!(press(&mut b, "e"), vec![Intent::BeginEdit("A".into())]);
        assert_eq!(
            b.mode,
            Mode::Detail {
                id: "A".into(),
                editing: true
            }
        );
        // While editing, plain keys belong to the editor.
        assert!(b.handle_key(key("j")).is_none());
        let out = b.save_edit("A", "new body".into());
        assert_eq!(out, vec![Intent::Command(CommandKind::EditIssue { id: "A".into() })]);
        let job = b.begin_mutation(&CommandKind::EditIssue { id: "A".into() }).unwrap();
        assert_eq!(job.body.as_deref(), Some("new body"));
        assert_eq!(job.expected.unwrap().status, "todo");
        // An EditIssue with nothing staged (from the palette) opens the editor.
        b.finish_mutation("A", Ok(None));
        let mut later = issue("A", "todo");
        later.task.updated_at += chrono::Duration::seconds(1);
        b.apply_open(read(vec![later]));
        assert_eq!(
            b.begin_mutation(&CommandKind::EditIssue { id: "A".into() }),
            Err(vec![Intent::BeginEdit("A".into())])
        );
    }

    #[test]
    fn add_prompt_submits_add_issue_and_e_shift_opens_the_editor() {
        let mut b = board(vec![issue("A", "todo")]);
        assert_eq!(press(&mut b, "n"), vec![Intent::BeginAdd]);
        assert_eq!(b.mode, Mode::AddPrompt);
        assert_eq!(
            b.submit_add("  Write docs "),
            vec![Intent::Command(CommandKind::AddIssue {
                title: "Write docs".into()
            })]
        );
        let job = b
            .begin_mutation(&CommandKind::AddIssue {
                title: "Write docs".into(),
            })
            .unwrap();
        assert_eq!(job.expected, None, "an add has no prior state");
        b.select(1, 0);
        assert_eq!(press(&mut b, "E"), vec![Intent::OpenInEditor("A".into())]);
    }

    #[test]
    fn jobs_become_the_registrys_daemon_mutations() {
        let mut b = board(vec![issue("A", "todo")]);
        b.save_edit("A", "edited".into());
        let job = b.begin_mutation(&CommandKind::EditIssue { id: "A".into() }).unwrap();
        assert_eq!(
            job.mutation("demo"),
            Some(Mutation::EditIssue {
                project: "demo".into(),
                id: "A".into(),
                title: None,
                body: Some("edited".into()),
            })
        );
        b.finish_mutation("A", Err(ExecError::Backend("boom".into())));
        let job = b
            .begin_mutation(&CommandKind::MoveIssue {
                id: "A".into(),
                to_status: "review".into(),
            })
            .unwrap();
        assert_eq!(
            job.mutation("demo"),
            Some(Mutation::MoveIssue {
                project: "demo".into(),
                id: "A".into(),
                to_status: "review".into(),
            })
        );
        assert_eq!(b.notice("A"), None, "a new action clears the old notice");
        let not_issue = MutationJob {
            id: "x".into(),
            kind: CommandKind::ToggleZen,
            expected: None,
            body: None,
        };
        assert_eq!(not_issue.mutation("demo"), None);
    }

    #[test]
    fn the_done_column_fills_after_the_open_board() {
        let mut b = BoardState::new("demo", default_keymaps());
        b.apply_open(read(vec![issue("A", "todo")]));
        assert!(b.model().done_loading);
        assert_eq!(b.model().columns[1].cards.len(), 1);
        b.apply_closed(Some(ClosedPage {
            issues: vec![issue("D", "done")],
            next_cursor: Some("p2".into()),
        }));
        assert!(!b.model().done_loading);
        assert_eq!(press(&mut b, "m"), vec![Intent::LoadMore]);
    }

    // ---- drag and drop (dt-board-dnd) -------------------------------------

    /// A read under the shipped `task` + `subtask` workflows, `task` the
    /// project default.
    fn shipped_read(tasks: Vec<IssueFile>) -> BoardRead {
        let mut r = read(tasks);
        r.project.as_mut().unwrap().default_workflow_name = shelbi_core::TASK_WORKFLOW_NAME.into();
        r.workflows = Ok(vec![shelbi_core::task_workflow(), shelbi_core::subtask_workflow()]);
        r
    }

    fn shipped_board(tasks: Vec<IssueFile>) -> BoardState {
        let mut b = BoardState::new("demo", default_keymaps());
        b.apply_open(shipped_read(tasks));
        b.apply_closed(None);
        b
    }

    fn col_of(b: &BoardState, status: &str) -> usize {
        b.model()
            .columns
            .iter()
            .position(|c| c.status_id == status)
            .unwrap()
    }

    fn ids_in(b: &BoardState, status: &str) -> Vec<String> {
        b.model().columns[col_of(b, status)]
            .cards
            .iter()
            .map(|c| c.id.clone())
            .collect()
    }

    fn prio(id: &str, status: &str, priority: u32) -> IssueFile {
        let mut tf = issue(id, status);
        tf.task.priority = priority;
        tf
    }

    #[test]
    fn drop_legality_follows_the_cards_workflow() {
        let mut sub = issue("S", "in-progress");
        sub.task.workflow = Some(shelbi_core::SUBTASK_WORKFLOW_NAME.into());
        let mut b = shipped_board(vec![issue("T", "in-progress"), sub, issue("R", "review")]);
        let review = col_of(&b, "review");
        let done = col_of(&b, "done");
        let in_progress = col_of(&b, "in-progress");
        assert_eq!(b.drop_target("T", review), DropTarget::Move { merge: false });
        assert_eq!(b.drop_target("T", in_progress), DropTarget::Reorder);
        assert_eq!(b.drop_target("R", done), DropTarget::Move { merge: true });
        // A subtask has no review status: the REVIEW column refuses it before
        // the drop, and merges straight into done.
        assert!(matches!(b.drop_target("S", review), DropTarget::Refused(_)));
        assert_eq!(b.drop_target("S", done), DropTarget::Move { merge: true });

        // A drop on the refused column sends nothing and says why.
        assert!(b.begin_drag("S"));
        assert_eq!(b.dragging(), Some("S"));
        assert!(b.drop_on_column("S", review).is_empty());
        assert_eq!(b.dragging(), None);
        let status = b.status.clone().unwrap();
        assert!(status.contains("can't move to REVIEW") && status.contains("`subtask`"), "{status}");
        assert_eq!(ids_in(&b, "in-progress"), ["T", "S"], "nothing moved");
    }

    #[test]
    fn a_dropped_card_shows_pending_in_its_target_then_settles_when_accepted() {
        let mut b = shipped_board(vec![issue("R", "review")]);
        let done = col_of(&b, "done");
        assert!(b.begin_drag("R"));
        let intents = b.drop_on_column("R", done);
        assert_eq!(
            intents,
            vec![Intent::Command(CommandKind::MoveIssue {
                id: "R".into(),
                to_status: "done".into()
            })],
            "a drop is the registry's MoveIssue: the daemon runs the transition's merge"
        );
        let Intent::Command(kind) = &intents[0] else { unreachable!() };
        let job = b.begin_mutation(kind).unwrap();
        assert_eq!(job.expected.as_ref().unwrap().status, "review");
        // Pending: the card sits in its target, labelled with the gated merge.
        assert_eq!(ids_in(&b, "done"), ["R"]);
        assert!(ids_in(&b, "review").is_empty());
        assert!(b.pending("R").unwrap().label.contains("gated merge"));
        assert!(!b.begin_drag("R"), "a pending card can't be dragged again");

        // Accepted: stays pending (and in place) until a read shows it.
        b.finish_mutation("R", Ok(None));
        b.apply_open(shipped_read(vec![issue("R", "review")]));
        assert_eq!(ids_in(&b, "done"), ["R"]);
        assert!(b.pending("R").is_some());
        b.apply_open(shipped_read(vec![issue("R", "done")]));
        assert!(b.pending("R").is_none());
        assert_eq!(ids_in(&b, "done"), ["R"]);
    }

    #[test]
    fn a_refused_drop_snaps_back_with_the_daemons_message() {
        let mut b = shipped_board(vec![issue("R", "review")]);
        let done = col_of(&b, "done");
        b.begin_drag("R");
        let Intent::Command(kind) = b.drop_on_column("R", done).remove(0) else {
            panic!("a command");
        };
        b.begin_mutation(&kind).unwrap();
        assert_eq!(ids_in(&b, "done"), ["R"]);
        // The gated merge failed on the daemon.
        b.finish_mutation(
            "R",
            Err(ExecError::Backend("merge failed: conflicts in src/lib.rs".into())),
        );
        assert_eq!(ids_in(&b, "review"), ["R"], "snapped back");
        assert!(ids_in(&b, "done").is_empty());
        assert!(b.pending("R").is_none());
        assert_eq!(b.notice("R"), Some("merge failed: conflicts in src/lib.rs"));
    }

    #[test]
    fn dropping_on_a_card_in_its_own_column_reorders_through_the_registry() {
        let mut b = shipped_board(vec![
            prio("A", "todo", 0),
            prio("B", "todo", 1),
            prio("C", "todo", 2),
        ]);
        let todo = col_of(&b, "todo");
        b.begin_drag("C");
        let intents = b.drop_on_card("C", todo, "A");
        assert_eq!(
            intents,
            vec![Intent::Command(CommandKind::PrioritizeIssue {
                id: "C".into(),
                position: 0
            })]
        );
        let job = b.begin_mutation(&CommandKind::PrioritizeIssue {
            id: "C".into(),
            position: 0,
        })
        .unwrap();
        assert_eq!(
            job.mutation("demo"),
            Some(Mutation::PrioritizeIssue {
                project: "demo".into(),
                id: "C".into(),
                position: 0
            })
        );
        assert_eq!(ids_in(&b, "todo"), ["C", "A", "B"], "shown at its slot while pending");
        assert_eq!(b.pending("C").unwrap().label, "Reordering…");
        // The column's other cards wait: their revisions are about to change.
        assert!(b.begin_mutation(&CommandKind::StartIssue { id: "A".into() }).is_err());
        let status = b.status.clone().unwrap();
        assert!(status.contains("waiting for the last change in TO DO"), "{status}");

        b.finish_mutation("C", Ok(None));
        let mut after = vec![prio("C", "todo", 0), prio("A", "todo", 1), prio("B", "todo", 2)];
        for tf in &mut after {
            tf.task.updated_at += chrono::Duration::seconds(1);
        }
        b.apply_open(shipped_read(after));
        assert!(b.pending("C").is_none(), "settled");
        assert_eq!(ids_in(&b, "todo"), ["C", "A", "B"]);
        assert!(b.begin_mutation(&CommandKind::StartIssue { id: "A".into() }).is_ok());
    }

    #[test]
    fn reorder_drops_that_change_nothing_send_nothing() {
        let mut b = shipped_board(vec![prio("A", "todo", 0), prio("B", "todo", 1)]);
        let todo = col_of(&b, "todo");
        b.begin_drag("A");
        assert!(b.drop_on_card("A", todo, "A").is_empty(), "dropped on itself");
        b.begin_drag("B");
        assert!(b.drop_on_column("B", todo).is_empty(), "already at the bottom");
        b.begin_drag("A");
        assert_eq!(
            b.drop_on_column("A", todo),
            vec![Intent::Command(CommandKind::PrioritizeIssue {
                id: "A".into(),
                position: 1
            })],
            "the column body is the bottom"
        );
        // A drop on a card in another column is a move to that column.
        let mut b = shipped_board(vec![prio("A", "todo", 0), prio("X", "in-progress", 0)]);
        let in_progress = col_of(&b, "in-progress");
        b.begin_drag("A");
        assert_eq!(
            b.drop_on_card("A", in_progress, "X"),
            vec![Intent::Command(CommandKind::MoveIssue {
                id: "A".into(),
                to_status: "in-progress".into()
            })]
        );
    }

    #[test]
    fn a_refused_reorder_snaps_back() {
        let mut b = shipped_board(vec![prio("A", "todo", 0), prio("B", "todo", 1)]);
        let kind = CommandKind::PrioritizeIssue {
            id: "B".into(),
            position: 0,
        };
        b.begin_mutation(&kind).unwrap();
        assert_eq!(ids_in(&b, "todo"), ["B", "A"]);
        b.finish_mutation(
            "B",
            Err(ExecError::Conflict("you saw `todo` rev 1, it is now `review` rev 2".into())),
        );
        assert_eq!(ids_in(&b, "todo"), ["A", "B"]);
        assert!(b.notice("B").unwrap().starts_with("Not applied: it changed elsewhere"));
    }

    #[test]
    fn shift_j_and_k_reorder_with_the_whole_column_slot() {
        let mut assigned = prio("A", "todo", 0);
        assigned.task.assigned_to = Some("alpha".into());
        let mut hidden = prio("B", "todo", 1);
        hidden.task.assigned_to = Some("bravo".into());
        let mut last = prio("C", "todo", 2);
        last.task.assigned_to = Some("alpha".into());
        let mut b = shipped_board(vec![assigned, hidden, last]);
        b.set_workspace_filter(Some(WorkspaceFilter::Workspace("alpha".into())));
        let todo = col_of(&b, "todo");
        b.select(todo, 0);
        assert_eq!(
            press(&mut b, "J"),
            vec![Intent::Command(CommandKind::PrioritizeIssue {
                id: "A".into(),
                position: 2
            })],
            "past the visible neighbour C, at slot 2 (B is filtered out)"
        );
        b.select(todo, 1);
        assert_eq!(
            press(&mut b, "K"),
            vec![Intent::Command(CommandKind::PrioritizeIssue {
                id: "C".into(),
                position: 0
            })]
        );
        assert!(press(&mut b, "J").is_empty(), "no card below C");
    }

    #[test]
    fn a_move_into_an_empty_collapsed_column_expands_it_while_pending() {
        let mut b = shipped_board(vec![issue("A", "todo")]);
        let review = col_of(&b, "review");
        assert!(b.model().columns[review].collapsed, "empty columns collapse");
        b.begin_mutation(&CommandKind::MoveIssue {
            id: "A".into(),
            to_status: "review".into(),
        })
        .unwrap();
        assert!(!b.model().columns[review].collapsed);
        assert!(b.model().columns[col_of(&b, "todo")].collapsed);
    }
}
