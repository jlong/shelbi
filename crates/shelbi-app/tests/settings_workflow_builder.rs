//! `shelbi_app::settings::workflow_graph` and `workflow_builder` over the
//! fixture project's files (moved here from the desktop app's
//! `workflow_graph` and `workflow_builder_state` tests with the modules, by
//! `tui-workflow-editor-builder`): the catalog's order down the columns,
//! the selection, drawing and Back, hover over the handles, the keyboard's
//! node and transition, and the action rows' reordering. The graph's pure
//! layout tests stay beside it, in the module. Tests that load the fixture
//! run in their own throwaway `SHELBI_HOME`, one at a time.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use shelbi_app::settings::workflow_builder::*;
use shelbi_app::settings::workflow_graph::*;
use shelbi_app::settings::workflows::fixture;
use shelbi_core::{ProjectStatus, StatusCategory, TransitionAction, Workflow};
use shelbi_state::workflow_config::WorkflowConfig;

const PROJECT: &str = "my-project";

/// Serializes the tests: `SHELBI_HOME` is process-wide.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A throwaway `SHELBI_HOME` holding the fixture project, set for the
/// test's length.
struct Home {
    root: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl Home {
    fn new(tag: &str) -> Home {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("shb-app-bld-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SHELBI_HOME", &root);
        fixture::write_project(&shelbi_state::projects_dir().unwrap(), PROJECT);
        Home { root, _lock: lock }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn spec(id: &str, category: StatusCategory) -> NodeSpec {
    NodeSpec { id: id.into(), category }
}

fn edge(from: &str, to: &str, actions: &[&str]) -> EdgeSpec {
    EdgeSpec {
        from: from.into(),
        to: to.into(),
        actions: actions.iter().map(|a| a.to_string()).collect(),
    }
}

/// The fixture's Task, as the frames draw it.
fn task() -> Graph {
    use StatusCategory::*;
    layout(
        &[
            spec("backlog", Backlog),
            spec("todo", Ready),
            spec("in-progress", Active),
            spec("review", Handoff),
            spec("done", Done),
            spec("canceled", Archived),
        ],
        &[
            edge("in-progress", "review", &["Push branch", "Open PR"]),
            edge("review", "done", &["Push branch", "Merge"]),
            edge("in-progress", "canceled", &["Close PR", "Delete branch"]),
            edge("review", "canceled", &["Close PR", "Delete branch"]),
        ],
    )
}

fn at(g: &Graph, id: &str) -> (f32, f32) {
    let r = g.rect(id).unwrap();
    (r.x, r.y)
}

/// The fixture's Task as loaded, and its project's catalog.
fn loaded(tag: &str) -> (Workflow, Vec<ProjectStatus>) {
    let _home = Home::new(tag);
    let config = WorkflowConfig::load(PROJECT).unwrap();
    (config.workflow("task").unwrap().clone(), config.statuses().to_vec())
}

/// The fixture's Task, as loaded.
fn workflow() -> Workflow {
    loaded("builder-state").0
}

// ---------------------------------------------------------------------------
// The graph over the catalog

fn order_dashes(g: &Graph) -> Vec<(&str, &str)> {
    g.edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Order)
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect()
}

/// `catalog` rearranged into `ids`' order.
fn reordered(catalog: &[ProjectStatus], ids: &[&str]) -> Vec<ProjectStatus> {
    ids.iter().map(|id| catalog.iter().find(|s| s.id == *id).unwrap().clone()).collect()
}

/// Task with a second handoff status, QA, listed last, and the catalog
/// with it after Review.
fn with_qa(tag: &str) -> (Workflow, Vec<ProjectStatus>) {
    let (mut workflow, mut catalog) = loaded(tag);
    let mut qa = workflow.status("review").unwrap().clone();
    qa.id = "qa".into();
    workflow.statuses.push(qa);
    let mut qa = catalog.iter().find(|s| s.id == "review").unwrap().clone();
    qa.id = "qa".into();
    catalog.insert(4, qa);
    (workflow, catalog)
}

#[test]
fn the_workflows_own_list_order_moves_nothing() {
    let (workflow, catalog) = loaded("graph-list-order");
    assert_eq!(Graph::of(&workflow, &catalog), task());

    // Listed backwards, Done before Review: Review still above Done.
    let mut listed = workflow.clone();
    listed.statuses.reverse();
    let g = Graph::of(&listed, &catalog);
    assert_eq!(g, task());
    assert_eq!(at(&g, "review"), (432.0, 416.0));
    assert_eq!(at(&g, "done"), (432.0, 624.0));
    assert_eq!(order_dashes(&g), [("backlog", "todo"), ("todo", "in-progress")]);
}

#[test]
fn categories_rank_before_the_catalogs_order() {
    // The catalog backwards: each column still runs Backlog, Ready,
    // Active, and Archived, Handoff, Done.
    let (workflow, catalog) = loaded("graph-category-rank");
    let backwards = reordered(&catalog, &["canceled", "done", "review", "in-progress", "todo", "backlog"]);
    assert_eq!(Graph::of(&workflow, &backwards), task());
}

#[test]
fn the_catalog_orders_statuses_within_a_category() {
    let (workflow, catalog) = with_qa("graph-catalog-order");
    let g = Graph::of(&workflow, &catalog);
    assert_eq!(at(&g, "review"), (432.0, 416.0), "beside the last status in flight");
    assert_eq!(at(&g, "qa"), (432.0, 592.0), "the catalog's next, the plain gap");
    assert_eq!(at(&g, "done"), (432.0, 768.0), "done after every handoff");
    assert_eq!(at(&g, "canceled"), (432.0, 64.0), "archived at the top");
    // The dashes run the same way, past Canceled to the handoffs (In
    // Progress to Review is a transition).
    assert_eq!(order_dashes(&g), [("backlog", "todo"), ("todo", "in-progress"), ("review", "qa")]);

    // QA moved above Review in the catalog.
    let moved = reordered(&catalog, &["backlog", "todo", "in-progress", "qa", "review", "done", "canceled"]);
    let g = Graph::of(&workflow, &moved);
    assert_eq!(at(&g, "qa"), (432.0, 416.0));
    assert_eq!(at(&g, "review"), (432.0, 592.0));
    assert_eq!(at(&g, "done"), (432.0, 800.0), "under Review, the labeled gap");
    assert_eq!(order_dashes(&g), [("backlog", "todo"), ("todo", "in-progress"), ("in-progress", "qa"), ("qa", "review")]);

    // A status the catalog has lost goes after the rest of its category.
    let lost: Vec<ProjectStatus> = moved.into_iter().filter(|s| s.id != "qa").collect();
    let g = Graph::of(&workflow, &lost);
    assert_eq!(at(&g, "review"), (432.0, 416.0));
    assert_eq!(at(&g, "qa"), (432.0, 592.0));
}

// ---------------------------------------------------------------------------
// The builder's state

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

#[test]
fn brackets_step_the_keyboard_through_transitions_and_enter_selects_one() {
    // Task's transitions: 0 In Progress → Review, 1 Review → Done, 2 In
    // Progress → Canceled, 3 Review → Canceled.
    let (g, w) = (task(), workflow());
    let mut b = BuilderState::default();
    b.step_transition(&w, true);
    assert_eq!((b.focus_transition, b.focus.as_deref()), (Some(0), Some("in-progress")), "nothing focused: the first");
    b.step_transition(&w, false);
    assert_eq!((b.focus_transition, b.focus.as_deref()), (Some(3), Some("review")), "wraps");
    assert_eq!(b.ringed(&w, true), Vec::<String>::new(), "the transition is ringed, not its source");

    // From a node: the first leaving it, else the first arriving.
    b.focus_transition = None;
    b.focus = Some("review".into());
    b.step_transition(&w, true);
    assert_eq!(b.focus_transition, Some(1));
    b.focus_transition = None;
    b.focus = Some("done".into());
    b.step_transition(&w, true);
    assert_eq!(b.focus_transition, Some(1), "Done has none leaving: Review → Done");
    b.focus_transition = None;
    b.focus = Some("backlog".into());
    b.step_transition(&w, false);
    assert_eq!(b.focus_transition, Some(3), "untouched: the last of all");

    // The arrows leave the transition, from its source.
    b.focus_transition = Some(0);
    b.focus = Some("in-progress".into());
    b.arrow(&g, Side::Right);
    assert_eq!((b.focus_transition, b.focus.as_deref()), (None, Some("review")));

    b.step_transition(&w, true);
    b.enter();
    assert_eq!(b.selection, Some(Selection::Transition { index: 1, added: false }), "Enter selects it");
    assert_eq!(b.back, None, "as a click on its edge: nothing to go back to");

    // With no transitions, nothing to step to.
    let mut bare = w.clone();
    bare.transitions = None;
    let mut b = BuilderState::default();
    b.step_transition(&bare, true);
    assert_eq!(b.focus_transition, None);
}

#[test]
fn a_drawing_aims_by_id_and_enter_picks_the_aimed_node() {
    let (g, w) = (task(), workflow());
    let mut b = BuilderState { focus: Some("in-progress".into()), ..Default::default() };
    b.select_status("in-progress");
    b.begin_draw("in-progress", None, None);
    b.aim(Some("in-progress"));
    assert_eq!(b.drawing().unwrap().over, None, "never its own source");
    b.arrow(&g, Side::Right);
    b.aim(b.focus.clone().as_deref());
    assert_eq!(b.drawing().unwrap().over.as_deref(), Some("review"));
    assert_eq!(b.ringed(&w, false), ["in-progress", "review"]);
    b.aim(None);
    assert_eq!(b.drawing().unwrap().over, None);
    // A focused transition doesn't steal Enter from a drawing.
    b.focus_transition = Some(2);
    b.focus = Some("done".into());
    b.enter();
    let n = b.new_transition().unwrap();
    assert_eq!((n.from.as_str(), n.to.as_str()), ("in-progress", "done"));
}

#[test]
fn reconcile_forgets_a_keyboard_transition_the_draft_lost() {
    let mut w = workflow();
    let mut b = BuilderState { focus_transition: Some(3), ..Default::default() };
    w.transitions.as_mut().unwrap().truncate(3);
    b.reconcile(&w);
    assert_eq!(b.focus_transition, None);
}
