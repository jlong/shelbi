use super::*;
use crate::test_lock::LOCK as TEST_LOCK;

const PROJECT: &str = "agc";

/// A temp `SHELBI_HOME` holding project `agc` as `shelbi init` leaves it:
/// the shipped agents (copies of the bundled defaults, with their deploy
/// provenance), the shared preamble, `statuses.yaml` and the shipped
/// `task` / `subtask` workflows. Restores the environment on drop.
struct Home {
    root: PathBuf,
    agents: PathBuf,
    workflows: PathBuf,
    _lock: Box<dyn std::any::Any>,
}

impl Home {
    fn new() -> Home {
        let lock = TEST_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "shelbi-agc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SHELBI_HOME", &root);
        let projects = crate::projects_dir().unwrap();
        let workflows = projects.join(PROJECT).join("workflows");
        fs::create_dir_all(&workflows).unwrap();
        fs::write(projects.join(format!("{PROJECT}.yaml")), PROJECT_YAML).unwrap();
        crate::materialize_default_agents(PROJECT).unwrap();
        crate::scaffold_shared_preamble(PROJECT).unwrap();
        let home = Home {
            root,
            agents: crate::agents_dir(PROJECT).unwrap(),
            workflows,
            _lock: Box::new(lock),
        };
        home.write_workflow(
            "statuses.yaml",
            &shelbi_core::scaffold::default_statuses_yaml().unwrap(),
        );
        home.write_workflow(
            "task.yaml",
            &shelbi_core::scaffold::task_workflow_yaml().unwrap(),
        );
        home.write_workflow(
            "subtask.yaml",
            &shelbi_core::scaffold::subtask_workflow_yaml().unwrap(),
        );
        home
    }

    fn write_workflow(&self, file: &str, text: &str) {
        fs::write(self.workflows.join(file), text).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.agents.join(rel)
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Every file under `agents/`.
    fn files(&self) -> Files {
        snapshot(&self.agents).unwrap()
    }

    /// A user agent with a manifest, instructions and a skill.
    fn user_agent(&self, name: &str) {
        self.write(
            &format!("{name}/agent.yaml"),
            &format!("# My own agent.\nname: {name}\ndescription: Writes docs\n"),
        );
        self.write(&format!("{name}/instructions.md"), "# Docs\n");
        self.write(
            &format!("{name}/skills/style/SKILL.md"),
            "---\nname: style\ndescription: House style\n---\n\nBe brief.\n",
        );
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&self.root);
    }
}

const PROJECT_YAML: &str = "\
repo: /tmp/agc
default_branch: main
default_workflow: task
machines:
  - name: local
    kind: local
    work_dir: /tmp/agc
workspaces:
  - { name: dev, machine: local, runner: claude }
";

fn load() -> AgentsConfig {
    AgentsConfig::load(PROJECT).unwrap()
}

/// Save, and check the real loaders read every agent back.
fn save(cfg: &AgentsConfig) -> Saved {
    let saved = cfg.save().unwrap();
    for agent in saved.config.agents() {
        crate::load_agent_manifest(PROJECT, agent.name()).unwrap();
        crate::compose_agent_prompt(PROJECT, agent.name()).unwrap();
    }
    let mut names: Vec<String> = saved
        .config
        .agents()
        .map(|a| a.name().to_string())
        .collect();
    names.sort();
    assert_eq!(crate::list_agents(PROJECT).unwrap(), names);
    saved
}

/// Lines `new` has that `old` doesn't, and the reverse.
fn line_diff(old: &str, new: &str) -> (Vec<String>, Vec<String>) {
    let mut removed: Vec<&str> = old.lines().collect();
    let mut added = Vec::new();
    for line in new.lines() {
        match removed.iter().position(|l| *l == line) {
            Some(at) => {
                removed.remove(at);
            }
            None => added.push(line.to_string()),
        }
    }
    (removed.into_iter().map(str::to_string).collect(), added)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn shipped_names() -> Vec<&'static str> {
    DEFAULT_AGENTS.iter().map(|a| a.name).collect()
}

// ---------------------------------------------------------------------------
// Load and the index

#[test]
fn the_index_lists_shipped_agents_in_shipped_order_then_the_users() {
    let home = Home::new();
    home.user_agent("docs");
    let cfg = load();
    let names: Vec<&str> = cfg.agents().map(Agent::name).collect();
    let mut expected = shipped_names();
    expected.push("docs");
    assert_eq!(names, expected);

    let rows = cfg.summaries();
    let developer = rows.iter().find(|r| r.name == "developer").unwrap();
    assert_eq!(
        developer.description.as_deref(),
        Some(bundled("developer").unwrap().description)
    );
    assert_eq!(developer.preferred_runner, Some(RunnerKind::Claude));
    assert!(developer.runner_recommended);
    assert_eq!(developer.provenance, Provenance::Shipped);
    assert_eq!(developer.skills, 0);
    assert!(developer.assignments.contains(&Assignment {
        workflow: "task".into(),
        status: "in-progress".into(),
        status_name: "In Progress".into(),
        category: Some(StatusCategory::Active),
    }));

    let review = rows.iter().find(|r| r.name == "review").unwrap();
    assert_eq!(review.skills, 1, "the shipped load-run-detection skill");

    let docs = rows.iter().find(|r| r.name == "docs").unwrap();
    assert_eq!(docs.provenance, Provenance::UserCreated);
    assert!(!docs.provenance.is_customized());
    assert_eq!(docs.preferred_runner, None);
    assert!(!docs.runner_recommended);
    assert_eq!(docs.skills, 1);
    assert!(docs.assignments.is_empty());
}

#[test]
fn assignments_come_from_every_workflow_in_status_order() {
    let home = Home::new();
    home.write_workflow(
        "docs.yaml",
        "name: docs\nstatuses:\n  - id: todo\n    owner: agent\n    agent: qa\n  \
         - id: in-progress\n    owner: agent\n    agent: qa\n  - id: ghost\n    agent: qa\n  \
         - id: done\n    owner: user\n",
    );
    let cfg = load();
    let qa = cfg.assignments("qa");
    assert_eq!(
        qa.iter()
            .map(|a| (a.workflow.as_str(), a.status.as_str()))
            .collect::<Vec<_>>(),
        vec![("docs", "todo"), ("docs", "in-progress"), ("docs", "ghost")]
    );
    assert_eq!(qa[0].status_name, "Todo");
    assert_eq!(qa[0].category, Some(StatusCategory::Ready));
    // A status the catalog lacks still counts, under its id.
    assert_eq!(qa[2].status_name, "ghost");
    assert_eq!(qa[2].category, None);
}

#[test]
fn an_unparseable_agent_is_listed_not_editable() {
    let home = Home::new();
    home.write("broken/agent.yaml", "name: [unclosed\n");
    let cfg = load();
    assert!(cfg.agent("broken").is_none());
    assert_eq!(cfg.unreadable().len(), 1);
    assert_eq!(cfg.unreadable()[0].name, "broken");
    assert_eq!(
        cfg.create_agent("broken", None).unwrap_err(),
        AgentError::AgentExists("broken".into())
    );
    assert_eq!(
        cfg.set_description("broken", Some("x")).unwrap_err(),
        AgentError::UnknownAgent("broken".into())
    );
}

#[test]
fn an_agent_without_a_manifest_loads_with_its_name() {
    let home = Home::new();
    home.write("bare/instructions.md", "# Bare\n");
    let cfg = load();
    let bare = cfg.agent("bare").unwrap();
    assert_eq!(bare.manifest().name, "bare");
    assert_eq!(bare.description(), None);
    assert!(!cfg.is_dirty());
    // Editing it writes a manifest.
    let cfg = cfg.set_description("bare", Some("Bare bones")).unwrap();
    save(&cfg);
    let manifest = crate::load_agent_manifest(PROJECT, "bare")
        .unwrap()
        .unwrap();
    assert_eq!(manifest.name, "bare");
    assert_eq!(manifest.description.as_deref(), Some("Bare bones"));
}

// ---------------------------------------------------------------------------
// Round trips

#[test]
fn loading_and_saving_without_edits_writes_nothing() {
    let home = Home::new();
    home.user_agent("docs");
    let before = home.files();
    let cfg = load();
    assert!(!cfg.is_dirty());
    let saved = save(&cfg);
    assert!(saved.written.is_empty() && saved.removed.is_empty());
    assert_eq!(home.files(), before);
}

/// Each manifest edit, on a fresh copy of every shipped agent, changes
/// exactly the lines it should: every comment and other key survives, and
/// no other file is touched.
#[test]
fn every_shipped_manifest_round_trips_with_only_the_edit() {
    type Edit = fn(&AgentsConfig, &str) -> AgentsConfig;
    let cases: Vec<(Edit, Vec<&str>, Vec<&str>)> = vec![
        (
            |c, a| c.set_description(a, Some("Something else")).unwrap(),
            vec!["description: "],
            vec!["description: Something else"],
        ),
        (
            |c, a| c.set_model(a, RunnerKind::Claude, Some("opus")).unwrap(),
            vec![],
            vec!["runners:", "  claude:", "    model: opus"],
        ),
        (
            |c, a| {
                c.set_reasoning_effort(a, RunnerKind::Codex, Some(ReasoningEffort::High))
                    .unwrap()
            },
            vec![],
            vec!["runners:", "  codex:", "    reasoning_effort: high"],
        ),
        (
            |c, a| c.set_permissions_mode(a, Some("plan")).unwrap(),
            vec![],
            vec!["permissions_mode: plan"],
        ),
        (
            |c, a| c.set_preferred_runner(a, Some(RunnerKind::Codex)).unwrap(),
            vec!["preferred_runner: claude"],
            vec!["preferred_runner: codex"],
        ),
        // The comments documenting a removed key go with it (and are
        // reported).
        (
            |c, a| c.set_preferred_runner(a, None).unwrap(),
            vec![
                "## Which runner KIND",
                "## A recommendation only",
                "preferred_runner: claude",
            ],
            vec![],
        ),
    ];
    let home = Home::new();
    let pristine = home.files();
    for name in shipped_names() {
        for (edit, removed_prefixes, added) in &cases {
            let manifest_rel = format!("{name}/agent.yaml");
            let before = home.read(&manifest_rel);
            let saved = save(&edit(&load(), name));
            let after = home.read(&manifest_rel);
            let (removed, got_added) = line_diff(&before, &after);
            let dropped: Vec<String> = saved
                .dropped_comments
                .iter()
                .flat_map(|(_, lines)| lines.clone())
                .collect();
            let removed_comments: Vec<String> = removed
                .iter()
                .filter(|l| l.starts_with('#'))
                .cloned()
                .collect();
            assert_eq!(dropped, removed_comments, "{name}");
            assert_eq!(removed.len(), removed_prefixes.len(), "{name}: {removed:?}");
            for (line, prefix) in removed.iter().zip(removed_prefixes) {
                assert!(line.starts_with(prefix), "{name}: removed {line:?}");
            }
            assert_eq!(got_added, strings(added), "{name}");
            // Nothing else changed.
            let mut now = home.files();
            let mut was = pristine.clone();
            now.remove(&manifest_rel);
            was.remove(&manifest_rel);
            assert_eq!(now, was, "{name}");
            // Back to the shipped text for the next case.
            fs::write(home.path(&manifest_rel), &before).unwrap();
        }
    }
}

#[test]
fn instructions_and_shared_instructions_save_as_raw_markdown() {
    let home = Home::new();
    for name in shipped_names() {
        let before = home.files();
        let rel = format!("{name}/instructions.md");
        let text = format!("{}\n<!-- my note -->\n", home.read(&rel));
        let cfg = load().set_instructions(name, &text).unwrap();
        assert_eq!(cfg.agent(name).unwrap().instructions(), text);
        let saved = save(&cfg);
        assert_eq!(saved.written, vec![home.path(&rel)]);
        assert_eq!(home.read(&rel), text);
        let mut now = home.files();
        let mut was = before;
        now.remove(&rel);
        was.remove(&rel);
        assert_eq!(now, was, "{name}");
    }

    let cfg = load();
    assert_eq!(cfg.shared_instructions(), crate::DEFAULT_SHARED_PREAMBLE);
    let cfg = cfg.set_shared_instructions("You work on agc.\n").unwrap();
    let saved = save(&cfg);
    assert_eq!(saved.written, vec![home.path("_shared/preamble.md")]);
    assert_eq!(home.read("_shared/preamble.md"), "You work on agc.\n");
    assert!(crate::compose_agent_prompt(PROJECT, "developer")
        .unwrap()
        .starts_with("You work on agc.\n"));
}

#[test]
fn shared_instructions_are_created_when_missing() {
    let home = Home::new();
    fs::remove_file(home.path("_shared/preamble.md")).unwrap();
    let cfg = load();
    assert_eq!(cfg.shared_instructions(), "");
    assert!(!cfg.is_dirty());
    save(&cfg.set_shared_instructions("Hi\n").unwrap());
    assert_eq!(home.read("_shared/preamble.md"), "Hi\n");
}

// ---------------------------------------------------------------------------
// Customized

#[test]
fn customized_tracks_shipped_unchanged_edited_and_user_created() {
    let home = Home::new();
    home.user_agent("docs");
    let cfg = load();
    for name in shipped_names() {
        assert_eq!(cfg.provenance(name), Provenance::Shipped, "{name}");
    }
    assert_eq!(cfg.provenance("docs"), Provenance::UserCreated);

    // Each kind of edit, in the model and on disk, makes it customized.
    let edits: Vec<AgentsConfig> = vec![
        cfg.set_instructions("qa", "# Mine\n").unwrap(),
        cfg.set_model("qa", RunnerKind::Claude, Some("opus"))
            .unwrap(),
        cfg.set_description("qa", Some("Mine")).unwrap(),
        cfg.add_skill("qa", "extra", "More", "").unwrap(),
        cfg.remove_skill("review", "load-run-detection").unwrap(),
    ];
    for (i, edited) in edits.iter().enumerate() {
        let name = if i == 4 { "review" } else { "qa" };
        assert_eq!(edited.provenance(name), Provenance::Customized, "edit {i}");
        assert!(edited.provenance(name).is_customized());
        let saved = edited.save().unwrap().config;
        assert_eq!(saved.provenance(name), Provenance::Customized, "edit {i}");
        let reset = saved.reset_to_default(name).unwrap();
        assert_eq!(reset.provenance(name), Provenance::Shipped, "edit {i}");
        assert_eq!(save(&reset).config.provenance(name), Provenance::Shipped);
    }

    // A skill edited on disk, and a comment-only manifest change.
    home.write(
        "review/skills/load-run-detection/SKILL.md",
        "---\ndescription: mine\n---\n",
    );
    assert_eq!(load().provenance("review"), Provenance::Customized);
    let manifest = home.read("security/agent.yaml");
    home.write("security/agent.yaml", &format!("# A note.\n{manifest}"));
    assert_eq!(load().provenance("security"), Provenance::Shipped);
}

/// Instructions left over from an older Shelbi's shipped default (they
/// match the recorded deploy hash) aren't a customization: the self-heal
/// upgrades them in place.
#[test]
fn stale_shipped_instructions_are_not_customized() {
    let home = Home::new();
    let old = "# Developer (older shipped default)\n";
    home.write("developer/instructions.md", old);
    assert_eq!(load().provenance("developer"), Provenance::Customized);
    crate::update_state(PROJECT, |s| {
        s.deployed_agent_defaults
            .insert("developer".into(), crate::content_hash(old));
        Ok(())
    })
    .unwrap();
    assert_eq!(load().provenance("developer"), Provenance::Shipped);
}

#[test]
fn recommended_means_the_shipped_preferred_runner() {
    let _home = Home::new();
    let cfg = load();
    assert!(cfg.runner_recommended("orchestrator"));
    let cfg = cfg
        .set_preferred_runner("orchestrator", Some(RunnerKind::Codex))
        .unwrap();
    assert!(!cfg.runner_recommended("orchestrator"));
    let cfg = cfg.create_agent("docs", None).unwrap();
    assert!(!cfg.runner_recommended("docs"));
}

// ---------------------------------------------------------------------------
// Create, duplicate, rename, delete

#[test]
fn create_writes_a_manifest_placeholder_instructions_and_skills_dir() {
    let home = Home::new();
    let cfg = load().create_agent("docs", Some("  Writes docs ")).unwrap();
    assert!(cfg.is_dirty());
    assert_eq!(
        cfg.agent("docs").unwrap().description(),
        Some("Writes docs")
    );
    assert_eq!(cfg.provenance("docs"), Provenance::UserCreated);
    let saved = save(&cfg);
    assert_eq!(
        home.read("docs/instructions.md"),
        placeholder_instructions("docs")
    );
    assert!(home.path("docs/skills").is_dir());
    let manifest = crate::load_agent_manifest(PROJECT, "docs")
        .unwrap()
        .unwrap();
    assert_eq!(manifest.name, "docs");
    assert_eq!(manifest.description.as_deref(), Some("Writes docs"));
    assert_eq!(manifest.preferred_runner, None);
    assert!(saved.config.agent("docs").is_some());
}

#[test]
fn new_names_must_be_valid_and_free() {
    let _home = Home::new();
    let cfg = load();
    for bad in ["", "Docs", "-docs", "_shared", ".hidden", "a/b", "a b"] {
        assert!(
            matches!(cfg.create_agent(bad, None), Err(AgentError::Invalid(_))),
            "{bad:?}"
        );
        assert!(
            matches!(cfg.duplicate_agent("qa", bad), Err(AgentError::Invalid(_))),
            "{bad:?}"
        );
    }
    assert_eq!(
        cfg.create_agent("qa", None).unwrap_err(),
        AgentError::AgentExists("qa".into())
    );
    assert_eq!(
        cfg.duplicate_agent("ghost", "x").unwrap_err(),
        AgentError::UnknownAgent("ghost".into())
    );
}

#[test]
fn a_new_directory_appearing_before_save_is_a_conflict() {
    let home = Home::new();
    let cfg = load().create_agent("docs", None).unwrap();
    home.write("docs/instructions.md", "# Someone else's\n");
    assert_eq!(
        cfg.save().unwrap_err(),
        AgentError::Conflict(vec![home.path("docs")])
    );
    assert_eq!(home.read("docs/instructions.md"), "# Someone else's\n");
}

#[test]
fn duplicate_copies_every_file_and_renames_the_manifest() {
    let home = Home::new();
    let cfg = load()
        .set_model("review", RunnerKind::Claude, Some("sonnet"))
        .unwrap()
        .duplicate_agent("review", "review-2")
        .unwrap();
    let copy = cfg.agent("review-2").unwrap();
    assert_eq!(copy.model(RunnerKind::Claude), Some("sonnet"));
    assert_eq!(cfg.provenance("review-2"), Provenance::UserCreated);
    save(&cfg);
    for rel in [
        "settings.json",
        "instructions.md",
        "skills/load-run-detection/SKILL.md",
    ] {
        assert_eq!(
            home.read(&format!("review-2/{rel}")),
            home.read(&format!("review/{rel}")),
            "{rel}"
        );
    }
    // The manifest is the source's text, comments included, with the edit
    // and the new name.
    let (removed, added) = line_diff(
        &home.read("review/agent.yaml"),
        &home.read("review-2/agent.yaml"),
    );
    assert_eq!(removed, strings(&["name: review"]));
    assert_eq!(added, strings(&["name: review-2"]));
    let manifest = crate::load_agent_manifest(PROJECT, "review-2")
        .unwrap()
        .unwrap();
    assert_eq!(manifest.name, "review-2");
}

#[test]
fn rename_moves_the_directory_and_a_matching_manifest_name() {
    let home = Home::new();
    home.user_agent("docs");
    home.write("other/agent.yaml", "name: other-package\n");
    home.write("other/instructions.md", "# Other\n");
    let before = home.read("docs/agent.yaml");
    let cfg = load()
        .rename_agent("docs", "writer")
        .unwrap()
        .rename_agent("other", "second")
        .unwrap()
        .set_description("writer", Some("Writes"))
        .unwrap();
    save(&cfg);
    assert!(!home.path("docs").exists() && !home.path("other").exists());
    let (removed, added) = line_diff(&before, &home.read("writer/agent.yaml"));
    assert_eq!(
        removed,
        strings(&["name: docs", "description: Writes docs"])
    );
    assert_eq!(added, strings(&["name: writer", "description: Writes"]));
    assert_eq!(home.read("writer/instructions.md"), "# Docs\n");
    assert!(home.path("writer/skills/style/SKILL.md").is_file());
    // A manifest name that isn't the directory is a package identity.
    assert_eq!(home.read("second/agent.yaml"), "name: other-package\n");
}

#[test]
fn renames_can_chain_and_reuse_a_freed_name() {
    let home = Home::new();
    home.user_agent("a");
    home.user_agent("b");
    let cfg = load()
        .rename_agent("b", "c")
        .unwrap()
        .rename_agent("a", "b")
        .unwrap();
    save(&cfg);
    assert!(!home.path("a").exists());
    assert_eq!(home.read("b/agent.yaml").lines().nth(1), Some("name: b"));
    assert_eq!(home.read("c/agent.yaml").lines().nth(1), Some("name: c"));
    // Delete one and create another under its name.
    let cfg = load()
        .delete_agent("c")
        .unwrap()
        .create_agent("c", Some("Fresh"))
        .unwrap();
    save(&cfg);
    assert!(!home.path("c/skills/style").exists());
    assert_eq!(
        crate::load_agent_manifest(PROJECT, "c")
            .unwrap()
            .unwrap()
            .description
            .as_deref(),
        Some("Fresh")
    );
}

#[test]
fn delete_removes_the_directory() {
    let home = Home::new();
    home.user_agent("docs");
    let cfg = load().delete_agent("docs").unwrap();
    assert!(cfg.agent("docs").is_none());
    let saved = save(&cfg);
    assert_eq!(saved.removed, vec![home.path("docs")]);
    assert!(!home.path("docs").exists());
}

#[test]
fn rename_and_delete_are_refused_while_workflows_reference_the_agent() {
    let home = Home::new();
    home.user_agent("docs");
    home.write_workflow(
        "docs.yaml",
        "name: docs\nstatuses:\n  - id: todo\n    owner: agent\n    agent: docs\n  \
         - id: in-progress\n    owner: agent\n    agent: docs\n  - id: done\n    owner: user\n",
    );
    let cfg = load();
    let err = cfg.check_rename("docs").unwrap_err();
    let AgentError::InUse { agent, references } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(agent, "docs");
    assert_eq!(references.len(), 2);
    assert_eq!(
        err.to_string(),
        "agent `docs` is used by workflow `docs` status `todo`, workflow `docs` status \
         `in-progress`; assign those to another agent first"
    );
    assert_eq!(cfg.check_delete("docs").unwrap_err(), err);
    assert_eq!(cfg.rename_agent("docs", "x").unwrap_err(), err);
    assert_eq!(cfg.delete_agent("docs").unwrap_err(), err);

    // An unparseable workflow still counts.
    home.write_workflow(
        "docs.yaml",
        "name: docs\nstatuses:\n  - id: todo\n    agent: docs\n    owner: [\n",
    );
    home.write_workflow("other.yaml", "name: other\ngit: [broken\n");
    home.write_workflow(
        "third.yaml",
        "name: third\ninitial_status: nope\nstatuses:\n  - id: todo\n    agent: docs\n",
    );
    let err = load().check_delete("docs").unwrap_err();
    assert!(
        err.to_string().contains("workflow `third` status `todo`"),
        "{err}"
    );

    // Once nothing names it, both are allowed.
    fs::remove_file(home.workflows.join("docs.yaml")).unwrap();
    fs::remove_file(home.workflows.join("third.yaml")).unwrap();
    let cfg = load();
    cfg.check_rename("docs").unwrap();
    cfg.check_delete("docs").unwrap();
}

#[test]
fn the_project_runner_pin_blocks_rename_and_delete() {
    let home = Home::new();
    home.user_agent("docs");
    let projects = crate::projects_dir().unwrap();
    fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!("{PROJECT_YAML}agents:\n  docs:\n    runner: codex\n"),
    )
    .unwrap();
    let err = load().check_delete("docs").unwrap_err();
    assert_eq!(
        err,
        AgentError::InUse {
            agent: "docs".into(),
            references: vec![AgentReference::ProjectRunnerPin],
        }
    );
    assert!(err.to_string().contains("the project's runner pin"));
}

#[test]
fn shipped_agents_cannot_be_renamed_or_deleted() {
    let _home = Home::new();
    let cfg = load();
    for name in shipped_names() {
        assert_eq!(
            cfg.check_rename(name).unwrap_err(),
            AgentError::Shipped(name.into())
        );
        assert_eq!(
            cfg.delete_agent(name).unwrap_err(),
            AgentError::Shipped(name.into())
        );
    }
    assert_eq!(
        cfg.check_rename("ghost").unwrap_err(),
        AgentError::UnknownAgent("ghost".into())
    );
}

// ---------------------------------------------------------------------------
// Runner preferences

#[test]
fn clearing_a_setting_removes_its_key() {
    let home = Home::new();
    let before = home.read("qa/agent.yaml");
    let cfg = load()
        .set_model("qa", RunnerKind::Claude, Some("claude-opus-5-5"))
        .unwrap()
        .set_reasoning_effort("qa", RunnerKind::Claude, Some(ReasoningEffort::Max))
        .unwrap()
        .set_permissions_mode("qa", Some("acceptEdits"))
        .unwrap();
    let qa = cfg.agent("qa").unwrap();
    assert_eq!(qa.model(RunnerKind::Claude), Some("claude-opus-5-5"));
    assert_eq!(
        qa.reasoning_effort(RunnerKind::Claude),
        Some(ReasoningEffort::Max)
    );
    assert_eq!(qa.permissions_mode(), Some("acceptEdits"));
    save(&cfg);
    let resolved = crate::load_agent_manifest(PROJECT, "qa").unwrap().unwrap();
    assert_eq!(
        resolved.runners[&RunnerKind::Claude].model.as_deref(),
        Some("claude-opus-5-5")
    );

    // Clearing the model leaves the effort; clearing both drops `runners:`.
    let cfg = load()
        .set_model("qa", RunnerKind::Claude, Some("  "))
        .unwrap();
    assert_eq!(cfg.agent("qa").unwrap().model(RunnerKind::Claude), None);
    save(&cfg);
    assert!(home
        .read("qa/agent.yaml")
        .contains("    reasoning_effort: max"));
    let cfg = load()
        .set_reasoning_effort("qa", RunnerKind::Claude, None)
        .unwrap()
        .set_permissions_mode("qa", None)
        .unwrap();
    save(&cfg);
    assert_eq!(home.read("qa/agent.yaml"), before);
}

#[test]
fn a_model_not_in_the_suggestions_is_kept_as_written() {
    let home = Home::new();
    home.write(
        "qa/agent.yaml",
        "name: qa\nrunners:\n  claude:\n    model: claude-opus-5-5 # pinned\n    extra: kept\n",
    );
    let cfg = load();
    assert_eq!(
        cfg.agent("qa").unwrap().model(RunnerKind::Claude),
        Some("claude-opus-5-5")
    );
    let cfg = cfg
        .set_reasoning_effort("qa", RunnerKind::Claude, Some(ReasoningEffort::Low))
        .unwrap();
    save(&cfg);
    assert_eq!(
        home.read("qa/agent.yaml"),
        "name: qa\nrunners:\n  claude:\n    model: claude-opus-5-5 # pinned\n    extra: kept\n    \
         reasoning_effort: low\n"
    );
}

#[test]
fn runner_settings_are_validated() {
    let home = Home::new();
    let cfg = load();
    assert_eq!(
        cfg.set_model("qa", RunnerKind::Generic, Some("x"))
            .unwrap_err(),
        AgentError::NotForRunner {
            runner: RunnerKind::Generic,
            setting: "model",
        }
    );
    assert_eq!(
        cfg.set_reasoning_effort("qa", RunnerKind::Generic, Some(ReasoningEffort::Low))
            .unwrap_err()
            .to_string(),
        "generic runners take no reasoning effort Shelbi can set"
    );
    cfg.set_model("qa", RunnerKind::Generic, None).unwrap();
    assert_eq!(
        cfg.set_permissions_mode("qa", Some("yolo")).unwrap_err(),
        AgentError::UnknownPermissionMode("yolo".into())
    );
    // Aliases a hand-edited file may use are understood.
    cfg.set_permissions_mode("qa", Some("read-only")).unwrap();
    assert_eq!(
        cfg.set_model("ghost", RunnerKind::Claude, Some("opus"))
            .unwrap_err(),
        AgentError::UnknownAgent("ghost".into())
    );

    // A preferred runner the manifest says the agent can't run.
    home.write(
        "qa/agent.yaml",
        "name: qa\npreferred_runner: claude\nrequires:\n  runner_kinds: [claude]\n",
    );
    let err = load()
        .set_preferred_runner("qa", Some(RunnerKind::Codex))
        .unwrap_err();
    assert!(
        matches!(&err, AgentError::Invalid(m) if m.contains("requires.runner_kinds")),
        "{err:?}"
    );
}

#[test]
fn picker_options_cover_every_runner() {
    let options = runner_options();
    assert_eq!(
        options.iter().map(|o| o.kind).collect::<Vec<_>>(),
        vec![RunnerKind::Claude, RunnerKind::Codex, RunnerKind::Generic]
    );
    assert_eq!(options[0].models, &["opus", "sonnet", "haiku"]);
    assert_eq!(options[0].efforts.len(), 4);
    assert!(options[2].models.is_empty() && options[2].efforts.is_empty());
    for mode in PERMISSION_MODES {
        assert!(shelbi_core::is_known_permission_mode(mode), "{mode}");
    }
}

// ---------------------------------------------------------------------------
// Skills

#[test]
fn add_skill_writes_the_layout_the_runner_mounts() {
    let home = Home::new();
    let cfg = load()
        .add_skill(
            "developer",
            "release-notes",
            "Writes: release notes",
            "Keep it short.",
        )
        .unwrap();
    let skills = cfg.agent("developer").unwrap().skills();
    assert_eq!(
        skills,
        vec![Skill {
            name: "release-notes".into(),
            description: Some("Writes: release notes".into()),
            layout: SkillLayout::Directory,
        }]
    );
    save(&cfg);
    assert_eq!(
        home.read("developer/skills/release-notes/SKILL.md"),
        "---\nname: release-notes\ndescription: 'Writes: release notes'\n---\n\nKeep it short.\n"
    );
    assert_eq!(load().summaries()[1].skills, 1);
}

#[test]
fn skill_edits_are_validated() {
    let home = Home::new();
    home.write(
        "developer/skills/legacy.md",
        "---\ndescription: Old style\n---\n",
    );
    let cfg = load();
    assert_eq!(
        cfg.agent("developer").unwrap().skills(),
        vec![Skill {
            name: "legacy".into(),
            description: Some("Old style".into()),
            layout: SkillLayout::File,
        }]
    );
    for bad in ["", "Caps", "-x", "a_b", "a/b", &"x".repeat(65)] {
        assert!(
            matches!(
                cfg.add_skill("developer", bad, "d", ""),
                Err(AgentError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        cfg.add_skill("developer", "ok", "  ", ""),
        Err(AgentError::Invalid(_))
    ));
    assert_eq!(
        cfg.add_skill("developer", "legacy", "d", "").unwrap_err(),
        AgentError::SkillExists {
            agent: "developer".into(),
            skill: "legacy".into(),
        }
    );
    assert_eq!(
        cfg.remove_skill("developer", "ghost").unwrap_err(),
        AgentError::UnknownSkill {
            agent: "developer".into(),
            skill: "ghost".into(),
        }
    );
    let cfg = cfg.remove_skill("developer", "legacy").unwrap();
    save(&cfg);
    assert!(!home.path("developer/skills/legacy.md").exists());
    assert!(home.path("developer/skills").is_dir());
}

#[test]
fn removing_a_directory_skill_removes_all_its_files() {
    let home = Home::new();
    home.write("review/skills/load-run-detection/scripts/run.sh", "echo\n");
    let cfg = load().remove_skill("review", "load-run-detection").unwrap();
    assert!(cfg.agent("review").unwrap().skills().is_empty());
    save(&cfg);
    assert!(!home.path("review/skills/load-run-detection").exists());
    assert!(home.path("review/skills").is_dir());
}

#[test]
fn import_skill_copies_a_directory_or_a_markdown_file() {
    let home = Home::new();
    let source = home.root.join("source");
    fs::create_dir_all(source.join("lint/refs")).unwrap();
    fs::write(
        source.join("lint/SKILL.md"),
        "---\ndescription: Lints\n---\n",
    )
    .unwrap();
    fs::write(source.join("lint/refs/rules.md"), "rules\n").unwrap();
    fs::write(source.join("tidy.md"), "---\ndescription: Tidies\n---\n").unwrap();
    fs::write(source.join("notes.txt"), "x").unwrap();
    fs::create_dir_all(source.join("empty")).unwrap();

    let cfg = load()
        .import_skill("qa", &source.join("lint"))
        .unwrap()
        .import_skill("qa", &source.join("tidy.md"))
        .unwrap();
    let names: Vec<String> = cfg
        .agent("qa")
        .unwrap()
        .skills()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, strings(&["lint", "tidy"]));
    save(&cfg);
    assert_eq!(home.read("qa/skills/lint/refs/rules.md"), "rules\n");
    assert_eq!(
        home.read("qa/skills/tidy/SKILL.md"),
        "---\ndescription: Tidies\n---\n"
    );

    let cfg = load();
    assert!(matches!(
        cfg.import_skill("qa", &source.join("notes.txt")),
        Err(AgentError::File { .. })
    ));
    assert!(matches!(
        cfg.import_skill("qa", &source.join("empty")),
        Err(AgentError::File { .. })
    ));
    assert!(matches!(
        cfg.import_skill("qa", &source.join("lint")),
        Err(AgentError::SkillExists { .. })
    ));
}

#[test]
fn import_skill_refuses_a_missing_path_or_a_manifest_without_a_description() {
    let home = Home::new();
    let source = home.root.join("source");
    fs::create_dir_all(source.join("bare")).unwrap();
    fs::write(source.join("bare/SKILL.md"), "Just a body.\n").unwrap();
    fs::create_dir_all(source.join("quiet")).unwrap();
    fs::write(source.join("quiet/SKILL.md"), "---\nname: quiet\n---\nBody\n").unwrap();
    fs::write(source.join("broken.md"), "---\ndescription: [unclosed\n---\n").unwrap();
    fs::write(source.join("open.md"), "---\ndescription: never closed\n").unwrap();

    let cfg = load();
    let refused = |rel: &str, says: &str| {
        let err = cfg.import_skill("qa", &source.join(rel)).unwrap_err();
        assert!(matches!(err, AgentError::File { .. }), "{rel}: {err:?}");
        assert!(err.to_string().contains(says), "{rel}: {err}");
    };
    refused("gone", "doesn't exist");
    refused("gone.md", "doesn't exist");
    refused("bare", "no frontmatter");
    refused("quiet", "no `description:`");
    refused("broken.md", "isn't valid YAML");
    refused("open.md", "no closing `---`");
    // Nothing went into the model.
    assert!(!cfg.agent("qa").unwrap().skills().iter().any(|s| s.name == "bare"));
}

#[test]
fn skill_files_and_text_read_the_draft() {
    let home = Home::new();
    let source = home.root.join("source");
    fs::create_dir_all(source.join("lint/refs/deep")).unwrap();
    fs::write(source.join("lint/SKILL.md"), "---\ndescription: Lints\n---\nRun it.\n").unwrap();
    fs::write(source.join("lint/refs/deep/rules.md"), "rules\n").unwrap();
    let cfg = load()
        .import_skill("qa", &source.join("lint"))
        .unwrap()
        .add_skill("qa", "notes", "Takes notes.", "Write them down.")
        .unwrap();
    let qa = cfg.agent("qa").unwrap();
    assert_eq!(
        qa.skill_files("lint"),
        strings(&["skills/lint/SKILL.md", "skills/lint/refs/deep/rules.md"])
    );
    assert_eq!(qa.skill_file("lint").as_deref(), Some("skills/lint/SKILL.md"));
    assert!(qa.skill_text("notes").unwrap().contains("Write them down."));
    assert!(qa.skill_text("missing").is_none());
    // A flat legacy skill.
    home.write("qa/skills/legacy.md", "---\ndescription: Old\n---\n");
    let qa = load();
    let qa = qa.agent("qa").unwrap();
    assert_eq!(qa.skill_file("legacy").as_deref(), Some("skills/legacy.md"));
    assert_eq!(qa.skill_files("legacy"), strings(&["skills/legacy.md"]));
}

#[test]
fn the_project_overrides_read_from_the_project_yaml() {
    let _home = Home::new();
    let cfg = load();
    assert_eq!(cfg.project_runner_pin("developer"), None);
    assert_eq!(cfg.project_model(RunnerKind::Claude), None);
    assert_eq!(cfg.project_permissions_ceiling(), None);

    let projects = crate::projects_dir().unwrap();
    fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "{PROJECT_YAML}agents:\n  developer:\n    runner: codex\n  qa: {{}}\n\
             runners:\n  codex:\n    command: codex\n    model: gpt-5.5\n    reasoning_effort: high\n\
             workspace_permissions_mode: acceptEdits\n"
        ),
    )
    .unwrap();
    let cfg = load();
    assert_eq!(cfg.project_runner_pin("developer"), Some(RunnerKind::Codex));
    assert_eq!(cfg.project_runner_pin("qa"), None, "a block without a runner pins none");
    assert_eq!(cfg.project_model(RunnerKind::Codex), Some("gpt-5.5"));
    assert_eq!(cfg.project_reasoning_effort(RunnerKind::Codex), Some(ReasoningEffort::High));
    assert_eq!(cfg.project_model(RunnerKind::Claude), None);
    assert_eq!(cfg.project_permissions_ceiling(), Some("acceptEdits"));
}

// ---------------------------------------------------------------------------
// Reset

#[test]
fn reset_restores_the_shipped_files_but_not_settings() {
    let home = Home::new();
    let pristine = home.files();
    home.write("review/settings.json", "{}\n");
    home.write("review/instructions.md", "# Mine\n");
    home.write(
        "review/agent.yaml",
        "# Mine\nname: review\npermissions_mode: plan\n",
    );
    home.write("review/skills/extra/SKILL.md", "---\ndescription: x\n---\n");
    home.write("review/skills/load-run-detection/SKILL.md", "mine\n");
    let cfg = load();
    assert!(cfg.can_reset("review"));
    let cfg = cfg.reset_to_default("review").unwrap();
    assert_eq!(cfg.provenance("review"), Provenance::Shipped);
    save(&cfg);
    let mut now = home.files();
    assert_eq!(now.remove("review/settings.json").unwrap(), b"{}\n");
    let mut was = pristine;
    was.remove("review/settings.json");
    assert_eq!(now, was);

    // An edit after a reset patches the shipped text.
    home.write("qa/agent.yaml", "name: qa\n");
    let cfg = load()
        .reset_to_default("qa")
        .unwrap()
        .set_permissions_mode("qa", Some("plan"))
        .unwrap();
    save(&cfg);
    let shipped = bundled_agent_manifest_yaml(bundled("qa").unwrap());
    let (removed, added) = line_diff(&shipped, &home.read("qa/agent.yaml"));
    assert!(removed.is_empty());
    assert_eq!(added, strings(&["permissions_mode: plan"]));
}

#[test]
fn only_shipped_agents_reset() {
    let home = Home::new();
    home.user_agent("docs");
    let cfg = load();
    assert!(!cfg.can_reset("docs"));
    assert_eq!(
        cfg.reset_to_default("docs").unwrap_err(),
        AgentError::NotShipped("docs".into())
    );
}

// ---------------------------------------------------------------------------
// Save

#[test]
fn a_save_refuses_when_the_agent_changed_on_disk() {
    let home = Home::new();
    let cfg = load().set_description("qa", Some("Mine")).unwrap();
    home.write("qa/skills/new/SKILL.md", "---\ndescription: x\n---\n");
    let manifest = home.read("qa/agent.yaml");
    assert_eq!(
        cfg.save().unwrap_err(),
        AgentError::Conflict(vec![home.path("qa")])
    );
    assert_eq!(home.read("qa/agent.yaml"), manifest);

    // Other agents changing doesn't matter.
    let cfg = load().set_description("qa", Some("Mine")).unwrap();
    home.write("developer/instructions.md", "# Changed\n");
    save(&cfg);

    // The preamble too.
    let cfg = load().set_shared_instructions("Mine\n").unwrap();
    home.write("_shared/preamble.md", "Theirs\n");
    assert_eq!(
        cfg.save().unwrap_err(),
        AgentError::Conflict(vec![home.path("_shared/preamble.md")])
    );
    assert_eq!(home.read("_shared/preamble.md"), "Theirs\n");
}

#[test]
fn a_failed_check_writes_nothing() {
    let home = Home::new();
    home.user_agent("docs");
    let before = home.files();
    let cfg = load()
        .create_agent("fresh", None)
        .unwrap()
        .set_description("qa", Some("Mine"))
        .unwrap()
        .delete_agent("docs")
        .unwrap();
    home.write("developer/instructions.md", "# untouched by the save\n");
    let before_developer = home.read("developer/instructions.md");
    home.write("qa/instructions.md", "# Theirs\n");
    assert!(matches!(cfg.save(), Err(AgentError::Conflict(_))));
    assert!(!home.path("fresh").exists());
    assert!(home.path("docs").exists());
    assert_eq!(home.read("developer/instructions.md"), before_developer);
    assert_eq!(
        home.read("docs/agent.yaml"),
        String::from_utf8_lossy(&before["docs/agent.yaml"])
    );
}
