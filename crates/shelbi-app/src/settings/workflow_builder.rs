//! The Workflow Builder's own state, toolkit-free (`dt-wf-editor-builder`):
//! what is selected, the transition being drawn, the keyboard's node, the
//! hovered node and the canvas's pan and zoom. The workflow itself is the
//! editor's draft ([`super::workflow_editor::EditorState`]); the
//! builder reads it and edits it through the same changes as the Form.
//!
//! **Drawing a transition** (frames 5 and 6): pressing a hovered node's
//! handle starts a [`Draw`] from that node; the line follows the pointer,
//! and the node under it is the destination. Letting go over another node
//! opens the New transition panel ([`NewTransition`]) with both ends and no
//! actions; letting go anywhere else keeps the drawing, waiting for a click
//! on the destination (as Add transition starts it). The New transition
//! panel's Add transition puts it in the draft and selects it, noting it
//! was just added; Cancel drops it.
//!
//! **Back** (`dt-builder-panel-back`): a transition panel opened from a
//! Status panel (an Outgoing transitions row, Add transition) remembers that
//! status in [`BuilderState::back`], and its ← and Esc return to it (a
//! pending drawing or new transition dropped). A drawing begun from a
//! node's handle with no Status panel open goes back to the node it was
//! drawn from. A transition selected on the canvas (its edge or label) has
//! nothing to go back to: no ←, and Esc deselects, as it does anywhere else.
//!
//! **Hover** (`dt-builder-handle-hit`): a node's handles show while it is
//! hovered, and it stays hovered while the pointer is over its handles,
//! whose dots sit half outside it ([`Graph::hover_at`]); the handle under
//! the pointer is [`BuilderState::hot`].
//!
//! **The keyboard's transition** (`tui-workflow-editor-builder`): besides
//! the keyboard's node, the keys can rest on a transition
//! ([`BuilderState::focus_transition`], the TUI's `[` and `]`), and Enter
//! selects it as a click on its edge does. A drawing's destination can be
//! aimed by id ([`BuilderState::aim`]): the node under the pointer, or (the
//! TUI) the keyboard's node as the arrows move it.
//!
//! **Action rows** (`dt-builder-action-drag`): the Transition and New
//! transition panels list their actions in the same row, reordered the way
//! the Statuses tables reorder statuses: drag a row's handle onto another
//! row and it takes that row's place ([`action_drop`]). With the handle
//! focused (a click focuses it), ⌥↑ and ⌥↓ step it ([`action_step`]).

use shelbi_core::{Transition, TransitionAction, Workflow};

use super::workflow_graph::{Graph, Pt, Side, Viewport};

/// A transition being drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Draw {
    pub from: String,
    /// The handle dragged from; `None` when Add transition started it.
    pub handle: Option<Side>,
    /// Where the pointer is (graph coordinates) while the handle is held.
    pub pointer: Option<Pt>,
    /// The node under the pointer: the destination.
    pub over: Option<String>,
}

/// A transition with both ends, not yet in the draft.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTransition {
    pub from: String,
    pub to: String,
    pub actions: Vec<TransitionAction>,
}

impl NewTransition {
    pub fn transition(&self) -> Transition {
        Transition {
            from: self.from.clone(),
            to: self.to.clone(),
            actions: self.actions.clone(),
            target: None,
            run: Vec::new(),
            ready: None,
            ready_timeout: None,
            url: None,
        }
    }
}

/// What the inspector shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    Status(String),
    /// Transition `index`; `added` while it's the one just added.
    Transition { index: usize, added: bool },
    Draw(Draw),
    New(NewTransition),
}

/// What Delete removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    Status(String),
    Transition(usize),
}

/// The inspector list an action row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionList {
    /// The New transition panel's (not yet in the draft).
    New,
    /// Transition `.0`'s, in the draft.
    Transition(usize),
}

/// Action `from` of `list` dropped on row `onto` of `onto_list`, among
/// `len`: the move, `(from, to)`. The dragged action takes the row's place,
/// as a status dropped on a Statuses table row does. A drop on its own row,
/// from another list or past the end moves nothing.
pub fn action_drop(list: ActionList, from: usize, onto_list: ActionList, onto: usize, len: usize) -> Option<(usize, usize)> {
    (list == onto_list && from != onto && from < len && onto < len).then_some((from, onto))
}

/// ⌥↑ (`earlier`) or ⌥↓ on action `slot` of `len`: where it goes, `None`
/// at that end.
pub fn action_step(slot: usize, len: usize, earlier: bool) -> Option<usize> {
    if slot >= len {
        return None;
    }
    if earlier {
        slot.checked_sub(1)
    } else {
        (slot + 1 < len).then_some(slot + 1)
    }
}

/// Move action `from` to `to` (both in range), the others closing up
/// behind it.
pub fn move_action(actions: &mut Vec<TransitionAction>, from: usize, to: usize) {
    if from < actions.len() && to < actions.len() {
        let a = actions.remove(from);
        actions.insert(to, a);
    }
}

#[derive(Debug, Clone, Default)]
pub struct BuilderState {
    pub selection: Option<Selection>,
    pub viewport: Viewport,
    /// The node under the pointer (or over its handles): its handles show.
    pub hover: Option<String>,
    /// The hovered node's handle under the pointer: it grows.
    pub hot: Option<Side>,
    /// The keyboard's node (Tab, the arrow keys), ringed while the canvas
    /// has focus.
    pub focus: Option<String>,
    /// The canvas point a background drag last panned from.
    pub panning: Option<Pt>,
    /// The status a transition panel goes back to (its ← and Esc); set
    /// only while one shows.
    pub back: Option<String>,
    /// The keyboard's transition (the TUI's `[` / `]`): ringed in place of
    /// the keyboard's node, which stays its source.
    pub focus_transition: Option<usize>,
}

impl BuilderState {
    pub fn selected_status(&self) -> Option<&str> {
        match &self.selection {
            Some(Selection::Status(id)) => Some(id),
            _ => None,
        }
    }

    pub fn selected_transition(&self) -> Option<usize> {
        match &self.selection {
            Some(Selection::Transition { index, .. }) => Some(*index),
            _ => None,
        }
    }

    pub fn drawing(&self) -> Option<&Draw> {
        match &self.selection {
            Some(Selection::Draw(d)) => Some(d),
            _ => None,
        }
    }

    pub fn new_transition(&self) -> Option<&NewTransition> {
        match &self.selection {
            Some(Selection::New(n)) => Some(n),
            _ => None,
        }
    }

    pub fn new_transition_mut(&mut self) -> Option<&mut NewTransition> {
        match &mut self.selection {
            Some(Selection::New(n)) => Some(n),
            _ => None,
        }
    }

    pub fn select_status(&mut self, id: &str) {
        self.selection = Some(Selection::Status(id.to_string()));
        self.focus = Some(id.to_string());
        self.back = None;
    }

    /// Select transition `index` from the canvas: nothing to go back to.
    pub fn select_transition(&mut self, index: usize) {
        self.selection = Some(Selection::Transition { index, added: false });
        self.back = None;
    }

    /// Open transition `index` from the selected status's Outgoing
    /// transitions: Back returns to the status.
    pub fn open_transition(&mut self, index: usize) {
        let back = self.selected_status().map(str::to_string);
        self.select_transition(index);
        self.back = back;
    }

    pub fn deselect(&mut self) {
        self.selection = None;
        self.back = None;
    }

    /// The nodes with the focus ring: the selected status, a drawing's ends,
    /// a transition's source (new or selected), and the keyboard's node.
    pub fn ringed(&self, workflow: &Workflow, focused: bool) -> Vec<String> {
        let mut ids: Vec<String> = match &self.selection {
            None => Vec::new(),
            Some(Selection::Status(id)) => vec![id.clone()],
            Some(Selection::Draw(d)) => std::iter::once(d.from.clone()).chain(d.over.clone()).collect(),
            Some(Selection::New(n)) => vec![n.from.clone()],
            Some(Selection::Transition { index, .. }) => {
                workflow.transitions.as_deref().and_then(|t| t.get(*index)).map(|t| vec![t.from.clone()]).unwrap_or_default()
            }
        };
        if focused && self.focus_transition.is_none() {
            ids.extend(self.focus.clone());
        }
        ids
    }

    /// The pointer moved to `p` (graph coordinates) on `graph`, at `zoom`,
    /// no button held: the node it hovers and the handle under it. Returns
    /// whether either changed.
    pub fn hover_to(&mut self, p: Pt, graph: &Graph, zoom: f32) -> bool {
        let hover = graph.hover_at(p, self.hover.as_deref(), zoom).map(|n| n.id.clone());
        let hot = hover.as_deref().and_then(|id| graph.handle_at(id, p, zoom));
        let changed = hover != self.hover || hot != self.hot;
        (self.hover, self.hot) = (hover, hot);
        changed
    }

    /// The pointer left the canvas. Returns whether anything hovered.
    pub fn unhover(&mut self) -> bool {
        self.hot = None;
        self.hover.take().is_some()
    }

    /// The handle drawn hot: the one held while drawing from it, else the
    /// hovered one under the pointer.
    pub fn hot_handle(&self) -> Option<(&str, Side)> {
        match self.drawing() {
            Some(Draw { from, handle: Some(side), pointer: Some(_), .. }) => Some((from, *side)),
            _ => Some((self.hover.as_deref()?, self.hot?)),
        }
    }

    /// The nodes whose handles show: the hovered one, and a drawing's ends.
    pub fn with_handles(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.hover.iter().cloned().collect();
        if let Some(d) = self.drawing() {
            ids.push(d.from.clone());
            ids.extend(d.over.clone());
        }
        ids
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// Start drawing from `from` (Add transition, or a handle pressed with
    /// the pointer at `pointer`). Back returns to the selected status, else
    /// to `from`.
    pub fn begin_draw(&mut self, from: &str, handle: Option<Side>, pointer: Option<Pt>) {
        self.back = Some(self.selected_status().unwrap_or(from).to_string());
        self.selection = Some(Selection::Draw(Draw {
            from: from.to_string(),
            handle,
            pointer,
            over: None,
        }));
    }

    /// The pointer moved to `p` (graph coordinates) with the handle held.
    pub fn draw_to(&mut self, p: Pt, graph: &Graph) {
        if let Some(Selection::Draw(d)) = &mut self.selection {
            if d.handle.is_some() && d.pointer.is_some() {
                d.pointer = Some(p);
                let over = graph.node_at(p).map(|n| n.id.clone());
                self.aim(over.as_deref());
            }
        }
    }

    /// Point the drawing at node `over` (`None`: at nothing): its
    /// destination, unless it's the node drawn from.
    pub fn aim(&mut self, over: Option<&str>) {
        if let Some(Selection::Draw(d)) = &mut self.selection {
            d.over = over.filter(|id| *id != d.from).map(str::to_string);
        }
    }

    /// The handle was let go: over a node, the new transition; elsewhere,
    /// the drawing waits for a click on its destination.
    pub fn release(&mut self) {
        let Some(Selection::Draw(d)) = &mut self.selection else {
            return;
        };
        if d.pointer.is_none() {
            return;
        }
        match d.over.clone() {
            Some(to) => {
                let from = d.from.clone();
                self.selection = Some(Selection::New(NewTransition { from, to, actions: Vec::new() }));
            }
            None => d.pointer = None,
        }
    }

    /// A click on node `id` while drawing picks it as the destination;
    /// returns whether it did.
    pub fn pick(&mut self, id: &str) -> bool {
        let Some(Selection::Draw(d)) = &self.selection else {
            return false;
        };
        if d.from == id {
            return false;
        }
        let from = d.from.clone();
        self.selection = Some(Selection::New(NewTransition {
            from,
            to: id.to_string(),
            actions: Vec::new(),
        }));
        true
    }

    /// Set the drawing's or the new transition's source.
    pub fn set_from(&mut self, id: &str) {
        match &mut self.selection {
            Some(Selection::Draw(d)) => {
                d.from = id.to_string();
                if d.over.as_deref() == Some(id) {
                    d.over = None;
                }
            }
            Some(Selection::New(n)) => n.from = id.to_string(),
            _ => {}
        }
    }

    /// The new transition was added to the draft at `index` (its Back still
    /// returns to the status the drawing went back to).
    pub fn added(&mut self, index: usize) {
        self.selection = Some(Selection::Transition { index, added: true });
    }

    /// Esc (or a transition panel's ←): back to the status the panel goes
    /// back to (dropping a drawing or a new transition), else deselect.
    /// `false` when nothing was selected.
    pub fn escape(&mut self) -> bool {
        self.panning = None;
        let back = self.back.take();
        if self.selection.take().is_none() {
            return false;
        }
        self.selection = back.map(Selection::Status);
        true
    }

    /// Drop what no longer exists in `workflow` (after a removal, a
    /// discard, a save that renumbered nothing but a reload might).
    pub fn reconcile(&mut self, workflow: &Workflow) {
        let has = |id: &str| workflow.status(id).is_some();
        let count = workflow.transitions.as_ref().map_or(0, Vec::len);
        let keep = match &self.selection {
            None => true,
            Some(Selection::Status(id)) => has(id),
            Some(Selection::Transition { index, .. }) => *index < count,
            Some(Selection::Draw(d)) => has(&d.from) && d.over.as_deref().is_none_or(has),
            Some(Selection::New(n)) => has(&n.from) && has(&n.to),
        };
        if !keep {
            self.selection = None;
        }
        if self.selection.is_none() || self.back.as_deref().is_some_and(|id| !has(id)) {
            self.back = None;
        }
        if self.hover.as_deref().is_some_and(|id| !has(id)) {
            self.unhover();
        }
        if self.focus.as_deref().is_some_and(|id| !has(id)) {
            self.focus = None;
        }
        if self.focus_transition.is_some_and(|i| i >= count) {
            self.focus_transition = None;
        }
    }

    // -----------------------------------------------------------------------
    // Keyboard

    /// Tab (or shift-Tab): the next (previous) status in workflow order.
    pub fn tab(&mut self, workflow: &Workflow, forward: bool) {
        let ids: Vec<&str> = workflow.statuses.iter().map(|s| s.id.as_str()).collect();
        if ids.is_empty() {
            return;
        }
        self.focus_transition = None;
        let at = self.focus.as_deref().and_then(|f| ids.iter().position(|id| *id == f));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => ids.len() - 1,
            (Some(i), true) => (i + 1) % ids.len(),
            (Some(i), false) => (i + ids.len() - 1) % ids.len(),
        };
        self.focus = Some(ids[next].to_string());
    }

    /// An arrow key: the nearest node that way.
    pub fn arrow(&mut self, graph: &Graph, side: Side) {
        self.focus_transition = None;
        let Some(from) = self.focus.clone().or_else(|| graph.nodes.first().map(|n| n.id.clone())) else {
            return;
        };
        if self.focus.is_none() {
            self.focus = Some(from);
            return;
        }
        if let Some(n) = graph.neighbor(&from, side) {
            self.focus = Some(n.id.clone());
        }
    }

    /// `]` (or `[`): the keyboard's next (previous) transition in the
    /// workflow's order. From a node, the first (last) one leaving it, else
    /// arriving at it, else the first (last) of all. Its source becomes the
    /// keyboard's node, so the arrows carry on from there.
    pub fn step_transition(&mut self, workflow: &Workflow, forward: bool) {
        let ts = workflow.transitions.as_deref().unwrap_or(&[]);
        let n = ts.len();
        if n == 0 {
            return;
        }
        let next = match (self.focus_transition.filter(|&i| i < n), self.focus.as_deref()) {
            (Some(i), _) if forward => (i + 1) % n,
            (Some(i), _) => (i + n - 1) % n,
            (None, node) => {
                let touching = |pick: &dyn Fn(&Transition) -> bool| {
                    let mut at = ts.iter().enumerate().filter(|(_, t)| pick(t)).map(|(i, _)| i);
                    if forward {
                        at.next()
                    } else {
                        at.last()
                    }
                };
                node.and_then(|id| touching(&|t| t.from == id).or_else(|| touching(&|t| t.to == id)))
                    .unwrap_or(if forward { 0 } else { n - 1 })
            }
        };
        self.focus_transition = Some(next);
        self.focus = Some(ts[next].from.clone());
    }

    /// Enter: pick the keyboard's node as a drawing's destination, else
    /// select the keyboard's transition, else its node.
    pub fn enter(&mut self) {
        if let (Some(index), None) = (self.focus_transition, self.drawing()) {
            self.select_transition(index);
            return;
        }
        let Some(id) = self.focus.clone() else {
            return;
        };
        if !self.pick(&id) && self.drawing().is_none() {
            self.select_status(&id);
        }
    }

    /// Delete: what it removes.
    pub fn removal(&self) -> Option<Removal> {
        match &self.selection {
            Some(Selection::Status(id)) => Some(Removal::Status(id.clone())),
            Some(Selection::Transition { index, .. }) => Some(Removal::Transition(*index)),
            _ => None,
        }
    }
}
