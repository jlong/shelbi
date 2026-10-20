use super::*;
use crate::test_lock::LOCK as TEST_LOCK;
use shelbi_core::{MergeStrategy, TransitionAction};

const PROJECT: &str = "wfc";

/// A temp `SHELBI_HOME` holding project `wfc` with the files `shelbi init`
/// ships (`statuses.yaml`, `task.yaml`, `subtask.yaml`) and the agents the
/// shipped workflows name. Restores the environment on drop.
struct Home {
    root: PathBuf,
    projects: PathBuf,
    _lock: Box<dyn std::any::Any>,
}

impl Home {
    fn new() -> Home {
        let lock = TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "shelbi-wfc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SHELBI_HOME", &root);
        let projects = crate::projects_dir().unwrap();
        fs::create_dir_all(projects.join(PROJECT).join("workflows")).unwrap();
        for agent in ["orchestrator", "developer", "review"] {
            fs::create_dir_all(projects.join(PROJECT).join("agents").join(agent)).unwrap();
        }
        fs::write(projects.join(format!("{PROJECT}.yaml")), PROJECT_YAML).unwrap();
        let home = Home {
            root,
            projects,
            _lock: Box::new(lock),
        };
        home.write(
            "statuses.yaml",
            &shelbi_core::scaffold::default_statuses_yaml().unwrap(),
        );
        home.write(
            "task.yaml",
            &shelbi_core::scaffold::task_workflow_yaml().unwrap(),
        );
        home.write(
            "subtask.yaml",
            &shelbi_core::scaffold::subtask_workflow_yaml().unwrap(),
        );
        home
    }

    fn path(&self, file: &str) -> PathBuf {
        self.projects.join(PROJECT).join("workflows").join(file)
    }

    fn read(&self, file: &str) -> String {
        fs::read_to_string(self.path(file)).unwrap()
    }

    fn write(&self, file: &str, text: &str) {
        fs::write(self.path(file), text).unwrap();
    }

    fn project_yaml(&self) -> String {
        fs::read_to_string(self.projects.join(format!("{PROJECT}.yaml"))).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&self.root);
    }
}

const PROJECT_YAML: &str = "\
## Project header.
repo: /tmp/wfc
default_branch: main
default_workflow: task   # tasks without a workflow
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: /tmp/wfc
workspaces:
  - { name: dev, machine: local, runner: claude }
";

fn load() -> WorkflowConfig {
    WorkflowConfig::load(PROJECT).unwrap()
}

/// Save, and check the real loader reads every workflow back.
fn save(cfg: &WorkflowConfig) -> Saved {
    let saved = cfg.save().unwrap();
    let names: Vec<String> = crate::list_workflows(PROJECT)
        .unwrap()
        .into_iter()
        .map(|w| w.name)
        .collect();
    let mut expected: Vec<String> = saved.config.workflows().map(|w| w.name.clone()).collect();
    expected.sort();
    assert_eq!(names, expected);
    saved
}

fn transition(from: &str, to: &str, actions: &[TransitionAction]) -> Transition {
    Transition {
        from: from.into(),
        to: to.into(),
        actions: actions.to_vec(),
        target: None,
        run: Vec::new(),
        ready: None,
        ready_timeout: None,
        url: None,
    }
}

// ---------------------------------------------------------------------------
// Load and the index

#[test]
fn the_index_shows_counts_default_and_usage() {
    let _home = Home::new();
    let cfg = load();
    assert_eq!(
        cfg.workflow_summaries(),
        vec![
            WorkflowSummary {
                name: "subtask".into(),
                description: shelbi_core::subtask_workflow().description,
                statuses: 5,
                transitions: shelbi_core::subtask_workflow().transitions.unwrap().len(),
                is_default: false,
            },
            WorkflowSummary {
                name: "task".into(),
                description: shelbi_core::task_workflow().description,
                statuses: 6,
                transitions: 4,
                is_default: true,
            },
        ]
    );
    let rows = cfg.status_summaries();
    assert_eq!(rows.len(), 6);
    let review = rows.iter().find(|r| r.id == "review").unwrap();
    assert_eq!(review.used_in, vec!["task"]);
    let done = rows.iter().find(|r| r.id == "done").unwrap();
    assert_eq!(done.used_in, vec!["subtask", "task"]);
    assert_eq!(done.category, StatusCategory::Done);
    assert_eq!(cfg.statuses_path(), _home.path("statuses.yaml"));
    assert_eq!(
        cfg.agents().unwrap(),
        ["developer", "orchestrator", "review"]
    );
    assert!(!cfg.is_dirty());
}

#[test]
fn a_save_without_edits_writes_nothing() {
    let home = Home::new();
    let before = home.read("task.yaml");
    let saved = save(&load());
    assert!(saved.written.is_empty() && saved.removed.is_empty());
    assert_eq!(home.read("task.yaml"), before);
}

#[test]
fn an_unreadable_workflow_is_listed_and_protects_its_statuses() {
    let home = Home::new();
    home.write(
        "broken.yaml",
        "name: broken\nstatuses:\n- {id: todo, owner: user}\n- {id: nope, owner: user}\n",
    );
    let cfg = load();
    assert_eq!(cfg.unreadable().len(), 1);
    assert_eq!(cfg.unreadable()[0].name, "broken");
    assert!(
        cfg.unreadable()[0].error.contains("nope"),
        "{}",
        cfg.unreadable()[0].error
    );
    assert!(cfg.workflow("broken").is_none());
    assert_eq!(cfg.used_in("todo"), vec!["subtask", "task", "broken"]);
    // Its name is taken.
    assert_eq!(
        cfg.create_workflow("broken", None).unwrap_err(),
        ConfigError::WorkflowExists("broken".into())
    );
}

// ---------------------------------------------------------------------------
// The status catalog

#[test]
fn renaming_a_status_keeps_its_id_and_touches_one_line() {
    let home = Home::new();
    let before = home.read("statuses.yaml");
    let task_before = home.read("task.yaml");
    let cfg = load().rename_status("todo", "To Do").unwrap();
    assert_eq!(cfg.status("todo").unwrap().name, "To Do");
    // The workflows see the new name under the same id.
    assert_eq!(
        cfg.workflow("task").unwrap().status("todo").unwrap().name,
        "To Do"
    );
    let saved = save(&cfg);
    assert_eq!(saved.written, vec![home.path("statuses.yaml")]);
    assert_eq!(
        home.read("statuses.yaml"),
        before.replace("name: Todo", "name: To Do")
    );
    assert_eq!(home.read("task.yaml"), task_before);
    assert_eq!(
        crate::load_workflow(PROJECT, "task")
            .unwrap()
            .status("todo")
            .unwrap()
            .name,
        "To Do"
    );
}

#[test]
fn adding_a_status_derives_a_unique_kebab_id() {
    let home = Home::new();
    let before = home.read("statuses.yaml");
    let cfg = load();
    assert_eq!(cfg.status_id_for("QA / Staging!"), "qa-staging");
    assert_eq!(cfg.status_id_for("In Progress"), "in-progress-2");
    assert_eq!(cfg.status_id_for("  "), "status");
    let cfg = cfg.add_status("QA", StatusCategory::Handoff).unwrap();
    assert_eq!(cfg.statuses().last().unwrap().id, "qa");
    assert_eq!(cfg.used_in("qa"), Vec::<String>::new());
    save(&cfg);
    // Appended to the list, above the commented example section.
    assert_eq!(
        home.read("statuses.yaml"),
        before.replace(
            "  category: archived\n",
            "  category: archived\n- id: qa\n  name: QA\n  category: handoff\n"
        )
    );
    assert!(matches!(
        load().add_status("", StatusCategory::Done),
        Err(ConfigError::Invalid(_))
    ));
}

#[test]
fn a_status_category_and_order_can_change() {
    let home = Home::new();
    let before = home.read("statuses.yaml");
    let cfg = load()
        .set_status_category("canceled", StatusCategory::Done)
        .unwrap();
    assert_eq!(
        cfg.workflow("task")
            .unwrap()
            .status("canceled")
            .unwrap()
            .category,
        StatusCategory::Done
    );
    let cfg = cfg.move_status("backlog", 1).unwrap();
    let ids: Vec<&str> = cfg.statuses().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "todo",
            "backlog",
            "in-progress",
            "review",
            "done",
            "canceled"
        ]
    );
    save(&cfg);
    let expected = before
        .replace("  name: Canceled\n  category: archived", "  name: Canceled\n  category: done")
        .replace(
            "- id: backlog\n  name: Backlog\n  category: backlog\n- id: todo\n  name: Todo\n  category: ready\n",
            "- id: todo\n  name: Todo\n  category: ready\n- id: backlog\n  name: Backlog\n  category: backlog\n",
        );
    assert_eq!(home.read("statuses.yaml"), expected);
    assert_eq!(
        load().move_status("todo", 6).unwrap_err(),
        ConfigError::OutOfRange { index: 6 }
    );
}

#[test]
fn the_catalog_keeps_a_terminal_status() {
    let _home = Home::new();
    let cfg = load()
        .set_status_category("done", StatusCategory::Handoff)
        .unwrap();
    let err = cfg
        .set_status_category("canceled", StatusCategory::Handoff)
        .unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("terminal")),
        "{err}"
    );
}

#[test]
fn deleting_a_status_in_use_names_the_workflows() {
    let home = Home::new();
    let cfg = load();
    let err = cfg.delete_status("todo").unwrap_err();
    assert_eq!(
        err,
        ConfigError::StatusInUse {
            status: "todo".into(),
            workflows: vec!["subtask".into(), "task".into()],
        }
    );
    assert_eq!(err.to_string(), "status `todo` is used in subtask, task");
    // Once no workflow uses it, it goes.
    let before = home.read("statuses.yaml");
    let cfg = cfg
        .remove_workflow_status("task", "todo")
        .unwrap()
        .remove_workflow_status("subtask", "todo")
        .unwrap()
        .delete_status("todo")
        .unwrap();
    assert!(cfg.status("todo").is_none());
    save(&cfg);
    assert_eq!(
        home.read("statuses.yaml"),
        before.replace("- id: todo\n  name: Todo\n  category: ready\n", "")
    );
    assert_eq!(
        load().delete_status("nope").unwrap_err(),
        ConfigError::UnknownStatus("nope".into())
    );
}

// ---------------------------------------------------------------------------
// Workflows

#[test]
fn creating_a_workflow_writes_a_new_file() {
    let home = Home::new();
    let cfg = load()
        .create_workflow("research", Some("Reading, no code."))
        .unwrap();
    let wf = cfg.workflow("research").unwrap();
    assert_eq!(wf.statuses.len(), 6);
    assert_eq!(
        wf.status("in-progress").unwrap().agent.as_deref(),
        Some("developer")
    );
    assert_eq!(cfg.workflow_summaries().last().unwrap().name, "research");
    let saved = save(&cfg);
    assert_eq!(saved.written, vec![home.path("research.yaml")]);
    let text = home.read("research.yaml");
    assert!(text.starts_with("## Shelbi workflow"), "{text}");
    assert!(
        text.contains("name: research\ndescription: Reading, no code.\n"),
        "{text}"
    );
    for bad in ["", "Bad Name", "../x", "-x"] {
        assert!(
            matches!(
                load().create_workflow(bad, None),
                Err(ConfigError::Invalid(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(
        load().create_workflow("task", None).unwrap_err(),
        ConfigError::WorkflowExists("task".into())
    );
}

#[test]
fn a_duplicate_keeps_the_source_files_comments() {
    let home = Home::new();
    let source = home.read("task.yaml");
    let cfg = load().duplicate_workflow("task", "task-copy").unwrap();
    assert_eq!(
        cfg.workflow("task-copy").unwrap().transitions,
        cfg.workflow("task").unwrap().transitions
    );
    let saved = save(&cfg);
    assert_eq!(saved.written, vec![home.path("task-copy.yaml")]);
    assert_eq!(
        home.read("task-copy.yaml"),
        source.replace("name: task\n", "name: task-copy\n")
    );
    assert_eq!(home.read("task.yaml"), source);
}

#[test]
fn renaming_a_workflow_moves_its_file_and_the_default() {
    let home = Home::new();
    let source = home.read("task.yaml");
    let cfg = load().rename_workflow("task", "feature").unwrap();
    assert_eq!(cfg.default_workflow(), "feature");
    assert!(cfg
        .workflow_summaries()
        .iter()
        .any(|w| w.name == "feature" && w.is_default));
    let saved = save(&cfg);
    assert_eq!(saved.removed, vec![home.path("task.yaml")]);
    assert!(!home.path("task.yaml").exists());
    assert_eq!(
        home.read("feature.yaml"),
        source.replace("name: task\n", "name: feature\n")
    );
    assert_eq!(
        home.project_yaml(),
        PROJECT_YAML.replace(
            "default_workflow: task   #",
            "default_workflow: feature   #"
        )
    );
    assert_eq!(saved.config.default_workflow(), "feature");
    assert_eq!(
        load().rename_workflow("feature", "subtask").unwrap_err(),
        ConfigError::WorkflowExists("subtask".into())
    );
}

fn issue(id: &str, status: &str, workflow: Option<&str>) {
    let now = chrono::Utc::now();
    let task = shelbi_core::Issue {
        id: id.to_string(),
        title: id.to_string(),
        column: shelbi_core::Column::from_status_id(status),
        priority: 1,
        assigned_to: None,
        workflow: workflow.map(str::to_string),
        branch: None,
        depends_on: Vec::new(),
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: now,
        updated_at: now,
        params: std::collections::BTreeMap::new(),
    };
    crate::save_task(PROJECT, &task, "").unwrap();
}

#[test]
fn open_issues_on_a_workflow_block_its_rename() {
    let _home = Home::new();
    issue("finished", "done", Some("subtask"));
    issue("dropped", "canceled", Some("subtask"));
    issue("no-workflow", "todo", None);
    // Terminal issues and issues on the default don't block.
    load().check_rename("subtask").unwrap();
    load().rename_workflow("task", "feature").unwrap();

    issue("busy", "in-progress", Some("subtask"));
    let err = load().rename_workflow("subtask", "piece").unwrap_err();
    assert_eq!(
        err,
        ConfigError::WorkflowInUse {
            workflow: "subtask".into(),
            issues: vec!["busy".into()],
            total: 1,
        }
    );
    assert_eq!(
        err.to_string(),
        "workflow `subtask` is used by 1 open issue: busy; finish them or move them to another \
         workflow first"
    );
    // Other edits to it are fine, and so is renaming a copy made here.
    load()
        .set_description("subtask", Some("x"))
        .unwrap()
        .duplicate_workflow("subtask", "copy")
        .unwrap()
        .rename_workflow("copy", "copy-2")
        .unwrap();

    for n in 0..11 {
        issue(&format!("more-{n:02}"), "backlog", Some("subtask"));
    }
    let err = load().check_rename("subtask").unwrap_err();
    let ConfigError::WorkflowInUse { issues, total, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!((issues.len(), *total), (10, 12));
    assert!(err.to_string().contains(" and 2 more; "), "{err}");
    // Deleting it would strand them the same way.
    assert!(matches!(
        load().delete_workflow("subtask"),
        Err(ConfigError::WorkflowInUse { .. })
    ));
    assert!(matches!(
        load().check_delete("subtask"),
        Err(ConfigError::WorkflowInUse { .. })
    ));
}

#[test]
fn issues_on_the_legacy_default_name_block_its_target() {
    let _home = Home::new();
    // `workflow: default` resolves to `task` when there's no default.yaml.
    issue("legacy", "todo", Some("default"));
    assert!(matches!(
        load().check_rename("task"),
        Err(ConfigError::WorkflowInUse { .. })
    ));
    load().check_rename("subtask").unwrap();
}

#[test]
fn a_rename_is_refused_when_the_board_cant_be_read() {
    let home = Home::new();
    let path = home.projects.join(format!("{PROJECT}.yaml"));
    fs::write(
        &path,
        format!("{PROJECT_YAML}issue_tracker:\n  backend: carrier-pigeon\n"),
    )
    .unwrap();
    let err = load().rename_workflow("subtask", "piece").unwrap_err();
    assert!(
        matches!(&err, ConfigError::BoardUnavailable { workflow, reason } if workflow == "subtask" && reason.starts_with("couldn't read the board")),
        "{err}"
    );
}

#[test]
fn renames_can_swap_names() {
    let home = Home::new();
    let task = home.read("task.yaml");
    let subtask = home.read("subtask.yaml");
    let cfg = load()
        .rename_workflow("task", "tmp")
        .unwrap()
        .rename_workflow("subtask", "task")
        .unwrap()
        .rename_workflow("tmp", "subtask")
        .unwrap();
    assert_eq!(cfg.default_workflow(), "subtask");
    save(&cfg);
    assert_eq!(
        home.read("subtask.yaml"),
        task.replace("name: task\n", "name: subtask\n")
    );
    assert_eq!(
        home.read("task.yaml"),
        subtask.replace("name: subtask\n", "name: task\n")
    );
}

#[test]
fn description_and_default_edits() {
    let home = Home::new();
    let cfg = load()
        .set_description("subtask", Some("Smaller work."))
        .unwrap()
        .set_default_workflow("subtask")
        .unwrap();
    let subtask_before = home.read("subtask.yaml");
    save(&cfg);
    let desc_line = subtask_before
        .lines()
        .find(|l| l.starts_with("description:"))
        .unwrap();
    assert_eq!(
        home.read("subtask.yaml"),
        subtask_before.replace(desc_line, "description: Smaller work.")
    );
    assert_eq!(
        home.project_yaml(),
        PROJECT_YAML.replace(
            "default_workflow: task   #",
            "default_workflow: subtask   #"
        )
    );
    assert_eq!(load().default_workflow(), "subtask");
    assert_eq!(
        load().set_default_workflow("nope").unwrap_err(),
        ConfigError::UnknownWorkflow("nope".into())
    );
}

#[test]
fn the_default_and_the_last_workflow_cant_be_deleted() {
    let home = Home::new();
    let cfg = load();
    assert_eq!(
        cfg.delete_workflow("task").unwrap_err(),
        ConfigError::DeleteDefault("task".into())
    );
    let cfg = cfg.delete_workflow("subtask").unwrap();
    assert_eq!(
        cfg.delete_workflow("task").unwrap_err(),
        ConfigError::DeleteDefault("task".into())
    );
    let saved = save(&cfg);
    assert_eq!(saved.removed, vec![home.path("subtask.yaml")]);
    // A project whose only workflow isn't the default (say it's named in
    // the project YAML but missing).
    let cfg = load().set_default_workflow("task").unwrap();
    let mut cfg = cfg;
    cfg.default_workflow = "elsewhere".into();
    assert_eq!(
        cfg.delete_workflow("task").unwrap_err(),
        ConfigError::DeleteLast("task".into())
    );
}

// ---------------------------------------------------------------------------
// Inside a workflow

#[test]
fn status_assignment_edits_touch_only_their_lines() {
    let home = Home::new();
    let before = home.read("task.yaml");
    let cfg = load()
        .set_status_assignment("task", "review", Owner::Agent, Some("review"))
        .unwrap()
        .set_status_agent("task", "backlog", None)
        .unwrap()
        .set_status_tags(
            "task",
            "in-progress",
            &[" gpu ".into(), "gpu".into(), "".into(), "mac".into()],
        )
        .unwrap()
        .set_status_tags("task", "review", &[])
        .unwrap();
    assert_eq!(
        cfg.workflow("task")
            .unwrap()
            .status("in-progress")
            .unwrap()
            .tags,
        ["gpu", "mac"]
    );
    let saved = save(&cfg);
    assert_eq!(saved.written, vec![home.path("task.yaml")]);
    let expected = before
        .replace(
            "- id: backlog\n  owner: user\n  agent: orchestrator\n",
            "- id: backlog\n  owner: user\n",
        )
        .replace(
            "  agent: developer\n",
            "  agent: developer\n  tags:\n  - gpu\n  - mac\n",
        )
        .replace(
            "- id: review\n  owner: user\n  agent: review\n  tags:\n  - review\n",
            "- id: review\n  owner: agent\n  agent: review\n",
        );
    assert_eq!(home.read("task.yaml"), expected);
}

#[test]
fn owner_agent_needs_an_existing_agent() {
    let _home = Home::new();
    let cfg = load();
    let err = cfg
        .set_status_owner("task", "done", Owner::Agent)
        .unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("no agent")),
        "{err}"
    );
    let err = cfg
        .set_status_agent("task", "done", Some("ghost"))
        .unwrap_err();
    assert_eq!(
        err,
        ConfigError::UnknownAgent {
            workflow: "task".into(),
            status: "done".into(),
            agent: "ghost".into(),
            available: vec!["developer".into(), "orchestrator".into(), "review".into()],
        }
    );
    assert!(err
        .to_string()
        .contains("available: developer, orchestrator, review"));
    assert_eq!(
        cfg.set_status_agent("task", "qa", Some("developer"))
            .unwrap_err(),
        ConfigError::NotInWorkflow {
            workflow: "task".into(),
            status: "qa".into()
        }
    );
    assert_eq!(
        cfg.set_status_agent("nope", "done", None).unwrap_err(),
        ConfigError::UnknownWorkflow("nope".into())
    );
}

#[test]
fn workflow_statuses_are_added_removed_and_reordered() {
    let home = Home::new();
    let before = home.read("subtask.yaml");
    let cfg = load();
    assert_eq!(
        cfg.add_workflow_status("subtask", "todo", None)
            .unwrap_err(),
        ConfigError::AlreadyInWorkflow {
            workflow: "subtask".into(),
            status: "todo".into()
        }
    );
    assert_eq!(
        cfg.add_workflow_status("subtask", "qa", None).unwrap_err(),
        ConfigError::UnknownStatus("qa".into())
    );
    let cfg = cfg
        .add_workflow_status("subtask", "review", Some(3))
        .unwrap();
    let ids: Vec<&str> = cfg
        .workflow("subtask")
        .unwrap()
        .statuses
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "backlog",
            "todo",
            "in-progress",
            "review",
            "done",
            "canceled"
        ]
    );
    assert_eq!(cfg.used_in("review"), vec!["subtask", "task"]);
    save(&cfg);
    assert_eq!(
        home.read("subtask.yaml"),
        before.replace("- id: done\n", "- id: review\n  owner: user\n- id: done\n")
    );

    let cfg = load().move_workflow_status("subtask", "review", 5).unwrap();
    save(&cfg);
    assert_eq!(
        home.read("subtask.yaml"),
        before.replace(
            "  owner: user\n- id: canceled\n  owner: user\n",
            "  owner: user\n- id: canceled\n  owner: user\n- id: review\n  owner: user\n"
        )
    );
}

#[test]
fn removing_a_workflow_status_takes_its_transitions() {
    let home = Home::new();
    let cfg = load().set_initial_status("task", Some("review")).unwrap();
    let cfg = cfg.remove_workflow_status("task", "review").unwrap();
    let wf = cfg.workflow("task").unwrap();
    assert!(wf.status("review").is_none());
    assert_eq!(wf.initial_status, None);
    let edges: Vec<(&str, &str)> = wf
        .transitions
        .iter()
        .flatten()
        .map(|t| (t.from.as_str(), t.to.as_str()))
        .collect();
    assert_eq!(edges, [("in-progress", "canceled")]);
    save(&cfg);
    let text = home.read("task.yaml");
    assert!(
        !text.contains("id: review") && !text.contains(": review\n"),
        "{text}"
    );
    assert!(text.contains("## Stable id a new task lands in."), "{text}");
}

#[test]
fn the_initial_status_must_be_in_the_workflow() {
    let home = Home::new();
    let before = home.read("task.yaml");
    let cfg = load();
    let err = cfg
        .set_initial_status("subtask", Some("review"))
        .unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("initial_status")),
        "{err}"
    );
    let cfg = cfg.set_initial_status("task", Some("todo")).unwrap();
    save(&cfg);
    // A new top-level key goes after the last entry, above the commented
    // sections.
    assert_eq!(
        home.read("task.yaml"),
        before.replace(
            "  merge_strategy: squash\n",
            "  merge_strategy: squash\ninitial_status: todo\n"
        )
    );
    save(&load().set_initial_status("task", None).unwrap());
    assert_eq!(home.read("task.yaml"), before);
}

#[test]
fn transitions_are_added_edited_reordered_and_deleted() {
    let home = Home::new();
    let before = home.read("task.yaml");
    let cfg = load();
    let add = transition("todo", "in-progress", &[TransitionAction::Restack]);
    let cfg = cfg.add_transition("task", add.clone()).unwrap();
    assert_eq!(
        cfg.workflow("task").unwrap().transitions.as_ref().unwrap()[4],
        add
    );
    // Edit the first: one more action, and a details field.
    let mut first = cfg.workflow("task").unwrap().transitions.as_ref().unwrap()[0].clone();
    first.actions.push(TransitionAction::Restack);
    first.url = Some("http://localhost:$PORT".into());
    let cfg = cfg.update_transition("task", 0, first).unwrap();
    save(&cfg);
    let expected = before
        .replace(
            "  - push_branch\n  - open_pr\n",
            "  - push_branch\n  - open_pr\n  - restack\n  url: http://localhost:$PORT\n",
        )
        .replace(
            "  to: canceled\n  actions:\n  - close_pr\n  - delete_branch\ngit:",
            "  to: canceled\n  actions:\n  - close_pr\n  - delete_branch\n- from: todo\n  to: in-progress\n  actions:\n  - restack\ngit:",
        );
    assert_eq!(home.read("task.yaml"), expected);

    // Move the new one first, then delete it: back to the edited file.
    let cfg = load().move_transition("task", 4, 0).unwrap();
    assert_eq!(
        cfg.workflow("task").unwrap().transitions.as_ref().unwrap()[0].from,
        "todo"
    );
    let cfg = cfg.delete_transition("task", 0).unwrap();
    save(&cfg);
    assert_eq!(
        home.read("task.yaml"),
        expected.replace(
            "- from: todo\n  to: in-progress\n  actions:\n  - restack\n",
            ""
        )
    );

    let cfg = load();
    assert_eq!(
        cfg.delete_transition("task", 9).unwrap_err(),
        ConfigError::UnknownTransition {
            workflow: "task".into(),
            index: 9
        }
    );
    let err = cfg
        .add_transition("subtask", transition("in-progress", "review", &[]))
        .unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("undeclared status id `review`")),
        "{err}"
    );
    let err = cfg
        .add_transition("task", transition("in-progress", "review", &[]))
        .unwrap_err();
    assert!(
        matches!(&err, ConfigError::Invalid(m) if m.contains("duplicate transition")),
        "{err}"
    );
}

#[test]
fn deleting_every_transition_drops_the_block() {
    let home = Home::new();
    let mut cfg = load();
    for _ in 0..4 {
        cfg = cfg.delete_transition("task", 0).unwrap();
    }
    assert_eq!(cfg.workflow("task").unwrap().transitions, None);
    save(&cfg);
    assert!(!home.read("task.yaml").contains("transitions:"));
}

#[test]
fn git_review_and_other_fields() {
    let home = Home::new();
    let before = home.read("task.yaml");
    let cfg = load();
    let mut git = cfg.workflow("task").unwrap().git.clone().unwrap();
    git.base_branch = Some("develop".into());
    git.merge_strategy = MergeStrategy::Rebase;
    let cfg = cfg.set_git("task", Some(git)).unwrap();
    save(&cfg);
    assert_eq!(
        home.read("task.yaml"),
        before
            .replace("  base_branch: main\n", "  base_branch: develop\n")
            .replace("  merge_strategy: squash\n", "  merge_strategy: rebase\n")
    );

    // `branch` and `branch_prefix` are exclusive.
    let mut git = load().workflow("task").unwrap().git.clone().unwrap();
    assert_eq!(git.branch.as_deref(), Some("{{github_user}}/{{id}}"));
    git.branch_prefix = Some("feature".into());
    let err = load().set_git("task", Some(git)).unwrap_err();
    assert!(err.to_string().contains("branch_prefix"), "{err}");

    let review = ReviewServe {
        workdir: None,
        setup: None,
        serve: "npm run dev -- --port $PORT &".into(),
        ready: None,
        url: Some("http://localhost:$PORT".into()),
    };
    let cfg = load()
        .set_review("task", Some(review.clone()))
        .unwrap()
        .update_workflow("task", |w| w.name = "ignored".into())
        .unwrap();
    assert_eq!(cfg.workflow("task").unwrap().review.as_ref(), Some(&review));
    save(&cfg);
    assert_eq!(
        crate::load_workflow(PROJECT, "task").unwrap().review,
        Some(review)
    );
    save(&load().set_review("task", None).unwrap());
    assert_eq!(
        home.read("task.yaml"),
        before
            .replace("  base_branch: main\n", "  base_branch: develop\n")
            .replace("  merge_strategy: squash\n", "  merge_strategy: rebase\n")
    );

    let err = load()
        .set_git(
            "task",
            Some(GitConfig {
                base_branch: Some("task/{{task}}".into()),
                ..GitConfig::default()
            }),
        )
        .unwrap_err();
    assert!(err.to_string().contains("required_params"), "{err}");
    let cfg = load()
        .set_required_params("task", vec!["task".into()])
        .unwrap()
        .set_git(
            "task",
            Some(GitConfig {
                base_branch: Some("task/{{task}}".into()),
                ..GitConfig::default()
            }),
        )
        .unwrap();
    save(&cfg);
    assert!(home
        .read("task.yaml")
        .contains("required_params:\n- task\n"));
}

// ---------------------------------------------------------------------------
// Hand-written files

/// A workflow the way users hand-edit them: flow transitions, comments
/// inside lists, a block-scalar description, unknown and aliased keys.
const HAND: &str = "\
## My workflow, by hand.

name: app
description: |
  Rust app track.
  Squash-merge to main.
future_key: kept   # not a Shelbi key (yet)

statuses:
- { id: backlog,     owner: user, agent: orchestrator }
- { id: in-progress, owner: agent, agent: developer }
- id: review
  owner: user
  tag: review      # one tag, scalar alias
- { id: done,        owner: user }

initial_status: backlog

transitions:
  # Disk cap on the way out.
  - from: in-progress
    to: review
    actions: [push_branch, open_pr]
    run:
      - '[ -d target ] && rm -rf target; true'
  - { from: review,      to: done,     actions: [merge, delete_branch] }

zen:
  checks:
    local: []  # CI runs these
";

#[test]
fn hand_written_files_keep_their_shape() {
    let home = Home::new();
    home.write("app.yaml", HAND);
    let cfg = load();
    assert_eq!(
        cfg.workflow("app").unwrap().status("review").unwrap().tags,
        ["review"]
    );

    let cfg = cfg
        .set_status_agent("app", "backlog", None)
        .unwrap()
        .set_status_tags("app", "review", &["review".into(), "web".into()])
        .unwrap();
    let mut t = cfg.workflow("app").unwrap().transitions.as_ref().unwrap()[1].clone();
    t.actions = vec![TransitionAction::Merge];
    let cfg = cfg
        .update_transition("app", 1, t)
        .unwrap()
        .add_transition(
            "app",
            transition("review", "in-progress", &[TransitionAction::Restack]),
        )
        .unwrap();
    let saved = save(&cfg);
    // The aliased tag's line is rewritten as a list; its comment goes with it.
    assert_eq!(
        saved.dropped_comments,
        vec![(
            home.path("app.yaml"),
            vec!["# one tag, scalar alias".to_string()]
        )]
    );
    let expected = HAND
        .replace(
            "- { id: backlog,     owner: user, agent: orchestrator }",
            "- { id: backlog, owner: user }",
        )
        .replace(
            "  owner: user\n  tag: review      # one tag, scalar alias\n",
            "  owner: user\n  tags:\n  - review\n  - web\n",
        )
        .replace(
            "  - { from: review,      to: done,     actions: [merge, delete_branch] }\n",
            "  - { from: review, to: done, actions: [merge] }\n  - { from: review, to: in-progress, actions: [restack] }\n",
        );
    assert_eq!(home.read("app.yaml"), expected);

    // Reordering statuses moves each one's text.
    let cfg = load().move_workflow_status("app", "review", 0).unwrap();
    save(&cfg);
    assert_eq!(
        home.read("app.yaml"),
        expected.replace(
            "statuses:\n- { id: backlog, owner: user }\n- { id: in-progress, owner: agent, agent: developer }\n- id: review\n  owner: user\n  tags:\n  - review\n  - web\n",
            "statuses:\n- id: review\n  owner: user\n  tags:\n  - review\n  - web\n- { id: backlog, owner: user }\n- { id: in-progress, owner: agent, agent: developer }\n",
        )
    );
    assert!(home
        .read("app.yaml")
        .contains("future_key: kept   # not a Shelbi key (yet)"));
}

#[test]
fn a_missing_statuses_file_is_written_from_the_shipped_default() {
    let home = Home::new();
    fs::remove_file(home.path("statuses.yaml")).unwrap();
    // The loader would refuse these workflows without the file, but the
    // model falls back to the default catalog, as `load_project_statuses`
    // does, and a save writes it.
    let cfg = load().rename_status("todo", "Ready").unwrap();
    save(&cfg);
    assert_eq!(
        home.read("statuses.yaml"),
        shelbi_core::scaffold::default_statuses_yaml()
            .unwrap()
            .replace("name: Todo", "name: Ready")
    );
}

// ---------------------------------------------------------------------------
// Concurrent edits

#[test]
fn a_hand_edit_since_load_is_never_overwritten() {
    let home = Home::new();
    let cfg = load().rename_status("todo", "To Do").unwrap();
    let hand = home
        .read("statuses.yaml")
        .replace("name: Backlog", "name: Inbox");
    home.write("statuses.yaml", &hand);
    let err = cfg.save().unwrap_err();
    assert_eq!(err, ConfigError::Conflict(vec![home.path("statuses.yaml")]));
    assert!(err
        .to_string()
        .starts_with("changed on disk since it was loaded: "));
    assert_eq!(home.read("statuses.yaml"), hand);
    // Reloading picks the hand edit up, and the edit applies over it.
    save(&load().rename_status("todo", "To Do").unwrap());
    let text = home.read("statuses.yaml");
    assert!(
        text.contains("name: Inbox") && text.contains("name: To Do"),
        "{text}"
    );
}

#[test]
fn a_conflict_anywhere_writes_nothing() {
    let home = Home::new();
    let statuses = home.read("statuses.yaml");
    let cfg = load()
        .add_status("QA", StatusCategory::Handoff)
        .unwrap()
        .add_workflow_status("task", "qa", None)
        .unwrap()
        .create_workflow("research", None)
        .unwrap();
    // Someone creates research.yaml by hand meanwhile.
    home.write(
        "research.yaml",
        "name: research\nstatuses:\n- {id: done, owner: user}\n",
    );
    let err = cfg.save().unwrap_err();
    assert_eq!(err, ConfigError::Conflict(vec![home.path("research.yaml")]));
    assert_eq!(home.read("statuses.yaml"), statuses);
    assert!(!home.read("task.yaml").contains("qa"));
}

#[test]
fn a_deleted_file_counts_as_a_change() {
    let home = Home::new();
    let cfg = load().set_description("task", Some("x")).unwrap();
    fs::remove_file(home.path("task.yaml")).unwrap();
    assert_eq!(
        cfg.save().unwrap_err(),
        ConfigError::Conflict(vec![home.path("task.yaml")])
    );
    assert!(!home.path("task.yaml").exists());
}
