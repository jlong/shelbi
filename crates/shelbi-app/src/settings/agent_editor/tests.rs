//! The agent editor's state against real files (moved here from the
//! desktop app's `agent_editor_state` tests with the state, by
//! `tui-agent-editor`), over the shared Agents fixture: each test in its own
//! throwaway `SHELBI_HOME`, under the crate's one lock.

use super::*;
use crate::settings::agents::fixture::Home;

const PROJECT: &str = "my-project";

fn home(tag: &str) -> Home {
    let lock = crate::test_env::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Home::new(&format!("ed-{tag}"), PROJECT, lock)
}

fn editor(name: &str) -> AgentEditorState {
    AgentEditorState::new(AgentsConfig::load(PROJECT).unwrap(), name)
}

/// Save as a client does, here rather than off the UI thread.
fn save(state: &mut AgentEditorState, texts: Vec<Change>) -> Option<Leave> {
    let job = state.begin_save(texts)?;
    state.saved(run_save(job))
}

fn on_disk(name: &str) -> Agent {
    AgentsConfig::load(PROJECT).unwrap().agent(name).unwrap().clone()
}

fn agent_file(home: &Home, agent: &str, file: &str) -> PathBuf {
    home.agents().join(agent).join(file)
}

/// A user agent `name`, saved.
fn user_agent(name: &str) -> AgentEditorState {
    let config = AgentsConfig::load(PROJECT).unwrap().create_agent(name, None).unwrap().save().unwrap().config;
    AgentEditorState::new(config, name)
}

fn set_project_yaml(home: &Home, extra: &str) {
    let path = home.project_dir().with_extension("yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}{extra}")).unwrap();
}

#[test]
fn edits_stay_in_the_draft_until_save_and_discard_reverts_them() {
    let home = home("draft");
    let yaml = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
    let mut state = editor("developer");
    assert_eq!(state.provenance(), Provenance::Shipped);
    assert_eq!(state.badge(), Some("Default"));
    assert!(!state.is_dirty());

    assert!(state.change(Change::Description("Ships the feature.".into())));
    assert!(state.is_dirty());
    assert_eq!(std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap(), yaml);

    assert!(!state.discard());
    assert!(!state.is_dirty());
    assert_eq!(state.agent().description(), on_disk("developer").description());

    // Typed into the field, then saved: written, comments kept.
    save(&mut state, vec![Change::Description("Ships the feature.".into())]);
    assert!(!state.is_dirty());
    assert_eq!(on_disk("developer").description(), Some("Ships the feature."));
    assert_eq!(state.provenance(), Provenance::Customized, "the badge follows the save");
    assert_eq!(state.badge(), Some("Customized"));
    let text = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
    let comments = |t: &str| t.lines().filter(|l| l.trim_start().starts_with('#')).count();
    assert_eq!(comments(&text), comments(&yaml), "{text}");
}

#[test]
fn instructions_save_as_markdown() {
    let home = home("instructions");
    let mut state = editor("developer");
    let text = "# Developer\n\nShip it.\n";
    save(&mut state, vec![Change::Instructions(text.into())]);
    assert_eq!(std::fs::read_to_string(agent_file(&home, "developer", "instructions.md")).unwrap(), text);
    // The same text again is nothing to save.
    assert!(state.begin_save(vec![Change::Instructions(text.into())]).is_none());
}

#[test]
fn type_texts_makes_only_the_changes_that_differ() {
    let _home = home("type-texts");
    let mut state = editor("developer");
    let same = vec![
        Change::Description(state.agent().description().unwrap_or("").to_string()),
        Change::Instructions(state.agent().instructions().to_string()),
    ];
    state.type_texts(same);
    assert!(!state.is_dirty(), "the client filling its fields changes nothing");
    state.type_texts(vec![Change::Instructions("New.\n".into())]);
    assert!(state.is_dirty());
    assert_eq!(state.agent().instructions(), "New.\n");
}

#[test]
fn runner_preferences_map_default_choices_to_absent_keys() {
    let home = home("runner");
    let mut state = editor("developer");
    assert_eq!(state.runner(), RunnerKind::Claude);
    assert!(state.models().iter().any(|m| m == "opus"));

    assert!(state.change(Change::Model(Some("opus".into()))));
    assert!(state.change(Change::Effort(Some(ReasoningEffort::High))));
    assert!(state.change(Change::Permissions(Some("acceptEdits".into()))));
    save(&mut state, vec![]);
    let a = on_disk("developer");
    assert_eq!(a.model(RunnerKind::Claude), Some("opus"));
    assert_eq!(a.reasoning_effort(RunnerKind::Claude), Some(ReasoningEffort::High));
    assert_eq!(a.permissions_mode(), Some("acceptEdits"));

    // The defaults remove the keys.
    assert!(state.change(Change::Model(None)));
    assert!(state.change(Change::Effort(None)));
    assert!(state.change(Change::Permissions(None)));
    assert!(state.change(Change::PreferredRunner(None)));
    save(&mut state, vec![]);
    let text = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
    for key in ["model:", "reasoning_effort:", "permissions_mode:", "preferred_runner:"] {
        assert!(!text.lines().any(|l| l.trim_start().starts_with(key)), "{key} in {text}");
    }
    assert_eq!(state.runner(), RunnerKind::Claude, "Claude Code applies without one");
    assert_eq!(state.runner_in_effect(), (RunnerKind::Claude, Source::Default));

    // Codex: its own model list; generic takes neither, so it's refused.
    assert!(state.change(Change::PreferredRunner(Some(RunnerKind::Codex))));
    assert_eq!(state.runner_in_effect(), (RunnerKind::Codex, Source::Agent));
    assert!(state.models().iter().any(|m| m.starts_with("gpt")));
    assert!(state.change(Change::PreferredRunner(Some(RunnerKind::Generic))));
    assert!(!state.runner_takes_options());
    assert!(state.options_blocked().unwrap().contains("Generic runners take no model"));
    assert!(state.model_choices().is_empty() && state.effort_choices().is_empty());
    assert!(!state.change(Change::Model(Some("opus".into()))));
    assert!(!state.change(Change::Model(Some("my-own-model".into()))), "nor a custom one");
    assert!(state.error_at(&Field::Model).is_some());
    assert!(!state.change(Change::Effort(Some(ReasoningEffort::Low))));
}

#[test]
fn a_custom_model_name_is_kept_as_typed_and_stays_a_choice() {
    let _home = home("custom-model");
    let mut state = editor("developer");
    assert!(state.change(Change::Model(Some("claude-next-preview".into()))));
    save(&mut state, vec![]);
    assert_eq!(on_disk("developer").model(RunnerKind::Claude), Some("claude-next-preview"));
    let choices = state.model_choices();
    let custom = choices.iter().find(|c| c.key == "claude-next-preview").expect("listed");
    assert!(custom.current);
    assert_eq!(choices[0].label, RUNNER_DEFAULT);
    assert_eq!(state.model_in_effect(), Some(("claude-next-preview".into(), Source::Agent)));
}

#[test]
fn an_unknown_model_or_mode_on_disk_stays_a_choice() {
    let home = home("own-model");
    let path = agent_file(&home, "developer", "agent.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}permissions_mode: someday\nrunners:\n  claude:\n    model: claude-x-1\n")).unwrap();
    let state = editor("developer");
    assert_eq!(state.model(), Some("claude-x-1"));
    assert_eq!(state.models().last().map(String::as_str), Some("claude-x-1"));
    assert_eq!(state.permission_modes().last().map(String::as_str), Some("someday"));
    let modes = state.permission_choices();
    assert_eq!(modes[0].label, PROJECT_SETTINGS);
    assert!(modes.iter().any(|c| c.key == "someday" && c.current));
}

#[test]
fn the_choices_mark_what_the_draft_holds() {
    let _home = home("choices");
    let mut state = editor("developer");
    let runners = state.runner_choices();
    assert_eq!(runners[0].label, PROJECT_DEFAULT);
    assert_eq!(runners.iter().filter(|c| c.current).count(), 1);
    state.change(Change::PreferredRunner(Some(RunnerKind::Codex)));
    assert!(state.runner_choices().iter().any(|c| c.key == "codex" && c.current));
    let efforts = state.effort_choices();
    assert_eq!(efforts[0].change, Change::Effort(None));
    assert!(efforts[0].current);
}

#[test]
fn the_project_settings_in_effect_are_shown_with_their_source() {
    let home = home("in-effect");
    set_project_yaml(
        &home,
        "agents:\n  developer:\n    runner: codex\nrunners:\n  codex:\n    command: codex\n    model: gpt-house\n\
         workspace_permissions_mode: acceptEdits\n",
    );
    let mut state = editor("developer");
    // The pin wins over the preference, and Model applies to the pinned runner.
    assert!(state.change(Change::PreferredRunner(Some(RunnerKind::Claude))));
    assert_eq!(state.runner_in_effect(), (RunnerKind::Codex, Source::Project));
    assert!(state.runner_note().contains("agents.developer.runner"), "{}", state.runner_note());
    assert!(state.runner_footnote().starts_with("The project pins this agent to Codex."));
    assert!(state.change(Change::Model(Some("gpt-5.5".into()))));
    assert_eq!(state.agent().model(RunnerKind::Codex), Some("gpt-5.5"));
    // ... but the project's model for every Codex agent wins over it.
    assert_eq!(state.model_in_effect(), Some(("gpt-house".into(), Source::Project)));
    assert!(state.model_note().contains("gpt-house"), "{}", state.model_note());
    assert_eq!(state.effort_in_effect(), None);
    assert!(state.effort_note().contains("default effort"));

    // No request: the ceiling applies; a lower one stands; a higher one is lowered.
    assert_eq!(state.permissions_in_effect(), Some(("acceptEdits".into(), Source::Project)));
    assert!(state.change(Change::Permissions(Some("plan".into()))));
    assert_eq!(state.permissions_in_effect(), Some(("plan".into(), Source::Agent)));
    assert!(state.change(Change::Permissions(Some("bypassPermissions".into()))));
    assert_eq!(state.permissions_in_effect(), Some(("acceptEdits".into(), Source::Project)));
    assert!(state.permissions_note().contains("at most Accept edits"), "{}", state.permissions_note());

    // Without project settings: the defaults (a user agent prefers none).
    let other = user_agent("docs");
    assert_eq!(other.runner_in_effect(), (RunnerKind::Claude, Source::Default));
    assert!(other.model_note().contains("own default model"), "{}", other.model_note());
}

#[test]
fn skills_add_import_inspect_and_remove() {
    let home = home("skills");
    let mut state = user_agent("docs");
    assert!(state.skills().is_empty());

    // A skill needs a description.
    assert!(!state.change(Change::AddSkill { name: "release".into(), description: " ".into(), body: String::new() }));
    assert!(state.error_at(&Field::NewSkill).is_some());
    assert!(state.change(Change::AddSkill {
        name: "release".into(),
        description: "Cut a release.".into(),
        body: "Run the release script.".into(),
    }));
    assert!(state.error_at(&Field::NewSkill).is_none());
    let release = state.skill("release").unwrap();
    assert!(release.unsaved && release.text.contains("Run the release script."));
    assert_eq!(release.file, "skills/release/SKILL.md");

    // An existing skill folder imports whole, its structure kept.
    let src = home.root.join("import").join("triage");
    std::fs::create_dir_all(src.join("refs/deep")).unwrap();
    std::fs::write(src.join("SKILL.md"), "---\nname: triage\ndescription: Sort issues.\n---\n").unwrap();
    std::fs::write(src.join("refs/deep/labels.md"), "bug, chore\n").unwrap();
    assert!(state.change(Change::ImportSkill(src.clone())));
    let names: Vec<String> = state.skills().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["release", "triage"]);
    assert_eq!(state.skill("triage").unwrap().files, ["skills/triage/SKILL.md", "skills/triage/refs/deep/labels.md"]);

    // A missing path and a manifest without a description show at the form.
    assert!(!state.change(Change::ImportSkill(home.root.join("nowhere"))));
    assert!(state.error_at(&Field::NewSkill).unwrap().contains("doesn't exist"));
    let bad = home.root.join("import").join("bad.md");
    std::fs::write(&bad, "No frontmatter here.\n").unwrap();
    assert!(!state.change(Change::ImportSkill(bad)));
    assert!(state.error_at(&Field::NewSkill).unwrap().contains("frontmatter"));
    // Importing the same skill twice is refused too.
    assert!(!state.change(Change::ImportSkill(src)));

    save(&mut state, vec![]);
    let text = std::fs::read_to_string(agent_file(&home, "docs", "skills/release/SKILL.md")).unwrap();
    assert!(text.contains("description: Cut a release.") && text.contains("Run the release script."), "{text}");
    assert_eq!(
        std::fs::read_to_string(agent_file(&home, "docs", "skills/triage/refs/deep/labels.md")).unwrap(),
        "bug, chore\n"
    );
    assert!(!state.skill("release").unwrap().unsaved);
    assert_eq!(state.file(&AgentFile::Skill("release".into())), agent_file(&home, "docs", "skills/release/SKILL.md"));

    // Remove drops the files at Save.
    assert!(state.change(Change::RemoveSkill("triage".into())));
    assert!(agent_file(&home, "docs", "skills/triage/SKILL.md").exists());
    save(&mut state, vec![]);
    assert!(!agent_file(&home, "docs", "skills/triage/SKILL.md").exists());
    assert!(!agent_file(&home, "docs", "skills/triage/refs/deep/labels.md").exists());
    assert!(!state.change(Change::RemoveSkill("triage".into())), "already gone");
    assert!(state.error_at(&Field::Skill("triage".into())).is_some());
}

#[test]
fn reset_puts_the_shipped_files_in_the_draft() {
    let _home = home("reset");
    let mut state = editor("orchestrator");
    assert_eq!(state.provenance(), Provenance::Customized);
    assert!(state.reset_blocked().is_none());
    assert!(state.change(Change::Reset));
    assert!(state.is_dirty());
    assert_eq!(state.reset_blocked().as_deref(), Some("Already the shipped default."));
    // Discard puts the customized files back.
    state.discard();
    assert_eq!(state.provenance(), Provenance::Customized);
    assert!(state.change(Change::Reset));
    save(&mut state, vec![]);
    assert_eq!(state.provenance(), Provenance::Shipped);

    // A user's own agent has nothing to reset to.
    let config = AgentsConfig::load(PROJECT).unwrap().create_agent("docs", None).unwrap();
    let state = AgentEditorState::new(config, "docs");
    assert!(state.reset_blocked().is_some());
    assert_eq!(state.provenance(), Provenance::UserCreated);
    assert_eq!(state.badge(), None);
}

#[test]
fn a_rename_waits_for_save_and_shipped_or_assigned_agents_refuse_it() {
    let home = home("rename");
    // Shipped: refused at Save, at the field; nothing moved.
    let mut state = editor("qa");
    assert!(state.rename_blocked().is_some());
    state.type_name("tester");
    assert!(state.is_dirty());
    assert!(save(&mut state, vec![]).is_none());
    assert!(state.error_at(&Field::Name).is_some());
    assert!(agent_file(&home, "qa", "agent.yaml").exists());

    // The user's own: renamed at Save.
    let mut state = user_agent("docs");
    assert!(state.rename_blocked().is_none());
    state.type_name("writer");
    assert!(!agent_file(&home, "writer", "agent.yaml").exists(), "not before Save");
    save(&mut state, vec![]);
    assert_eq!(state.name(), "writer");
    assert!(agent_file(&home, "writer", "agent.yaml").exists() && !agent_file(&home, "docs", "agent.yaml").exists());

    // Pinned by the project: the guard says why.
    set_project_yaml(&home, "agents:\n  writer:\n    runner: codex\n");
    let state = editor("writer");
    assert!(state.rename_blocked().unwrap().contains("runner pin"), "{:?}", state.rename_blocked());
    assert!(state.delete_blocked().is_some());
}

#[test]
fn leaving_with_edits_asks_first() {
    let _home = home("leave");
    let mut state = editor("developer");
    assert_eq!(state.leave(Leave::Back), Some(Leave::Back), "nothing unsaved");
    state.change(Change::Permissions(Some("plan".into())));
    let workflow = Leave::Workflow { name: "task".into(), status: Some("in-progress".into()) };
    assert_eq!(state.leave(workflow.clone()), None);
    assert_eq!(state.prompt(), Some(&workflow));
    state.cancel_prompt();
    assert!(state.prompt().is_none() && state.is_dirty());

    state.leave(Leave::Open(AgentFile::Instructions));
    assert_eq!(state.discard_and_leave(), Some(Leave::Open(AgentFile::Instructions)));
    assert!(!state.is_dirty());

    state.change(Change::Permissions(Some("plan".into())));
    state.leave(Leave::Workflows);
    let job = state.save_and_leave(vec![]).unwrap();
    assert_eq!(state.saved(run_save(job)), Some(Leave::Workflows));
    assert_eq!(on_disk("developer").permissions_mode(), Some("plan"));
}

#[test]
fn a_file_changed_under_the_save_is_kept_and_discard_reloads() {
    let home = home("stale");
    let mut state = editor("developer");
    state.change(Change::Description("Mine.".into()));
    let path = agent_file(&home, "developer", "instructions.md");
    std::fs::write(&path, "# By hand\n").unwrap();
    assert!(save(&mut state, vec![]).is_none());
    assert!(state.conflict());
    let error = state.error_at(&Field::Save).unwrap().to_string();
    assert!(error.contains("Discard to reload"), "{error}");
    assert!(state.is_dirty(), "the draft is kept");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# By hand\n", "the hand edit stays");
    assert!(state.discard(), "the files must be read again");
    assert!(!state.conflict());
    assert!(state.reloaded(AgentsConfig::load(PROJECT).unwrap()));
    assert_eq!(state.agent().instructions(), "# By hand\n");
}

#[test]
fn a_failed_save_keeps_the_draft_and_says_why() {
    let home = home("failed");
    let mut state = editor("developer");
    state.change(Change::Instructions("Mine.\n".into()));
    // The agent's directory can't be written: a file where it should be.
    let dir = home.agents().join("developer");
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, "not a directory").unwrap();
    assert!(save(&mut state, vec![]).is_none());
    assert!(state.error_at(&Field::Save).is_some());
    assert!(state.is_dirty() && !state.busy());
    assert_eq!(state.agent().instructions(), "Mine.\n");
}

#[test]
fn duplicate_and_delete_run_on_the_saved_model() {
    let home = home("dup");
    let mut state = editor("developer");
    assert_eq!(state.duplicate_name(), "developer-copy");
    assert!(state.delete_blocked().is_some(), "shipped");
    state.ask_delete();
    assert!(!state.confirming_delete(), "a blocked delete asks nothing");

    let base = state.begin_job().unwrap();
    assert!(state.begin_job().is_none(), "one at a time");
    let config = run_duplicate(base, "developer", "developer-copy").unwrap();
    assert!(agent_file(&home, "developer-copy", "instructions.md").exists());

    let mut copy = AgentEditorState::new(config, "developer-copy");
    assert_eq!(copy.duplicate_name(), "developer-copy-copy");
    assert!(copy.delete_blocked().is_none());
    copy.ask_delete();
    assert!(copy.confirming_delete());
    let base = copy.begin_job().unwrap();
    assert!(!copy.confirming_delete());
    run_delete(base, "developer-copy").unwrap();
    assert!(!agent_file(&home, "developer-copy", "agent.yaml").exists());
}

#[test]
fn assignments_list_the_statuses_that_run_the_agent() {
    let _home = home("assign");
    let state = editor("developer");
    let rows: Vec<(String, String)> = state.assignments().into_iter().map(|a| (a.workflow, a.status_name)).collect();
    assert!(rows.contains(&("task".to_string(), "In Progress".to_string())), "{rows:?}");
    assert!(state.rename_blocked().is_some());
    assert!(editor("qa").assignments().is_empty());
}

#[test]
fn files_open_where_they_are_saved() {
    let home = home("files");
    let state = editor("developer");
    assert_eq!(state.manifest_file(), agent_file(&home, "developer", "agent.yaml"));
    assert_eq!(state.file(&AgentFile::Instructions), agent_file(&home, "developer", "instructions.md"));
    assert_eq!(AgentFile::Manifest.label(), "agent.yaml");
}

#[test]
fn an_import_path_expands_home_and_relative_paths() {
    let home = Path::new("/home/me");
    let cwd = Path::new("/work");
    assert_eq!(import_path(" ~/skills/lint ", Some(home), cwd), PathBuf::from("/home/me/skills/lint"));
    assert_eq!(import_path("~", Some(home), cwd), PathBuf::from("/home/me"));
    assert_eq!(import_path("skills/lint", Some(home), cwd), PathBuf::from("/work/skills/lint"));
    assert_eq!(import_path("/abs/lint.md", None, cwd), PathBuf::from("/abs/lint.md"));
}
