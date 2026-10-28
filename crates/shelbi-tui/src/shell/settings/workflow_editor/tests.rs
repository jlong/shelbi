//! The TUI's workflow editor against real files in a throwaway
//! `SHELBI_HOME`: an edit in every section through the keys alone, refused
//! text kept in view, Save / Discard / Cancel, the guards, on-disk conflicts,
//! keys the form doesn't show, and what it draws at 80x24.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use shelbi_app::settings::workflows::load;
use shelbi_core::{MergeStrategy, Owner, TransitionAction};
use shelbi_state::workflow_config::WorkflowConfig;

use super::super::workflows::tests::Home;
use super::*;

const PROJECT: &str = "my-project";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Press `k` with the built-in nav (arrows, Enter).
fn press(ed: &mut WorkflowEditorPage, k: KeyEvent) -> EditorAction {
    let nav = Nav {
        up: k.code == KeyCode::Up && k.modifiers.is_empty(),
        down: k.code == KeyCode::Down && k.modifiers.is_empty(),
        activate: k.code == KeyCode::Enter,
    };
    ed.handle_key(k, nav)
}

fn typed(ed: &mut WorkflowEditorPage, text: &str) {
    for c in text.chars() {
        press(ed, key(KeyCode::Char(c)));
    }
}

/// Clear the field being typed (Ctrl+U from its end) and type `text`.
fn retype(ed: &mut WorkflowEditorPage, text: &str) {
    press(ed, key(KeyCode::End));
    press(ed, ctrl('u'));
    typed(ed, text);
}

/// Run the editor's waiting work, as the shell does off the UI thread.
/// Returns what the last landing asked.
fn drain(ed: &mut WorkflowEditorPage) -> Option<Landed> {
    let mut last = None;
    while let Some((ticket, work)) = ed.take_work() {
        let done = run_work(work, &load);
        last = ed.landed(&ticket, done);
    }
    last
}

/// The editor for `name`, loaded.
fn editor(name: &str) -> WorkflowEditorPage {
    let mut ed = WorkflowEditorPage::open(PROJECT, name, None, None);
    drain(&mut ed);
    assert!(ed.state().is_some(), "loaded");
    ed
}

fn go(ed: &mut WorkflowEditorPage, item: Item) {
    assert!(ed.select(&item), "{item:?} is a row");
}

/// Pick the entry labelled `label` in the open list.
fn pick(ed: &mut WorkflowEditorPage, label: &str) {
    let Some(Modal::List { choices, .. }) = ed.modal.clone() else {
        panic!("no list open");
    };
    let i = choices.iter().position(|c| c.label == label).unwrap_or_else(|| panic!("no {label} in {choices:?}"));
    ed.choose(&choices, i);
}

fn on_disk(name: &str) -> shelbi_core::Workflow {
    WorkflowConfig::load(PROJECT).unwrap().workflow(name).unwrap().clone()
}

fn render(ed: &mut WorkflowEditorPage, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ed.render(f, f.area(), None, "↑↓")).unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn it_opens_after_its_read_with_the_header_and_every_section() {
    let _home = Home::new("ed-open");
    let mut ed = WorkflowEditorPage::open(PROJECT, "task", None, None);
    assert!(render(&mut ed, 100, 30).contains("Loading…"));
    drain(&mut ed);
    let rows: Vec<String> = ed
        .rows(100)
        .into_iter()
        .map(|r| r.line.spans.iter().map(|s| s.content.to_string()).collect())
        .collect();
    let text = rows.join("\n");
    for want in [
        "Workflow", "Name", "Description", "Initial status", "Statuses (6)", "+ Add status", "Manage project statuses",
        "Transitions (4)", "In Progress → Review", "1. Push branch", "2. Open PR", "+ Add transition", "Git",
        "Base branch", "Branch name", "{{github_user}}/{{id}}", "Merge strategy", "Squash", "Review", "Diff only",
        "Automation", "Zen settings", "Use project settings", "Required fields", "Delete workflow…",
    ] {
        assert!(text.contains(want), "{want} in:\n{text}");
    }
    assert!(text.contains("The default workflow can’t be deleted"), "the guard says why up front");
    let screen = render(&mut ed, 100, 30);
    let header = screen.lines().next().unwrap();
    assert!(header.contains("← Back") && header.contains("task default"), "{header}");
    assert!(header.contains("Save changes") && header.contains("Discard") && header.contains("⋮ Actions"), "{header}");
    assert_eq!(ed.cursor_item(), Some(Item::Back));
    // No Builder yet: no Builder | Form control.
    assert_eq!(VIEWS, &[Mode::Form]);
    assert!(!header.contains("Builder"));
    // A workflow that isn't there says so.
    let mut gone = WorkflowEditorPage::open(PROJECT, "nope", None, None);
    drain(&mut gone);
    assert!(render(&mut gone, 80, 10).contains("There is no workflow named nope."));
}

#[test]
fn an_edit_in_every_section_by_keys_alone_waits_for_save() {
    let home = Home::new("ed-every");
    std::fs::write(
        home.workflows().join("statuses.yaml"),
        format!("{}- id: qa\n  name: QA\n  category: handoff\n", shelbi_app::settings::workflows::fixture::STATUSES),
    )
    .unwrap();
    let before = home.read("task.yaml");
    let mut ed = editor("task");

    // Workflow: the description, the initial status.
    go(&mut ed, Item::Text(Text::Description));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "Shipped by keys.");
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::InitialStatus);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "To Do");

    // Statuses: add QA, move it up, its owner and agent, its tags; remove
    // Canceled.
    go(&mut ed, Item::AddStatus);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "QA");
    assert_eq!(ed.cursor_item(), Some(Item::Status("qa".into())), "the new row is selected");
    press(&mut ed, shift(KeyCode::Up));
    press(&mut ed, shift(KeyCode::Up));
    assert_eq!(ed.cursor_item(), Some(Item::Status("qa".into())), "the cursor follows the move");
    press(&mut ed, key(KeyCode::Char('a')));
    pick(&mut ed, "review");
    press(&mut ed, key(KeyCode::Char('o')));
    pick(&mut ed, "Agent");
    press(&mut ed, key(KeyCode::Char('t')));
    typed(&mut ed, "gpu, mac");
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::Status("canceled".into()));
    press(&mut ed, key(KeyCode::Char('x')));

    // Transitions: add one, retarget it, two actions reordered, one
    // removed elsewhere; its details.
    go(&mut ed, Item::AddTransition);
    press(&mut ed, key(KeyCode::Enter));
    let added = ed.state().unwrap().transitions().len() - 1;
    assert_eq!(ed.cursor_item(), Some(Item::Transition(added)));
    press(&mut ed, key(KeyCode::Char('t')));
    pick(&mut ed, "In Progress");
    press(&mut ed, key(KeyCode::Char('n')));
    pick(&mut ed, "Push branch");
    go(&mut ed, Item::AddAction(added));
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Open PR");
    go(&mut ed, Item::Action(added, 1));
    press(&mut ed, shift(KeyCode::Up));
    assert_eq!(ed.cursor_item(), Some(Item::Action(added, 0)), "the cursor follows the action");
    go(&mut ed, Item::Action(0, 1));
    press(&mut ed, key(KeyCode::Delete));
    go(&mut ed, Item::Details(0));
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::Text(Text::Target));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "develop");
    press(&mut ed, key(KeyCode::Down));
    assert_eq!(ed.cursor_item(), Some(Item::AddRun), "↓ stops typing and moves");
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "npm ci");
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, key(KeyCode::Char('n')));
    typed(&mut ed, "npm test");
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, shift(KeyCode::Up));
    go(&mut ed, Item::Text(Text::Timeout));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "45");
    press(&mut ed, key(KeyCode::Enter));

    // Git: the base branch and the strategy.
    go(&mut ed, Item::Text(Text::BaseBranch));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "develop");
    press(&mut ed, key(KeyCode::Esc));
    go(&mut ed, Item::MergeStrategy);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Rebase");

    // Review: run the branch.
    go(&mut ed, Item::ReviewMode);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Run the branch");
    go(&mut ed, Item::Text(Text::Review(0)));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "npm run dev -- -p $SLOT");
    press(&mut ed, key(KeyCode::Enter));

    // Automation: required task fields.
    go(&mut ed, Item::Text(Text::Params));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "feature");
    press(&mut ed, key(KeyCode::Enter));

    assert!(ed.is_dirty());
    assert!(!ed.state().unwrap().has_errors(), "{:?}", ed.state().unwrap().error_at(&Field::Save));
    assert_eq!(home.read("task.yaml"), before, "nothing written before Save");

    assert_eq!(press(&mut ed, ctrl('s')), EditorAction::Work);
    assert_eq!(drain(&mut ed), Some(Landed::Wrote));
    assert!(!ed.is_dirty());

    let w = on_disk("task");
    assert_eq!(w.description.as_deref(), Some("Shipped by keys."));
    assert_eq!(w.initial_status.as_deref(), Some("todo"));
    let ids: Vec<_> = w.statuses.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["backlog", "todo", "in-progress", "review", "qa", "done"]);
    let qa = w.status("qa").unwrap();
    assert_eq!((qa.owner, qa.agent.as_deref(), qa.tags.clone()), (Owner::Agent, Some("review"), vec!["gpu".to_string(), "mac".to_string()]));
    let ts = w.transitions.unwrap();
    assert_eq!(ts[0].actions, [TransitionAction::PushBranch]);
    assert_eq!(ts[0].target.as_deref(), Some("develop"));
    assert_eq!(ts[0].run, ["npm test", "npm ci"]);
    assert_eq!(ts[0].ready_timeout, Some(std::time::Duration::from_secs(45)));
    let last = ts.last().unwrap();
    assert_eq!(last.actions, [TransitionAction::OpenPr, TransitionAction::PushBranch]);
    assert_eq!(last.to, "in-progress");
    assert!(ts.iter().all(|t| t.to != "canceled"), "its transitions went with the status");
    let git = w.git.unwrap();
    assert_eq!((git.base_branch.as_deref(), git.merge_strategy), (Some("develop"), MergeStrategy::Rebase));
    assert_eq!(w.review.unwrap().serve, "npm run dev -- -p $SLOT");
    assert_eq!(w.required_params, ["feature"]);
    assert!(home.read("task.yaml").contains("## Statuses run left to right."), "comments kept");
}

#[test]
fn refused_text_stays_in_view_with_its_error_until_corrected() {
    let home = Home::new("ed-refused");
    let before = home.read("task.yaml");
    let mut ed = editor("task");
    go(&mut ed, Item::Details(1));
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::Text(Text::Timeout));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "soon");
    press(&mut ed, key(KeyCode::Enter));
    let error = ed.state().unwrap().error_at(&Field::Transition(1)).unwrap().to_string();
    assert!(error.contains("whole number of seconds"), "{error}");
    assert_eq!(ed.typed_text(&Text::Timeout), "soon", "the field keeps what was typed");
    // Moving on and another edit elsewhere don't put the text back.
    go(&mut ed, Item::AddStatus);
    go(&mut ed, Item::Status("done".into()));
    press(&mut ed, key(KeyCode::Char('t')));
    typed(&mut ed, "x");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.typed_text(&Text::Timeout), "soon");
    let screen = render(&mut ed, 100, 200);
    assert!(screen.contains("soon") && screen.contains("whole number of seconds"), "{screen}");

    // Save is refused while it's wrong, and nothing is written.
    assert_eq!(press(&mut ed, ctrl('s')), EditorAction::Stay);
    assert_eq!(home.read("task.yaml"), before);

    // A Git placeholder needs a required field: refused at Git, kept.
    go(&mut ed, Item::Text(Text::BaseBranch));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "feature/{{feature}}");
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.state().unwrap().error_at(&Field::Git).unwrap().contains("required task field"));
    assert_eq!(ed.typed_text(&Text::BaseBranch), "feature/{{feature}}");
    go(&mut ed, Item::Text(Text::Params));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "feature");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.state().unwrap().error_at(&Field::Git), None, "the required field fixed it");

    // Corrected: the error goes and Save writes.
    go(&mut ed, Item::Text(Text::Timeout));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "30");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.state().unwrap().error_at(&Field::Transition(1)), None);
    press(&mut ed, ctrl('s'));
    drain(&mut ed);
    let w = on_disk("task");
    assert_eq!(w.transitions.unwrap()[1].ready_timeout, Some(std::time::Duration::from_secs(30)));
    assert_eq!(w.git.unwrap().base_branch.as_deref(), Some("feature/{{feature}}"));
}

#[test]
fn run_the_branch_needs_a_serve_command_and_zen_goes_back_to_the_projects() {
    let home = Home::new("ed-review");
    let text = format!("{}zen:\n  ci_timeout: 600\nx-notes: keep me\n", home.read("subtask.yaml"));
    std::fs::write(home.workflows().join("subtask.yaml"), text).unwrap();
    let mut ed = editor("subtask");
    let rows = render(&mut ed, 100, 200);
    assert!(rows.contains("Custom") && rows.contains("This workflow sets its own CI timeout"), "{rows}");
    go(&mut ed, Item::ReviewMode);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Run the branch");
    assert!(ed.is_dirty(), "a mode to save");
    assert_eq!(press(&mut ed, ctrl('s')), EditorAction::Stay);
    assert!(ed.state().unwrap().error_at(&Field::Review).unwrap().contains("starts the server"));
    // Custom only explains; Use project settings drops the block.
    go(&mut ed, Item::Zen);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Custom");
    assert!(ed.state().unwrap().zen_override().is_some());
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Use project settings");
    go(&mut ed, Item::ReviewMode);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Diff only");
    press(&mut ed, ctrl('s'));
    drain(&mut ed);
    let w = on_disk("subtask");
    assert_eq!((w.zen.is_none(), w.review.is_none()), (true, true));
    assert!(home.read("subtask.yaml").contains("x-notes: keep me"), "keys the form doesn't show stay");
}

#[test]
fn an_unchanged_branch_prefix_is_kept_and_the_projects_git_shows_as_placeholders() {
    let home = Home::new("ed-prefix");
    let text = home.read("task.yaml").replace(
        "git:\n  base_branch: main\n  branch: '{{github_user}}/{{id}}'\n  merge_strategy: squash\n",
        "git:\n  branch_prefix: team\n",
    );
    std::fs::write(home.workflows().join("task.yaml"), text).unwrap();
    let mut ed = editor("task");
    assert_eq!(ed.typed_text(&Text::Branch), "team/{{id}}");
    assert_eq!(ed.typed_text(&Text::BaseBranch), "", "inherits the project's");
    let screen = render(&mut ed, 100, 200);
    assert!(screen.contains("Base branch") && screen.contains("main"), "the project's base as the placeholder");
    go(&mut ed, Item::Text(Text::Description));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, " More.");
    press(&mut ed, ctrl('s'));
    drain(&mut ed);
    let saved = home.read("task.yaml");
    assert!(saved.contains("branch_prefix: team") && !saved.contains("\n  branch: "), "{saved}");
}

#[test]
fn leaving_with_edits_asks_save_discard_or_cancel() {
    let home = Home::new("ed-leave");
    let mut ed = editor("task");
    // Nothing to lose: Back goes at once.
    assert_eq!(press(&mut ed, key(KeyCode::Esc)), EditorAction::Close);

    go(&mut ed, Item::Text(Text::Description));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "!");
    press(&mut ed, key(KeyCode::Enter));
    // Esc: the prompt, and Cancel keeps the edits.
    assert_eq!(press(&mut ed, key(KeyCode::Esc)), EditorAction::Stay);
    assert_eq!(ed.state().unwrap().prompt(), Some(Leave::Back));
    assert!(render(&mut ed, 80, 24).contains("Save your changes to task before leaving?"));
    assert_eq!(press(&mut ed, key(KeyCode::Char('c'))), EditorAction::Stay);
    assert!(ed.state().unwrap().prompt().is_none() && ed.is_dirty());

    // Manage project statuses and Duplicate ask too; Discard goes.
    go(&mut ed, Item::ManageStatuses);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.state().unwrap().prompt(), Some(Leave::ManageStatuses));
    press(&mut ed, key(KeyCode::Esc));
    go(&mut ed, Item::Actions);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Duplicate");
    assert_eq!(ed.state().unwrap().prompt(), Some(Leave::Duplicate));
    press(&mut ed, key(KeyCode::Esc));
    // Edit YAML too; Discard opens the file as saved.
    press(&mut ed, key(KeyCode::Char('y')));
    assert_eq!(ed.state().unwrap().prompt(), Some(Leave::EditYaml));
    let EditorAction::EditYaml(file) = press(&mut ed, key(KeyCode::Char('d'))) else {
        panic!("Edit YAML after Discard");
    };
    assert_eq!(file.path, home.workflows().join("task.yaml"));
    assert!(!ed.is_dirty());

    // Save: it saves, then goes.
    go(&mut ed, Item::Text(Text::Description));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "Saved on the way out.");
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(press(&mut ed, key(KeyCode::Char('s'))), EditorAction::Work);
    let Some(Landed::Then(then)) = drain(&mut ed) else {
        panic!("the prompt's save goes on");
    };
    assert_eq!(ed.then(then), EditorAction::Close);
    assert_eq!(on_disk("task").description.as_deref(), Some("Saved on the way out."));
}

#[test]
fn a_failed_save_keeps_the_draft_and_says_why() {
    let home = Home::new("ed-fail");
    let mut ed = editor("subtask");
    go(&mut ed, Item::Text(Text::Name));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "task");
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, key(KeyCode::Esc));
    // The prompt's Save fails: it stays, the draft and its error kept.
    press(&mut ed, key(KeyCode::Char('s')));
    assert_eq!(drain(&mut ed), Some(Landed::Stay));
    assert!(ed.state().unwrap().error_at(&Field::Name).unwrap().contains("already exists"));
    assert!(ed.is_dirty() && ed.state().unwrap().prompt().is_none());
    assert_eq!(ed.typed_text(&Text::Name), "task");
    assert!(home.workflows().join("subtask.yaml").exists());
}

#[test]
fn rename_and_delete_are_guarded_by_the_shared_model() {
    let home = Home::new("ed-guards");
    // The default can't be deleted: the menu entry is dimmed with why, and
    // the form's button does nothing.
    let mut ed = editor("task");
    go(&mut ed, Item::Delete);
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.modal.is_none(), "no confirm for a guarded delete");
    go(&mut ed, Item::Actions);
    press(&mut ed, key(KeyCode::Enter));
    let Some(Modal::List { choices, .. }) = ed.modal.clone() else { panic!() };
    let delete = choices.iter().find(|c| c.label.starts_with("Delete")).unwrap();
    assert!(delete.inert && delete.help.as_deref().unwrap().contains("default"));
    let default = choices.iter().find(|c| c.label == "Set as default").unwrap();
    assert!(default.inert, "already the default");

    // An open issue keeps a workflow from being renamed or deleted.
    write_issue("sub-1", "subtask");
    let mut ed = editor("subtask");
    go(&mut ed, Item::Text(Text::Name));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "child");
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, ctrl('s'));
    drain(&mut ed);
    assert!(ed.state().unwrap().error_at(&Field::Name).unwrap().contains("sub-1"));
    assert!(home.workflows().join("subtask.yaml").exists() && !home.workflows().join("child.yaml").exists());

    go(&mut ed, Item::Delete);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.modal_title().as_deref(), Some("Delete workflow"));
    // Enter alone cancels (Cancel is focused); y deletes.
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.modal.is_none());
    go(&mut ed, Item::Delete);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(press(&mut ed, key(KeyCode::Char('y'))), EditorAction::Work);
    assert_eq!(drain(&mut ed), Some(Landed::Stay));
    assert!(ed.state().unwrap().error_at(&Field::Save).unwrap().contains("sub-1"));
    assert!(home.workflows().join("subtask.yaml").exists());

    // Set as default goes through the draft (written at Save).
    go(&mut ed, Item::Actions);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Set as default");
    assert!(ed.state().unwrap().is_default());
    assert_eq!(WorkflowConfig::load(PROJECT).unwrap().default_workflow(), "task");
}

/// An open issue `id` on `workflow`. (Gated so the board-seam gate,
/// which skips test items, reads it as the fixture it is.)
#[cfg(test)]
fn write_issue(id: &str, workflow: &str) {
    let at = chrono::Utc::now();
    let issue = shelbi_core::Issue {
        id: id.into(),
        title: id.into(),
        column: shelbi_core::Column::todo(),
        priority: 1,
        assigned_to: None,
        workflow: Some(workflow.into()),
        branch: None,
        depends_on: vec![],
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: at,
        updated_at: at,
        params: Default::default(),
    };
    shelbi_state::save_task(PROJECT, &issue, "").expect("save an issue");
}

#[test]
fn a_concurrent_hand_edit_asks_reload_or_keep_editing_and_never_overwrites() {
    let home = Home::new("ed-conflict");
    let mut ed = editor("task");
    go(&mut ed, Item::Text(Text::Description));
    press(&mut ed, key(KeyCode::Enter));
    retype(&mut ed, "Mine.");
    press(&mut ed, key(KeyCode::Enter));
    let edited = home.read("task.yaml").replace("Branch work", "Hand-edited work");
    std::fs::write(home.workflows().join("task.yaml"), &edited).unwrap();
    press(&mut ed, ctrl('s'));
    drain(&mut ed);
    assert_eq!(ed.modal_title().as_deref(), Some("Changed on disk"));
    assert_eq!(home.read("task.yaml"), edited, "the hand edit stands");
    assert!(render(&mut ed, 80, 24).contains("Reload"));
    // Keep editing (Enter: it's focused): the draft stays, Save stays refused.
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.modal.is_none() && ed.is_dirty());
    assert_eq!(ed.typed_text(&Text::Description), "Mine.");
    press(&mut ed, ctrl('s'));
    drain(&mut ed);
    assert_eq!(home.read("task.yaml"), edited);
    // Reload: the draft goes, the files are read again.
    assert_eq!(press(&mut ed, key(KeyCode::Char('r'))), EditorAction::Work);
    drain(&mut ed);
    assert!(!ed.is_dirty());
    assert!(ed.typed_text(&Text::Description).starts_with("Hand-edited"));
}

#[test]
fn a_reload_takes_hand_edits_into_a_clean_draft_and_leaves_edits_alone() {
    let home = Home::new("ed-reload");
    let mut ed = editor("task");
    let edited = home.read("task.yaml").replace("Branch work", "Edited in vim");
    std::fs::write(home.workflows().join("task.yaml"), &edited).unwrap();
    ed.reload();
    drain(&mut ed);
    assert!(ed.typed_text(&Text::Description).starts_with("Edited in vim"));

    // With edits, a reload doesn't touch the draft.
    go(&mut ed, Item::Text(Text::Params));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "team");
    press(&mut ed, key(KeyCode::Enter));
    std::fs::write(home.workflows().join("task.yaml"), edited.replace("Edited in vim", "Again")).unwrap();
    ed.reload();
    drain(&mut ed);
    assert!(ed.typed_text(&Text::Description).starts_with("Edited in vim"));
    assert_eq!(ed.typed_text(&Text::Params), "team");

    // A workflow gone from the files closes the editor.
    let mut ed = editor("subtask");
    std::fs::remove_file(home.workflows().join("subtask.yaml")).unwrap();
    ed.reload();
    assert!(matches!(drain(&mut ed), Some(Landed::Close { wrote: false, notice: Some(_) })));
}

#[test]
fn duplicate_edits_the_copy() {
    let home = Home::new("ed-dup");
    let mut ed = editor("subtask");
    press(&mut ed, key(KeyCode::Char('m')));
    pick(&mut ed, "Duplicate");
    assert_eq!(drain(&mut ed), Some(Landed::Wrote));
    assert_eq!(ed.name(), "subtask-copy");
    assert!(home.workflows().join("subtask-copy.yaml").exists());
}

#[test]
fn a_stale_result_is_dropped() {
    let _home = Home::new("ed-stale");
    let mut ed = editor("task");
    ed.reload();
    let (old, work) = ed.take_work().unwrap();
    ed.reload();
    let done = run_work(work, &load);
    assert_eq!(ed.landed(&old, done), None, "superseded by the newer read");
}

#[test]
fn it_scrolls_to_keep_the_focus_in_view_at_80x24_and_tab_steps_sections() {
    let _home = Home::new("ed-80x24");
    let mut ed = editor("task");
    let first = render(&mut ed, 80, 24);
    for line in first.lines() {
        assert!(panel::display_width(line) <= 80);
    }
    assert!(first.contains("↓ more"), "{first}");
    // Tab: Workflow, Statuses, Transitions, Git, Review, Automation, the
    // delete; Shift+Tab back.
    let mut sections = Vec::new();
    for _ in 0..7 {
        press(&mut ed, key(KeyCode::Tab));
        sections.push(ed.cursor_item().unwrap());
    }
    assert_eq!(
        sections,
        [
            Item::Text(Text::Name),
            Item::Status("backlog".into()),
            Item::Transition(0),
            Item::Text(Text::BaseBranch),
            Item::ReviewMode,
            Item::Zen,
            Item::Delete,
        ]
    );
    press(&mut ed, shift(KeyCode::BackTab));
    assert_eq!(ed.cursor_item(), Some(Item::Zen));
    // Walk every row: the focused one is always drawn, its marker in view.
    press(&mut ed, key(KeyCode::Home));
    let n = ed.items().len();
    for _ in 0..n {
        press(&mut ed, key(KeyCode::Down));
        let screen = render(&mut ed, 80, 24);
        if matches!(ed.cursor_item(), Some(Item::Back | Item::Save | Item::Discard | Item::Actions)) {
            continue;
        }
        assert!(screen.lines().skip(2).any(|l| l.trim_start().starts_with('›')), "{:?} in view:\n{screen}", ed.cursor_item());
    }
    assert_eq!(ed.cursor_item(), Some(Item::Delete));
    // The wheel scrolls without moving the focus.
    let before = ed.cursor;
    ed.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    render(&mut ed, 80, 24);
    assert_eq!(ed.cursor, before);
    // A menu and the prompt fit too.
    press(&mut ed, key(KeyCode::Char('m')));
    let menu = render(&mut ed, 80, 24);
    assert!(menu.contains("Edit YAML") && menu.contains("Manage project statuses"), "{menu}");
}

#[test]
fn an_agents_link_opens_on_its_status_row() {
    let _home = Home::new("ed-link");
    let config = WorkflowConfig::load(PROJECT).unwrap();
    let mut ed = WorkflowEditorPage::open(PROJECT, "task", Some(&config), Some("review".into()));
    assert_eq!(ed.cursor_item(), Some(Item::Status("review".into())), "at once from the page's model");
    drain(&mut ed);
    assert_eq!(ed.cursor_item(), Some(Item::Status("review".into())), "the read keeps it");
    // Read first: it lands there once the draft loads.
    let mut ed = WorkflowEditorPage::open(PROJECT, "task", None, Some("in-progress".into()));
    drain(&mut ed);
    assert_eq!(ed.cursor_item(), Some(Item::Status("in-progress".into())));
}

#[test]
fn renders_without_panicking_at_tiny_sizes() {
    let _home = Home::new("ed-tiny");
    let mut ed = editor("task");
    for (w, h) in [(1, 1), (10, 3), (20, 5), (40, 8)] {
        render(&mut ed, w, h);
    }
    press(&mut ed, key(KeyCode::Char('m')));
    render(&mut ed, 12, 4);
}
