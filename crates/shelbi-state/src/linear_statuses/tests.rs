use super::*;
use crate::linear_client::{test_home, FakeLinear};
use crate::workflow_config::{ConfigError, WorkflowConfig};
use serde_json::json;

const PROJECT: &str = "lin";

fn state(id: &str, name: &str, kind: &str, position: f64) -> LinearState {
    serde_json::from_value(json!({ "id": id, "name": name, "type": kind, "position": position })).unwrap()
}

/// The ENG team in `tests/fixtures/linear/team_states.json`, in position order.
fn eng_states() -> Vec<LinearState> {
    vec![
        state("state-triage", "Triage", "triage", 0.0),
        state("state-backlog", "Backlog", "backlog", 1.0),
        state("state-todo", "Todo", "unstarted", 2.0),
        state("state-progress", "In Progress", "started", 3.0),
        state("state-review", "In Review", "started", 4.0),
        state("state-done", "Done", "completed", 5.0),
        state("state-canceled", "Canceled", "canceled", 6.0),
    ]
}

/// A `TeamStates` response carrying `states`.
fn states_json(states: &[LinearState]) -> String {
    json!({ "data": { "team": { "states": {
        "nodes": states,
        "pageInfo": { "hasNextPage": false, "endCursor": null }
    } } } })
    .to_string()
}

fn rows(c: &ProjectStatuses) -> Vec<(String, String, StatusCategory, Option<String>)> {
    c.statuses
        .iter()
        .map(|s| (s.id.clone(), s.name.clone(), s.category, s.external_id.clone()))
        .collect()
}

fn row(id: &str, name: &str, cat: StatusCategory, ext: &str) -> (String, String, StatusCategory, Option<String>) {
    (id.into(), name.into(), cat, Some(ext.into()))
}

/// A Linear project (team ENG) with Shelbi's default statuses and workflows.
fn linear_project(_home: &test_home::Home) {
    let projects = crate::projects_dir().unwrap();
    std::fs::create_dir_all(projects.join(PROJECT)).unwrap();
    std::fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "name: {PROJECT}\nrepo: /tmp/{PROJECT}\ndefault_branch: main\n\
orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{PROJECT}\nworkspaces: []\n\
issue_tracker:\n  backend: linear\n  linear:\n    team: ENG\n"
        ),
    )
    .unwrap();
    crate::scaffold_project_statuses(PROJECT).unwrap();
    crate::scaffold_project_workflow(PROJECT).unwrap();
}

use StatusCategory::*;

// ---------------------------------------------------------------------------
// The rules

#[test]
fn state_types_default_to_categories() {
    let cases = [
        (LinearStateType::Triage, "Triage", Backlog),
        (LinearStateType::Backlog, "Icebox", Backlog),
        (LinearStateType::Unstarted, "Todo", Ready),
        (LinearStateType::Started, "In Progress", Active),
        (LinearStateType::Completed, "Done", Done),
        (LinearStateType::Canceled, "Canceled", Archived),
        (LinearStateType::Canceled, "Duplicate", Archived),
        (LinearStateType::Duplicate, "Duplicate", Archived),
        (LinearStateType::Unknown, "Whatever", Backlog),
    ];
    for (kind, name, want) in cases {
        assert_eq!(default_category(kind, name), want, "{kind:?} {name}");
    }
}

#[test]
fn started_states_named_like_a_review_are_handoff() {
    for name in ["In Review", "Code review", "QA", "Verifying", "Verification", "Testing", "TEST"] {
        assert_eq!(default_category(LinearStateType::Started, name), Handoff, "{name}");
    }
    // Only started states: a review-named backlog state stays backlog.
    assert_eq!(default_category(LinearStateType::Backlog, "Needs review"), Backlog);
    assert_eq!(default_category(LinearStateType::Started, "Doing"), Active);
}

#[test]
fn first_sync_takes_the_teams_states_in_order_with_uuids() {
    let catalog = plan_catalog(&default_project_statuses(), &eng_states()).unwrap();
    assert_eq!(catalog.managed_by, Some(TrackerName::Linear));
    assert_eq!(
        rows(&catalog),
        vec![
            row("triage", "Triage", Backlog, "state-triage"),
            row("backlog", "Backlog", Backlog, "state-backlog"),
            row("todo", "Todo", Ready, "state-todo"),
            row("in-progress", "In Progress", Active, "state-progress"),
            row("in-review", "In Review", Handoff, "state-review"),
            row("done", "Done", Done, "state-done"),
            row("canceled", "Canceled", Archived, "state-canceled"),
        ]
    );
    // Order comes from `position`, not from the order Linear lists them in.
    let mut shuffled = eng_states();
    shuffled.reverse();
    assert_eq!(plan_catalog(&default_project_statuses(), &shuffled).unwrap(), catalog);
}

#[test]
fn resync_keeps_ids_and_category_overrides_across_rename_reorder_add_remove() {
    let mut catalog = plan_catalog(&default_project_statuses(), &eng_states()).unwrap();
    // The user says In Progress is a handoff step (an override).
    catalog.statuses.iter_mut().find(|s| s.id == "in-progress").unwrap().category = Handoff;

    // In Linear: In Progress renamed to Doing, In Review renamed to Shipping
    // (no override there: its category follows the new name), Todo moved
    // after Doing, Triage removed, QA added.
    let states = vec![
        state("state-backlog", "Backlog", "backlog", 1.0),
        state("state-progress", "Doing", "started", 2.0),
        state("state-todo", "Todo", "unstarted", 2.5),
        state("state-review", "Shipping", "started", 4.0),
        state("state-qa", "QA", "started", 4.5),
        state("state-done", "Done", "completed", 5.0),
        state("state-canceled", "Canceled", "canceled", 6.0),
    ];
    let next = plan_catalog(&catalog, &states).unwrap();
    assert_eq!(
        rows(&next),
        vec![
            row("backlog", "Backlog", Backlog, "state-backlog"),
            row("in-progress", "Doing", Handoff, "state-progress"),
            row("todo", "Todo", Ready, "state-todo"),
            row("in-review", "Shipping", Active, "state-review"),
            row("qa", "QA", Handoff, "state-qa"),
            row("done", "Done", Done, "state-done"),
            row("canceled", "Canceled", Archived, "state-canceled"),
        ]
    );

    let changes = diff_catalogs(&catalog, &next);
    assert_eq!(
        changes,
        vec![
            CatalogChange::Renamed { id: "in-progress".into(), from: "In Progress".into(), to: "Doing".into() },
            CatalogChange::Renamed { id: "in-review".into(), from: "In Review".into(), to: "Shipping".into() },
            CatalogChange::Category { id: "in-review".into(), from: Handoff, to: Active },
            CatalogChange::Added { id: "qa".into(), name: "QA".into(), category: Handoff },
            CatalogChange::Removed { id: "triage".into(), name: "Triage".into() },
            CatalogChange::Reordered {
                order: ["backlog", "in-progress", "todo", "in-review", "qa", "done", "canceled"]
                    .map(String::from)
                    .to_vec()
            },
        ]
    );
    // A second pass over the same states changes nothing.
    assert!(diff_catalogs(&next, &plan_catalog(&next, &states).unwrap()).is_empty());
}

#[test]
fn linears_duplicate_type_reads_as_archived_and_heals_the_old_backlog_default() {
    let dup: LinearState =
        serde_json::from_value(json!({"id": "state-dup", "name": "Duplicate", "type": "duplicate", "position": 7.0}))
            .unwrap();
    assert_eq!(dup.kind, LinearStateType::Duplicate);
    let mut states = eng_states();
    states.push(dup);
    let catalog = plan_catalog(&default_project_statuses(), &states).unwrap();
    assert_eq!(catalog.get("duplicate").unwrap().category, Archived);

    // A catalog an older Shelbi synced (the type read as unknown: backlog)
    // moves to archived on the next sync, instead of keeping backlog as if
    // the user had picked it.
    let mut old = catalog.clone();
    old.statuses.iter_mut().find(|s| s.id == "duplicate").unwrap().category = Backlog;
    let resynced = plan_catalog(&old, &states).unwrap();
    assert_eq!(resynced.get("duplicate").unwrap().category, Archived);
    // A category the user did pick is still kept.
    old.statuses.iter_mut().find(|s| s.id == "duplicate").unwrap().category = Done;
    assert_eq!(plan_catalog(&old, &states).unwrap().get("duplicate").unwrap().category, Done);
}

#[test]
fn a_new_state_never_takes_a_renamed_states_id() {
    let catalog = plan_catalog(&default_project_statuses(), &eng_states()).unwrap();
    let mut states = eng_states();
    states[4].name = "Reviewing".into(); // state-review keeps `in-review`
    states.push(state("state-new", "In Review", "started", 4.5));
    let next = plan_catalog(&catalog, &states).unwrap();
    assert_eq!(next.get("in-review").unwrap().external_id.as_deref(), Some("state-review"));
    assert_eq!(next.get("in-review").unwrap().name, "Reviewing");
    assert_eq!(next.get("in-review-2").unwrap().external_id.as_deref(), Some("state-new"));
}

#[test]
fn a_team_with_no_terminal_state_is_refused() {
    let states = vec![state("a", "Todo", "unstarted", 0.0), state("b", "Doing", "started", 1.0)];
    let err = plan_catalog(&default_project_statuses(), &states).unwrap_err().to_string();
    assert!(err.contains("Linear team's states"), "{err}");
}

#[test]
fn line_diff_marks_changed_lines() {
    let d = line_diff("a\nb\nc\n", "a\nx\nc\n");
    assert_eq!(
        d,
        vec![
            DiffLine::Same("a".into()),
            DiffLine::Removed("b".into()),
            DiffLine::Added("x".into()),
            DiffLine::Same("c".into()),
        ]
    );
}

// ---------------------------------------------------------------------------
// On disk

#[test]
fn sync_writes_a_managed_catalog_that_settings_shows_read_only() {
    let home = test_home::fresh();
    linear_project(&home);

    let synced = sync_with_states(PROJECT, Some("ENG (Engineering)"), &eng_states(), false).unwrap();
    assert!(synced.written);
    let text = std::fs::read_to_string(crate::statuses_path(PROJECT).unwrap()).unwrap();
    assert!(text.starts_with("# Statuses synced from the Linear team ENG (Engineering)."), "{text}");
    assert!(text.contains("managed_by: linear"), "{text}");
    assert!(text.contains("external_id: state-review"), "{text}");
    let loaded = crate::load_project_statuses(PROJECT).unwrap();
    assert_eq!(loaded, synced.after);

    // Structural edits are refused; a category edit isn't.
    let config = WorkflowConfig::load(PROJECT).unwrap();
    assert_eq!(config.statuses_read_only(), Some(TrackerName::Linear));
    let refused = [
        config.add_status("Blocked", Active).unwrap_err(),
        config.rename_status("todo", "Ready").unwrap_err(),
        config.move_status("todo", 0).unwrap_err(),
        config.delete_status("triage").unwrap_err(),
    ];
    for err in refused {
        assert_eq!(err, ConfigError::ReadOnlyStatus(TrackerName::Linear));
        assert_eq!(err.to_string(), "Statuses are managed by Linear");
    }
    let edited = config.set_status_category("triage", Ready).unwrap();
    edited.save().unwrap();

    // The save kept the header, the marker and the uuids; a re-sync keeps the
    // category (and has nothing else to change).
    let text = std::fs::read_to_string(crate::statuses_path(PROJECT).unwrap()).unwrap();
    assert!(text.starts_with("# Statuses synced from the Linear team"), "{text}");
    let saved = crate::load_project_statuses(PROJECT).unwrap();
    assert_eq!(saved.managed_by, Some(TrackerName::Linear));
    assert_eq!(saved.get("triage").unwrap().category, Ready);
    assert_eq!(saved.get("triage").unwrap().external_id.as_deref(), Some("state-triage"));
    let again = sync_with_states(PROJECT, None, &eng_states(), false).unwrap();
    assert!(again.changes.is_empty(), "{:?}", again.changes);
    assert!(!again.written);
    assert_eq!(crate::load_project_statuses(PROJECT).unwrap().get("triage").unwrap().category, Ready);
}

#[test]
fn a_hand_written_catalog_stays_editable() {
    let home = test_home::fresh();
    linear_project(&home);
    let config = WorkflowConfig::load(PROJECT).unwrap();
    assert_eq!(config.statuses_read_only(), None);
    assert!(config.add_status("Blocked", Active).is_ok());
}

#[test]
fn dry_run_writes_nothing() {
    let home = test_home::fresh();
    linear_project(&home);
    let before = std::fs::read_to_string(crate::statuses_path(PROJECT).unwrap()).unwrap();
    let synced = sync_with_states(PROJECT, None, &eng_states(), true).unwrap();
    assert!(!synced.written);
    assert!(!synced.changes.is_empty());
    assert_eq!(std::fs::read_to_string(crate::statuses_path(PROJECT).unwrap()).unwrap(), before);
    assert!(!filter_cache_path(PROJECT).unwrap().exists(), "no stamp on a dry run");
}

#[test]
fn workflows_naming_a_status_linear_lacks_fail_validation_until_rewritten() {
    let home = test_home::fresh();
    linear_project(&home);
    let synced = sync_with_states(PROJECT, None, &eng_states(), false).unwrap();
    // The default `task` workflow's `review` isn't a Linear status.
    assert_eq!(synced.broken, vec![BrokenRef { workflow: "task".into(), status: "review".into() }]);
    let err = crate::load_workflow(PROJECT, "task").unwrap_err().to_string();
    assert!(err.contains("status `review` is not one of the Linear team's statuses"), "{err}");
    assert!(err.contains("`in-review`"), "{err}");
    assert!(err.contains("--rewrite-workflows"), "{err}");

    // Dry run: the plan maps review -> in-review, writes nothing.
    let task_path = crate::workflow_path(PROJECT, "task").unwrap();
    let on_disk = std::fs::read_to_string(&task_path).unwrap();
    let rewrites = plan_workflow_rewrites(PROJECT, &synced.after, &synced.before).unwrap();
    assert_eq!(rewrites.len(), 1, "{rewrites:?}");
    let task = &rewrites[0];
    assert_eq!(task.workflow, "task");
    assert_eq!(task.remapped, vec![("review".to_string(), "in-review".to_string())]);
    assert!(task.merged.is_empty() && task.unmapped.is_empty());
    assert_eq!(task.error, None);
    assert_eq!(std::fs::read_to_string(&task_path).unwrap(), on_disk);
    // Owners, agents, tags and transitions carry over to the new id.
    let before = Workflow::from_yaml_str(&task.before).unwrap();
    let after = Workflow::from_yaml_str(&task.after).unwrap();
    let (old, new) = (before.status("review").unwrap(), after.status("in-review").unwrap());
    assert_eq!((&old.owner, &old.agent, &old.tags), (&new.owner, &new.agent, &new.tags));
    let edges = |w: &Workflow| -> Vec<(String, String)> {
        w.transitions.iter().flatten().map(|t| (t.from.clone(), t.to.clone())).collect()
    };
    let renamed: Vec<(String, String)> = edges(&before)
        .into_iter()
        .map(|(f, t)| {
            let m = |s: String| if s == "review" { "in-review".to_string() } else { s };
            (m(f), m(t))
        })
        .collect();
    assert_eq!(edges(&after), renamed);
    // The rest of the file (header comments included) is untouched.
    let diff = line_diff(&task.before, &task.after);
    assert!(diff.iter().all(|l| match l {
        DiffLine::Same(_) => true,
        DiffLine::Removed(l) | DiffLine::Added(l) => l.contains("review"),
    }), "{diff:?}");

    assert_eq!(apply_workflow_rewrites(&rewrites).unwrap(), 1);
    crate::load_workflow(PROJECT, "task").unwrap();
    assert!(plan_workflow_rewrites(PROJECT, &synced.after, &synced.after).unwrap().is_empty());
}

#[test]
fn rewrite_folds_duplicates_and_leaves_unmappable_ids() {
    let catalog = plan_catalog(&default_project_statuses(), &eng_states()).unwrap();
    // `qa` (handoff) maps to in-review, which `review` also maps to; `blocked`
    // has no category anywhere.
    let previous = ProjectStatuses::new(vec![
        ProjectStatus::new("review", "Review", Handoff),
        ProjectStatus::new("qa", "QA", Handoff),
    ]);
    let text = "name: x\ninitial_status: review\nstatuses:\n  - { id: todo, owner: user }\n  - { id: review, owner: user }\n  - { id: qa, owner: agent, agent: tester }\n  - { id: blocked, owner: user }\n  - { id: done, owner: user }\ntransitions:\n  - { from: review, to: qa }\n  - { from: qa, to: done }\n  - { from: review, to: done }\n";
    let category_of = |id: &str| previous.get(id).map(|s| s.category);
    let r = rewrite_workflow_text("x", PathBuf::from("x.yaml"), text, &catalog, &category_of).unwrap();
    assert_eq!(r.remapped, vec![("review".to_string(), "in-review".to_string())]);
    assert_eq!(r.merged, vec!["qa".to_string()]);
    assert_eq!(r.unmapped, vec!["blocked".to_string()]);
    let wf: Workflow = yaml_serde::from_str(&r.after).unwrap();
    assert_eq!(wf.initial_status.as_deref(), Some("in-review"));
    let ids: Vec<&str> = wf.statuses.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["todo", "in-review", "blocked", "done"]);
    let edges: Vec<(&str, &str)> =
        wf.transitions.iter().flatten().map(|t| (t.from.as_str(), t.to.as_str())).collect();
    // review->qa collapsed to a self edge (dropped); qa->done and review->done
    // are now the same edge (kept once).
    assert_eq!(edges, [("in-review", "done")]);
    assert!(r.error.as_deref().unwrap_or("").contains("`blocked`"), "{:?}", r.error);
}

#[test]
fn a_status_removed_by_an_earlier_sync_still_rewrites_by_its_category() {
    let home = test_home::fresh();
    linear_project(&home);
    // The first sync retires `review` (handoff); the rewrite is planned later,
    // against a catalog that no longer knows it.
    let synced = sync_with_states(PROJECT, None, &eng_states(), false).unwrap();
    let rewrites = plan_workflow_rewrites(PROJECT, &synced.after, &synced.after).unwrap();
    assert_eq!(rewrites[0].remapped, vec![("review".to_string(), "in-review".to_string())]);

    // A custom status retired later is remembered too.
    let mut states = eng_states();
    states.push(state("state-qa", "QA", "started", 4.5));
    sync_with_states(PROJECT, None, &states, false).unwrap();
    let doc = read_doc(PROJECT).unwrap();
    assert_eq!(doc[RETIRED_KEY], json!({ "review": "handoff" }));
    sync_with_states(PROJECT, None, &eng_states(), false).unwrap();
    let doc = read_doc(PROJECT).unwrap();
    assert_eq!(doc[RETIRED_KEY], json!({ "qa": "handoff", "review": "handoff" }));
}

#[test]
fn board_refresh_resyncs_only_when_the_states_change() {
    let home = test_home::fresh();
    linear_project(&home);
    let fake = FakeLinear::new();
    let _guard = fake.install();

    // First refresh: never synced, so it syncs.
    let first = refresh_if_changed(PROJECT, Duration::ZERO).unwrap().expect("first sync");
    assert_eq!(first.after.get("in-review").unwrap().external_id.as_deref(), Some("state-review"));
    assert!(read_doc(PROJECT).unwrap().contains_key(STATES_HASH_KEY));

    // Same states: no sync. Within the check interval: no request at all.
    assert!(refresh_if_changed(PROJECT, Duration::ZERO).unwrap().is_none());
    let reads = fake.calls_for("TeamStates").len();
    assert!(refresh_if_changed(PROJECT, Duration::from_secs(3600)).unwrap().is_none());
    assert_eq!(fake.calls_for("TeamStates").len(), reads, "skipped inside the interval");

    // Linear renames a state: the next refresh picks it up.
    let mut states = eng_states();
    states[3].name = "Doing".into();
    fake.on("TeamStates", states_json(&states));
    let resynced = refresh_if_changed(PROJECT, Duration::ZERO).unwrap().expect("re-sync");
    assert_eq!(
        resynced.changes,
        vec![CatalogChange::Renamed { id: "in-progress".into(), from: "In Progress".into(), to: "Doing".into() }]
    );
    assert_eq!(crate::load_project_statuses(PROJECT).unwrap().get("in-progress").unwrap().name, "Doing");
}

#[test]
fn the_filter_cache_keeps_the_sync_stamp() {
    let home = test_home::fresh();
    linear_project(&home);
    let fake = FakeLinear::new();
    let _guard = fake.install();
    sync(PROJECT, false).unwrap();
    let hash = read_doc(PROJECT).unwrap()[STATES_HASH_KEY].clone();

    // Re-resolving the filters (a config edit, `status --refresh`) keeps it.
    linear_client::invalidate_filter_cache(PROJECT).unwrap();
    assert_eq!(read_doc(PROJECT).unwrap()[STATES_HASH_KEY], hash);
    assert!(!read_doc(PROJECT).unwrap().contains_key("resolved"));
    let client = LinearClient::for_project(Some(PROJECT)).unwrap();
    let conn = shelbi_core::LinearConnection::for_team("ENG");
    linear_client::resolve_filters_cached(&client, PROJECT, &conn).unwrap();
    let doc = read_doc(PROJECT).unwrap();
    assert_eq!(doc[STATES_HASH_KEY], hash);
    assert!(doc.contains_key("resolved"));
}

/// [`linear_project`] with `default_workflow: task`, as `shelbi init`
/// makes it: the project fails to load while `task` names a status the
/// catalog lacks.
fn linear_project_with_default_workflow(home: &test_home::Home) {
    linear_project(home);
    let path = crate::projects_dir().unwrap().join(format!("{PROJECT}.yaml"));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}default_workflow: task\n")).unwrap();
    crate::load_project(PROJECT).unwrap();
}

#[test]
fn connect_syncs_and_maps_the_workflows_so_the_project_loads() {
    let home = test_home::fresh();
    linear_project_with_default_workflow(&home);
    let fake = FakeLinear::new();
    let _guard = fake.install();
    let connected = connect(PROJECT).unwrap();
    assert!(connected.sync.written);
    assert_eq!(connected.rewritten, 1);
    assert_eq!(connected.rewrites[0].workflow, "task");
    assert_eq!(connected.rewrites[0].remapped, vec![("review".to_string(), "in-review".to_string())]);
    assert!(connected.unmappable.is_empty());
    crate::load_project(PROJECT).unwrap();
    let task = crate::load_workflow(PROJECT, "task").unwrap();
    assert!(task.status("in-review").is_some() && task.status("review").is_none());
    let edges: Vec<(String, String)> =
        task.transitions.iter().flatten().map(|t| (t.from.clone(), t.to.clone())).collect();
    assert!(edges.contains(&("in-progress".into(), "in-review".into())), "{edges:?}");
    assert!(edges.contains(&("in-review".into(), "done".into())), "{edges:?}");
    assert!(edges.iter().all(|(f, t)| f != "review" && t != "review"), "{edges:?}");
}

#[test]
fn a_team_without_a_handoff_status_is_planned_as_a_refusal_and_nothing_is_written() {
    let home = test_home::fresh();
    linear_project_with_default_workflow(&home);
    // Linear's stock team: no review state.
    let states: Vec<LinearState> = eng_states().into_iter().filter(|s| s.id != "state-review").collect();
    let statuses = crate::statuses_path(PROJECT).unwrap();
    let task = crate::workflow_path(PROJECT, "task").unwrap();
    let before = (std::fs::read_to_string(&statuses).unwrap(), std::fs::read_to_string(&task).unwrap());

    let plan = plan_connect(PROJECT, "ENG (Engineering)", "ENG", &states).unwrap();
    assert_eq!(
        plan.unmappable,
        vec![Unmappable { workflow: "task".into(), status: "review".into(), category: Some(Handoff) }]
    );
    let why = plan.refusal().expect("refused");
    assert!(why.contains("The Linear team ENG has no status"), "{why}");
    assert!(why.contains("Nothing was written."), "{why}");
    assert!(why.contains("workflow `task`: `review` (a handoff status"), "{why}");
    assert!(why.contains("like In Review"), "{why}");
    assert!(why.contains("`in-progress`"), "{why}");
    let after = (std::fs::read_to_string(&statuses).unwrap(), std::fs::read_to_string(&task).unwrap());
    assert_eq!(after, before, "a plan writes nothing");
    assert!(!plan.sync.written);

    // A team that has one plans clean.
    let plan = plan_connect(PROJECT, "ENG (Engineering)", "ENG", &eng_states()).unwrap();
    assert_eq!(plan.refusal(), None);
    assert_eq!(plan.rewrites[0].remapped, vec![("review".to_string(), "in-review".to_string())]);
}

#[test]
fn plan_connect_to_reads_the_resolved_team() {
    let home = test_home::fresh();
    linear_project(&home);
    let fake = FakeLinear::new();
    let _guard = fake.install();
    let client = LinearClient::for_project(Some(PROJECT)).unwrap();
    let filters = linear_client::resolve_filters(&client, &shelbi_core::LinearConnection::for_team("ENG")).unwrap();
    let plan = plan_connect_to(PROJECT, &filters).unwrap();
    assert_eq!((plan.team.as_str(), plan.team_key.as_str()), ("ENG (Engineering)", "ENG"));
    assert_eq!(fake.calls_for("TeamStates")[0].var("teamId"), "team-eng");
    assert_eq!(plan.refusal(), None);
}

#[test]
fn sync_repairs_a_project_whose_workflow_names_a_status_linear_lacks() {
    let home = test_home::fresh();
    linear_project_with_default_workflow(&home);
    let fake = FakeLinear::new();
    let _guard = fake.install();
    // An older connect synced the catalog without mapping the workflows:
    // the project no longer loads.
    sync(PROJECT, false).unwrap();
    let err = crate::load_project(PROJECT).unwrap_err().to_string();
    assert!(err.contains("status `review` is not one of the Linear team's statuses"), "{err}");

    // The repair still reads the connection and syncs, then maps `review`
    // (retired by that sync, a handoff status) onto in-review.
    let synced = sync(PROJECT, false).unwrap();
    assert!(synced.changes.is_empty());
    let rewrites = plan_workflow_rewrites(PROJECT, &synced.after, &synced.before).unwrap();
    assert_eq!(rewrites[0].remapped, vec![("review".to_string(), "in-review".to_string())]);
    apply_workflow_rewrites(&rewrites).unwrap();
    crate::load_project(PROJECT).unwrap();
    // The board refresh reads it without the project loading, too.
    let _ = refresh_if_changed(PROJECT, Duration::ZERO).unwrap();
}

#[test]
fn sync_refuses_a_project_not_on_linear() {
    let home = test_home::fresh();
    linear_project(&home);
    let path = crate::projects_dir().unwrap().join(format!("{PROJECT}.yaml"));
    let text = std::fs::read_to_string(&path).unwrap();
    let cut = text.find("issue_tracker:").unwrap();
    std::fs::write(&path, &text[..cut]).unwrap();
    let err = sync(PROJECT, true).unwrap_err().to_string();
    assert!(err.contains("isn't connected to Linear"), "{err}");
    assert!(refresh_if_changed(PROJECT, Duration::ZERO).unwrap().is_none());
}
