//! `shelbi_app::settings::workflow_editor` against real files (moved here
//! from the desktop app's `workflow_editor_state` tests with the state, by
//! `tui-workflow-editor-form`): the draft and what it writes at Save, every
//! section's mapping onto the workflow YAML, refusals at their field, the
//! leave prompt, on-disk conflicts, and the text fields' form. Each test runs
//! in its own throwaway `SHELBI_HOME`, one at a time.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use shelbi_app::settings::workflow_editor::*;
use shelbi_app::settings::workflows::fixture::{self, STATUSES};
use shelbi_core::{MergeStrategy, Owner, Transition, TransitionAction, Workflow};
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
        let root = std::env::temp_dir().join(format!("shb-app-ed-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SHELBI_HOME", &root);
        fixture::write_project(&shelbi_state::projects_dir().unwrap(), PROJECT);
        Home { root, _lock: lock }
    }

    fn workflows(&self) -> PathBuf {
        shelbi_state::project_dir(PROJECT).unwrap().join("workflows")
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.workflows().join(file)).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn editor(name: &str) -> EditorState {
    EditorState::new(WorkflowConfig::load(PROJECT).unwrap(), name)
}

/// Save as the view does, here rather than off the UI thread.
fn save(state: &mut EditorState, texts: Vec<Change>) -> Option<Leave> {
    let job = state.begin_save(texts)?;
    state.saved(run_save(job))
}

fn on_disk(name: &str) -> Workflow {
    WorkflowConfig::load(PROJECT).unwrap().workflow(name).unwrap().clone()
}

#[test]
fn edits_stay_in_the_draft_until_save_and_discard_reverts_them() {
    let home = Home::new("ed-draft");
    let before = home.read("task.yaml");
    let mut state = editor("task");
    assert!(!state.is_dirty());
    assert!(state.change(Change::Description("Shipped work.".into())));
    assert!(state.is_dirty());
    assert_eq!(home.read("task.yaml"), before, "nothing written yet");

    state.discard();
    assert!(!state.is_dirty());
    assert_eq!(state.workflow().description.as_deref(), Some("Branch work with a review gate."));

    state.change(Change::Description("Shipped work.".into()));
    save(&mut state, vec![]);
    assert!(!state.is_dirty());
    assert_eq!(on_disk("task").description.as_deref(), Some("Shipped work."));
    let text = home.read("task.yaml");
    assert!(text.contains("## Statuses run left to right."), "comments kept:\n{text}");
}

#[test]
fn undoing_an_edit_by_hand_leaves_nothing_to_save() {
    let _home = Home::new("ed-undo");
    let mut state = editor("task");
    state.change(Change::Owner { id: "backlog".into(), owner: Owner::Agent });
    assert!(state.is_dirty());
    state.change(Change::Owner { id: "backlog".into(), owner: Owner::User });
    assert!(!state.is_dirty());
    assert!(state.begin_save(vec![]).is_none(), "nothing to save");
}

#[test]
fn status_edits_add_remove_reorder_and_assign() {
    let home = Home::new("ed-statuses");
    std::fs::write(
        home.workflows().join("statuses.yaml"),
        format!("{}- id: qa\n  name: QA\n  category: handoff\n", STATUSES),
    )
    .unwrap();
    let mut state = editor("task");
    assert_eq!(state.addable_statuses().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["qa"]);
    assert!(state.change(Change::AddStatus("qa".into())));
    assert!(state.change(Change::MoveStatus { id: "qa".into(), index: 4 }));
    assert!(state.change(Change::Agent { id: "qa".into(), agent: Some("review".into()) }));
    assert!(state.change(Change::Owner { id: "qa".into(), owner: Owner::Agent }));
    assert!(state.change(Change::Tags { id: "qa".into(), text: "review, gpu  mac".into() }));
    assert!(state.change(Change::RemoveStatus("canceled".into())));
    save(&mut state, vec![]);

    let w = on_disk("task");
    let ids: Vec<_> = w.statuses.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["backlog", "todo", "in-progress", "review", "qa", "done"]);
    let qa = w.status("qa").unwrap();
    assert_eq!((qa.owner, qa.agent.as_deref()), (Owner::Agent, Some("review")));
    assert_eq!(qa.tags, ["review", "gpu", "mac"]);
    let transitions = w.transitions.as_ref().unwrap();
    assert!(transitions.iter().all(|t| t.to != "canceled"), "removing a status takes its transitions");
}

#[test]
fn an_owner_without_an_agent_is_refused_at_its_row() {
    let _home = Home::new("ed-owner");
    let mut state = editor("task");
    assert!(!state.change(Change::Owner { id: "done".into(), owner: Owner::Agent }));
    let error = state.error_at(&Field::Status("done".into())).unwrap();
    assert!(error.contains("owner: agent but no agent"), "{error}");
    assert_eq!(state.status("done").unwrap().owner, Owner::User, "the draft is as it was");
    assert!(!state.is_dirty());
    // An agent from outside the project's is refused too.
    assert!(!state.change(Change::Agent { id: "done".into(), agent: Some("nobody".into()) }));
    assert!(state.error_at(&Field::Status("done".into())).unwrap().contains("doesn't exist"));
}

#[test]
fn transition_edits_and_their_actions() {
    let _home = Home::new("ed-transitions");
    let mut state = editor("task");
    assert_eq!(state.transitions().len(), 4);
    assert!(state.change(Change::AddTransition));
    let added = state.transitions()[4].clone();
    assert_eq!((added.from.as_str(), added.to.as_str()), ("backlog", "todo"), "the first free neighbors");
    assert!(state.change(Change::To { index: 4, status: "in-progress".into() }));
    assert!(state.change(Change::AddAction { index: 4, action: TransitionAction::PushBranch }));
    assert!(state.change(Change::AddAction { index: 4, action: TransitionAction::OpenPr }));
    assert!(state.change(Change::MoveAction { index: 4, slot: 1, to: 0 }));
    assert!(state.change(Change::SetAction { index: 4, slot: 1, action: TransitionAction::Restack }));
    assert!(state.change(Change::RemoveAction { index: 0, slot: 1 }));
    assert!(state.change(Change::RemoveTransition(3)));
    // The same move twice is refused.
    assert!(!state.change(Change::To { index: 0, status: "canceled".into() }));
    assert!(state.error_at(&Field::Transition(0)).unwrap().contains("duplicate transition"));
    save(&mut state, vec![]);

    let ts = on_disk("task").transitions.unwrap();
    assert_eq!(ts.len(), 4);
    assert_eq!(ts[0].actions, [TransitionAction::PushBranch]);
    assert_eq!((ts[3].from.as_str(), ts[3].to.as_str()), ("backlog", "in-progress"));
    assert_eq!(ts[3].actions, [TransitionAction::OpenPr, TransitionAction::Restack]);
}

#[test]
fn the_builders_new_transition_goes_in_whole_and_a_duplicate_is_refused() {
    let _home = Home::new("ed-insert");
    let mut state = editor("task");
    let t = Transition {
        from: "in-progress".into(),
        to: "done".into(),
        actions: vec![TransitionAction::PushBranch, TransitionAction::Merge],
        target: None,
        run: Vec::new(),
        ready: None,
        ready_timeout: None,
        url: None,
    };
    assert!(state.change(Change::InsertTransition(t.clone())));
    assert_eq!(state.transitions().last(), Some(&t));
    assert!(!state.change(Change::InsertTransition(t)));
    assert!(state.error_at(&Field::Transitions).unwrap().contains("duplicate transition"));
    state.clear_error_at(&Field::Transitions);
    assert!(!state.has_errors());
    save(&mut state, vec![]);
    assert_eq!(on_disk("task").transitions.unwrap().len(), 5);
}

#[test]
fn details_write_the_target_and_url() {
    let _home = Home::new("ed-details");
    let mut state = editor("task");
    assert!(!state.has_details(0));
    let form = DetailsForm { target: "develop".into(), url: "http://localhost:$SLOT".into() };
    assert!(state.change(Change::Details { index: 0, form }));
    assert!(state.has_details(0));
    assert!(!state.has_script(0), "Details set no Run script");
    save(&mut state, vec![]);
    let t = on_disk("task").transitions.unwrap()[0].clone();
    assert_eq!(t.target.as_deref(), Some("develop"));
    assert_eq!(t.url.as_deref(), Some("http://localhost:$SLOT"));
    assert!(t.run.is_empty() && t.ready.is_none() && t.ready_timeout.is_none());
}

#[test]
fn run_script_writes_run_ready_and_timeout_and_remove_clears_them() {
    let home = Home::new("ed-script");
    let mut state = editor("task");
    assert!(!state.has_script(0));
    assert_eq!(state.script_form(0), ScriptForm::default());
    let form = ScriptForm {
        run: vec!["npm ci".into(), "  ".into(), " npm run dev -- -p $SLOT & ".into()],
        ready: "curl -sf localhost:$SLOT".into(),
        ready_timeout: "soon".into(),
    };
    assert!(!state.change(Change::Script { index: 0, form: form.clone() }));
    assert!(state.error_at(&Field::Transition(0)).unwrap().contains("whole number of seconds"));
    assert!(!state.has_script(0), "a refused script leaves the draft as it was");
    let form = ScriptForm { ready_timeout: "120".into(), ..form };
    assert!(state.change(Change::Script { index: 0, form }));
    assert!(state.has_script(0) && !state.has_details(0));
    save(&mut state, vec![]);
    let t = on_disk("task").transitions.unwrap()[0].clone();
    assert_eq!(t.run, ["npm ci", "npm run dev -- -p $SLOT &"], "in order, blank rows dropped");
    assert_eq!(t.ready.as_deref(), Some("curl -sf localhost:$SLOT"));
    assert_eq!(t.ready_timeout, Some(Duration::from_secs(120)));

    // Opened again: the chip and its commands, and an unedited save writes
    // the same keys.
    let mut state = editor("task");
    assert!(state.has_script(0));
    let script = state.script_form(0);
    assert_eq!(script.run, ["npm ci", "npm run dev -- -p $SLOT &"]);
    assert_eq!((script.ready.as_str(), script.ready_timeout.as_str()), ("curl -sf localhost:$SLOT", "120"));
    let before = home.read("task.yaml");
    let form = state.text_form(None, Some(0));
    state.type_texts(&form);
    assert!(!state.is_dirty(), "the editor as filled changes nothing");
    assert_eq!(home.read("task.yaml"), before);

    // A row typed blank changes nothing either (it's dropped).
    let mut typed = form.clone();
    typed.script.as_mut().unwrap().1.run.push(String::new());
    state.type_texts(&typed);
    assert!(!state.is_dirty());

    // Reordered, then removed: Remove clears all three keys.
    typed.script.as_mut().unwrap().1.run.swap(0, 1);
    state.type_texts(&typed);
    assert_eq!(state.transitions()[0].run, ["npm run dev -- -p $SLOT &", "npm ci"]);
    assert!(state.change(Change::RemoveScript(0)));
    assert!(!state.has_script(0));
    save(&mut state, vec![]);
    let t = on_disk("task").transitions.unwrap()[0].clone();
    assert!(t.run.is_empty() && t.ready.is_none() && t.ready_timeout.is_none());
    let saved = home.read("task.yaml");
    assert!(!saved.contains("run:") && !saved.contains("ready"), "{saved}");
}

#[test]
fn git_fields_map_to_the_git_block() {
    let home = Home::new("ed-git");
    let mut state = editor("task");
    let form = state.git_form();
    assert_eq!((form.base_branch.as_str(), form.branch.as_str()), ("main", "{{github_user}}/{{id}}"));
    assert!(state.change(Change::Git(GitForm { merge_strategy: MergeStrategy::Rebase, ..form })));
    save(&mut state, vec![]);
    let git = on_disk("task").git.unwrap();
    assert_eq!(git.merge_strategy, MergeStrategy::Rebase);
    assert_eq!(git.branch.as_deref(), Some("{{github_user}}/{{id}}"));

    // Emptied, with the project's strategy: no block, so the project's
    // apply.
    assert!(state.change(Change::Git(GitForm {
        base_branch: String::new(),
        branch: String::new(),
        merge_strategy: MergeStrategy::Squash,
    })));
    save(&mut state, vec![]);
    assert_eq!(on_disk("task").git, None);
    assert!(!home.read("task.yaml").contains("git:"));

    // A placeholder in the base branch needs a required field.
    let templated = GitForm {
        base_branch: "feature/{{feature}}".into(),
        branch: String::new(),
        merge_strategy: MergeStrategy::Squash,
    };
    assert!(!state.change(Change::Git(templated.clone())));
    assert!(state.error_at(&Field::Git).unwrap().contains("required task field"));
    assert!(state.change(Change::RequiredParams("feature".into())));
    assert!(state.change(Change::Git(templated)));
}

#[test]
fn a_branch_prefix_round_trips_through_the_branch_name() {
    let home = Home::new("ed-prefix");
    let text = home.read("task.yaml").replace("  branch: '{{github_user}}/{{id}}'\n", "  branch_prefix: team\n");
    std::fs::write(home.workflows().join("task.yaml"), text).unwrap();
    let mut state = editor("task");
    let form = state.git_form();
    assert_eq!(form.branch, "team/{{id}}");
    assert!(state.change(Change::Git(GitForm { base_branch: "develop".into(), ..form.clone() })));
    save(&mut state, vec![]);
    let git = on_disk("task").git.unwrap();
    assert_eq!((git.branch_prefix.as_deref(), git.branch.as_deref()), (Some("team"), None));
    assert!(state.change(Change::Git(GitForm { branch: "{{id}}".into(), ..state.git_form() })));
    save(&mut state, vec![]);
    let git = on_disk("task").git.unwrap();
    assert_eq!((git.branch_prefix.as_deref(), git.branch.as_deref()), (None, Some("{{id}}")));
}

#[test]
fn review_mode_needs_a_serve_command_at_save() {
    let _home = Home::new("ed-review");
    let mut state = editor("task");
    assert_eq!(state.review_mode(), ReviewMode::DiffOnly);
    state.change(Change::ReviewMode(ReviewMode::Serve));
    assert!(state.is_dirty(), "a mode to save");
    assert!(state.begin_save(vec![]).is_none());
    assert!(state.error_at(&Field::Review).unwrap().contains("starts the server"));

    let form = ReviewForm {
        serve: "npm run dev -- -p $SLOT".into(),
        url: "http://localhost:$SLOT".into(),
        ..ReviewForm::default()
    };
    save(&mut state, vec![Change::Review(form)]);
    assert!(!state.has_errors());
    let review = on_disk("task").review.unwrap();
    assert_eq!(review.serve, "npm run dev -- -p $SLOT");
    assert_eq!(review.setup, None);

    state.change(Change::ReviewMode(ReviewMode::DiffOnly));
    save(&mut state, vec![]);
    assert_eq!(on_disk("task").review, None);
}

#[test]
fn initial_status_params_default_and_zen() {
    let home = Home::new("ed-misc");
    let text = format!("{}zen:\n  ci_timeout: 600\nx-notes: keep me\n", home.read("subtask.yaml"));
    std::fs::write(home.workflows().join("subtask.yaml"), text).unwrap();
    let mut state = editor("subtask");
    assert_eq!(state.initial_status().unwrap().id, "backlog");
    assert_eq!(state.zen_override().as_deref(), Some("CI timeout"));
    assert!(!state.is_default());
    state.change(Change::InitialStatus("todo".into()));
    state.change(Change::SetDefault);
    assert!(state.is_default());
    save(&mut state, vec![Change::RequiredParams("feature, team".into())]);
    let w = on_disk("subtask");
    assert_eq!(w.initial_status.as_deref(), Some("todo"));
    assert_eq!(w.required_params, ["feature", "team"]);
    assert_eq!(WorkflowConfig::load(PROJECT).unwrap().default_workflow(), "subtask");
    let text = home.read("subtask.yaml");
    assert!(text.contains("x-notes: keep me") && text.contains("ci_timeout: 600"), "keys the form doesn't show stay:\n{text}");

    state.change(Change::InheritZen);
    save(&mut state, vec![]);
    assert_eq!(on_disk("subtask").zen, None);
    assert!(home.read("subtask.yaml").contains("x-notes: keep me"));
}

#[test]
fn a_rename_waits_for_save_and_a_bad_name_shows_at_the_field() {
    let home = Home::new("ed-rename");
    let mut state = editor("subtask");
    state.type_name("task");
    assert!(state.is_dirty());
    save(&mut state, vec![]);
    assert!(state.error_at(&Field::Name).unwrap().contains("already exists"));
    assert_eq!(state.name(), "subtask");

    state.type_name("child");
    save(&mut state, vec![]);
    assert_eq!(state.name(), "child");
    assert!(!state.is_dirty());
    assert!(home.workflows().join("child.yaml").exists());
    assert!(!home.workflows().join("subtask.yaml").exists());
}

#[test]
fn a_refused_text_field_holds_the_save_back() {
    let home = Home::new("ed-held");
    let before = home.read("task.yaml");
    let mut state = editor("task");
    state.change(Change::Description("New words.".into()));
    let script = ScriptForm { ready_timeout: "x".into(), ..ScriptForm::default() };
    assert!(state.begin_save(vec![Change::Script { index: 1, form: script }]).is_none());
    assert!(state.error_at(&Field::Transition(1)).is_some());
    assert_eq!(home.read("task.yaml"), before, "nothing written");
    assert!(state.is_dirty(), "the description is still to save");
}

#[test]
fn leaving_with_edits_asks_first() {
    let _home = Home::new("ed-leave");
    let mut state = editor("task");
    assert_eq!(state.leave(Leave::Back), Some(Leave::Back), "nothing to lose");
    state.change(Change::Description("Changed.".into()));
    assert_eq!(state.leave(Leave::Back), None);
    assert_eq!(state.prompt(), Some(Leave::Back));
    state.cancel_prompt();
    assert_eq!(state.prompt(), None);
    assert!(state.is_dirty(), "Cancel keeps the edits");

    state.leave(Leave::ManageStatuses);
    assert_eq!(state.discard_and_leave(), Some(Leave::ManageStatuses));
    assert!(!state.is_dirty());

    state.change(Change::Description("Saved this time.".into()));
    state.leave(Leave::Back);
    let job = state.save_and_leave(vec![]).unwrap();
    assert_eq!(state.saved(run_save(job)), Some(Leave::Back));
    assert_eq!(on_disk("task").description.as_deref(), Some("Saved this time."));
}

#[test]
fn a_file_changed_under_the_save_is_kept_and_discard_reloads() {
    let home = Home::new("ed-conflict");
    let mut state = editor("task");
    let edited = home.read("task.yaml").replace("Branch work", "Hand-edited work");
    std::fs::write(home.workflows().join("task.yaml"), &edited).unwrap();
    state.change(Change::Description("Mine.".into()));
    save(&mut state, vec![]);
    assert!(state.error_at(&Field::Save).unwrap().starts_with("Changed on disk"));
    assert_eq!(home.read("task.yaml"), edited, "the hand edit stands");
    assert!(state.discard(), "the files must be read again");
    assert!(state.reloaded(WorkflowConfig::load(PROJECT).unwrap()));
    assert!(state.workflow().description.as_deref().unwrap().starts_with("Hand-edited"));
}

#[test]
fn duplicate_and_delete_run_on_the_saved_model() {
    let home = Home::new("ed-dup");
    let mut state = editor("subtask");
    assert_eq!(state.duplicate_name(), "subtask-copy");
    let base = state.begin_job().unwrap();
    let copy = run_duplicate(base, "subtask", "subtask-copy").unwrap();
    assert!(copy.workflow("subtask-copy").is_some());
    assert!(home.workflows().join("subtask-copy.yaml").exists());

    let mut state = EditorState::new(copy, "subtask-copy");
    assert_eq!(state.delete_blocked(), None);
    let base = state.begin_job().unwrap();
    run_delete(base, "subtask-copy").unwrap();
    assert!(!home.workflows().join("subtask-copy.yaml").exists());
    assert!(editor("task").delete_blocked().is_some_and(|e| e.contains("default")));
}

/// Issue `id` on `workflow`, in `column`.
fn save_issue(id: &str, workflow: &str, column: shelbi_core::Column) {
    let at = chrono::Utc::now();
    let issue = shelbi_core::Issue {
        id: id.into(),
        title: id.into(),
        column,
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
fn the_text_form_round_trips_the_draft_and_keeps_refused_text_out_of_it() {
    let _home = Home::new("ed-texts");
    let mut state = editor("task");
    let form = state.text_form(Some(0), Some(0));
    assert_eq!(form.description, "Branch work with a review gate.");
    assert_eq!((form.base_branch.as_str(), form.branch.as_str()), ("main", "{{github_user}}/{{id}}"));
    assert_eq!(form.tags.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["backlog", "todo", "in-progress", "review", "done", "canceled"]);
    assert_eq!(form.tags[3].1, "review");
    assert_eq!(form.details.as_ref().map(|(i, _)| *i), Some(0));
    assert_eq!(form.script.as_ref().map(|(i, _)| *i), Some(0));
    let gone = state.text_form(Some(99), Some(99));
    assert!(gone.details.is_none() && gone.script.is_none(), "no such transition");

    // The fields as filled change nothing.
    state.type_texts(&form);
    assert!(!state.is_dirty());

    // Typed: the description goes in; a bad timeout is refused at its row
    // and the draft keeps the last good value.
    let mut typed = form.clone();
    typed.description = "Typed.".into();
    typed.tags[0].1 = "gpu mac".into();
    typed.script.as_mut().unwrap().1.ready_timeout = "soon".into();
    state.type_texts(&typed);
    assert_eq!(state.workflow().description.as_deref(), Some("Typed."));
    assert_eq!(state.status("backlog").unwrap().tags, ["gpu", "mac"]);
    assert!(state.error_at(&Field::Transition(0)).unwrap().contains("whole number"));
    assert_eq!(state.transitions()[0].ready_timeout, None);
    // Save refuses while the text is wrong, and takes it once corrected.
    assert!(state.begin_save(state.text_changes(&typed)).is_none());
    typed.script.as_mut().unwrap().1.ready_timeout = "90".into();
    let job = state.begin_save(state.text_changes(&typed)).expect("a save");
    assert_eq!(state.saved(run_save(job)), None);
    assert!(!state.has_errors());
    let w = on_disk("task");
    assert_eq!(w.transitions.unwrap()[0].ready_timeout, Some(Duration::from_secs(90)));
    assert_eq!(w.description.as_deref(), Some("Typed."));

    // A status removed meanwhile: its tags are skipped, not refused.
    state.change(Change::RemoveStatus("backlog".into()));
    assert!(state.text_changes(&typed).iter().all(|c| !matches!(c, Change::Tags { id, .. } if id == "backlog")));
}

#[test]
fn an_unchanged_legacy_prefix_survives_an_unrelated_save() {
    let home = Home::new("ed-legacy");
    let text = home.read("task.yaml").replace("  branch: '{{github_user}}/{{id}}'\n", "  branch_prefix: team\n");
    std::fs::write(home.workflows().join("task.yaml"), text).unwrap();
    let mut state = editor("task");
    let form = state.text_form(None, None);
    assert_eq!(form.branch, "team/{{id}}");
    let mut typed = form.clone();
    typed.description = "Something else.".into();
    let job = state.begin_save(state.text_changes(&typed)).unwrap();
    state.saved(run_save(job));
    let saved = home.read("task.yaml");
    assert!(saved.contains("branch_prefix: team") && !saved.contains("\n  branch: "), "{saved}");
}

#[test]
fn a_conflict_holds_until_discard_reads_the_files_again() {
    let home = Home::new("ed-conflict-flag");
    let mut state = editor("task");
    assert!(!state.conflict());
    let edited = home.read("task.yaml").replace("Branch work", "By hand");
    std::fs::write(home.workflows().join("task.yaml"), &edited).unwrap();
    state.change(Change::Description("Mine.".into()));
    let job = state.begin_save(vec![]).unwrap();
    assert_eq!(state.saved(run_save(job)), None);
    assert!(state.conflict() && state.is_dirty(), "the draft is kept");
    assert!(state.error_at(&Field::Save).unwrap().contains("Discard to reload"));
    // Saving again is refused again: nothing overwrites the hand edit.
    let job = state.begin_save(vec![]).unwrap();
    state.saved(run_save(job));
    assert_eq!(home.read("task.yaml"), edited);
    assert!(state.discard());
    assert!(state.reloaded(WorkflowConfig::load(PROJECT).unwrap()));
    assert!(!state.conflict() && !state.is_dirty());
    assert_eq!(state.workflow().description.as_deref(), Some("By hand with a review gate."));
}

#[test]
fn rename_and_delete_are_refused_while_open_issues_use_the_workflow() {
    let home = Home::new("ed-guards");
    save_issue("sub-1", "subtask", shelbi_core::Column::todo());
    let mut state = editor("subtask");
    state.type_name("child");
    let job = state.begin_save(vec![]).unwrap();
    state.saved(run_save(job));
    let error = state.error_at(&Field::Name).unwrap().to_string();
    assert!(error.contains("sub-1"), "{error}");
    assert_eq!(state.name(), "subtask");
    assert!(state.is_dirty(), "the typed name is kept");
    assert!(home.workflows().join("subtask.yaml").exists());

    assert_eq!(state.delete_blocked(), None, "the board is read when the delete runs");
    let base = state.begin_job().unwrap();
    let e = run_delete(base, "subtask").unwrap_err();
    state.job_failed(e);
    assert!(state.error_at(&Field::Save).unwrap().contains("sub-1"));
    assert!(home.workflows().join("subtask.yaml").exists());

    // The default moves only through the model: Set as default on the draft.
    assert!(state.change(Change::SetDefault));
    assert!(state.is_default());
    assert_eq!(WorkflowConfig::load(PROJECT).unwrap().default_workflow(), "task", "not before Save");
}

#[test]
fn a_field_refused_for_want_of_another_goes_in_once_that_one_does() {
    let _home = Home::new("ed-depends");
    let mut state = editor("task");
    let mut form = state.text_form(None, None);
    form.base_branch = "feature/{{feature}}".into();
    state.type_texts(&form);
    assert!(state.error_at(&Field::Git).unwrap().contains("required task field"));
    // The required field goes in after Git in the form's order: Git is
    // tried again then.
    form.params = "feature".into();
    state.type_texts(&form);
    assert_eq!(state.error_at(&Field::Git), None);
    assert_eq!(state.workflow().git.as_ref().unwrap().base_branch.as_deref(), Some("feature/{{feature}}"));
}
