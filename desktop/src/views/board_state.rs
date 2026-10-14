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

use std::collections::BTreeMap;

use shelbi_app::board::{
    self, BoardInput, BoardRead, ClosedPage, WorkflowOption, WorkspaceFilter, WorkspaceOption,
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
    /// The card's read state when the mutation was sent.
    pub sent_from: ExpectedState,
    /// For a move, the target status: the board shows the move settled once a
    /// read has the card there.
    pub to_status: Option<String>,
    /// The daemon acknowledged it; the card stays pending until a read shows
    /// the change (so a read already in flight can't flash the old state).
    pub acked: bool,
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
            | Mutation::AddIssue { .. })) => Some(m),
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
    /// One line of feedback under the board.
    pub status: Option<String>,
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
            status: None,
        }
    }

    pub fn model(&self) -> &IssuesModel {
        &self.model
    }

    pub fn input(&self) -> &BoardInput {
        &self.input
    }

    #[cfg(test)]
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

    /// Fold an open-board read. The persisted workspace filter is reloaded
    /// from it, so another client's (or the TUI's) filter change shows up.
    pub fn apply_open(&mut self, read: BoardRead) {
        self.input.apply_open(read);
        self.filters.workspace = self.input.workspace_filter.clone();
        self.rebuild();
        self.settle_pending();
    }

    pub fn apply_closed(&mut self, page: Option<ClosedPage>) {
        self.input.apply_closed(page);
        self.rebuild();
        self.settle_pending();
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
        self.model = IssuesModel::build(&self.input, &self.filters);
        if let Some(id) = selected {
            if let Some((c, r)) = self.model.position(&id) {
                self.selected_column = c;
                self.selected_row = r;
            }
        }
        self.clamp_selection();
    }

    /// Drop acked mutations a read now confirms: a move once the card sits in
    /// its target status, anything else once the card's revision moved on (or
    /// the card is gone, e.g. filtered out).
    fn settle_pending(&mut self) {
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
            match &p.to_status {
                Some(to) => tf.task.column.as_str() != to,
                None => tf.task.updated_at.to_rfc3339() == p.sent_from.updated_at,
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
                    // The registry has no priority mutation yet (the TUI writes
                    // the store directly), and every desktop mutation goes
                    // through the registry and the daemon.
                    self.status = Some("reordering isn't available in the desktop yet".into());
                    Vec::new()
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
        let (id, label, to_status) = match kind {
            CommandKind::MoveIssue { id, to_status } => {
                let label = self
                    .model
                    .columns
                    .iter()
                    .find(|c| &c.status_id == to_status)
                    .map_or(to_status.clone(), |c| c.label.clone());
                (id.clone(), format!("Moving to {label}…"), Some(to_status.clone()))
            }
            CommandKind::StartIssue { id } => (id.clone(), "Starting…".to_string(), None),
            CommandKind::AssignIssue { id, workspace } => {
                (id.clone(), format!("Assigning to @{workspace}…"), None)
            }
            CommandKind::EditIssue { id } => {
                if !self.staged_edits.contains_key(id) {
                    return Err(self.begin_edit(id));
                }
                (id.clone(), "Saving…".to_string(), None)
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
                        to_status: None,
                        acked: false,
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
        if self.pending.contains_key(&id) {
            self.status = Some(format!("{id}: a change is already in progress"));
            return Err(Vec::new());
        }
        let staged = match kind {
            CommandKind::EditIssue { .. } => self.staged_edits.remove(&id),
            _ => None,
        };
        let expected = match &staged {
            Some(s) => s.expected.clone(),
            None => self.model.card(&id).map(IssueCard::expected),
        };
        let Some(sent_from) = expected.clone() else {
            self.status = Some(format!("{id} is no longer on the board"));
            return Err(vec![Intent::Refresh]);
        };
        self.notices.remove(&id);
        self.pending.insert(
            id.clone(),
            Pending {
                label,
                sent_from,
                to_status,
                acked: false,
            },
        );
        Ok(MutationJob {
            id,
            kind: kind.clone(),
            expected,
            body: staged.map(|s| s.body),
        })
    }

    /// The daemon answered `id`'s mutation. An ack leaves the card pending
    /// until a read shows the change; a refusal (another client changed it
    /// first) or failure clears the pending state, pins the daemon's message
    /// on the card, and re-reads so the board shows the real state.
    pub fn finish_mutation(&mut self, id: &str, result: Result<(), ExecError>) -> Vec<Intent> {
        match result {
            Ok(()) => {
                if let Some(p) = self.pending.get_mut(id) {
                    p.acked = true;
                }
                if id == ADD_KEY {
                    self.status = Some("issue added".into());
                }
            }
            Err(e) => {
                self.pending.remove(id);
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
            }
        }
        vec![Intent::Refresh]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::board::{BoardOutcome, BoardProject};
    use shelbi_core::{Column, Issue};
    use shelbi_state::IssueFile;

    fn issue(id: &str, status: &str) -> IssueFile {
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

    fn read(tasks: Vec<IssueFile>) -> BoardRead {
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
        b.finish_mutation("A", Ok(()));
        assert!(b.pending("A").unwrap().acked);
        // A read from before the daemon applied it: still pending.
        b.apply_open(read(vec![issue("A", "todo")]));
        assert!(b.pending("A").is_some());
        b.apply_open(read(vec![issue("A", "in-progress")]));
        assert!(b.pending("A").is_none());
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
        b.finish_mutation("A", Ok(()));
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
}
