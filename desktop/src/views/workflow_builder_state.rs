//! The Workflow Builder's own state, toolkit-free (`dt-wf-editor-builder`):
//! what is selected, the transition being drawn, the keyboard's node, the
//! hovered node and the canvas's pan and zoom. The workflow itself is the
//! editor's draft ([`super::workflow_editor_state::EditorState`]); the
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
        if focused {
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
                d.over = graph.node_at(p).map(|n| n.id.clone()).filter(|id| *id != d.from);
            }
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
    }

    // -----------------------------------------------------------------------
    // Keyboard

    /// Tab (or shift-Tab): the next (previous) status in workflow order.
    pub fn tab(&mut self, workflow: &Workflow, forward: bool) {
        let ids: Vec<&str> = workflow.statuses.iter().map(|s| s.id.as_str()).collect();
        if ids.is_empty() {
            return;
        }
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

    /// Enter: pick the keyboard's node as a drawing's destination, else
    /// select it.
    pub fn enter(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::workflow_graph::tests::task;

    /// The fixture's Task, as loaded.
    fn workflow() -> Workflow {
        use crate::views::workflows_state::tests::{Home, PROJECT};
        let _home = Home::new("builder-state");
        let config = shelbi_state::workflow_config::WorkflowConfig::load(PROJECT).unwrap();
        config.workflow("task").unwrap().clone()
    }

    #[test]
    fn an_action_dropped_on_a_row_takes_its_place() {
        use TransitionAction::*;
        let list = ActionList::Transition(1);
        let moved = |from, onto| {
            let mut actions = vec![PushBranch, OpenPr, Merge];
            if let Some((from, to)) = action_drop(list, from, list, onto, actions.len()) {
                move_action(&mut actions, from, to);
            }
            actions
        };
        assert_eq!(moved(0, 2), [OpenPr, Merge, PushBranch], "first to last");
        assert_eq!(moved(2, 0), [Merge, PushBranch, OpenPr], "last to first");
        assert_eq!(moved(1, 2), [PushBranch, Merge, OpenPr], "one later");
        assert_eq!(action_drop(list, 1, list, 1, 3), None, "a drop in place");
        assert_eq!(moved(1, 1), [PushBranch, OpenPr, Merge]);
        assert_eq!(action_drop(list, 0, ActionList::New, 1, 3), None, "another list");
        assert_eq!(action_drop(list, 0, ActionList::Transition(2), 1, 3), None, "another transition");
        assert_eq!(action_drop(list, 3, list, 0, 3), None, "a stale row");
    }

    #[test]
    fn option_arrows_step_an_action_to_either_end() {
        assert_eq!(action_step(1, 3, true), Some(0));
        assert_eq!(action_step(1, 3, false), Some(2));
        assert_eq!(action_step(0, 3, true), None, "already first");
        assert_eq!(action_step(2, 3, false), None, "already last");
        assert_eq!(action_step(0, 1, false), None, "the only one");
        assert_eq!(action_step(3, 3, true), None, "a stale row");
    }

    #[test]
    fn hovering_keeps_the_node_over_its_handles_and_heats_the_one_under_the_pointer() {
        let g = task();
        let mut b = BuilderState::default();
        // In Progress, then out onto its right handle's outer half.
        assert!(b.hover_to(Pt::new(156.0, 460.0), &g, 1.0));
        assert_eq!((b.hover.as_deref(), b.hot), (Some("in-progress"), None));
        assert!(b.hover_to(Pt::new(268.0, 460.0), &g, 1.0));
        assert_eq!((b.hover.as_deref(), b.hot), (Some("in-progress"), Some(Side::Right)));
        assert_eq!(b.hot_handle(), Some(("in-progress", Side::Right)));
        assert!(!b.hover_to(Pt::new(269.0, 461.0), &g, 1.0), "nothing shown changed");
        // Held while drawing, it stays hot wherever the pointer goes.
        b.begin_draw("in-progress", Some(Side::Right), Some(Pt::new(268.0, 460.0)));
        b.draw_to(Pt::new(300.0, 600.0), &g);
        assert_eq!(b.hot_handle(), Some(("in-progress", Side::Right)));
        b.release();
        // Leaving the canvas drops both.
        assert!(b.unhover());
        assert_eq!((b.hover.as_deref(), b.hot, b.hot_handle()), (None, None, None));
    }

    #[test]
    fn dragging_a_handle_onto_a_node_opens_a_new_transition() {
        let g = task();
        let mut b = BuilderState::default();
        let start = g.rect("in-progress").unwrap().side(Side::Bottom);
        b.begin_draw("in-progress", Some(Side::Bottom), Some(start));
        assert_eq!(b.with_handles(), ["in-progress"]);
        // Over the background: no destination.
        b.draw_to(Pt::new(300.0, 600.0), &g);
        assert_eq!(b.drawing().unwrap().over, None);
        // Over its own node: still none.
        b.draw_to(Pt::new(100.0, 450.0), &g);
        assert_eq!(b.drawing().unwrap().over, None);
        // Over Done.
        b.draw_to(Pt::new(500.0, 650.0), &g);
        assert_eq!(b.drawing().unwrap().over.as_deref(), Some("done"));
        assert_eq!(b.with_handles(), ["in-progress", "done"], "both ends' handles show");
        assert_eq!(b.ringed(&workflow(), false), ["in-progress", "done"]);
        b.release();
        let n = b.new_transition().unwrap();
        assert_eq!((n.from.as_str(), n.to.as_str()), ("in-progress", "done"));
        assert!(n.actions.is_empty());
    }

    #[test]
    fn letting_go_over_nothing_waits_for_a_click_and_esc_steps_back() {
        let g = task();
        let mut b = BuilderState::default();
        b.begin_draw("review", Some(Side::Right), Some(Pt::new(648.0, 460.0)));
        b.draw_to(Pt::new(700.0, 300.0), &g);
        b.release();
        let d = b.drawing().unwrap();
        assert_eq!((d.pointer, d.over.as_deref()), (None, None), "still drawing, without a line");
        assert!(!b.pick("review"), "not to itself");
        assert!(b.pick("backlog"));
        assert_eq!(b.new_transition().unwrap().to, "backlog");

        assert!(b.escape());
        assert_eq!(b.selected_status(), Some("review"), "back to its status");
        assert!(b.escape());
        assert_eq!(b.selection, None);
        assert!(!b.escape(), "nothing left to step back from");
    }

    #[test]
    fn add_transition_draws_from_the_status_and_adding_selects_it() {
        let mut b = BuilderState::default();
        b.select_status("todo");
        b.begin_draw("todo", None, None);
        b.set_from("in-progress");
        assert!(b.pick("done"));
        b.new_transition_mut().unwrap().actions.push(TransitionAction::Merge);
        let t = b.new_transition().unwrap().transition();
        assert_eq!((t.from.as_str(), t.to.as_str(), t.actions.as_slice()), ("in-progress", "done", &[TransitionAction::Merge][..]));
        b.added(4);
        assert_eq!(b.selection, Some(Selection::Transition { index: 4, added: true }));
        assert_eq!(b.ringed(&workflow(), false), Vec::<String>::new(), "index 4 isn't in the fixture");
        b.select_transition(0);
        assert_eq!(b.ringed(&workflow(), false), ["in-progress"]);
    }

    #[test]
    fn a_transition_opened_from_a_status_goes_back_to_it() {
        let mut b = BuilderState::default();
        b.select_status("review");
        assert_eq!(b.back, None, "a Status panel has nothing to go back to");
        b.open_transition(1);
        assert_eq!(b.selection, Some(Selection::Transition { index: 1, added: false }));
        assert_eq!(b.back.as_deref(), Some("review"));
        assert!(b.escape());
        assert_eq!(b.selected_status(), Some("review"), "back to the status");
        assert_eq!(b.back, None);
        assert!(b.escape());
        assert_eq!(b.selection, None, "then nothing");
        assert!(!b.escape());
    }

    #[test]
    fn add_transition_goes_back_to_its_status_dropping_the_drawing() {
        let mut b = BuilderState::default();
        b.select_status("todo");
        b.begin_draw("todo", None, None);
        assert_eq!(b.back.as_deref(), Some("todo"));
        assert!(b.escape());
        assert_eq!(b.selected_status(), Some("todo"), "the drawing dropped");

        // Through a new transition whose source moved: still back to the
        // status it was started from.
        b.begin_draw("todo", None, None);
        b.set_from("in-progress");
        assert!(b.pick("done"));
        assert_eq!(b.back.as_deref(), Some("todo"));
        assert!(b.escape());
        assert_eq!(b.selected_status(), Some("todo"), "the new transition dropped");

        // Once added, the transition still goes back to it.
        b.begin_draw("todo", None, None);
        assert!(b.pick("done"));
        b.added(4);
        assert_eq!(b.back.as_deref(), Some("todo"));
        assert!(b.escape());
        assert_eq!(b.selected_status(), Some("todo"));
    }

    #[test]
    fn a_transition_selected_on_the_canvas_has_nothing_to_go_back_to() {
        let mut b = BuilderState::default();
        b.select_status("review");
        b.select_transition(1);
        assert_eq!(b.back, None, "an edge clicked with a status selected");
        assert!(b.escape());
        assert_eq!(b.selection, None, "Esc deselects");
        // From a transition reached by its status, a click on another edge
        // forgets the status.
        b.select_status("review");
        b.open_transition(1);
        b.select_transition(3);
        assert_eq!(b.back, None);
        // A handle dragged with nothing open goes back to its node.
        b.deselect();
        b.begin_draw("done", Some(Side::Left), Some(Pt::new(0.0, 0.0)));
        assert_eq!(b.back.as_deref(), Some("done"));
    }

    #[test]
    fn reconcile_forgets_a_back_status_the_draft_lost() {
        let mut w = workflow();
        let mut b = BuilderState::default();
        b.select_status("canceled");
        b.open_transition(0);
        w.statuses.retain(|s| s.id != "canceled");
        b.reconcile(&w);
        assert_eq!(b.selected_transition(), Some(0), "the transition stays");
        assert_eq!(b.back, None);
        assert!(b.escape());
        assert_eq!(b.selection, None);
    }

    #[test]
    fn reconcile_drops_what_the_draft_lost() {
        let mut w = workflow();
        let mut b = BuilderState::default();
        b.select_transition(3);
        b.hover = Some("canceled".into());
        w.transitions.as_mut().unwrap().truncate(3);
        w.statuses.retain(|s| s.id != "canceled");
        b.reconcile(&w);
        assert_eq!((b.selection.clone(), b.hover.clone()), (None, None));
        b.select_status("todo");
        b.reconcile(&w);
        assert_eq!(b.selected_status(), Some("todo"));
    }

    #[test]
    fn the_keyboard_moves_between_nodes_selects_and_removes() {
        let (g, w) = (task(), workflow());
        let mut b = BuilderState::default();
        b.tab(&w, true);
        assert_eq!(b.focus.as_deref(), Some("backlog"));
        b.tab(&w, false);
        assert_eq!(b.focus.as_deref(), Some("canceled"), "wraps");
        b.arrow(&g, Side::Bottom);
        assert_eq!(b.focus.as_deref(), Some("review"), "under Canceled");
        b.arrow(&g, Side::Left);
        assert_eq!(b.focus.as_deref(), Some("in-progress"));
        assert_eq!(b.ringed(&w, true), ["in-progress"]);
        b.enter();
        assert_eq!(b.selected_status(), Some("in-progress"));
        assert_eq!(b.removal(), Some(Removal::Status("in-progress".into())));
        // Enter while drawing picks the destination.
        b.begin_draw("in-progress", None, None);
        b.arrow(&g, Side::Right);
        b.enter();
        assert_eq!(b.new_transition().unwrap().to, "review");
        assert_eq!(b.removal(), None, "Delete leaves a new transition alone");
        b.select_transition(1);
        assert_eq!(b.removal(), Some(Removal::Transition(1)));
    }
}
