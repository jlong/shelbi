//! A project's orchestrator setup: whether its orchestrator has been set up
//! (the Orchestrator view's onboarding shows until it has), and the
//! project-YAML half of choosing the orchestrator's runner.
//!
//! ## Onboarded
//!
//! [`orchestrator_onboarded`] is the skip rule. A project counts as set up
//! when any of these holds:
//!
//! - its `state.json` carries the marker ([`mark_orchestrator_onboarded`],
//!   written when the onboarding's Launch Orchestrator finishes);
//! - its orchestrator has launched before: every launch deploys the
//!   orchestrator's composed instructions into the project directory
//!   (`<project>/.claude/agent-instructions.md`, written by
//!   `shelbi_orchestrator::ensure_dashboard` and nothing else), and an
//!   orchestrator from before that file existed left `<project>/CLAUDE.md`;
//! - the project is open (`state.json` `open: true`, also set only by
//!   `ensure_dashboard`).
//!
//! So every existing project whose orchestrator ever ran skips onboarding
//! with no migration, and a project made since shows it once.
//!
//! ## The orchestrator's runner
//!
//! The orchestrator launches on the `agent_runners` entry the project YAML's
//! `orchestrator.runner` names. [`set_orchestrator_runner`] points it at an
//! entry of the chosen kind, comment-preserving ([`crate::yaml_edit`]): a
//! runner already of that kind is left as it is (its command and flags
//! kept), else an existing entry of the kind is named, else
//! `agent_runners.<kind>: { command: <kind> }` is added. A generic runner has
//! no command Shelbi could invent, so choosing one needs an entry already
//! declared.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};
use shelbi_core::{Project, RunnerKind};

use crate::yaml_edit;

/// The orchestrator's composed instructions, relative to the project
/// directory: deployed there by every orchestrator launch
/// (`shelbi_orchestrator::ORCH_AGENT_INSTRUCTIONS_REL`).
const DEPLOYED_INSTRUCTIONS: &str = ".claude/agent-instructions.md";
/// What an orchestrator launched before the composed instructions existed
/// left in the project directory.
const LEGACY_CLAUDE_MD: &str = "CLAUDE.md";

/// Why the runner couldn't be set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunnerSetupError {
    /// No `agent_runners` entry runs `kind`, and Shelbi can't add one.
    #[error("No {} runner is declared in this project. Add one under agent_runners in the project YAML first.", kind_name(*.0))]
    NotDeclared(RunnerKind),
    /// The project YAML couldn't be read, patched or written.
    #[error("{path}: {message}")]
    File { path: PathBuf, message: String },
}

fn kind_name(kind: RunnerKind) -> &'static str {
    match kind {
        RunnerKind::Claude => "Claude Code",
        RunnerKind::Codex => "Codex",
        RunnerKind::Generic => "generic",
    }
}

/// What [`set_orchestrator_runner`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerSetup {
    /// The `agent_runners` key `orchestrator.runner` now names.
    pub runner: String,
    /// The file changed (`false`: the orchestrator already ran on `kind`).
    pub written: bool,
    /// An `agent_runners` entry was added for the kind.
    pub added: bool,
    /// Comment lines the patch lost.
    pub dropped_comments: Vec<String>,
}

/// Whether `project`'s orchestrator has been set up (see the module docs):
/// the marker, an orchestrator that launched before, or an open project.
/// Unreadable state reads as set up, so a broken `state.json` never puts an
/// existing project back through onboarding.
pub fn orchestrator_onboarded(project: &str) -> bool {
    let state = match crate::read_state(project) {
        Ok(state) => state,
        Err(_) => return true,
    };
    if state.orchestrator_onboarded || state.open {
        return true;
    }
    match crate::project_dir(project) {
        Ok(dir) => dir.join(DEPLOYED_INSTRUCTIONS).is_file() || dir.join(LEGACY_CLAUDE_MD).is_file(),
        Err(_) => true,
    }
}

/// Record that `project`'s orchestrator is set up. Idempotent.
pub fn mark_orchestrator_onboarded(project: &str) -> crate::Result<()> {
    crate::update_state(project, |state| {
        state.orchestrator_onboarded = true;
        Ok(())
    })
}

/// The runner kind `project`'s orchestrator launches on: the kind of the
/// `agent_runners` entry `orchestrator.runner` names (Claude Code when it
/// names none, as the launch would refuse anyway).
pub fn orchestrator_runner_kind(project: &Project) -> RunnerKind {
    project
        .runner(&project.orchestrator.runner)
        .map(RunnerKind::detect)
        .unwrap_or(RunnerKind::Claude)
}

/// Whether some `agent_runners` entry runs `kind`.
pub fn runner_declared(project: &Project, kind: RunnerKind) -> bool {
    project.agent_runners.values().any(|s| RunnerKind::detect(s) == kind)
}

/// Point `project`'s `orchestrator.runner` at a runner of `kind` (see the
/// module docs), comment-preserving. Writes nothing when the orchestrator
/// already runs on `kind`, or when the result wouldn't load.
pub fn set_orchestrator_runner(project: &str, kind: RunnerKind) -> Result<RunnerSetup, RunnerSetupError> {
    let (path, local) = orchestrator_yaml(project)?;
    let text = read(&path)?;
    let parsed = parse(&path, &text, local.as_ref())?;
    if orchestrator_runner_kind(&parsed) == kind {
        return Ok(RunnerSetup {
            runner: parsed.orchestrator.runner,
            written: false,
            added: false,
            dropped_comments: Vec::new(),
        });
    }
    // An entry of the kind: the one under the kind's own name first.
    let existing = parsed
        .agent_runners
        .get(kind.as_str())
        .filter(|s| RunnerKind::detect(s) == kind)
        .map(|_| kind.as_str().to_string())
        .or_else(|| {
            parsed
                .agent_runners
                .iter()
                .find(|(_, s)| RunnerKind::detect(s) == kind)
                .map(|(k, _)| k.clone())
        });
    let (runner, added) = match existing {
        Some(name) => (name, false),
        None if kind == RunnerKind::Generic || parsed.agent_runners.contains_key(kind.as_str()) => {
            return Err(RunnerSetupError::NotDeclared(kind))
        }
        None => (kind.as_str().to_string(), true),
    };

    let raw = yaml_edit::parse(&text).map_err(|e| file_err(&path, e))?;
    let mut target = raw.clone();
    let root = target
        .as_mapping_mut()
        .ok_or_else(|| file_err(&path, "the project YAML isn't a mapping"))?;
    let orchestrator = root
        .entry(Value::from("orchestrator"))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let orchestrator = orchestrator
        .as_mapping_mut()
        .ok_or_else(|| file_err(&path, "`orchestrator` isn't a mapping"))?;
    orchestrator.insert(Value::from("runner"), Value::from(runner.clone()));
    if added {
        let runners = root
            .entry(Value::from("agent_runners"))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        let runners = runners
            .as_mapping_mut()
            .ok_or_else(|| file_err(&path, "`agent_runners` isn't a mapping"))?;
        let mut spec = Mapping::new();
        spec.insert(Value::from("command"), Value::from(kind.as_str()));
        runners.insert(Value::from(runner.clone()), Value::Mapping(spec));
    }
    let patched = yaml_edit::patch(&text, &target).map_err(|e| file_err(&path, e))?;
    // Never write a project that wouldn't load.
    let check = parse(&path, &patched.text, local.as_ref())?;
    if orchestrator_runner_kind(&check) != kind {
        return Err(file_err(&path, "the edit didn't take"));
    }
    crate::atomic_write(&path, patched.text.as_bytes()).map_err(|e| file_err(&path, e))?;
    Ok(RunnerSetup {
        runner,
        written: true,
        added,
        dropped_comments: patched.dropped_comments,
    })
}

/// The file holding `orchestrator:` (the global project YAML, or the
/// committed shared half of an in-repo project) and, in-repo, the local
/// half's text (to validate the merged project).
fn orchestrator_yaml(project: &str) -> Result<(PathBuf, Option<String>), RunnerSetupError> {
    let projects = crate::projects_dir().map_err(|e| file_err(Path::new(project), e))?;
    let local = projects.join(project).join("local.yaml");
    if local.is_file() {
        let local_text = read(&local)?;
        let repo = crate::migrate::extract_repo_from_local_yaml(&local_text, &local).map_err(|e| file_err(&local, e))?;
        let shared = crate::expand_tilde_str(&repo).join(".shelbi").join("project.yaml");
        Ok((shared, Some(local_text)))
    } else {
        Ok((projects.join(format!("{project}.yaml")), None))
    }
}

fn parse(path: &Path, text: &str, local: Option<&String>) -> Result<Project, RunnerSetupError> {
    let project = match local {
        Some(local) => Project::from_split_yaml_str(text, local),
        None => Project::from_yaml_str(text),
    };
    project.map_err(|e| file_err(path, e))
}

fn read(path: &Path) -> Result<String, RunnerSetupError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(file_err(path, "the file doesn't exist")),
        Err(e) => Err(file_err(path, e)),
    }
}

fn file_err(path: &Path, e: impl std::fmt::Display) -> RunnerSetupError {
    RunnerSetupError::File {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    const PROJECT: &str = "orx";

    /// A temp `SHELBI_HOME` with project `orx`'s YAML. Restores nothing:
    /// every test sets its own home under the lock.
    struct Home {
        _lock: std::sync::MutexGuard<'static, ()>,
        yaml: PathBuf,
    }

    impl Home {
        fn new(yaml: &str) -> Home {
            let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = std::env::temp_dir().join(format!(
                "shelbi-orx-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::env::set_var("SHELBI_HOME", &root);
            let projects = crate::projects_dir().unwrap();
            fs::create_dir_all(projects.join(PROJECT)).unwrap();
            let path = projects.join(format!("{PROJECT}.yaml"));
            fs::write(&path, yaml).unwrap();
            Home { _lock: lock, yaml: path }
        }

        fn text(&self) -> String {
            fs::read_to_string(&self.yaml).unwrap()
        }
    }

    const YAML: &str = "\
## The project header.
repo: /tmp/orx
default_branch: main
# Which runner the orchestrator runs on.
orchestrator:
  runner: claude # keep me
agent_runners:
  # The house Claude.
  claude:
    command: claude
    flags: [--verbose]
machines:
  - name: local
    kind: local
    work_dir: /tmp/orx
";

    #[test]
    fn a_new_project_needs_onboarding_and_the_marker_ends_it() {
        let _home = Home::new(YAML);
        assert!(!orchestrator_onboarded(PROJECT));
        mark_orchestrator_onboarded(PROJECT).unwrap();
        assert!(orchestrator_onboarded(PROJECT));
        assert!(crate::read_state(PROJECT).unwrap().orchestrator_onboarded);
    }

    #[test]
    fn an_orchestrator_that_ran_before_counts_as_onboarded() {
        let _home = Home::new(YAML);
        let dir = crate::project_dir(PROJECT).unwrap();
        fs::create_dir_all(dir.join(".claude")).unwrap();
        fs::write(dir.join(DEPLOYED_INSTRUCTIONS), "# composed").unwrap();
        assert!(orchestrator_onboarded(PROJECT));

        fs::remove_file(dir.join(DEPLOYED_INSTRUCTIONS)).unwrap();
        fs::write(dir.join(LEGACY_CLAUDE_MD), "# legacy").unwrap();
        assert!(orchestrator_onboarded(PROJECT), "a pre-composition orchestrator");

        fs::remove_file(dir.join(LEGACY_CLAUDE_MD)).unwrap();
        crate::set_project_open(PROJECT, true).unwrap();
        assert!(orchestrator_onboarded(PROJECT), "an open project");
    }

    #[test]
    fn the_same_kind_keeps_a_custom_runner_untouched() {
        let home = Home::new(&YAML.replace("command: claude", "command: /opt/bin/claude-wrapper\n    integration: claude"));
        let before = home.text();
        let done = set_orchestrator_runner(PROJECT, RunnerKind::Claude).unwrap();
        assert!(!done.written);
        assert_eq!(done.runner, "claude");
        assert_eq!(home.text(), before, "command and flags kept byte for byte");
    }

    #[test]
    fn choosing_codex_adds_a_runner_and_keeps_comments() {
        let home = Home::new(YAML);
        let done = set_orchestrator_runner(PROJECT, RunnerKind::Codex).unwrap();
        assert!(done.written && done.added);
        let text = home.text();
        for comment in ["## The project header.", "# Which runner the orchestrator runs on.", "# The house Claude.", "flags: [--verbose]"] {
            assert!(text.contains(comment), "kept {comment:?}:\n{text}");
        }
        let project = Project::from_yaml_str(&text).unwrap();
        assert_eq!(project.orchestrator.runner, "codex");
        assert_eq!(project.agent_runners["codex"].command, "codex");
        assert_eq!(project.agent_runners["claude"].flags, ["--verbose"]);
        assert_eq!(orchestrator_runner_kind(&project), RunnerKind::Codex);
    }

    #[test]
    fn an_existing_entry_of_the_kind_is_named_not_added() {
        let yaml = YAML.replace(
            "machines:",
            "  my-codex:\n    command: codex\n    flags: [--search]\nmachines:",
        );
        let home = Home::new(&yaml);
        let done = set_orchestrator_runner(PROJECT, RunnerKind::Codex).unwrap();
        assert_eq!(done.runner, "my-codex");
        assert!(!done.added);
        let project = Project::from_yaml_str(&home.text()).unwrap();
        assert_eq!(project.agent_runners.len(), 2);
        assert_eq!(project.agent_runners["my-codex"].flags, ["--search"]);
    }

    #[test]
    fn a_generic_runner_must_be_declared() {
        let home = Home::new(YAML);
        let before = home.text();
        let err = set_orchestrator_runner(PROJECT, RunnerKind::Generic).unwrap_err();
        assert_eq!(err, RunnerSetupError::NotDeclared(RunnerKind::Generic));
        assert_eq!(home.text(), before);
        assert!(!runner_declared(&Project::from_yaml_str(&before).unwrap(), RunnerKind::Generic));
    }
}
