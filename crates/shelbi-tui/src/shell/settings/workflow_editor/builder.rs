//! The workflow editor's Builder in the TUI (`tui-workflow-editor-builder`):
//! the workflow as a graph of its statuses and transitions, with one details
//! panel for what's selected, after the desktop's Builder
//! (`desktop/src/views/workflow_builder.rs`). It's the editor's second view
//! over the same draft as the Form (`v`, or the header's Builder | Form):
//! every edit goes through the same `Change`s, so each view shows the
//! other's at once, and Save, Discard and the leave prompt stay the editor's.
//!
//! **Shared with the desktop** (`shelbi_app::settings`): where each status
//! sits (`workflow_graph`: columns by category, rows by the catalog's
//! order), which node an arrow key goes to, and the selection
//! (`workflow_builder::BuilderState`: a status, a transition, a transition
//! being drawn or added, Back to the status a transition panel came from).
//! The cells, the lines and the keys are the TUI's ([`super::canvas`]).
//!
//! **Canvas** (the editor's middle section): each status a box (its symbol
//! and name, `Owner · agent`, `Initial`), transitions as lines with
//! arrowheads and their actions where there's room, the status order
//! dotted. Keys, with the canvas focused: ←↑↓→ move the keyboard's node (a
//! heavy border, its four ◆ handles showing), `]` / `[` step through the
//! transitions instead, Enter selects (the panel opens, the keys go to it),
//! `a` adds a transition from the keyboard's node, `n` adds a status, `x`
//! removes the selection, Shift+arrows scroll, Home scrolls back, Esc steps
//! back (a drawing, then the selection, then out of the editor). The mouse:
//! a click selects a node or a line, hovering a node shows its handles, a
//! press on one draws a transition (drop it on a status, or let go and click
//! one), the wheel scrolls. Nothing needs a drag.
//!
//! **Add transition**: from a status (`a`, its panel's Add transition, a
//! handle), choose the destination (arrows then Enter, a click, or the
//! panel's To), configure its actions, then Add transition (or Cancel). Esc
//! at any step drops it: nothing reaches the draft before Add transition,
//! which the draft checks as the Form's Add transition is.
//!
//! **The panel** (beside the canvas, or in its place when the editor is
//! narrower than [`WIDE`], with ← Graph back to the same selection): a
//! status (the status picker, its category, Owner, Agent, Workspace tags,
//! Set as initial, its outgoing transitions, Add transition, Remove), a
//! transition (From, To, its ordered actions, its Details: Target branch,
//! Run, Ready check, Ready timeout, URL; Delete), one being drawn (From, To,
//! Cancel) and a new one (From, To, its actions, Cancel, Add transition).
//! The rows work as the Form's: the same pickers, text fields and keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use shelbi_app::settings::workflow_builder::{move_action, BuilderState, Removal, Selection};
use shelbi_app::settings::workflow_editor::{
    action_label, owner_label, Change, EditorState, Field, Mode, ACTIONS, TAGS_PLACEHOLDER,
};
use shelbi_app::settings::workflow_graph::{EdgeKind, Graph, Pt, Side};
use shelbi_app::settings::workflows::category_label;
use shelbi_core::{StatusCategory, TransitionAction, Workflow};

use super::canvas::{self, Border, Canvas, EdgeIn, Input, Look, NodeLook, Route};
use super::{Act, Choice, EditorAction, Hit, Item, Pick, Section, Text, WorkflowEditorPage};
use crate::panel;
use crate::shell::settings::workflows::{add_line, error_rows, plain_line, Nav};
use crate::shell::settings::{follow, heading, picker_rows, wrapped, Row};
use crate::theme::palette;

/// From this width the panel sits beside the canvas; narrower, it takes the
/// canvas's place while it's open.
pub(crate) const WIDE: u16 = 104;
/// The panel's width beside the canvas.
const PANEL_W: u16 = 38;
/// Shift+arrows scroll the canvas this far.
const PAN_X: i32 = 6;
const PAN_Y: i32 = 3;

/// The Builder's presentation over the draft: the shared selection, plus
/// the TUI's scroll, the panel's place and the laid-out canvas. Nothing
/// here is written to the workflow's YAML.
#[derive(Debug, Default)]
pub(crate) struct BuilderView {
    pub state: BuilderState,
    /// The canvas, laid out again whenever its input changes.
    canvas: Option<Canvas>,
    /// The canvas's scroll (cells), and where it last drew.
    pub pan: (i32, i32),
    pub area: Rect,
    /// Narrower than [`WIDE`] at the last draw.
    pub narrow: bool,
    /// Narrow: the panel shows in the canvas's place.
    pub panel_open: bool,
    /// Scroll the keyboard's node into view at the next draw.
    pub follow: bool,
    panel_scroll: u16,
}

impl BuilderView {
    /// Drop what the draft no longer has (a removal, a discard, a reload).
    pub(crate) fn reconcile(&mut self, workflow: Option<&Workflow>) {
        match workflow {
            Some(w) => self.state.reconcile(w),
            None => self.state = BuilderState::default(),
        }
        if self.state.selection.is_none() {
            self.panel_open = false;
        }
    }

    #[cfg(test)]
    pub(crate) fn canvas(&self) -> Option<&Canvas> {
        self.canvas.as_ref()
    }
}

/// The Builder's own acts (its rows' and its menus').
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BAct {
    SelectStatus(String),
    OpenTransition(usize),
    /// Add transition from this status.
    BeginDraw(String),
    /// The drawing's or the new transition's source.
    SetFrom(String),
    /// The drawing's destination: the new transition.
    PickTo(String),
    /// The new transition's destination.
    SetNewTo(String),
    NewSetAction(usize, TransitionAction),
    NewAddAction(TransitionAction),
    NewRemoveAction(usize),
    NewMoveAction(usize, bool),
    CancelNew,
    ConfirmNew,
    RemoveStatus(String),
    RemoveTransition(usize),
    /// The panel's ← (and Esc in it).
    Back,
    /// Deselect (the panel's ✕).
    Close,
}

/// A category's symbol on the canvas and in the panel.
pub(crate) fn category_glyph(category: StatusCategory) -> &'static str {
    match category {
        StatusCategory::Backlog => "◌",
        StatusCategory::Ready => "○",
        StatusCategory::Active => "◐",
        StatusCategory::Handoff => "◑",
        StatusCategory::Done => "●",
        StatusCategory::Archived => "⊘",
    }
}

/// A draft's workflow, if one's loaded.
pub(crate) fn state_workflow(state: &Option<EditorState>) -> Option<&Workflow> {
    state.as_ref().map(EditorState::workflow)
}

impl WorkflowEditorPage {
    /// `item`'s section: in the Builder, everything under the canvas is
    /// its panel's.
    pub(super) fn section_of(&self, item: &Item) -> Section {
        match item.section() {
            s @ (Section::Header | Section::Canvas) => s,
            s if !self.in_builder() => s,
            _ => Section::Panel,
        }
    }

    pub(super) fn in_builder(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.mode == Mode::Builder)
    }

    /// The shared graph of the draft (positions, arrow-key neighbors).
    pub(super) fn graph(&self) -> Option<Graph> {
        self.state.as_ref().map(|s| Graph::of(s.workflow(), s.catalog()))
    }

    fn status_name(&self, id: &str) -> String {
        self.state
            .as_ref()
            .and_then(|s| s.status(id))
            .map(|s| s.name.clone())
            .unwrap_or_else(|| self.catalog_name(id))
    }

    /// Whether the panel shows: something's selected and, narrow, it's open
    /// (a drawing keeps the canvas, where its destination is chosen).
    fn panel_shown(&self) -> bool {
        let b = &self.builder;
        match &b.state.selection {
            None => false,
            Some(Selection::Draw(_)) => !b.narrow,
            Some(_) => !b.narrow || b.panel_open,
        }
    }

    fn canvas_shown(&self) -> bool {
        !(self.builder.narrow && self.panel_shown())
    }

    // --- rows -----------------------------------------------------------------

    /// The Builder's rows after the header: the canvas, then the panel's.
    pub(super) fn builder_items(&self, out: &mut Vec<Item>) {
        if self.canvas_shown() {
            out.push(Item::Canvas);
        }
        if !self.panel_shown() {
            return;
        }
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let b = &self.builder.state;
        if b.back.is_some() || self.builder.narrow {
            out.push(Item::PanelBack);
        }
        match b.selection.clone() {
            Some(Selection::Status(id)) => {
                out.extend([Item::PanelStatus, Item::Owner(id.clone()), Item::Agent(id.clone()), Item::Text(Text::Tags(id.clone()))]);
                if state.initial_status().is_none_or(|s| s.id != id) {
                    out.push(Item::SetInitial(id.clone()));
                }
                for (i, t) in state.transitions().iter().enumerate() {
                    if t.from == id {
                        out.push(Item::Outgoing(i));
                    }
                }
                out.extend([Item::StartTransition(id.clone()), Item::RemoveStatus(id)]);
            }
            Some(Selection::Transition { index, .. }) => {
                let Some(t) = state.transitions().get(index) else {
                    return;
                };
                out.extend([Item::From(index), Item::To(index)]);
                out.extend((0..t.actions.len()).map(|slot| Item::Action(index, slot)));
                if t.actions.len() < ACTIONS.len() {
                    out.push(Item::AddAction(index));
                }
                if self.details == Some(index) {
                    out.push(Item::Text(Text::Target));
                    out.extend((0..self.run.len()).map(|l| Item::Text(Text::Run(l))));
                    out.extend([Item::AddRun, Item::Text(Text::Ready), Item::Text(Text::Timeout), Item::Text(Text::Url)]);
                }
                out.push(Item::RemoveTransition(index));
            }
            Some(Selection::Draw(_)) => out.extend([Item::DrawFrom, Item::DrawTo, Item::CancelNew]),
            Some(Selection::New(n)) => {
                out.extend([Item::NewFrom, Item::NewTo]);
                out.extend((0..n.actions.len()).map(Item::NewAction));
                if n.actions.len() < ACTIONS.len() {
                    out.push(Item::NewAddAction);
                }
                out.extend([Item::CancelNew, Item::ConfirmNew]);
            }
            None => {}
        }
    }

    /// What Enter does on a Builder row.
    pub(super) fn builder_primary(&self, item: &Item) -> Act {
        match item {
            Item::Canvas => Act::Nothing,
            Item::PanelBack => Act::Builder(BAct::Back),
            Item::PanelStatus => Act::Pick(Pick::PanelStatus),
            Item::Owner(id) => Act::Pick(Pick::Owner(id.clone())),
            Item::Agent(id) => Act::Pick(Pick::Agent(id.clone())),
            Item::SetInitial(id) => Act::Change(Change::InitialStatus(id.clone())),
            Item::Outgoing(i) => Act::Builder(BAct::OpenTransition(*i)),
            Item::StartTransition(id) => Act::Builder(BAct::BeginDraw(id.clone())),
            Item::RemoveStatus(id) => Act::Builder(BAct::RemoveStatus(id.clone())),
            Item::From(i) => Act::Pick(Pick::From(*i)),
            Item::To(i) => Act::Pick(Pick::To(*i)),
            Item::RemoveTransition(i) => Act::Builder(BAct::RemoveTransition(*i)),
            Item::DrawFrom | Item::NewFrom => Act::Pick(if *item == Item::DrawFrom { Pick::DrawFrom } else { Pick::NewFrom }),
            Item::DrawTo => Act::Pick(Pick::DrawTo),
            Item::NewTo => Act::Pick(Pick::NewTo),
            Item::NewAction(slot) => Act::Pick(Pick::NewSetAction(*slot)),
            Item::NewAddAction => Act::Pick(Pick::NewAddAction),
            Item::CancelNew => Act::Builder(BAct::CancelNew),
            Item::ConfirmNew => Act::Builder(BAct::ConfirmNew),
            _ => Act::Nothing,
        }
    }

    /// The Builder's pickers' title and choices (`None`: not the Builder's).
    pub(super) fn builder_pick(&self, pick: &Pick) -> Option<(String, Vec<Choice>)> {
        let state = self.state.as_ref()?;
        let b = &self.builder.state;
        let current = |yes: bool| if yes { "current" } else { "" };
        let statuses = |at: Option<&str>, skip: Option<&str>, act: &dyn Fn(String) -> BAct| -> Vec<Choice> {
            state
                .workflow()
                .statuses
                .iter()
                .filter(|s| Some(s.id.as_str()) != skip)
                .map(|s| Self::choice(&s.name, current(at == Some(s.id.as_str())), Act::Builder(act(s.id.clone()))))
                .collect()
        };
        let new_actions = || b.new_transition().map(|n| n.actions.clone()).unwrap_or_default();
        Some(match pick {
            Pick::PanelStatus => ("Status".into(), statuses(b.selected_status(), None, &BAct::SelectStatus)),
            Pick::DrawFrom | Pick::NewFrom => {
                let at = b.drawing().map(|d| d.from.clone()).or_else(|| b.new_transition().map(|n| n.from.clone()));
                ("From".into(), statuses(at.as_deref(), None, &BAct::SetFrom))
            }
            Pick::DrawTo => {
                let d = b.drawing()?;
                ("To".into(), statuses(d.over.as_deref(), Some(&d.from), &BAct::PickTo))
            }
            Pick::NewTo => {
                let n = b.new_transition()?;
                ("To".into(), statuses(Some(&n.to), None, &BAct::SetNewTo))
            }
            Pick::NewSetAction(slot) => {
                let actions = new_actions();
                let at = actions.get(*slot).copied();
                let choices = ACTIONS
                    .iter()
                    .filter(|a| Some(**a) == at || !actions.contains(a))
                    .map(|a| Self::choice(action_label(*a), current(Some(*a) == at), Act::Builder(BAct::NewSetAction(*slot, *a))))
                    .collect();
                ("Action".into(), choices)
            }
            Pick::NewAddAction => {
                let actions = new_actions();
                let choices = ACTIONS
                    .iter()
                    .filter(|a| !actions.contains(a))
                    .map(|a| Self::choice(action_label(*a), "", Act::Builder(BAct::NewAddAction(*a))))
                    .collect();
                ("Add action".into(), choices)
            }
            _ => return None,
        })
    }

    // --- acting ---------------------------------------------------------------

    /// Switch the editor to `mode`, carrying the place across: the Form's
    /// status or transition row becomes the Builder's selection, and back.
    pub(super) fn set_view(&mut self, mode: Mode) {
        let Some(from) = self.state.as_ref().map(|s| s.mode) else {
            return;
        };
        if from == mode {
            return;
        }
        self.typing = None;
        let here = self.current();
        if let Some(state) = self.state.as_mut() {
            state.mode = mode;
        }
        match mode {
            Mode::Builder => {
                let b = &mut self.builder;
                match here {
                    Some(Item::Status(id) | Item::Text(Text::Tags(id))) => b.state.select_status(&id),
                    Some(Item::Transition(i) | Item::Action(i, _) | Item::AddAction(i) | Item::Details(i)) => {
                        let from = self.state.as_ref().and_then(|s| s.transitions().get(i)).map(|t| t.from.clone());
                        b.state.select_transition(i);
                        b.state.focus = from;
                    }
                    _ => {}
                }
                b.follow = true;
                if let Some(i) = self.builder.state.selected_transition() {
                    self.open_details(Some(i));
                }
                if !self.select_item(&Item::Canvas) {
                    self.cursor = self.first_panel_item().unwrap_or(0);
                }
            }
            Mode::Form => {
                let item = match &self.builder.state.selection {
                    Some(Selection::Status(id)) => Some(Item::Status(id.clone())),
                    Some(Selection::Transition { index, .. }) => Some(Item::Transition(*index)),
                    _ => None,
                };
                if !item.is_some_and(|it| self.select_item(&it)) {
                    self.select_item(&Item::View);
                }
            }
        }
        self.clamp();
    }

    fn first_panel_item(&self) -> Option<usize> {
        self.items().iter().position(|it| self.section_of(it) == Section::Panel && *it != Item::PanelBack)
    }

    /// Put the keys in the panel (its first row, or `item`).
    fn open_panel(&mut self, item: Option<Item>) {
        self.builder.panel_open = true;
        self.builder.panel_scroll = 0;
        if !item.is_some_and(|it| self.select_item(&it)) {
            if let Some(i) = self.first_panel_item() {
                self.cursor = i;
            }
        }
    }

    fn focus_canvas(&mut self) {
        self.builder.follow = true;
        self.select_item(&Item::Canvas);
    }

    /// After the selection changed: a transition's Details open, and the
    /// cursor lands on a row that exists.
    pub(super) fn builder_synced(&mut self) {
        if let Some(i) = self.builder.state.selected_transition() {
            if self.details != Some(i) {
                self.open_details(Some(i));
            }
        }
        if self.builder.state.selection.is_none() {
            self.builder.panel_open = false;
        }
        self.clamp();
    }

    pub(super) fn builder_act(&mut self, act: BAct) -> EditorAction {
        let b = &mut self.builder.state;
        match act {
            BAct::SelectStatus(id) => {
                b.select_status(&id);
                self.builder.follow = true;
                self.open_panel(Some(Item::PanelStatus));
            }
            BAct::OpenTransition(i) => {
                b.open_transition(i);
                self.builder_synced();
                self.open_panel(None);
            }
            BAct::BeginDraw(id) => {
                b.begin_draw(&id, None, None);
                b.focus = Some(id);
                b.focus_transition = None;
                self.focus_canvas();
            }
            BAct::SetFrom(id) => {
                b.set_from(&id);
                if b.drawing().is_some() {
                    b.focus = Some(id);
                }
            }
            BAct::PickTo(id) => {
                if b.pick(&id) {
                    self.open_panel(Some(Item::NewAddAction));
                }
            }
            BAct::SetNewTo(id) => {
                if let Some(n) = b.new_transition_mut() {
                    n.to = id;
                }
            }
            BAct::NewSetAction(slot, a) => {
                if let Some(action) = b.new_transition_mut().and_then(|n| n.actions.get_mut(slot)) {
                    *action = a;
                }
            }
            BAct::NewAddAction(a) => {
                if let Some(n) = b.new_transition_mut() {
                    n.actions.push(a);
                    let slot = n.actions.len() - 1;
                    self.select_item(&Item::NewAction(slot));
                }
            }
            BAct::NewRemoveAction(slot) => {
                if let Some(n) = b.new_transition_mut().filter(|n| slot < n.actions.len()) {
                    n.actions.remove(slot);
                }
            }
            BAct::NewMoveAction(slot, down) => {
                if let Some(n) = b.new_transition_mut() {
                    let to = if down { slot + 1 } else { slot.wrapping_sub(1) };
                    if to < n.actions.len() {
                        move_action(&mut n.actions, slot, to);
                        self.select_item(&Item::NewAction(to));
                    }
                }
            }
            BAct::CancelNew | BAct::Back => return self.panel_back(),
            BAct::ConfirmNew => {
                let Some(t) = b.new_transition().map(|n| n.transition()) else {
                    return EditorAction::Stay;
                };
                self.change(Change::InsertTransition(t));
                let added = self.state.as_ref().filter(|s| s.error_at(&Field::Transitions).is_none()).map(|s| s.transitions().len() - 1);
                if let Some(index) = added {
                    self.builder.state.added(index);
                    self.builder_synced();
                    self.open_panel(Some(Item::AddAction(index)));
                }
            }
            BAct::RemoveStatus(id) => {
                let only = self.state.as_ref().is_some_and(|s| s.workflow().statuses.len() <= 1);
                if !only {
                    self.change(Change::RemoveStatus(id));
                    if self.builder.state.selection.is_none() {
                        self.focus_canvas();
                    }
                }
            }
            BAct::RemoveTransition(i) => {
                self.change(Change::RemoveTransition(i));
                self.builder.state.deselect();
                self.builder.state.focus_transition = None;
                self.focus_canvas();
            }
            BAct::Close => {
                self.clear_new_error();
                self.builder.state.deselect();
                self.focus_canvas();
            }
        }
        self.builder_synced();
        EditorAction::Stay
    }

    fn clear_new_error(&mut self) {
        let b = &self.builder.state;
        if b.drawing().is_some() || b.new_transition().is_some() {
            if let Some(state) = self.state.as_mut() {
                state.clear_error_at(&Field::Transitions);
            }
        }
    }

    /// The panel's ← (Esc in it): back to the status a transition panel
    /// came from (a drawing or a new transition dropped), else to the
    /// graph, the selection kept (narrow, the canvas shows again).
    fn panel_back(&mut self) -> EditorAction {
        let b = &self.builder.state;
        let steps = b.back.is_some() || b.drawing().is_some() || b.new_transition().is_some();
        if steps {
            self.clear_new_error();
            self.builder.state.escape();
            self.builder_synced();
            if self.builder.state.selection.is_some() && self.panel_shown() {
                self.open_panel(None);
            } else {
                self.focus_canvas();
            }
        } else {
            self.builder.panel_open = false;
            self.focus_canvas();
        }
        self.clamp();
        EditorAction::Stay
    }

    /// Add transition from the canvas: from the keyboard's node (or the
    /// selected status, else the first).
    fn start_transition(&mut self) {
        let from = self
            .builder
            .state
            .focus
            .clone()
            .or_else(|| self.builder.state.selected_status().map(str::to_string))
            .or_else(|| self.state.as_ref().and_then(|s| s.workflow().statuses.first().map(|s| s.id.clone())));
        if let Some(from) = from {
            self.builder_act(BAct::BeginDraw(from));
        }
    }

    // --- keys -----------------------------------------------------------------

    /// A key while the Builder shows (no prompt, menu or typing).
    pub(super) fn builder_key(&mut self, k: KeyEvent, nav: Nav) -> EditorAction {
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let Some(item) = self.current() else {
            return EditorAction::Stay;
        };
        match k.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.step_section(k.code == KeyCode::Tab);
                return EditorAction::Stay;
            }
            KeyCode::Char('v') if plain => return self.run(Act::ToggleView),
            KeyCode::Char('y') if plain => return self.run(Act::Leave(shelbi_app::settings::workflow_editor::Leave::EditYaml)),
            KeyCode::Char('m') | KeyCode::Char('.') if plain => return self.run(Act::Actions),
            _ => {}
        }
        match self.section_of(&item) {
            Section::Header => self.header_key(k, nav, &item),
            Section::Canvas => self.canvas_key(k, nav),
            _ => self.panel_key(k, nav, item),
        }
    }

    fn header_key(&mut self, k: KeyEvent, nav: Nav, item: &Item) -> EditorAction {
        let header = self.items().iter().take_while(|it| self.section_of(it) == Section::Header).count();
        match k.code {
            KeyCode::Esc => return self.run(Act::Leave(shelbi_app::settings::workflow_editor::Leave::Back)),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(header.saturating_sub(1)),
            _ if nav.activate => {
                let act = self.primary(item);
                return self.run(act);
            }
            _ if nav.up => self.cursor = self.cursor.saturating_sub(1),
            _ if nav.down => self.cursor = header.min(self.items().len().saturating_sub(1)),
            _ => {}
        }
        EditorAction::Stay
    }

    fn canvas_key(&mut self, k: KeyEvent, nav: Nav) -> EditorAction {
        let Some(g) = self.graph() else {
            return EditorAction::Stay;
        };
        let Some(workflow) = self.state.as_ref().map(|s| s.workflow().clone()) else {
            return EditorAction::Stay;
        };
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let arrow = match k.code {
            KeyCode::Up => Some(Side::Top),
            KeyCode::Down => Some(Side::Bottom),
            KeyCode::Left => Some(Side::Left),
            KeyCode::Right => Some(Side::Right),
            _ => None,
        };
        if let (Some(side), true) = (arrow, shift) {
            let (dx, dy) = match side {
                Side::Top => (0, -PAN_Y),
                Side::Bottom => (0, PAN_Y),
                Side::Left => (-PAN_X, 0),
                Side::Right => (PAN_X, 0),
            };
            self.pan_by(dx, dy);
            return EditorAction::Stay;
        }
        let b = &mut self.builder.state;
        if let Some(side) = arrow {
            b.arrow(&g, side);
            if b.drawing().is_some() {
                let focus = b.focus.clone();
                b.aim(focus.as_deref());
            }
            self.builder.follow = true;
            return EditorAction::Stay;
        }
        match k.code {
            KeyCode::Char(']') => b.step_transition(&workflow, true),
            KeyCode::Char('[') => b.step_transition(&workflow, false),
            KeyCode::Home => self.builder.pan = (0, 0),
            KeyCode::Esc => {
                let b = &self.builder.state;
                if b.selection.is_none() {
                    return self.run(Act::Leave(shelbi_app::settings::workflow_editor::Leave::Back));
                }
                self.clear_new_error();
                self.builder.state.escape();
                self.builder_synced();
            }
            KeyCode::Char('a') => self.start_transition(),
            KeyCode::Char('n') => return self.run(Act::Pick(Pick::AddStatus)),
            KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                let removal = b.removal().or_else(|| match (b.focus_transition, &b.focus) {
                    (Some(i), _) => Some(Removal::Transition(i)),
                    (None, Some(id)) if b.selection.is_none() => Some(Removal::Status(id.clone())),
                    _ => None,
                });
                match removal {
                    Some(Removal::Status(id)) => return self.builder_act(BAct::RemoveStatus(id)),
                    Some(Removal::Transition(i)) => return self.builder_act(BAct::RemoveTransition(i)),
                    None => {}
                }
            }
            _ if nav.activate => {
                let drawing = b.drawing().is_some();
                let already = match (&b.selection, b.focus_transition, &b.focus) {
                    (Some(Selection::Transition { index, .. }), Some(i), _) => *index == i,
                    (Some(Selection::Status(id)), None, Some(f)) => id == f,
                    _ => false,
                };
                if b.focus.is_none() && b.focus_transition.is_none() {
                    b.tab(&workflow, true);
                    self.builder.follow = true;
                    return EditorAction::Stay;
                }
                if !already {
                    b.enter();
                }
                self.builder_synced();
                if drawing && self.builder.state.new_transition().is_none() {
                    // The source itself: still choosing.
                    return EditorAction::Stay;
                }
                let at = if drawing { Some(Item::NewAddAction) } else { None };
                self.open_panel(at);
            }
            _ => {}
        }
        EditorAction::Stay
    }

    fn panel_key(&mut self, k: KeyEvent, nav: Nav, item: Item) -> EditorAction {
        let moving = k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        if moving && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            let down = k.code == KeyCode::Down;
            let act = match item {
                Item::Action(i, slot) => Act::MoveAction(i, slot, down),
                Item::NewAction(slot) => Act::Builder(BAct::NewMoveAction(slot, down)),
                Item::Text(Text::Run(line)) => Act::MoveRun(line, down),
                _ => return EditorAction::Stay,
            };
            return self.run(act);
        }
        let items = self.items();
        let first = items.iter().position(|it| self.section_of(it) == Section::Panel).unwrap_or(0);
        let last = items.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc => return self.builder_act(BAct::Back),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(8).max(first),
            KeyCode::PageDown => self.cursor = (self.cursor + 8).min(last),
            KeyCode::Home => self.cursor = first,
            KeyCode::End => self.cursor = last,
            _ if nav.activate => {
                let act = self.primary(&item);
                return self.run(act);
            }
            _ if nav.up => self.cursor = self.cursor.saturating_sub(1).max(first),
            _ if nav.down => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Char('x') | KeyCode::Delete => {
                let act = match item {
                    Item::Action(i, slot) => Act::Change(Change::RemoveAction { index: i, slot }),
                    Item::NewAction(slot) => Act::Builder(BAct::NewRemoveAction(slot)),
                    Item::Text(Text::Run(line)) => Act::RemoveRun(line),
                    Item::RemoveStatus(id) => Act::Builder(BAct::RemoveStatus(id)),
                    Item::RemoveTransition(i) | Item::From(i) | Item::To(i) => Act::Builder(BAct::RemoveTransition(i)),
                    _ => return EditorAction::Stay,
                };
                return self.run(act);
            }
            KeyCode::Char('n') => {
                let act = match item {
                    Item::Action(i, _) | Item::AddAction(i) | Item::From(i) | Item::To(i) => Act::Pick(Pick::AddAction(i)),
                    Item::Text(Text::Run(_)) | Item::AddRun => Act::AddRun,
                    Item::NewAction(_) | Item::NewAddAction | Item::NewFrom | Item::NewTo => Act::Pick(Pick::NewAddAction),
                    _ => return EditorAction::Stay,
                };
                return self.run(act);
            }
            _ => {}
        }
        EditorAction::Stay
    }

    fn pan_by(&mut self, dx: i32, dy: i32) {
        let b = &mut self.builder;
        let (w, h) = b.canvas.as_ref().map_or((0, 0), |c| (c.w, c.h));
        let max_x = (w - b.area.width as i32).max(0);
        let max_y = (h - b.area.height as i32).max(0);
        b.pan = ((b.pan.0 + dx).clamp(0, max_x), (b.pan.1 + dy).clamp(0, max_y));
    }

    // --- mouse ----------------------------------------------------------------

    /// A mouse event over the Builder's canvas (`None`: not the canvas's).
    pub(super) fn builder_mouse(&mut self, m: MouseEvent) -> Option<EditorAction> {
        let area = self.builder.area;
        if !self.canvas_shown() || area.width == 0 {
            return None;
        }
        let inside = panel::in_rect(area, m.column, m.row);
        let at = (m.column as i32 - area.x as i32 + self.builder.pan.0, m.row as i32 - area.y as i32 + self.builder.pan.1);
        let canvas = self.builder.canvas.as_ref()?;
        let node = canvas.node_at(at.0, at.1).map(|n| n.id.clone());
        let handle = |id: &str| canvas.node(id).and_then(|n| Side::ALL.into_iter().find(|&s| n.handle(s) == at));
        let hovered = self.builder.state.hover.clone();
        let on_handle = hovered.as_deref().and_then(handle).map(|s| (hovered.clone().unwrap_or_default(), s));
        let edge = canvas.transition_at(at.0, at.1);
        let held = self.builder.state.drawing().is_some_and(|d| d.pointer.is_some());
        let b = &mut self.builder.state;
        match m.kind {
            MouseEventKind::Moved if inside => {
                b.hover = node.clone().or_else(|| on_handle.as_ref().map(|(id, _)| id.clone()));
                b.hot = on_handle.map(|(_, s)| s);
                Some(EditorAction::Stay)
            }
            MouseEventKind::Drag(MouseButton::Left) if held => {
                if let Some(Selection::Draw(d)) = &mut b.selection {
                    d.pointer = Some(Pt::new(at.0 as f32, at.1 as f32));
                }
                b.aim(node.as_deref());
                Some(EditorAction::Stay)
            }
            MouseEventKind::Up(MouseButton::Left) if held => {
                b.release();
                self.after_pick();
                Some(EditorAction::Stay)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside => {
                let d = if m.kind == MouseEventKind::ScrollDown { 2 } else { -2 };
                if m.modifiers.contains(KeyModifiers::SHIFT) {
                    self.pan_by(d * 2, 0);
                } else {
                    self.pan_by(0, d);
                }
                Some(EditorAction::Stay)
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight if inside => {
                self.pan_by(if m.kind == MouseEventKind::ScrollRight { 4 } else { -4 }, 0);
                Some(EditorAction::Stay)
            }
            MouseEventKind::Down(MouseButton::Left) if inside => {
                self.typing = None;
                self.select_item(&Item::Canvas);
                let b = &mut self.builder.state;
                if let Some((id, side)) = on_handle {
                    b.begin_draw(&id, Some(side), Some(Pt::new(at.0 as f32, at.1 as f32)));
                    b.focus = Some(id);
                    return Some(EditorAction::Stay);
                }
                let drawing = b.drawing().is_some();
                match (node, edge) {
                    (Some(id), _) if drawing => {
                        b.pick(&id);
                        self.after_pick();
                    }
                    (Some(id), _) => {
                        b.select_status(&id);
                        b.focus_transition = None;
                        self.builder_synced();
                        if self.builder.narrow {
                            self.open_panel(None);
                        }
                    }
                    (None, Some(i)) if !drawing => {
                        let from = self.state.as_ref().and_then(|s| s.transitions().get(i)).map(|t| t.from.clone());
                        b.select_transition(i);
                        b.focus = from;
                        b.focus_transition = None;
                        self.builder_synced();
                        if self.builder.narrow {
                            self.open_panel(None);
                        }
                    }
                    _ if !drawing => {
                        b.deselect();
                        self.builder_synced();
                    }
                    _ => {}
                }
                Some(EditorAction::Stay)
            }
            _ => inside.then_some(EditorAction::Stay),
        }
    }

    /// A destination was picked (or a drag let go): a new transition opens
    /// in the panel; otherwise the drawing waits for a click.
    fn after_pick(&mut self) {
        if self.builder.state.new_transition().is_some() {
            self.builder_synced();
            self.open_panel(Some(Item::NewAddAction));
        }
    }

    // --- drawing --------------------------------------------------------------

    /// The hint band's keys for the Builder.
    pub(super) fn builder_hints(&self, moves: &str) -> String {
        let b = &self.builder.state;
        match self.current() {
            Some(Item::Canvas) if b.drawing().is_some() => "←↑↓→ destination · ⏎ choose · Esc cancel".into(),
            Some(Item::Canvas) => {
                let x = if b.selection.is_some() || b.focus_transition.is_some() { " · x remove" } else { "" };
                format!("←↑↓→/[ ] move · ⏎ open · a transition · n status{x} · v form · Tab · Esc back")
            }
            Some(item) if self.section_of(&item) == Section::Header => format!("←→ choose · ⏎ press · Tab canvas · v {} · ^S save · Esc back", if self.in_builder() { "form" } else { "builder" }),
            Some(Item::Action(..) | Item::NewAction(_)) => format!("{moves} · ⏎ change · ⇧↑↓ move · x remove · n add · Esc back"),
            Some(Item::Text(Text::Run(_))) => format!("{moves} · ⏎ edit · ⇧↑↓ move · x remove · n add · Esc back"),
            Some(Item::Text(_)) => format!("{moves} · ⏎ edit · Tab section · ^S save · Esc back"),
            _ => format!("{moves} · ⏎ choose · Tab section · ^S save · Esc back"),
        }
    }

    /// The canvas's input from the draft.
    fn canvas_input(&self, state: &EditorState, graph: &Graph) -> Input {
        let initial = state.initial_status().map(|s| s.id.clone());
        Input::of(
            graph,
            |id| {
                let s = state.status(id);
                let name = s.map_or_else(|| id.to_string(), |s| s.name.clone());
                let summary = s.map_or(String::new(), |s| format!("{} · {}", owner_label(s.owner), s.agent.as_deref().unwrap_or("None")));
                (name, summary, initial.as_deref() == Some(id))
            },
            |i| {
                let t = state.transitions().get(i);
                t.map(|t| t.actions.iter().map(|&a| action_label(a)).collect::<Vec<_>>().join(" · ")).unwrap_or_default()
            },
        )
    }

    /// Draw the Builder over `area` (under the header): the hint, then the
    /// canvas and the panel.
    pub(super) fn render_builder(&mut self, f: &mut Frame, area: Rect) {
        if self.state.is_none() {
            return;
        }
        // Narrow or wide: the rows change, so keep the cursor's row.
        let here = self.current();
        let narrow = area.width < WIDE;
        if narrow != self.builder.narrow {
            self.builder.narrow = narrow;
            if let Some(it) = here {
                if !self.select_item(&it) {
                    if self.panel_shown() {
                        self.open_panel(None);
                    } else {
                        self.select_item(&Item::Canvas);
                    }
                }
            }
            self.clamp();
        }
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let graph = Graph::of(state.workflow(), state.catalog());
        let input = self.canvas_input(state, &graph);
        if self.builder.canvas.as_ref().is_none_or(|c| c.input != input) {
            self.builder.canvas = Some(canvas::layout(input));
        }
        let (canvas_rect, panel_rect) = if !self.panel_shown() {
            (Some(area), None)
        } else if narrow {
            (None, Some(area))
        } else {
            let pw = PANEL_W.min(area.width / 2);
            let c = Rect { width: area.width - pw - 1, ..area };
            let p = Rect { x: area.right() - pw, width: pw, ..area };
            // The divider.
            for y in area.y..area.bottom() {
                f.render_widget(
                    Paragraph::new(Span::styled("│", Style::default().fg(palette().muted))),
                    Rect { x: c.right(), y, width: 1, height: 1 },
                );
            }
            (Some(c), Some(p))
        };
        match canvas_rect {
            Some(r) => self.render_canvas(f, r),
            None => self.builder.area = Rect::default(),
        }
        if let Some(r) = panel_rect {
            self.render_panel(f, r);
        }
    }

    fn render_canvas(&mut self, f: &mut Frame, area: Rect) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let muted = Style::default().fg(palette().muted);
        // The hint over the canvas.
        let b = &self.builder.state;
        let hint = match (b.drawing(), state.workflow().statuses.is_empty()) {
            (_, true) => String::new(),
            (Some(d), _) => match d.over.as_deref() {
                Some(to) => format!("Transition from {} to {}: ⏎ to choose it, Esc to cancel.", self.status_name(&d.from), self.status_name(to)),
                None => format!("Transition from {}: move to its destination (←↑↓→) and press ⏎, or click it.", self.status_name(&d.from)),
            },
            (None, _) => "←↑↓→ a status, [ ] a transition, ⏎ to open it; a adds a transition. ⇧+arrows scroll.".into(),
        };
        let unrouted = self.builder.canvas.as_ref().map_or(0, Canvas::unrouted);
        let mut top = area;
        let mut notes: Vec<(String, Style)> = Vec::new();
        if !hint.is_empty() {
            notes.push((hint, muted));
        }
        if unrouted > 0 {
            notes.push((format!("{unrouted} transition(s) have no room to draw; the Form lists every one."), Style::default().fg(palette().yellow)));
        }
        for (text, style) in notes {
            if top.height < 3 {
                break;
            }
            f.render_widget(Paragraph::new(Span::styled(panel::truncate_right(&text, top.width as usize), style)), Rect { height: 1, ..top });
            top.y += 1;
            top.height -= 1;
        }
        let area = top;
        self.builder.area = area;
        if state.workflow().statuses.is_empty() {
            let lines = [
                Line::from(Span::styled("This workflow has no statuses yet.", Style::default().fg(palette().white))),
                Line::from(Span::styled("Press n to add one from the project's statuses, or v for the Form.", muted)),
            ];
            for (i, line) in lines.into_iter().enumerate() {
                if (i as u16) < area.height {
                    f.render_widget(Paragraph::new(line), Rect { y: area.y + i as u16, height: 1, ..area });
                }
            }
            return;
        }
        let Some(canvas) = self.builder.canvas.as_ref() else {
            return;
        };
        // Keep the keyboard's node (or transition) in view.
        let b = &self.builder.state;
        if self.builder.follow {
            let target = match b.focus_transition.and_then(|i| canvas.transition_route(i)) {
                Some(r) => r.cells.get(r.cells.len() / 2).map(|&(x, y)| (x, y, 1, 1)),
                None => b.focus.as_deref().or(b.selected_status()).and_then(|id| canvas.node(id)).map(|n| (n.x - 1, n.y - 1, n.w + 2, canvas::NODE_H + 2)),
            };
            if let Some((x, y, w, h)) = target {
                let (aw, ah) = (area.width as i32, area.height as i32);
                let pan = &mut self.builder.pan;
                if x < pan.0 {
                    pan.0 = x;
                } else if x + w > pan.0 + aw {
                    pan.0 = (x + w - aw).min(x);
                }
                if y < pan.1 {
                    pan.1 = y;
                } else if y + h > pan.1 + ah {
                    pan.1 = (y + h - ah).min(y);
                }
            }
            self.builder.follow = false;
        }
        let canvas = self.builder.canvas.as_ref().expect("laid out");
        let max = ((canvas.w - area.width as i32).max(0), (canvas.h - area.height as i32).max(0));
        self.builder.pan = (self.builder.pan.0.clamp(0, max.0), self.builder.pan.1.clamp(0, max.1));
        let focused = self.current() == Some(Item::Canvas);
        let b = &self.builder.state;
        let ringed = b.ringed(state.workflow(), false);
        let keyboard = if focused && b.focus_transition.is_none() { b.focus.clone() } else { None };
        let selected_status = b.selected_status().map(str::to_string);
        let selected_transition = b.selected_transition();
        let focus_transition = if focused { b.focus_transition } else { None };
        let category = |id: &str| state.status(id).map(|s| s.category).or_else(|| self.category(id));
        let node = |id: &str| {
            let c = category(id);
            let selected = selected_status.as_deref() == Some(id);
            let ring = ringed.iter().any(|r| r == id);
            let key = keyboard.as_deref() == Some(id);
            let (border, color) = match (selected, ring, key) {
                (true, _, _) => (Border::Double, palette().cyan),
                (false, true, _) => (Border::Double, palette().yellow),
                (false, false, true) => (Border::Heavy, palette().white),
                _ => (Border::Plain, palette().gray),
            };
            let border_style = if border == Border::Plain { Style::default().fg(color) } else { Style::default().fg(color).add_modifier(Modifier::BOLD) };
            NodeLook {
                border,
                border_style,
                glyph: c.map_or("●", category_glyph),
                glyph_style: Style::default().fg(c.map_or(palette().foreground, crate::kanban::category_color)),
                name_style: Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
                fill: Style::default(),
            }
        };
        let edge = |e: &EdgeIn| match e.kind {
            EdgeKind::Order => (Style::default().fg(palette().muted), 0),
            EdgeKind::Transition(i) if Some(i) == selected_transition => (Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD), 2),
            EdgeKind::Transition(i) if Some(i) == focus_transition => (Style::default().fg(palette().white).add_modifier(Modifier::BOLD), 2),
            EdgeKind::Transition(_) => (Style::default().fg(palette().foreground), 1),
        };
        let yellow = Style::default().fg(palette().yellow).add_modifier(Modifier::BOLD);
        let mut preview: Vec<(Route, Style)> = Vec::new();
        match &b.selection {
            Some(Selection::Draw(d)) => match (&d.over, d.pointer, d.handle) {
                (Some(to), _, _) => preview.extend(canvas.preview(&d.from, to).map(|r| (r, yellow))),
                (None, Some(p), Some(side)) => preview.extend(pointer_route(canvas, &d.from, side, (p.x as i32, p.y as i32)).map(|r| (r, yellow))),
                _ => {}
            },
            Some(Selection::New(n)) => preview.extend(canvas.preview(&n.from, &n.to).map(|r| (r, yellow))),
            _ => {}
        }
        // Handles show where they can be used: the hovered node, a held
        // handle's drawing, and the keyboard's node (not while the keys
        // choose a destination: the panel and the hint say how).
        let mut handles: Vec<(String, Option<Side>)> = Vec::new();
        let keyboard_drawing = b.drawing().is_some_and(|d| d.pointer.is_none());
        let shown: Vec<String> = if keyboard_drawing { b.hover.iter().cloned().collect() } else { b.with_handles().into_iter().chain(keyboard.clone()).collect() };
        for id in shown {
            if !handles.iter().any(|(h, _)| *h == id) {
                let hot = b.hot_handle().filter(|(h, _)| *h == id).map(|(_, s)| s);
                handles.push((id, hot));
            }
        }
        let look = Look {
            line: Style::default().fg(palette().foreground),
            label: Style::default().fg(palette().gray),
            summary: Style::default().fg(palette().gray),
            initial: Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
            handle: Style::default().fg(palette().cyan),
            hot: Style::default().fg(palette().yellow).add_modifier(Modifier::BOLD),
            node: &node,
            edge: &edge,
            preview,
            handles,
        };
        canvas.paint(f.buffer_mut(), area, self.builder.pan, &look);
        // Clipped: say there's more.
        let more_right = self.builder.pan.0 < max.0;
        let more_below = self.builder.pan.1 < max.1;
        if (more_right || more_below) && area.height > 0 && area.width > 10 {
            let text = match (more_right, more_below) {
                (true, true) => "→ ↓ more",
                (true, false) => "→ more",
                _ => "↓ more",
            };
            let w = panel::display_width(text) as u16;
            f.render_widget(
                Paragraph::new(Span::styled(text, muted)),
                Rect { x: area.right() - w, y: area.bottom() - 1, width: w, height: 1 },
            );
        }
    }

    /// The panel's title for the selection.
    fn panel_title(&self) -> &'static str {
        match &self.builder.state.selection {
            Some(Selection::Status(_)) => "Status",
            Some(Selection::Transition { .. }) => "Transition",
            Some(Selection::Draw(_)) => "Add transition",
            Some(Selection::New(_)) => "New transition",
            None => "",
        }
    }

    fn render_panel(&mut self, f: &mut Frame, area: Rect) {
        if area.width < 4 || area.height == 0 {
            return;
        }
        let inner = Rect { x: area.x + 1, width: area.width.saturating_sub(2), ..area };
        let width = inner.width as usize;
        // The title, and ✕.
        let title = Rect { height: 1, ..inner };
        f.render_widget(
            Paragraph::new(Span::styled(self.panel_title(), Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD))),
            title,
        );
        let close = Rect { x: title.right().saturating_sub(3), width: 3.min(title.width), ..title };
        f.render_widget(Paragraph::new(Span::styled(" ✕ ", Style::default().fg(palette().gray))), close);
        self.hits.push((close, Hit::PanelClose));
        let body = Rect { y: inner.y + 2.min(inner.height), height: inner.height.saturating_sub(2), ..inner };
        if body.height == 0 {
            return;
        }
        let rows = self.panel_rows(width);
        let max = (rows.len() as u16).saturating_sub(body.height);
        // The Form's rule.
        self.builder.panel_scroll = follow(&rows, self.cursor, self.builder.panel_scroll, body.height);
        self.builder.panel_scroll = self.builder.panel_scroll.min(max);
        let clipped = self.builder.panel_scroll < max;
        for (i, row) in rows.iter().skip(self.builder.panel_scroll as usize).take(body.height as usize).enumerate() {
            let rect = Rect { y: body.y + i as u16, height: 1, ..body };
            if i as u16 + 1 == body.height && clipped {
                f.render_widget(Paragraph::new(Span::styled("↓ more", Style::default().fg(palette().muted))), rect);
                continue;
            }
            f.render_widget(Paragraph::new(row.line.clone()), rect);
            if let Some(a) = row.action {
                self.hits.push((rect, Hit::Item(a)));
            }
        }
    }

    /// The panel's rows at `width`.
    fn panel_rows(&self, width: usize) -> Vec<Row> {
        let mut rows = Vec::new();
        let Some(state) = self.state.as_ref() else {
            return rows;
        };
        let muted = Style::default().fg(palette().muted);
        let items = self.items();
        let index = |item: &Item| items.iter().position(|it| it == item);
        // Only the cursor's row shows the selection.
        let selected = |item: &Item| (index(item) == Some(self.cursor)).then_some(true);
        let row = |rows: &mut Vec<Row>, item: Item, line: Line<'static>| rows.push(Row { action: index(&item), ..Row::text(line) });
        let button = |rows: &mut Vec<Row>, item: Item, text: &str, style: Style| {
            let line = plain_line(text, selected(&item), width, style);
            row(rows, item, line);
        };
        // A picker or a field a blank row under what's before it: its label
        // over its box.
        let picker = |rows: &mut Vec<Row>, label: &str, value: &str, item: Item| {
            rows.push(Row::blank());
            picker_rows(rows, label, value, selected(&item) == Some(true), false, index(&item), width);
        };
        let field = |rows: &mut Vec<Row>, label: &str, text: Text, placeholder: &str| {
            let item = Item::Text(text.clone());
            rows.push(Row::blank());
            self.field(rows, label, &text, placeholder, selected(&item) == Some(true), false, index(&item), width);
        };
        let b = &self.builder.state;
        if index(&Item::PanelBack).is_some() {
            let text = match &b.back {
                Some(id) => format!("← {}", self.status_name(id)),
                None => "← Graph".into(),
            };
            button(&mut rows, Item::PanelBack, &text, Style::default().fg(palette().cyan));
        }
        let name = |id: &str| self.status_name(id);
        let action_rows = |rows: &mut Vec<Row>, actions: &[TransitionAction], item: &dyn Fn(usize) -> Item, add: Option<Item>, from: &str, to: &str| {
            rows.push(Row::blank());
            heading(rows, "Actions");
            wrapped(rows, &format!("Run in this order when a task moves from {from} to {to}."), width, muted);
            if actions.is_empty() {
                wrapped(rows, "  No actions", width, muted);
            }
            for (slot, a) in actions.iter().enumerate() {
                let it = item(slot);
                let line = plain_line(&format!("{}. {}", slot + 1, action_label(*a)), selected(&it), width, Style::default().fg(palette().foreground));
                row(rows, it, line);
            }
            if let Some(add) = add {
                let line = add_line("+ Add action", selected(&add), width);
                row(rows, add, line);
            }
        };
        match b.selection.clone() {
            Some(Selection::Status(id)) => {
                let Some(s) = state.status(&id) else {
                    return rows;
                };
                picker(&mut rows, "Status", &format!("{} {}", category_glyph(s.category), s.name), Item::PanelStatus);
                rows.push(Row::blank());
                rows.push(Row::text(Line::from(Span::styled("Category", Style::default().fg(palette().gray)))));
                rows.push(Row::text(Line::from(Span::styled(category_label(s.category), Style::default().fg(palette().foreground)))));
                picker(&mut rows, "Owner", owner_label(s.owner), Item::Owner(id.clone()));
                picker(&mut rows, "Agent", s.agent.as_deref().unwrap_or("None"), Item::Agent(id.clone()));
                field(&mut rows, "Workspace tags", Text::Tags(id.clone()), TAGS_PLACEHOLDER);
                if let Some(e) = state.error_at(&Field::Status(id.clone())) {
                    wrapped(&mut rows, e, width, Style::default().fg(palette().red));
                }
                rows.push(Row::blank());
                if index(&Item::SetInitial(id.clone())).is_some() {
                    button(&mut rows, Item::SetInitial(id.clone()), "Set as initial", Style::default().fg(palette().cyan));
                } else {
                    rows.push(Row::text(Line::from(Span::styled("  Initial status", Style::default().fg(palette().cyan)))));
                }
                error_rows(&mut rows, state.error_at(&Field::InitialStatus), width);
                rows.push(Row::blank());
                heading(&mut rows, "Outgoing transitions");
                let mut any = false;
                for (i, t) in state.transitions().iter().enumerate().filter(|(_, t)| t.from == id) {
                    any = true;
                    let actions: Vec<&str> = t.actions.iter().map(|&a| action_label(a)).collect();
                    let text = if actions.is_empty() { format!("→ {}", name(&t.to)) } else { format!("→ {} · {}", name(&t.to), actions.join(", ")) };
                    button(&mut rows, Item::Outgoing(i), &text, Style::default().fg(palette().foreground));
                }
                if !any {
                    wrapped(&mut rows, "  None yet", width, muted);
                }
                let add = Item::StartTransition(id.clone());
                let line = add_line("+ Add transition", selected(&add), width);
                row(&mut rows, add, line);
                rows.push(Row::blank());
                let only = state.workflow().statuses.len() <= 1;
                let style = if only { muted } else { Style::default().fg(palette().red) };
                button(&mut rows, Item::RemoveStatus(id), "Remove from workflow", style);
                if only {
                    wrapped(&mut rows, "    A workflow needs at least one status.", width, muted);
                }
            }
            Some(Selection::Transition { index: i, added }) => {
                let Some(t) = state.transitions().get(i).cloned() else {
                    return rows;
                };
                picker(&mut rows, "From", &name(&t.from), Item::From(i));
                picker(&mut rows, "To", &name(&t.to), Item::To(i));
                error_rows(&mut rows, state.error_at(&Field::Transition(i)), width);
                let add = (t.actions.len() < ACTIONS.len()).then_some(Item::AddAction(i));
                action_rows(&mut rows, &t.actions, &|slot| Item::Action(i, slot), add, &name(&t.from), &name(&t.to));
                if self.details == Some(i) {
                    rows.push(Row::blank());
                    heading(&mut rows, "Details");
                    self.details_rows(&mut rows, width);
                }
                if added {
                    rows.push(Row::blank());
                    wrapped(&mut rows, "Transition added. Save changes to apply it to this project.", width, muted);
                }
                rows.push(Row::blank());
                button(&mut rows, Item::RemoveTransition(i), "Delete transition", Style::default().fg(palette().red));
            }
            Some(Selection::Draw(d)) => {
                picker(&mut rows, "From", &name(&d.from), Item::DrawFrom);
                wrapped(&mut rows, "Choose where this transition goes: move to a status on the graph and press ⏎, click one, or pick it here.", width, muted);
                let to = d.over.as_deref().map_or_else(|| "None yet".to_string(), name);
                picker(&mut rows, "To", &to, Item::DrawTo);
                rows.push(Row::blank());
                button(&mut rows, Item::CancelNew, "Cancel", Style::default().fg(palette().gray));
            }
            Some(Selection::New(n)) => {
                picker(&mut rows, "From", &name(&n.from), Item::NewFrom);
                picker(&mut rows, "To", &name(&n.to), Item::NewTo);
                let add = (n.actions.len() < ACTIONS.len()).then_some(Item::NewAddAction);
                action_rows(&mut rows, &n.actions, &Item::NewAction, add, &name(&n.from), &name(&n.to));
                rows.push(Row::blank());
                error_rows(&mut rows, state.error_at(&Field::Transitions), width);
                button(&mut rows, Item::CancelNew, "Cancel", Style::default().fg(palette().gray));
                let confirm = Item::ConfirmNew;
                let line = match selected(&confirm) {
                    Some(f) => plain_line("Add transition", Some(f), width, Style::default()),
                    None => Line::from(vec![Span::raw("  "), Span::styled("Add transition", Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD))]),
                };
                row(&mut rows, confirm, line);
            }
            None => {}
        }
        rows
    }
}

/// A drawing line from `from`'s `side` handle to the pointer at `to`
/// (canvas cells): along the handle's axis, then across.
fn pointer_route(canvas: &Canvas, from: &str, side: Side, to: (i32, i32)) -> Option<Route> {
    let start = canvas.node(from)?.handle(side);
    let mut cells = vec![start];
    let mut at = start;
    let vertical_first = matches!(side, Side::Top | Side::Bottom);
    let mut go = |target: (i32, i32), horizontal: bool, cells: &mut Vec<(i32, i32)>| {
        while if horizontal { at.0 != target.0 } else { at.1 != target.1 } {
            if horizontal {
                at.0 += (target.0 - at.0).signum();
            } else {
                at.1 += (target.1 - at.1).signum();
            }
            cells.push(at);
        }
    };
    go(to, !vertical_first, &mut cells);
    go(to, vertical_first, &mut cells);
    let out_of = match side {
        Side::Top => 4,
        Side::Bottom => 1,
        Side::Left => 2,
        Side::Right => 8,
    };
    let into = match cells.len() {
        0 | 1 => out_of,
        n => {
            let (a, b) = (cells[n - 2], cells[n - 1]);
            match (b.0 - a.0, b.1 - a.1) {
                (0, -1) => 1,
                (1, 0) => 2,
                (0, 1) => 4,
                _ => 8,
            }
        }
    };
    Some(Route { edge: usize::MAX, cells, out_of, into })
}
