use std::fs;
use std::path::PathBuf;

use super::*;
use crate::settings::agent_editor::RUNNER_DEFAULT;
use crate::settings::agents::fixture::Home;

const PROJECT: &str = "onb";

fn home(tag: &str) -> Home {
    let lock = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Home::new(&format!("onb-{tag}"), PROJECT, lock)
}

fn project_yaml() -> PathBuf {
    shelbi_state::projects_dir().unwrap().join(format!("{PROJECT}.yaml"))
}

fn read(path: &PathBuf) -> String {
    fs::read_to_string(path).unwrap()
}

fn loaded() -> OnboardingState {
    let mut state = OnboardingState::new(PROJECT);
    state.loaded(load(PROJECT));
    assert!(state.is_loaded(), "{:?}", state.model().error);
    state
}

/// Get started and Continue with the current choices, running the save
/// here rather than off the UI thread.
fn to_instructions(state: &mut OnboardingState) {
    state.get_started();
    let job = state.begin_continue().expect("continue");
    state.continued(run_continue(job));
    assert_eq!(state.step(), OnboardingStep::Instructions, "{:?}", state.model().error);
}

#[test]
fn a_project_whose_orchestrator_never_ran_needs_onboarding_until_launch() {
    let home = home("skip");
    assert!(needs_onboarding(PROJECT));
    let mut state = loaded();
    to_instructions(&mut state);
    let job = state.begin_launch().unwrap();
    assert!(state.launched(run_launch(job)));
    assert!(state.finished());
    assert!(!needs_onboarding(PROJECT), "reopening skips onboarding");

    // An existing project whose orchestrator ran (its deployed instructions)
    // skips it with no marker.
    home.add_project("ran");
    let dir = shelbi_state::project_dir("ran").unwrap();
    fs::create_dir_all(dir.join(".claude")).unwrap();
    fs::write(dir.join(".claude/agent-instructions.md"), "# composed").unwrap();
    assert!(!needs_onboarding("ran"));
}

#[test]
fn the_steps_go_forward_and_back() {
    let _home = home("steps");
    let mut state = OnboardingState::new(PROJECT);
    assert_eq!(state.step(), OnboardingStep::Intro);
    // Not loaded yet: step 2 shows, but Continue waits.
    state.get_started();
    assert_eq!(state.step(), OnboardingStep::Harness);
    assert!(state.model().loading);
    assert!(state.begin_continue().is_none());
    state.back();
    assert_eq!(state.step(), OnboardingStep::Intro);

    state.loaded(load(PROJECT));
    to_instructions(&mut state);
    // Launch only from step 3, Continue only from step 2.
    state.back();
    assert_eq!(state.step(), OnboardingStep::Harness);
    assert!(state.begin_launch().is_none());
}

#[test]
fn step_two_starts_on_what_the_orchestrator_runs() {
    let _home = home("defaults");
    let state = loaded();
    let model = state.model();
    let harnesses: Vec<(&str, Option<&str>, bool)> = model
        .harnesses
        .iter()
        .map(|h| (h.label.as_str(), h.provider.as_deref(), h.selected))
        .collect();
    assert_eq!(
        harnesses,
        [("Claude Code", Some("Anthropic"), true), ("Codex", Some("OpenAI"), false), ("Generic", None, false)]
    );
    assert_eq!(model.model_label, RUNNER_DEFAULT);
    let labels: Vec<&str> = model.models.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, [RUNNER_DEFAULT, "Opus", "Sonnet", "Haiku"]);
    assert!(model.model_enabled);
}

#[test]
fn keeping_the_defaults_writes_nothing() {
    let home = home("keep");
    let manifest = home.agents().join("orchestrator/agent.yaml");
    let (yaml, agent) = (read(&project_yaml()), read(&manifest));
    let mut state = loaded();
    to_instructions(&mut state);
    assert_eq!(read(&project_yaml()), yaml);
    assert_eq!(read(&manifest), agent);
}

#[test]
fn continue_saves_the_harness_and_model_with_comments() {
    let home = home("codex");
    let manifest = home.agents().join("orchestrator/agent.yaml");
    let mut state = loaded();
    state.get_started();
    state.select_runner(RunnerKind::Codex);
    let labels: Vec<String> = state.model().models.into_iter().map(|m| m.label).collect();
    assert_eq!(labels, [RUNNER_DEFAULT, "GPT-5", "GPT-5 Codex"]);
    state.select_model(Some("gpt-5"));
    assert_eq!(state.model().model_label, "GPT-5");
    let job = state.begin_continue().unwrap();
    assert!(state.busy());
    state.continued(run_continue(job));
    assert_eq!(state.step(), OnboardingStep::Instructions);

    let agent = read(&manifest);
    assert!(agent.starts_with("## Shelbi agent manifest"), "comments kept:\n{agent}");
    let parsed = shelbi_state::load_agent_manifest(PROJECT, "orchestrator").unwrap().unwrap();
    assert_eq!(parsed.preferred_runner, Some(RunnerKind::Codex));
    assert_eq!(parsed.runners[&RunnerKind::Codex].model.as_deref(), Some("gpt-5"));

    let project = shelbi_state::load_project(PROJECT).unwrap();
    assert_eq!(project.orchestrator.runner, "codex");
    assert_eq!(project.agent_runners["codex"].command, "codex");
    assert_eq!(shelbi_core::orchestrator_model(&project, Some(&parsed)).as_deref(), Some("gpt-5"));

    // Back keeps the choice; a custom model is kept as typed.
    state.back();
    state.select_model(Some("  gpt-5.5-preview "));
    assert_eq!(state.model_choice(), Some("gpt-5.5-preview"));
    assert!(state.model().models.iter().any(|m| m.label == "gpt-5.5-preview" && m.selected));
}

#[test]
fn a_generic_runner_needs_one_declared() {
    let _home = home("generic");
    let mut state = loaded();
    state.get_started();
    state.select_runner(RunnerKind::Generic);
    let model = state.model();
    assert!(!model.model_enabled && model.models.is_empty());
    assert_eq!(model.model_note.as_deref(), Some("Generic runners take no model."));
    assert!(state.begin_continue().is_none());
    assert!(state.model().error.unwrap().contains("No generic runner is declared"));
    assert!(!state.busy());
    // Choosing another runner clears it.
    state.select_runner(RunnerKind::Claude);
    assert_eq!(state.model().error, None);
}

#[test]
fn launch_saves_changed_instructions_and_back_keeps_them() {
    let home = home("launch");
    let file = home.agents().join("orchestrator/instructions.md");
    let mut state = loaded();
    assert_eq!(state.instructions(), read(&file));
    to_instructions(&mut state);
    state.type_instructions("# You are the Orchestrator\n\nShip small changes.\n");
    state.back();
    state.select_model(Some("opus"));
    let job = state.begin_continue().unwrap();
    state.continued(run_continue(job));
    assert_eq!(state.instructions(), "# You are the Orchestrator\n\nShip small changes.\n", "Back kept the edit");
    assert!(!read(&file).contains("Ship small changes."), "nothing written before Launch");

    let job = state.begin_launch().unwrap();
    assert!(state.launched(run_launch(job)));
    assert_eq!(read(&file), "# You are the Orchestrator\n\nShip small changes.\n");
    assert!(shelbi_state::read_state(PROJECT).unwrap().orchestrator_onboarded);
}

#[test]
fn unchanged_instructions_are_not_rewritten() {
    let home = home("same");
    let file = home.agents().join("orchestrator/instructions.md");
    let before = fs::metadata(&file).unwrap().modified().unwrap();
    let mut state = loaded();
    to_instructions(&mut state);
    let job = state.begin_launch().unwrap();
    assert!(job.config.is_none());
    assert!(state.launched(run_launch(job)));
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), before);
}

#[test]
fn a_failed_save_stays_on_its_step_with_the_error() {
    let _home = home("fail");
    let mut state = loaded();
    to_instructions(&mut state);
    let job = state.begin_launch().unwrap();
    assert!(!state.launched(Err("disk full".into())));
    assert_eq!(state.step(), OnboardingStep::Instructions);
    assert_eq!(state.model().error.as_deref(), Some("disk full"));
    assert!(!state.finished());
    drop(job);
}

#[test]
fn model_labels_name_the_suggestions() {
    assert_eq!(model_label("opus"), "Opus");
    assert_eq!(model_label("gpt-5-codex"), "GPT-5 Codex");
    assert_eq!(model_label("claude-opus-4-8"), "claude-opus-4-8");
}
