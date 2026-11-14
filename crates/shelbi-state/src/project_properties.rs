//! The project YAML's own settings, as Settings ▸ Properties edits them: the
//! human label (`name:`), the repository (`github_url:`), the default branch
//! and the two poll intervals.
//!
//! The values travel inside [`WorkflowConfig`](crate::workflow_config::WorkflowConfig),
//! which already reads and writes the project YAML (`default_workflow:`,
//! `orchestrator.awareness:`) through one hash-checked, comment-preserving
//! plan, on this machine or a far one. This module is the toolkit-free half:
//! reading [`Properties`] out of the YAML trees, checking them
//! ([`check`]), and patching only the keys that changed into the trees
//! ([`apply`]) for [`yaml_edit::patch`](crate::yaml_edit::patch) to write.
//!
//! Every one of them is a shared key ([`shelbi_core::SHARED_PROJECT_FIELDS`]),
//! so it lives in the project YAML: `projects/<id>.yaml` for a global
//! project, `<repo>/.shelbi/project.yaml` for an in-repo one. The page leaves
//! the user-local keys ([`shelbi_core::LOCAL_PROJECT_FIELDS`]) alone, the
//! project's `editor:` among them: nothing in Shelbi reads that key (an
//! editor opens with Settings ▸ General's), so the page doesn't offer it,
//! and a save keeps whatever the file has.
//!
//! ## Decisions
//!
//! - The label is `name:`. A project still on the deprecated `display_name:`
//!   shows that one (it wins at load); renaming writes `name:` and drops
//!   `display_name:`, so the file ends on the current spelling.
//! - An empty label or repository removes the key: the label falls back to
//!   the id.
//! - Only keys whose value changed are written, so a project that leans on a
//!   default (no `workspace_poll_interval_secs:`) doesn't grow the key when
//!   something else is saved.

use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig};
use yaml_serde::{Mapping, Value};

/// The default `default_branch:` ([`shelbi_core::Project`]'s).
pub const DEFAULT_BRANCH: &str = "main";
/// The default `workspace_poll_interval_secs:`.
pub const DEFAULT_POLL_SECS: u64 = 5;
/// The default `github_reconcile_interval_secs:`.
pub const DEFAULT_RECONCILE_SECS: u64 = 900;
/// The fastest the orchestrator may poll its workspaces.
pub const MIN_POLL_SECS: u64 = 1;
/// The slowest poll the page accepts: an hour.
pub const MAX_POLL_SECS: u64 = 3_600;
/// The fastest GitHub reconcile pass (it shells out to `gh` per in-review
/// task); `0` turns the pass off.
pub const MIN_RECONCILE_SECS: u64 = 60;
/// The slowest reconcile pass the page accepts: a day.
pub const MAX_RECONCILE_SECS: u64 = 86_400;

/// The editable settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Properties {
    /// `name:` (or the legacy `display_name:`); `None` shows the id.
    pub label: Option<String>,
    /// `github_url:`.
    pub github_url: Option<String>,
    pub default_branch: String,
    pub workspace_poll_interval_secs: u64,
    pub github_reconcile_interval_secs: u64,
}

impl Default for Properties {
    fn default() -> Self {
        Properties {
            label: None,
            github_url: None,
            default_branch: DEFAULT_BRANCH.into(),
            workspace_poll_interval_secs: DEFAULT_POLL_SECS,
            github_reconcile_interval_secs: DEFAULT_RECONCILE_SECS,
        }
    }
}

/// One of [`Properties`]' fields, to say which one a [`check`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Label,
    GithubUrl,
    DefaultBranch,
    DefaultWorkflow,
    PollInterval,
    ReconcileInterval,
}

/// A refused value: which field, and why, for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub field: Field,
    pub message: String,
}

/// The project's issue tracker, as the page summarizes it (it's changed
/// elsewhere).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueTracker {
    pub backend: IssueTrackerBackend,
    /// What the backend's connection names (`owner/repo`, a Jira project
    /// key, a Linear team), when it names one.
    pub target: Option<String>,
}

impl IssueTracker {
    /// `GitHub · jlong/shelbi`, `Files on disk`.
    pub fn summary(&self) -> String {
        let kind = match self.backend {
            IssueTrackerBackend::FileSystem => "Files on disk",
            IssueTrackerBackend::Github => "GitHub",
            IssueTrackerBackend::Jira => "Jira",
            IssueTrackerBackend::Linear => "Linear",
        };
        match &self.target {
            Some(t) => format!("{kind} · {t}"),
            None => kind.to_string(),
        }
    }
}

/// Read the settings out of the project YAML (`shared`). A key that's
/// missing or of the wrong type reads as its default.
pub fn read(shared: &Value) -> Properties {
    let text = |key: &str| shared.get(key).and_then(Value::as_str).map(str::to_string);
    let secs = |key: &str, default: u64| shared.get(key).and_then(Value::as_u64).unwrap_or(default);
    Properties {
        label: text("display_name").or_else(|| text("name")),
        github_url: text("github_url"),
        default_branch: text("default_branch").unwrap_or_else(|| DEFAULT_BRANCH.into()),
        workspace_poll_interval_secs: secs("workspace_poll_interval_secs", DEFAULT_POLL_SECS),
        github_reconcile_interval_secs: secs("github_reconcile_interval_secs", DEFAULT_RECONCILE_SECS),
    }
}

/// The project's issue tracker, from the project YAML.
pub fn issue_tracker(shared: &Value) -> IssueTracker {
    let config: IssueTrackerConfig = shared
        .get("issue_tracker")
        .and_then(|v| yaml_serde::from_value(v.clone()).ok())
        .unwrap_or_default();
    let target = match config.backend {
        IssueTrackerBackend::FileSystem => None,
        IssueTrackerBackend::Github => config.github.map(|g| g.repo),
        IssueTrackerBackend::Jira => config.jira.map(|j| j.project),
        IssueTrackerBackend::Linear => config.linear.map(|l| l.team),
    }
    .filter(|t| !t.trim().is_empty());
    IssueTracker {
        backend: config.backend,
        target,
    }
}

/// Every value [`apply`] would refuse, field by field. Empty when they can
/// be saved.
pub fn check(p: &Properties) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut refuse = |field, message: String| out.push(Problem { field, message });
    let one_line = |s: &Option<String>| s.as_deref().is_some_and(|s| s.contains(['\n', '\r']));
    if one_line(&p.label) {
        refuse(Field::Label, "The name must be one line.".into());
    }
    if let Some(url) = &p.github_url {
        if url.chars().any(char::is_whitespace) {
            refuse(Field::GithubUrl, "The repository URL can't contain spaces.".into());
        }
    }
    if let Err(e) = check_branch(&p.default_branch) {
        refuse(Field::DefaultBranch, e);
    }
    let poll = p.workspace_poll_interval_secs;
    if !(MIN_POLL_SECS..=MAX_POLL_SECS).contains(&poll) {
        refuse(
            Field::PollInterval,
            format!("Use {MIN_POLL_SECS} to {MAX_POLL_SECS} seconds."),
        );
    }
    let reconcile = p.github_reconcile_interval_secs;
    if reconcile != 0 && !(MIN_RECONCILE_SECS..=MAX_RECONCILE_SECS).contains(&reconcile) {
        refuse(
            Field::ReconcileInterval,
            format!("Use {MIN_RECONCILE_SECS} to {MAX_RECONCILE_SECS} seconds, or 0 to turn it off."),
        );
    }
    out
}

/// Whether git would take `name` as a branch name (the parts of
/// `git check-ref-format --branch` a typo trips).
fn check_branch(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name the default branch.".into());
    }
    let bad = name.starts_with(['-', '/', '.'])
        || name.ends_with(['/', '.'])
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    if bad {
        return Err(format!("`{name}` isn't a valid branch name."));
    }
    Ok(())
}

/// Write the fields that differ between `before` (as loaded) and `after`
/// into `shared`, the project YAML's tree. Every other key, `editor:` among
/// them, is left as it is. Returns whether the tree changed.
pub fn apply(shared: &mut Mapping, before: &Properties, after: &Properties) -> bool {
    let mut shared_changed = false;
    let set = |map: &mut Mapping, key: &str, value: Option<Value>| match value {
        Some(v) => {
            map.insert(key.into(), v);
        }
        None => {
            map.remove(key);
        }
    };
    if before.label != after.label {
        shared.remove("display_name");
        set(shared, "name", after.label.clone().map(Value::from));
        shared_changed = true;
    }
    if before.github_url != after.github_url {
        set(shared, "github_url", after.github_url.clone().map(Value::from));
        shared_changed = true;
    }
    if before.default_branch != after.default_branch {
        set(shared, "default_branch", Some(after.default_branch.clone().into()));
        shared_changed = true;
    }
    if before.workspace_poll_interval_secs != after.workspace_poll_interval_secs {
        set(
            shared,
            "workspace_poll_interval_secs",
            Some(after.workspace_poll_interval_secs.into()),
        );
        shared_changed = true;
    }
    if before.github_reconcile_interval_secs != after.github_reconcile_interval_secs {
        set(
            shared,
            "github_reconcile_interval_secs",
            Some(after.github_reconcile_interval_secs.into()),
        );
        shared_changed = true;
    }
    shared_changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(text: &str) -> Value {
        crate::yaml_edit::parse(text).unwrap()
    }

    #[test]
    fn missing_keys_read_as_their_defaults() {
        let p = read(&yaml("repo: /tmp/x\n"));
        assert_eq!(p, Properties::default());
        let p = read(
            &yaml(
                "name: Shelbi\ndisplay_name: Old\ngithub_url: git@github.com:a/b.git\n\
                 default_branch: trunk\neditor: zed\nworkspace_poll_interval_secs: 9\n\
                 github_reconcile_interval_secs: 0\n",
            ),
        );
        assert_eq!(p.label.as_deref(), Some("Old"), "the legacy key wins, as at load");
        assert_eq!(p.github_url.as_deref(), Some("git@github.com:a/b.git"));
        assert_eq!(p.default_branch, "trunk");
        assert_eq!((p.workspace_poll_interval_secs, p.github_reconcile_interval_secs), (9, 0));
    }

    #[test]
    fn the_issue_tracker_summarizes_its_backend_and_target() {
        assert_eq!(issue_tracker(&yaml("repo: x\n")).summary(), "Files on disk");
        let gh = issue_tracker(&yaml("issue_tracker:\n  backend: github\n  github:\n    repo: jlong/shelbi\n"));
        assert_eq!(gh.summary(), "GitHub · jlong/shelbi");
        let linear = issue_tracker(&yaml("issue_tracker:\n  backend: linear\n"));
        assert_eq!(linear.summary(), "Linear");
    }

    #[test]
    fn check_refuses_each_bad_field() {
        assert!(check(&Properties::default()).is_empty());
        let bad = Properties {
            label: Some("a\nb".into()),
            github_url: Some("git@x .git".into()),
            default_branch: "feature..x".into(),
            workspace_poll_interval_secs: 0,
            github_reconcile_interval_secs: 30,
        };
        let fields: Vec<Field> = check(&bad).iter().map(|p| p.field).collect();
        assert_eq!(
            fields,
            [
                Field::Label,
                Field::GithubUrl,
                Field::DefaultBranch,
                Field::PollInterval,
                Field::ReconcileInterval
            ]
        );
        for ok in ["main", "release/1.2", "trunk_2"] {
            assert!(check_branch(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "a b", "x.lock", "a/", "a~1", "a//b"] {
            assert!(check_branch(bad).is_err(), "{bad}");
        }
        let off = Properties {
            github_reconcile_interval_secs: 0,
            ..Properties::default()
        };
        assert!(check(&off).is_empty(), "0 turns the reconcile pass off");
    }

    #[test]
    fn apply_writes_only_what_changed_and_keeps_the_editor() {
        let before = read(&yaml("display_name: Old\neditor: vim\n"));
        let mut shared = yaml("display_name: Old\neditor: vim\n").as_mapping().unwrap().clone();
        let after = Properties {
            label: Some("New".into()),
            ..before.clone()
        };
        assert!(apply(&mut shared, &before, &after));
        assert_eq!(
            Value::Mapping(shared),
            yaml("editor: vim\nname: New\n"),
            "display_name gives way to name; editor: is left alone"
        );

        // Nothing changed: nothing is written, defaults included.
        let mut shared = Mapping::new();
        assert!(!apply(&mut shared, &Properties::default(), &Properties::default()));
        assert!(shared.is_empty(), "defaults aren't written");
    }
}
