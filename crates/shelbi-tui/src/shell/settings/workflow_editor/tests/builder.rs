//! The Builder (`tui-workflow-editor-builder`) over the same draft as the
//! Form: keyboard-only node and edge editing, Add transition (create,
//! cancel at every step, the draft's validation), round trips between the
//! views with unsaved edits, Save and Discard, a cyclic and disconnected
//! workflow, resizing with the panel open, the mouse, and tiny sizes.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use shelbi_app::settings::workflow_builder::Selection;

use super::*;

/// The editor for `name`, loaded, on the Builder (`v`).
fn builder(name: &str) -> WorkflowEditorPage {
    let mut ed = editor(name);
    assert_eq!(ed.state().unwrap().mode, Mode::Form, "it opens on the Form");
    press(&mut ed, key(KeyCode::Char('v')));
    assert_eq!(ed.state().unwrap().mode, Mode::Builder);
    ed
}

fn b(ed: &WorkflowEditorPage) -> &shelbi_app::settings::workflow_builder::BuilderState {
    &ed.builder.state
}

fn transitions(ed: &WorkflowEditorPage) -> Vec<(String, String, Vec<TransitionAction>)> {
    ed.state().unwrap().transitions().iter().map(|t| (t.from.clone(), t.to.clone(), t.actions.clone())).collect()
}

/// Move the keyboard's node to `id` with the arrows alone.
fn arrow_to(ed: &mut WorkflowEditorPage, id: &str) {
    assert_eq!(ed.cursor_item(), Some(Item::Canvas), "the canvas has the keys");
    if b(ed).focus.is_none() {
        press(ed, key(KeyCode::Down));
    }
    for _ in 0..12 {
        if b(ed).focus.as_deref() == Some(id) {
            return;
        }
        let graph = ed.graph().unwrap();
        let from = graph.rect(b(ed).focus.as_deref().unwrap()).unwrap().center();
        let to = graph.rect(id).unwrap().center();
        let code = if (to.x - from.x).abs() > 1.0 {
            if to.x > from.x { KeyCode::Right } else { KeyCode::Left }
        } else if to.y > from.y {
            KeyCode::Down
        } else {
            KeyCode::Up
        };
        press(ed, key(code));
    }
    assert_eq!(b(ed).focus.as_deref(), Some(id), "the arrows reach {id}");
}

fn screen_has(screen: &str, want: &str) -> bool {
    screen.contains(want)
}

#[test]
fn v_switches_views_over_one_draft_carrying_the_place() {
    let _home = Home::new("bld-views");
    let mut ed = editor("task");
    let header = render(&mut ed, 100, 30).lines().next().unwrap().to_string();
    assert!(header.contains("Builder") && header.contains("Form"), "{header}");
    assert_eq!(VIEWS, &[Mode::Builder, Mode::Form]);
    // From the Form's Review row: the Builder opens on Review, selected.
    go(&mut ed, Item::Status("review".into()));
    press(&mut ed, key(KeyCode::Char('v')));
    assert_eq!(b(&ed).selected_status(), Some("review"));
    assert_eq!(ed.cursor_item(), Some(Item::Canvas));
    // A transition selected in the Builder is the Form's row on the way back.
    press(&mut ed, key(KeyCode::Char(']')));
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(b(&ed).selected_transition(), Some(1), "Review → Done, Review's first");
    press(&mut ed, key(KeyCode::Char('v')));
    assert_eq!(ed.state().unwrap().mode, Mode::Form);
    assert_eq!(ed.cursor_item(), Some(Item::Transition(1)));
    assert_eq!(ed.details, Some(1), "its Details stay open in the Form");
    // The header's control does the same, by Enter or a click.
    go(&mut ed, Item::View);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.state().unwrap().mode, Mode::Builder);
    render(&mut ed, 100, 30);
    let (rect, _) = *ed.hits.iter().find(|(_, h)| *h == Hit::View(Mode::Form)).expect("Form is clickable");
    ed.handle_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: rect.x, row: rect.y, modifiers: KeyModifiers::NONE });
    assert_eq!(ed.state().unwrap().mode, Mode::Form);
}

#[test]
fn the_canvas_draws_statuses_transitions_and_the_initial_status_at_80x24() {
    let _home = Home::new("bld-draw");
    let mut ed = builder("task");
    let screen = render(&mut ed, 80, 24);
    for want in ["◌ Backlog", "○ To Do", "◐ In Progress", "◑ Review", "● Done", "⊘ Canceled", "Agent · developer", "User · review", "Initial"] {
        assert!(screen_has(&screen, want), "{want}:\n{screen}");
    }
    // Every transition has its line and arrowhead; the dashes have none
    // selectable.
    let canvas = ed.builder.canvas().unwrap();
    assert_eq!(canvas.unrouted(), 0);
    for i in 0..4 {
        assert!(canvas.transition_route(i).is_some(), "transition {i} drawn");
    }
    let arrows = screen.chars().filter(|c| "▲▶▼◀".contains(*c)).count();
    assert!(arrows >= 5, "{arrows} arrowheads:\n{screen}");
    assert!(screen.contains("Push branch · Merge"), "a label where it fits:\n{screen}");
    // It's wider and taller than 80x24: it says so, and nothing overflows.
    assert!(screen.contains("more"), "{screen}");
    for line in screen.lines() {
        assert!(crate::panel::display_width(line) <= 80, "{line}");
    }
    // The keyboard's node shows its handles; Tab from the header lands on
    // the canvas.
    go(&mut ed, Item::Back);
    press(&mut ed, key(KeyCode::Tab));
    assert_eq!(ed.cursor_item(), Some(Item::Canvas));
    press(&mut ed, key(KeyCode::Down));
    let screen = render(&mut ed, 80, 24);
    assert!(screen.contains('◆'), "the focused node's handles show:\n{screen}");
    assert!(screen.contains('┏'), "the keyboard's ring:\n{screen}");
}

#[test]
fn a_status_is_selected_and_edited_by_keys_in_the_panel_and_the_form_shows_it() {
    let home = Home::new("bld-status");
    let before = home.read("task.yaml");
    let mut ed = builder("task");
    arrow_to(&mut ed, "in-progress");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(b(&ed).selected_status(), Some("in-progress"));
    assert_eq!(ed.cursor_item(), Some(Item::PanelStatus), "the keys go to the panel");
    let screen = render(&mut ed, 120, 60);
    for want in ["Status", "◐ In Progress", "Category", "Active", "Owner", "Agent", "developer", "Outgoing transitions", "→ Review · Push branch, Open PR", "+ Add transition", "Remove from workflow"] {
        assert!(screen.contains(want), "{want}:\n{screen}");
    }
    // Owner: User. Agent: None. Tags typed. Set as initial.
    go(&mut ed, Item::Owner("in-progress".into()));
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "User");
    go(&mut ed, Item::Agent("in-progress".into()));
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "None");
    go(&mut ed, Item::Text(Text::Tags("in-progress".into())));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "gpu");
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::SetInitial("in-progress".into()));
    press(&mut ed, key(KeyCode::Enter));
    let state = ed.state().unwrap();
    let s = state.status("in-progress").unwrap();
    assert_eq!((s.owner, s.agent.clone(), s.tags.clone()), (Owner::User, None, vec!["gpu".to_string()]));
    assert_eq!(state.initial_status().unwrap().id, "in-progress");
    assert_eq!(home.read("task.yaml"), before, "nothing written before Save");
    // The canvas shows the edits at once…
    let screen = render(&mut ed, 120, 34);
    assert!(screen.contains("User · None"), "{screen}");
    // …and so does the Form, from the same draft.
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(ed.cursor_item(), Some(Item::Canvas), "Esc in the panel: back to the graph");
    assert_eq!(b(&ed).selected_status(), Some("in-progress"), "the selection kept");
    press(&mut ed, key(KeyCode::Char('v')));
    assert_eq!(ed.cursor_item(), Some(Item::Status("in-progress".into())));
    assert_eq!(ed.typed_text(&Text::Tags("in-progress".into())), "gpu");
    let rows = render(&mut ed, 120, 60);
    let at = rows.lines().position(|l| l.trim() == "Initial status").expect("the picker's label");
    assert!(rows.lines().nth(at + 2).is_some_and(|l| l.contains(" In Progress")), "{rows}");
    // Save from the Builder writes them.
    press(&mut ed, key(KeyCode::Char('v')));
    press(&mut ed, ctrl('s'));
    drain(&mut ed);
    let w = on_disk("task");
    assert_eq!(w.initial_status.as_deref(), Some("in-progress"));
    assert_eq!(w.status("in-progress").unwrap().tags, ["gpu"]);
    assert!(!ed.is_dirty());
}

#[test]
fn brackets_reach_a_transition_and_its_panel_edits_actions_and_details() {
    let _home = Home::new("bld-edge");
    let mut ed = builder("task");
    // In Progress, then its first transition (to Review), then the next.
    arrow_to(&mut ed, "in-progress");
    press(&mut ed, key(KeyCode::Char(']')));
    assert_eq!(b(&ed).focus_transition, Some(0));
    press(&mut ed, key(KeyCode::Char(']')));
    assert_eq!(b(&ed).focus_transition, Some(1));
    press(&mut ed, key(KeyCode::Char('[')));
    let screen = render(&mut ed, 120, 34);
    assert!(!screen.contains('┏'), "the transition is ringed, not its source:\n{screen}");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(b(&ed).selected_transition(), Some(0));
    let screen = render(&mut ed, 120, 60);
    for want in ["Transition", "From", "In Progress", "To", "Review", "Actions", "1. Push branch", "2. Open PR", "+ Add action", "Details", "Target branch", "Ready timeout", "Delete transition"] {
        assert!(screen.contains(want), "{want}:\n{screen}");
    }
    // Reorder: Open PR first (Shift+↑ on it), then add Merge, remove Push branch.
    go(&mut ed, Item::Action(0, 1));
    press(&mut ed, shift(KeyCode::Up));
    assert_eq!(ed.cursor_item(), Some(Item::Action(0, 0)), "the cursor follows");
    press(&mut ed, key(KeyCode::Char('n')));
    pick(&mut ed, "Merge");
    go(&mut ed, Item::Action(0, 1));
    press(&mut ed, key(KeyCode::Char('x')));
    assert_eq!(transitions(&ed)[0].2, [TransitionAction::OpenPr, TransitionAction::Merge]);
    // To: Done (a new pair).
    go(&mut ed, Item::To(0));
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Done");
    assert_eq!(transitions(&ed)[0].1, "done");
    // A Details field: refused text stays with its error, then corrected.
    go(&mut ed, Item::Text(Text::Timeout));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "soon");
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.state().unwrap().error_at(&Field::Transition(0)).is_some(), "the draft's check");
    assert!(render(&mut ed, 120, 60).contains("soon"));
    go(&mut ed, Item::Text(Text::Timeout));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "45");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.state().unwrap().transitions()[0].ready_timeout, Some(std::time::Duration::from_secs(45)));
    // The Form shows all of it.
    press(&mut ed, key(KeyCode::Esc));
    press(&mut ed, key(KeyCode::Char('v')));
    let rows: String = ed.rows(100).into_iter().map(|r| r.line.spans.iter().map(|s| s.content.to_string()).collect::<String>() + "\n").collect();
    assert!(rows.contains("In Progress → Done") && rows.contains("1. Open PR") && rows.contains("2. Merge"), "{rows}");
}

#[test]
fn add_transition_by_keys_chooses_source_destination_configures_and_confirms() {
    let home = Home::new("bld-add");
    let before = home.read("task.yaml");
    let mut ed = builder("task");
    let count = transitions(&ed).len();
    // Source: To Do (the keyboard's node), `a`.
    arrow_to(&mut ed, "todo");
    press(&mut ed, key(KeyCode::Char('a')));
    assert_eq!(b(&ed).drawing().unwrap().from, "todo");
    assert!(render(&mut ed, 120, 34).contains("Add transition"), "the Draw panel beside");
    // Destination: Review, by the arrows; the line previews.
    arrow_to(&mut ed, "review");
    assert_eq!(b(&ed).drawing().unwrap().over.as_deref(), Some("review"));
    let screen = render(&mut ed, 120, 34);
    assert!(screen.contains("Transition from To Do to Review"), "{screen}");
    press(&mut ed, key(KeyCode::Enter));
    let n = b(&ed).new_transition().unwrap();
    assert_eq!((n.from.as_str(), n.to.as_str()), ("todo", "review"));
    assert_eq!(ed.cursor_item(), Some(Item::NewAddAction), "configure next");
    // Configure: two actions, then swap them.
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Push branch");
    press(&mut ed, key(KeyCode::Char('n')));
    pick(&mut ed, "Open PR");
    press(&mut ed, shift(KeyCode::Up));
    assert_eq!(b(&ed).new_transition().unwrap().actions, [TransitionAction::OpenPr, TransitionAction::PushBranch]);
    assert_eq!(transitions(&ed).len(), count, "nothing in the draft yet");
    assert!(!ed.is_dirty());
    // Confirm.
    go(&mut ed, Item::ConfirmNew);
    press(&mut ed, key(KeyCode::Enter));
    let ts = transitions(&ed);
    assert_eq!(ts.len(), count + 1);
    assert_eq!(ts[count], ("todo".into(), "review".into(), vec![TransitionAction::OpenPr, TransitionAction::PushBranch]));
    assert_eq!(b(&ed).selection, Some(Selection::Transition { index: count, added: true }));
    assert!(render(&mut ed, 120, 60).contains("Transition added. Save changes"));
    assert!(ed.builder.canvas().unwrap().transition_route(count).is_some(), "drawn");
    assert_eq!(home.read("task.yaml"), before, "the draft only");
    // The Form lists it.
    press(&mut ed, key(KeyCode::Char('v')));
    assert!(ed.items().contains(&Item::Transition(count)));
}

#[test]
fn esc_cancels_add_transition_at_every_step_without_changing_the_draft() {
    let _home = Home::new("bld-cancel");
    let mut ed = builder("task");
    let before = transitions(&ed);
    // From a selected status's panel: Add transition, Esc → back to it.
    arrow_to(&mut ed, "backlog");
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::StartTransition("backlog".into()));
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.cursor_item(), Some(Item::Canvas), "choose the destination on the graph");
    assert!(b(&ed).drawing().is_some());
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(b(&ed).selected_status(), Some("backlog"), "back to the status");
    // Destination chosen, then Esc in the New panel: back to the status.
    go(&mut ed, Item::StartTransition("backlog".into()));
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, key(KeyCode::Down));
    press(&mut ed, key(KeyCode::Enter));
    assert!(b(&ed).new_transition().is_some());
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Merge");
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(b(&ed).selected_status(), Some("backlog"));
    // The panel's Cancel does the same; with nothing selected, from the canvas.
    press(&mut ed, key(KeyCode::Esc));
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(b(&ed).selection, None);
    press(&mut ed, key(KeyCode::Char('a')));
    press(&mut ed, key(KeyCode::Right));
    press(&mut ed, key(KeyCode::Enter));
    let from = b(&ed).new_transition().unwrap().from.clone();
    go(&mut ed, Item::CancelNew);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(b(&ed).selected_status(), Some(from.as_str()), "back to the status it was drawn from");
    press(&mut ed, key(KeyCode::Esc));
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(b(&ed).selection, None);
    assert_eq!(transitions(&ed), before);
    assert!(!ed.is_dirty(), "the draft never changed");
    // With nothing selected and nothing to save, Esc leaves the editor.
    assert_eq!(ed.cursor_item(), Some(Item::Canvas));
    assert_eq!(press(&mut ed, key(KeyCode::Esc)), EditorAction::Close);
}

#[test]
fn a_transition_the_draft_refuses_stays_new_with_the_forms_message() {
    let _home = Home::new("bld-dup");
    let mut ed = builder("task");
    let before = transitions(&ed);
    arrow_to(&mut ed, "in-progress");
    press(&mut ed, key(KeyCode::Char('a')));
    arrow_to(&mut ed, "review");
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::ConfirmNew);
    press(&mut ed, key(KeyCode::Enter));
    let error = ed.state().unwrap().error_at(&Field::Transitions).expect("refused").to_string();
    assert!(error.contains("duplicate transition"), "{error}");
    assert!(b(&ed).new_transition().is_some(), "still the new transition");
    assert!(render(&mut ed, 120, 40).contains("duplicate"), "the panel shows why");
    assert_eq!(transitions(&ed), before);
    // The Form's own edit to the same pair meets the same check.
    press(&mut ed, key(KeyCode::Esc));
    press(&mut ed, key(KeyCode::Char('v')));
    go(&mut ed, Item::Transition(2));
    press(&mut ed, key(KeyCode::Char('t')));
    pick(&mut ed, "Review");
    let form = ed.state().unwrap().error_at(&Field::Transition(2)).expect("refused in the Form too").to_string();
    assert!(form.contains("duplicate transition"), "{form}");
    // A refused one leaves no message behind once cancelled.
    press(&mut ed, key(KeyCode::Char('v')));
    assert_eq!(ed.state().unwrap().error_at(&Field::Transitions), None);
}

#[test]
fn removals_and_form_edits_cross_over_and_discard_restores_both() {
    let home = Home::new("bld-cross");
    let before = home.read("task.yaml");
    let mut ed = editor("task");
    // A Form edit with the draft dirty…
    go(&mut ed, Item::Status("review".into()));
    press(&mut ed, key(KeyCode::Char('o')));
    pick(&mut ed, "Agent");
    assert!(ed.is_dirty(), "{:?}", ed.state().unwrap().error_at(&Field::Status("review".into())));
    // …shows in the Builder.
    press(&mut ed, key(KeyCode::Char('v')));
    assert!(render(&mut ed, 120, 34).contains("Agent · review"));
    // Remove a transition and a status from the canvas.
    press(&mut ed, key(KeyCode::Esc));
    arrow_to(&mut ed, "review");
    press(&mut ed, key(KeyCode::Char(']')));
    let removed = b(&ed).focus_transition.unwrap();
    press(&mut ed, key(KeyCode::Char('x')));
    assert_eq!(transitions(&ed).len(), 3, "transition {removed} gone");
    arrow_to(&mut ed, "todo");
    press(&mut ed, key(KeyCode::Char('x')));
    assert!(ed.state().unwrap().status("todo").is_none());
    assert!(!render(&mut ed, 120, 34).contains("To Do"));
    // The Form has them gone too.
    press(&mut ed, key(KeyCode::Char('v')));
    assert!(!ed.items().contains(&Item::Status("todo".into())));
    assert_eq!(ed.items().iter().filter(|i| matches!(i, Item::Transition(_))).count(), 3);
    // Discard (the header's) puts both views back.
    go(&mut ed, Item::Discard);
    press(&mut ed, key(KeyCode::Enter));
    assert!(!ed.is_dirty());
    press(&mut ed, key(KeyCode::Char('v')));
    let screen = render(&mut ed, 120, 34);
    assert!(screen.contains("To Do") && screen.contains("User · review"), "{screen}");
    assert_eq!(transitions(&ed).len(), 4);
    assert_eq!(home.read("task.yaml"), before);
}

#[test]
fn leaving_the_builder_with_edits_asks_first() {
    let _home = Home::new("bld-leave");
    let mut ed = builder("task");
    arrow_to(&mut ed, "review");
    press(&mut ed, key(KeyCode::Char('x')));
    assert!(ed.is_dirty());
    assert_eq!(press(&mut ed, key(KeyCode::Esc)), EditorAction::Stay);
    assert!(ed.state().unwrap().prompt().is_some(), "Save / Discard / Cancel");
    assert!(render(&mut ed, 80, 24).contains("Unsaved changes"));
    press(&mut ed, key(KeyCode::Char('c')));
    assert!(ed.state().unwrap().prompt().is_none() && ed.is_dirty());
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(press(&mut ed, key(KeyCode::Char('d'))), EditorAction::Close, "Discard, then out");
}

/// A workflow with a cycle (a status to itself, two each way), a status no
/// transition touches, and statuses in both columns.
const LOOPY: &str = "\
name: loopy
statuses:
- id: backlog
  owner: user
- id: todo
  owner: user
- id: in-progress
  owner: agent
  agent: developer
- id: review
  owner: user
- id: done
  owner: user
- id: canceled
  owner: user
transitions:
- from: in-progress
  to: review
  actions: [push_branch]
- from: review
  to: in-progress
  actions: [close_pr]
- from: review
  to: review
- from: review
  to: done
  actions: [merge]
- from: done
  to: in-progress
";

#[test]
fn a_cyclic_workflow_with_a_lone_status_draws_and_navigates_deterministically() {
    let home = Home::new("bld-loopy");
    std::fs::write(home.workflows().join("loopy.yaml"), LOOPY).unwrap();
    let mut ed = builder("loopy");
    render(&mut ed, 120, 40);
    let canvas = ed.builder.canvas().unwrap().clone();
    assert_eq!(canvas.unrouted(), 0, "every transition drawn");
    let mut ends = std::collections::HashSet::new();
    for r in &canvas.routes {
        assert!(ends.insert(r.cells[0]) && ends.insert(*r.cells.last().unwrap()), "no port shared");
    }
    let first = render(&mut ed, 120, 40);
    // The same draft draws the same way, laid out again or not.
    ed.builder = Default::default();
    press(&mut ed, key(KeyCode::Tab));
    ed.select(&Item::Canvas);
    assert_eq!(render(&mut ed, 120, 40), first);
    // The lone status (Backlog, Canceled) is reachable by the arrows.
    arrow_to(&mut ed, "canceled");
    press(&mut ed, key(KeyCode::Enter));
    assert!(render(&mut ed, 120, 40).contains("None yet"), "no outgoing transitions");
    // `]` walks every transition, wrapping.
    press(&mut ed, key(KeyCode::Esc));
    press(&mut ed, key(KeyCode::Esc));
    let mut seen = Vec::new();
    for _ in 0..6 {
        press(&mut ed, key(KeyCode::Char(']')));
        seen.push(b(&ed).focus_transition.unwrap());
    }
    let mut sorted = seen[..5].to_vec();
    sorted.sort();
    assert_eq!(sorted, [0, 1, 2, 3, 4]);
    assert_eq!(seen[5], seen[0], "wraps");
    // The self-loop's panel, and back.
    while b(&ed).focus_transition != Some(2) {
        press(&mut ed, key(KeyCode::Char(']')));
    }
    press(&mut ed, key(KeyCode::Enter));
    let screen = render(&mut ed, 120, 40);
    assert!(screen.contains("No actions"), "{screen}");
}

#[test]
fn resizing_with_the_panel_open_keeps_the_selection_and_the_row() {
    let _home = Home::new("bld-resize");
    let mut ed = builder("task");
    arrow_to(&mut ed, "review");
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::Agent("review".into()));
    // Wide: the panel beside the canvas.
    let wide = render(&mut ed, 130, 50);
    assert!(wide.contains("◐ In Progress") && wide.contains("Outgoing transitions"), "{wide}");
    // Narrow: the panel in the canvas's place, the same row (scrolled to
    // it, under ← Graph).
    let narrow = render(&mut ed, 80, 24);
    assert!(!narrow.contains("◐ In Progress") && narrow.contains("Agent"), "{narrow}");
    assert!(ed.items().contains(&Item::PanelBack), "← Graph is the panel's first row");
    assert_eq!(ed.cursor_item(), Some(Item::Agent("review".into())));
    assert_eq!(b(&ed).selected_status(), Some("review"));
    // ← Graph: the canvas, the same selection.
    go(&mut ed, Item::PanelBack);
    press(&mut ed, key(KeyCode::Enter));
    let graph = render(&mut ed, 80, 24);
    assert!(graph.contains("◑ Review") && graph.contains('╔'), "selected, double-ruled:\n{graph}");
    assert_eq!(b(&ed).selected_status(), Some("review"));
    // Enter opens it again; wide again keeps it all.
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.cursor_item(), Some(Item::PanelStatus));
    render(&mut ed, 80, 24);
    go(&mut ed, Item::Owner("review".into()));
    render(&mut ed, 130, 34);
    assert_eq!(ed.cursor_item(), Some(Item::Owner("review".into())));
    assert!(!render(&mut ed, 130, 34).contains("← Graph"));
    // Every size in between draws without overflowing or panicking.
    for (w, h) in [(1, 1), (10, 3), (40, 12), (80, 24), (103, 30), (104, 30), (200, 60)] {
        let screen = render(&mut ed, w, h);
        for line in screen.lines() {
            assert!(crate::panel::display_width(line) <= w as usize);
        }
    }
}

#[test]
fn scrolling_follows_the_keyboard_and_shift_arrows_pan() {
    let _home = Home::new("bld-scroll");
    let mut ed = builder("task");
    render(&mut ed, 60, 16);
    arrow_to(&mut ed, "done");
    render(&mut ed, 60, 16);
    let canvas = ed.builder.canvas().unwrap();
    let done = canvas.node("done").unwrap().clone();
    let (pan, area) = (ed.builder.pan, ed.builder.area);
    assert!(done.x >= pan.0 && done.x + done.w <= pan.0 + area.width as i32 + 1, "Done in view across");
    assert!(done.y >= pan.1 && done.y + canvas::NODE_H <= pan.1 + area.height as i32, "and down");
    press(&mut ed, key(KeyCode::Home));
    assert_eq!(ed.builder.pan, (0, 0));
    press(&mut ed, shift(KeyCode::Down));
    press(&mut ed, shift(KeyCode::Right));
    assert!(ed.builder.pan.0 > 0 && ed.builder.pan.1 > 0);
    assert!(b(&ed).focus.as_deref() == Some("done"), "panning moves no focus");
}

fn mouse(ed: &mut WorkflowEditorPage, kind: MouseEventKind, at: (i32, i32)) {
    let area = ed.builder.area;
    let (x, y) = (area.x as i32 + at.0 - ed.builder.pan.0, area.y as i32 + at.1 - ed.builder.pan.1);
    ed.handle_mouse(MouseEvent { kind, column: x as u16, row: y as u16, modifiers: KeyModifiers::NONE });
}

#[test]
fn the_mouse_selects_hovers_handles_and_draws_with_clicks_alone() {
    let _home = Home::new("bld-mouse");
    let mut ed = builder("task");
    render(&mut ed, 130, 40);
    let canvas = ed.builder.canvas().unwrap().clone();
    let review = canvas.node("review").unwrap().clone();
    let todo = canvas.node("todo").unwrap().clone();
    // A click on a node selects it; the panel beside.
    mouse(&mut ed, MouseEventKind::Down(MouseButton::Left), (review.x + 3, review.y + 1));
    assert_eq!(b(&ed).selected_status(), Some("review"));
    // A click on a line selects its transition.
    let route = canvas.transition_route(1).unwrap().cells.clone();
    let mid = route[route.len() / 2];
    mouse(&mut ed, MouseEventKind::Down(MouseButton::Left), mid);
    assert_eq!(b(&ed).selected_transition(), Some(1));
    // Hover shows a node's handles; none show on the others.
    mouse(&mut ed, MouseEventKind::Moved, (todo.x + 3, todo.y + 1));
    assert_eq!(b(&ed).hover.as_deref(), Some("todo"));
    let screen = render(&mut ed, 130, 40);
    let cell = |at: (i32, i32)| {
        let area = ed.builder.area;
        let line = screen.lines().nth((area.y as i32 + at.1 - ed.builder.pan.1) as usize).unwrap();
        line.chars().nth((area.x as i32 + at.0 - ed.builder.pan.0) as usize).unwrap()
    };
    use shelbi_app::settings::workflow_graph::Side as S;
    for side in S::ALL {
        assert_eq!(cell(todo.handle(side)), '◆', "To Do's {side:?} handle:\n{screen}");
        let backlog = canvas.node("backlog").unwrap();
        assert_ne!(cell(backlog.handle(side)), '◆', "none on Backlog:\n{screen}");
    }
    // Onto its right handle: still hovered, the handle hot.
    let handle = todo.handle(shelbi_app::settings::workflow_graph::Side::Right);
    mouse(&mut ed, MouseEventKind::Moved, handle);
    assert_eq!((b(&ed).hover.as_deref(), b(&ed).hot), (Some("todo"), Some(shelbi_app::settings::workflow_graph::Side::Right)));
    // Press and let go in place: the drawing waits for a click…
    mouse(&mut ed, MouseEventKind::Down(MouseButton::Left), handle);
    mouse(&mut ed, MouseEventKind::Up(MouseButton::Left), handle);
    assert_eq!(b(&ed).drawing().map(|d| d.from.as_str()), Some("todo"));
    // …on the destination.
    mouse(&mut ed, MouseEventKind::Down(MouseButton::Left), (review.x + 3, review.y + 1));
    let n = b(&ed).new_transition().unwrap();
    assert_eq!((n.from.as_str(), n.to.as_str()), ("todo", "review"));
    // A drag works too: handle to node.
    press(&mut ed, key(KeyCode::Esc));
    press(&mut ed, key(KeyCode::Esc));
    let backlog = canvas.node("backlog").unwrap().clone();
    mouse(&mut ed, MouseEventKind::Moved, (backlog.x + 2, backlog.y + 1));
    let h = backlog.handle(shelbi_app::settings::workflow_graph::Side::Bottom);
    mouse(&mut ed, MouseEventKind::Moved, h);
    mouse(&mut ed, MouseEventKind::Down(MouseButton::Left), h);
    mouse(&mut ed, MouseEventKind::Drag(MouseButton::Left), (review.x + 2, review.y + 1));
    assert_eq!(b(&ed).drawing().unwrap().over.as_deref(), Some("review"));
    assert!(render(&mut ed, 130, 40).contains('●'), "the held handle stays hot");
    mouse(&mut ed, MouseEventKind::Up(MouseButton::Left), (review.x + 2, review.y + 1));
    assert_eq!(b(&ed).new_transition().map(|n| (n.from.as_str(), n.to.as_str())), Some(("backlog", "review")));
    // The wheel scrolls the canvas.
    press(&mut ed, key(KeyCode::Esc));
    press(&mut ed, key(KeyCode::Esc));
    render(&mut ed, 60, 16);
    let before = ed.builder.pan;
    let at = (before.0 + 1, before.1 + 1);
    mouse(&mut ed, MouseEventKind::ScrollDown, at);
    assert_eq!(ed.builder.pan.1, before.1 + 2);
}


#[test]
fn the_panel_keeps_a_line_under_the_focus_when_it_clips() {
    let _home = Home::new("bld-panel-scroll");
    let mut ed = builder("task");
    // In Progress → Review: the longest panel, in the canvas's place, clipped at 80x16.
    arrow_to(&mut ed, "in-progress");
    press(&mut ed, key(KeyCode::Char(']')));
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(b(&ed).selected_transition(), Some(0));
    let first = render(&mut ed, 80, 16);
    assert!(first.contains("↓ more"), "the panel clips at 80x16:\n{first}");
    // Walk every row: the focused one is drawn above "↓ more", never covered
    // by it (the Form's rule).
    let mut seen = Vec::new();
    for _ in 0..40 {
        let screen = render(&mut ed, 80, 16);
        let lines: Vec<&str> = screen.lines().collect();
        let item = ed.cursor_item().unwrap();
        if ed.section_of(&item) == Section::Panel && item != Item::PanelBack {
            let at = focused_lines(&mut ed, 80, 16).iter().position(|f| *f).unwrap_or_else(|| panic!("{item:?} in view:\n{screen}"));
            if let Some(more) = lines.iter().position(|l| l.contains("↓ more")) {
                assert!(at < more, "{item:?} keeps a line under it:\n{screen}");
            }
            seen.push(item);
        }
        press(&mut ed, key(KeyCode::Down));
    }
    assert!(seen.contains(&Item::Text(Text::Timeout)) && seen.contains(&Item::RemoveTransition(0)), "{seen:?}");
}
