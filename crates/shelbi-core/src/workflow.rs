//! Workflow definitions: the per-project, YAML-declared status set that
//! supersedes the hardcoded five-column [`crate::Column`] enum.
//!
//! A workflow is the structure described in `Plans/workflows.md`: a named
//! list of statuses, each carrying a [`StatusCategory`] (the semantic
//! vocabulary the rest of the system reasons in) and an [`Owner`] (who is
//! expected to act). Workflows live at
//! `~/.shelbi/projects/<project>/workflows/<name>.yaml` and are loaded
//! through [`Workflow::from_yaml_str`].
//!
//! This module is **only** the schema + validator. Wiring workflows into
//! the orchestrator, TUI, events log, or task frontmatter happens in
//! later phases; this is the foundation those phases rely on.
//!
//! See `Plans/workflows.md` §2 for the canonical schema.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::placeholders::substitute_placeholders;
use crate::{Error, GitConfig, ZenChecks, ZenDangerPaths};

// ---------------------------------------------------------------------------
// Workflow

/// A named workflow: the ordered list of statuses a task moves through,
/// plus the optional rules that constrain those moves. Round-trips through
/// YAML; call [`Workflow::validate`] (or the all-in-one
/// [`Workflow::from_yaml_str`]) before trusting the values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workflow {
    /// Workflow id. Conventionally matches the filename
    /// (`workflows/<name>.yaml`); used in task frontmatter to point a
    /// task at this workflow. Required.
    pub name: String,

    /// Free-form description, surfaced in CLI listings and the workflow
    /// picker. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Ordered list of statuses. Order matters: it's the left-to-right
    /// column order in the Kanban TUI and the implicit default for
    /// [`Workflow::initial_status`] when that field is absent. At least
    /// one status is required.
    pub statuses: Vec<Status>,

    /// Which status a freshly created task lands in. When `None`, the
    /// first status in [`Workflow::statuses`] is used. Must reference a
    /// status declared in this workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_status: Option<String>,

    /// Action-based transition table: each entry declares which side-effect
    /// actions (`push_branch`, `open_pr`, `merge`, `close_pr`,
    /// `delete_branch`, `restack`) fire when a task crosses that edge.
    /// Transitions are **any-to-any** per `Plans/workflows.md` §11 — this
    /// block does *not* restrict which moves are legal, it only declares
    /// the side-effects to run on the edges where work needs to happen.
    /// Edges not listed are pure status moves with no actions. `None` ↔
    /// `Some(vec![])` ↔ no actions on any edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitions: Option<Vec<Transition>>,

    /// Per-workflow override of the project-level `git:` defaults. When
    /// `None`, callers inherit `Project::base_branch` and
    /// `Project::merge_strategy` unchanged. Field values may contain
    /// `{{var}}` placeholders that are resolved against the task's
    /// params at task-load time — see [`Workflow::resolve_git`] and
    /// `Plans/workflows.md` §12 "Parameterization".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitConfig>,

    /// Per-workflow override of project-level Zen Mode config.
    ///
    /// Each subfield is independently optional — a workflow can override
    /// just `checks`, just `ci_timeout`, just `danger_paths`, or any
    /// combination. Unset subfields fall back to the project's
    /// [`crate::ZenConfig`]. Resolution helpers
    /// ([`crate::ci_timeout_for_workflow`], [`crate::danger_paths_for_workflow`],
    /// [`crate::checks_for_task_in_workflow`]) do the merging — call sites
    /// shouldn't reach into this field directly.
    ///
    /// Letting a `research:` workflow opt out of code-style checks
    /// without affecting `default` is the canonical use case
    /// (`Plans/workflows.md` §Decisions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zen: Option<WorkflowZenConfig>,
}

impl Workflow {
    /// Parse YAML and validate in one step. The convenience constructor
    /// callers should reach for; raw [`serde_yaml::from_str`] skips the
    /// semantic checks that catch broken cross-references.
    pub fn from_yaml_str(s: &str) -> crate::Result<Self> {
        let wf: Workflow = serde_yaml::from_str(s)?;
        wf.validate()?;
        Ok(wf)
    }

    /// Resolved initial status — explicit `initial_status` if set,
    /// otherwise the first status in the list. Returns `None` only if
    /// [`Workflow::statuses`] is empty (which a validated workflow never
    /// is).
    pub fn resolved_initial_status(&self) -> Option<&str> {
        if let Some(s) = self.initial_status.as_deref() {
            return Some(s);
        }
        self.statuses.first().map(|s| s.name.as_str())
    }

    /// Look up a status by name. Linear scan — workflows are tiny (<10
    /// statuses in practice) so a hash map isn't worth the allocation.
    pub fn status(&self, name: &str) -> Option<&Status> {
        self.statuses.iter().find(|s| s.name == name)
    }

    /// True iff `from -> to` is a legal status move. Per
    /// `Plans/workflows.md` §11, transitions are any-to-any: both
    /// statuses just have to be declared in this workflow. Listing an
    /// edge in [`Workflow::transitions`] only declares its side-effects;
    /// it does not restrict which moves are legal.
    pub fn transition_allowed(&self, from: &str, to: &str) -> bool {
        self.status(from).is_some() && self.status(to).is_some()
    }

    /// Look up the [`Transition`] entry for `from -> to`, if one is
    /// declared. Returns `None` when no edge is declared (a pure status
    /// move with no side-effects).
    pub fn transition(&self, from: &str, to: &str) -> Option<&Transition> {
        self.transitions
            .as_ref()?
            .iter()
            .find(|t| t.from == from && t.to == to)
    }

    /// Action list to fire when a task moves `from -> to`. Empty when no
    /// edge is declared — that's the explicit "no side-effects" path,
    /// not an error.
    pub fn actions_for_transition(&self, from: &str, to: &str) -> &[TransitionAction] {
        self.transition(from, to)
            .map(|t| t.actions.as_slice())
            .unwrap_or(&[])
    }

    /// True iff the `from -> to` edge declares the `merge` action.
    ///
    /// Zen Mode's confidence bar fires on *any* transition whose actions
    /// include `merge`, regardless of source/target categories — see
    /// `Plans/workflows.md` §8 "The underlying rule is action-based, not
    /// category-pair-based." A trunk-based workflow that skips `Review`
    /// and merges straight from `InProgress -> Done` still trips the
    /// same high-bar probe.
    pub fn is_merge_transition(&self, from: &str, to: &str) -> bool {
        self.actions_for_transition(from, to)
            .contains(&TransitionAction::Merge)
    }

    /// All outgoing edges from `from` that fire `merge`. The orchestrator
    /// uses this to ask "if I'm in status X, is there a merge target I
    /// should be probing toward?" — useful for the dry-run preview and
    /// reaction-rule logic where we have the current status but not yet
    /// the proposed `to`.
    pub fn outgoing_merge_transitions(&self, from: &str) -> Vec<&Transition> {
        let Some(ts) = &self.transitions else {
            return Vec::new();
        };
        ts.iter()
            .filter(|t| t.from == from && t.actions.contains(&TransitionAction::Merge))
            .collect()
    }

    /// True iff a task in `from` is on a transition that fires `merge` —
    /// i.e., Zen Mode's confidence bar should apply.
    ///
    /// When this workflow has *no* `transitions:` block declared at all,
    /// fall back to the legacy 5-status convention: a task in `Review`
    /// triggers the bar. This back-compat path keeps existing projects
    /// (whose migrated `default.yaml` carries no transitions block) on
    /// the same trigger they had before workflows landed; new projects
    /// that opt into the action-based bar by declaring transitions get
    /// the action-based semantic exclusively.
    pub fn fires_merge_bar(&self, from: &str) -> bool {
        match &self.transitions {
            Some(_) => !self.outgoing_merge_transitions(from).is_empty(),
            None => from == LEGACY_REVIEW_STATUS,
        }
    }

    /// Full semantic check. Run after deserialization to catch the
    /// cross-reference errors that serde alone can't see: duplicate
    /// status names, an `initial_status` pointing at nothing, a
    /// transition that names a status the workflow doesn't define.
    pub fn validate(&self) -> crate::Result<()> {
        if self.name.trim().is_empty() {
            return Err(workflow_err("workflow name must not be empty"));
        }

        if self.statuses.is_empty() {
            return Err(workflow_err(format!(
                "workflow `{}`: must declare at least one status",
                self.name
            )));
        }

        // Status names: non-empty + unique. Linear-scan dup detection
        // keeps the error message deterministic (first dup wins).
        let mut seen: Vec<&str> = Vec::with_capacity(self.statuses.len());
        for st in &self.statuses {
            if st.name.trim().is_empty() {
                return Err(workflow_err(format!(
                    "workflow `{}`: status name must not be empty",
                    self.name
                )));
            }
            if seen.contains(&st.name.as_str()) {
                return Err(workflow_err(format!(
                    "workflow `{}`: duplicate status name `{}`",
                    self.name, st.name
                )));
            }
            seen.push(st.name.as_str());
        }

        if let Some(init) = self.initial_status.as_deref() {
            if self.status(init).is_none() {
                return Err(workflow_err(format!(
                    "workflow `{}`: initial_status `{}` does not match any declared status",
                    self.name, init
                )));
            }
        }

        if let Some(tr) = &self.transitions {
            let mut seen: BTreeMap<(&str, &str), ()> = BTreeMap::new();
            for t in tr {
                if self.status(&t.from).is_none() {
                    return Err(workflow_err(format!(
                        "workflow `{}`: transition `{} -> {}` from undeclared status `{}`",
                        self.name, t.from, t.to, t.from
                    )));
                }
                if self.status(&t.to).is_none() {
                    return Err(workflow_err(format!(
                        "workflow `{}`: transition `{} -> {}` targets undeclared status `{}`",
                        self.name, t.from, t.to, t.to
                    )));
                }
                let key = (t.from.as_str(), t.to.as_str());
                if seen.insert(key, ()).is_some() {
                    return Err(workflow_err(format!(
                        "workflow `{}`: duplicate transition `{} -> {}`",
                        self.name, t.from, t.to
                    )));
                }
            }
        }

        Ok(())
    }

    /// Resolve this workflow's `git:` block against a task's
    /// frontmatter params, returning a fully substituted [`GitConfig`].
    ///
    /// Returns `Ok(None)` when the workflow has no `git:` block — the
    /// caller should fall back to project-level git defaults.
    /// Returns `Err(Error::MissingTaskParams)` listing every unresolved
    /// `{{key}}` across every git field (one error per workflow, even
    /// when multiple keys are missing) so the user can fix the task's
    /// frontmatter in a single edit. See `Plans/workflows.md` §12.
    pub fn resolve_git(
        &self,
        params: &BTreeMap<String, String>,
    ) -> crate::Result<Option<GitConfig>> {
        let Some(git) = &self.git else {
            return Ok(None);
        };
        let mut missing: Vec<String> = Vec::new();
        let base_branch = git
            .base_branch
            .as_ref()
            .map(|s| substitute_placeholders(s, params, &mut missing));
        if !missing.is_empty() {
            return Err(Error::MissingTaskParams {
                workflow: self.name.clone(),
                params: missing,
            });
        }
        Ok(Some(GitConfig {
            base_branch,
            merge_strategy: git.merge_strategy,
        }))
    }
}

/// The canonical five-status default workflow shipped with every new
/// project. The constructor that drops `workflows/default.yaml` into a
/// fresh project should serialize this. Matches the table in
/// `Plans/workflows.md` §3.
pub fn default_workflow() -> Workflow {
    Workflow {
        name: "default".to_string(),
        description: Some(
            "The standard one-track flow shipped with every project.".to_string(),
        ),
        statuses: vec![
            Status {
                name: "Backlog".into(),
                category: StatusCategory::Backlog,
                owner: Owner::User,
                description: None,
            },
            Status {
                name: "Todo".into(),
                category: StatusCategory::Ready,
                owner: Owner::Agent,
                description: None,
            },
            Status {
                name: "InProgress".into(),
                category: StatusCategory::Active,
                owner: Owner::Agent,
                description: None,
            },
            Status {
                name: "Review".into(),
                category: StatusCategory::Handoff,
                owner: Owner::User,
                description: None,
            },
            Status {
                name: "Done".into(),
                category: StatusCategory::Done,
                owner: Owner::User,
                description: None,
            },
        ],
        initial_status: None,
        // Default workflow ships *without* explicit transitions so the
        // on-disk `default.yaml` stays lean and matches what existing
        // projects already have after Phase 1's migration. Generic code
        // (Zen Mode, action-based confidence bar) treats a workflow with
        // no transitions declared as the legacy 5-status flow — see
        // [`Workflow::merge_trigger_status_or_legacy`].
        transitions: None,
        git: None,
        zen: None,
    }
}

// ---------------------------------------------------------------------------
// Status

/// One step in a workflow. `name` doubles as the stable identifier
/// (referenced from task frontmatter and from [`Workflow::transitions`])
/// and the user-facing column label.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub name: String,
    pub category: StatusCategory,
    pub owner: Owner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// StatusCategory

/// Closed semantic vocabulary the orchestrator, Zen Mode, and event-log
/// reactions speak in. Status *names* are user-customizable; categories
/// are not — generic code keys off the category so a workflow that
/// renames `Review` to `QA` still triggers the auto-merge rule.
///
/// See `Plans/workflows.md` §1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    /// Not yet ready for work — triage stage.
    Backlog,
    /// Ready to be picked up by whoever owns it.
    Ready,
    /// Owner is working on it now.
    Active,
    /// One owner has finished their part; another's input is required next.
    Handoff,
    /// Terminal state — accepted, shipped.
    Done,
}

impl StatusCategory {
    /// Stable snake_case spelling used on the wire (events log line shape,
    /// YAML config). Matches the serde rename so callers don't have to
    /// round-trip through serde to format a single value.
    pub fn as_str(self) -> &'static str {
        match self {
            StatusCategory::Backlog => "backlog",
            StatusCategory::Ready => "ready",
            StatusCategory::Active => "active",
            StatusCategory::Handoff => "handoff",
            StatusCategory::Done => "done",
        }
    }
}

impl std::fmt::Display for StatusCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for StatusCategory {
    type Err = crate::Error;
    fn from_str(s: &str) -> crate::Result<Self> {
        match s.trim() {
            "backlog" => Ok(StatusCategory::Backlog),
            "ready" => Ok(StatusCategory::Ready),
            "active" => Ok(StatusCategory::Active),
            "handoff" => Ok(StatusCategory::Handoff),
            "done" => Ok(StatusCategory::Done),
            other => Err(crate::Error::Other(format!(
                "unknown status category: {other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Owner

/// Who is expected to act when a task sits in a given status.
///
/// - `User` keeps the task waiting; the orchestrator does not dispatch.
/// - `Agent` makes the task eligible for auto-dispatch onto a free worker.
/// - `Either` is dispatchable but low-priority; the user can grab it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    User,
    Agent,
    Either,
}

/// The legacy 5-status workflow's handoff status name. Used as the
/// fallback merge-bar trigger for workflows without an explicit
/// `transitions:` block — see [`Workflow::fires_merge_bar`].
pub const LEGACY_REVIEW_STATUS: &str = "Review";

// ---------------------------------------------------------------------------
// Transition + TransitionAction

/// One edge in a workflow's action graph. Declares the side-effects that
/// fire when a task moves from `from` to `to`, plus an optional `target:`
/// override for `merge` and `open_pr` actions that should land somewhere
/// other than the workflow's resolved `git.base_branch`. See
/// `Plans/workflows.md` §12.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transition {
    /// Status name a task moves out of. Must match a declared status.
    pub from: String,

    /// Status name a task moves into. Must match a declared status.
    pub to: String,

    /// Hub-side actions to run when this transition fires. Order is the
    /// order they execute; failures short-circuit the rest. Empty
    /// (omitted on the wire) means the edge declares no side-effects —
    /// the move is a pure status change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TransitionAction>,

    /// Per-transition `merge` / `open_pr` target override. When `None`,
    /// the workflow's resolved `git.base_branch` (or the project
    /// fallback) wins. Useful for multi-hop pipelines: a feature
    /// workflow that merges intermediate work into `develop` here but
    /// ships to `main` on a later transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// One of the six hub-side action primitives the workflow engine can
/// fire. Matches the action set in `Plans/workflows.md` §12 and the
/// functions in `shelbi-orchestrator::actions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionAction {
    /// Push the task's branch to origin.
    PushBranch,
    /// Open a PR for the task's branch.
    OpenPr,
    /// Merge the task's branch into its target.
    Merge,
    /// Close any open PR without merging.
    ClosePr,
    /// Delete the local + remote branch.
    DeleteBranch,
    /// Rebase the task's branch onto its parent's current branch.
    Restack,
}

impl TransitionAction {
    /// Stable wire-format spelling. Matches the serde rename so callers
    /// don't need to round-trip through serde to format a single value.
    pub fn as_str(self) -> &'static str {
        match self {
            TransitionAction::PushBranch => "push_branch",
            TransitionAction::OpenPr => "open_pr",
            TransitionAction::Merge => "merge",
            TransitionAction::ClosePr => "close_pr",
            TransitionAction::DeleteBranch => "delete_branch",
            TransitionAction::Restack => "restack",
        }
    }
}

impl std::fmt::Display for TransitionAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// WorkflowZenConfig

/// Per-workflow override of the project-level [`crate::ZenConfig`]. Every
/// field is optional so a workflow can pick which dimensions to override
/// — typically `checks:` for a workflow that needs a different test
/// suite, `danger_paths:` to widen or replace the danger-glob set, and
/// `ci_timeout:` for pipelines whose CI takes substantially longer (or
/// shorter) than the project default.
///
/// Use the resolution helpers — [`crate::checks_for_task_in_workflow`],
/// [`crate::ci_timeout_for_workflow`],
/// [`crate::danger_paths_for_workflow`] — to look up the effective value
/// for a (project, workflow, task) triple. Callers should not pattern-
/// match on this struct directly; the helpers handle the "unset → fall
/// back to project" rule in one place.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowZenConfig {
    /// Local checks to run before the merge bar — overrides
    /// `project.zen.checks` outright when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<ZenChecks>,

    /// CI watch timeout for this workflow — overrides
    /// `project.zen.ci_timeout` when set. Serialized as a number of
    /// seconds, matching the project-level field's wire format.
    #[serde(default, with = "opt_duration_secs", skip_serializing_if = "Option::is_none")]
    pub ci_timeout: Option<Duration>,

    /// Danger-glob list for this workflow — overrides
    /// `project.zen.danger_paths` (including its `extend` / `override`
    /// semantics) when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub danger_paths: Option<ZenDangerPaths>,
}

impl WorkflowZenConfig {
    /// True iff every override field is unset. A workflow with an
    /// `zen: {}` block parses to this — semantically identical to omitting
    /// the block entirely, but cheap to detect for diagnostics ("you
    /// declared `zen:` but didn't override anything").
    pub fn is_empty(&self) -> bool {
        self.checks.is_none() && self.ci_timeout.is_none() && self.danger_paths.is_none()
    }
}

/// Serde adapter for `Option<Duration>` stored as an optional integer
/// number of seconds. Used by [`WorkflowZenConfig::ci_timeout`] so the
/// wire format matches the project-level `zen.ci_timeout` field.
mod opt_duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        d: &Option<Duration>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_u64(d.as_secs()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Duration>, D::Error> {
        let opt = Option::<u64>::deserialize(d)?;
        Ok(opt.map(Duration::from_secs))
    }
}

// ---------------------------------------------------------------------------
// Error helper

fn workflow_err(msg: impl Into<String>) -> Error {
    Error::InvalidWorkflow(msg.into())
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MergeStrategy;

    const DEFAULT_YAML: &str = r#"
name: default
description: |
  Five-status default.
statuses:
  - { name: Backlog,    category: backlog, owner: user  }
  - { name: Todo,       category: ready,   owner: agent }
  - { name: InProgress, category: active,  owner: agent }
  - { name: Review,     category: handoff, owner: user  }
  - { name: Done,       category: done,    owner: user  }
"#;

    #[test]
    fn parses_the_default_workflow_yaml() {
        let wf = Workflow::from_yaml_str(DEFAULT_YAML).expect("parse default");
        assert_eq!(wf.name, "default");
        assert_eq!(wf.statuses.len(), 5);
        assert_eq!(wf.statuses[0].name, "Backlog");
        assert_eq!(wf.statuses[0].category, StatusCategory::Backlog);
        assert_eq!(wf.statuses[0].owner, Owner::User);
        assert_eq!(wf.statuses[2].category, StatusCategory::Active);
        assert_eq!(wf.statuses[2].owner, Owner::Agent);
        assert!(wf.initial_status.is_none());
        assert!(wf.transitions.is_none());
    }

    #[test]
    fn default_workflow_helper_matches_documented_table() {
        let wf = default_workflow();
        wf.validate().expect("built-in default validates");

        let cats: Vec<_> = wf.statuses.iter().map(|s| s.category).collect();
        assert_eq!(
            cats,
            vec![
                StatusCategory::Backlog,
                StatusCategory::Ready,
                StatusCategory::Active,
                StatusCategory::Handoff,
                StatusCategory::Done,
            ]
        );

        let owners: Vec<_> = wf.statuses.iter().map(|s| s.owner).collect();
        assert_eq!(
            owners,
            vec![Owner::User, Owner::Agent, Owner::Agent, Owner::User, Owner::User]
        );
    }

    #[test]
    fn round_trips_through_yaml() {
        let wf = default_workflow();
        let y = serde_yaml::to_string(&wf).unwrap();
        let back: Workflow = serde_yaml::from_str(&y).unwrap();
        assert_eq!(wf, back);
    }

    #[test]
    fn optional_fields_omitted_when_absent() {
        let wf = default_workflow();
        let y = serde_yaml::to_string(&wf).unwrap();
        // Nothing was set, so neither key should appear on the wire.
        assert!(!y.contains("initial_status"));
        assert!(!y.contains("transitions"));
        // Per-status `description` not set either.
        assert!(!y.contains("description: null"));
    }

    #[test]
    fn either_owner_round_trips() {
        let yaml = r#"
name: w
statuses:
  - { name: Open, category: ready, owner: either }
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        assert_eq!(wf.statuses[0].owner, Owner::Either);
        let y = serde_yaml::to_string(&wf).unwrap();
        assert!(y.contains("owner: either"));
    }

    #[test]
    fn rejects_empty_status_list() {
        let yaml = r#"
name: w
statuses: []
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("at least one status")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_blank_workflow_name() {
        let yaml = r#"
name: "   "
statuses:
  - { name: Todo, category: ready, owner: agent }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::InvalidWorkflow(ref m) if m.contains("name must not be empty")));
    }

    #[test]
    fn rejects_blank_status_name() {
        let yaml = r#"
name: w
statuses:
  - { name: "", category: ready, owner: agent }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::InvalidWorkflow(ref m) if m.contains("status name must not be empty")));
    }

    #[test]
    fn rejects_duplicate_status_names() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready, owner: agent }
  - { name: Todo, category: active, owner: agent }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::InvalidWorkflow(ref m) if m.contains("duplicate status name")));
    }

    #[test]
    fn rejects_unknown_initial_status() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready, owner: agent }
initial_status: Backlog
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::InvalidWorkflow(ref m) if m.contains("initial_status")));
    }

    #[test]
    fn accepts_known_initial_status_and_resolves_it() {
        let yaml = r#"
name: w
statuses:
  - { name: Backlog, category: backlog, owner: user  }
  - { name: Todo,    category: ready,   owner: agent }
initial_status: Todo
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        assert_eq!(wf.resolved_initial_status(), Some("Todo"));
    }

    #[test]
    fn resolved_initial_status_falls_back_to_first() {
        let wf = default_workflow();
        assert_eq!(wf.resolved_initial_status(), Some("Backlog"));
    }

    #[test]
    fn rejects_transition_from_undeclared_status() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready, owner: agent }
transitions:
  - { from: Bogus, to: Todo, actions: [push_branch] }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("from undeclared status `Bogus`")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_transition_to_undeclared_status() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready,  owner: agent }
  - { name: Done, category: done,   owner: user  }
transitions:
  - { from: Todo, to: Phantom, actions: [merge] }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("undeclared status `Phantom`")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_duplicate_transition_edge() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready,  owner: agent }
  - { name: Done, category: done,   owner: user  }
transitions:
  - { from: Todo, to: Done, actions: [merge] }
  - { from: Todo, to: Done, actions: [close_pr] }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("duplicate transition")),
            "got: {err}"
        );
    }

    #[test]
    fn transition_allowed_is_any_to_any_between_declared_statuses() {
        let wf = default_workflow();
        // Both endpoints exist → allowed, no whitelist semantic.
        assert!(wf.transition_allowed("Backlog", "Done"));
        assert!(wf.transition_allowed("Done", "Backlog"));
        // Unknown status is never reachable.
        assert!(!wf.transition_allowed("Ghost", "Done"));
        assert!(!wf.transition_allowed("Backlog", "Ghost"));
    }

    #[test]
    fn transition_allowed_does_not_restrict_when_transitions_declared() {
        // §11 in the workflows plan: declaring a `transitions:` block only
        // adds *side-effects* to edges. It does NOT restrict which moves
        // are legal — any declared status can move to any other.
        let yaml = r#"
name: w
statuses:
  - { name: Todo,    category: ready,  owner: agent }
  - { name: Doing,   category: active, owner: agent }
  - { name: Done,    category: done,   owner: user  }
transitions:
  - { from: Doing, to: Done, actions: [merge] }
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        assert!(wf.transition_allowed("Todo", "Doing"));
        assert!(wf.transition_allowed("Todo", "Done"));
        // Backwards is just as legal — the edge is just "an edge."
        assert!(wf.transition_allowed("Done", "Todo"));
    }

    #[test]
    fn transition_action_helpers_match_declared_edges() {
        let yaml = r#"
name: w
statuses:
  - { name: Doing,  category: active,  owner: agent }
  - { name: Review, category: handoff, owner: user  }
  - { name: Done,   category: done,    owner: user  }
transitions:
  - { from: Doing,  to: Review, actions: [push_branch, open_pr] }
  - { from: Review, to: Done,   actions: [merge, delete_branch] }
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();

        assert_eq!(
            wf.actions_for_transition("Doing", "Review"),
            &[TransitionAction::PushBranch, TransitionAction::OpenPr]
        );
        assert_eq!(
            wf.actions_for_transition("Review", "Done"),
            &[TransitionAction::Merge, TransitionAction::DeleteBranch]
        );
        // Unlisted edges have no actions — the move is a pure status change.
        assert!(wf.actions_for_transition("Doing", "Done").is_empty());

        assert!(wf.is_merge_transition("Review", "Done"));
        assert!(!wf.is_merge_transition("Doing", "Review"));

        let outgoing: Vec<&str> = wf
            .outgoing_merge_transitions("Review")
            .into_iter()
            .map(|t| t.to.as_str())
            .collect();
        assert_eq!(outgoing, vec!["Done"]);
    }

    #[test]
    fn fires_merge_bar_uses_actions_when_transitions_declared() {
        // Workflow has an explicit transitions block → action-based: only
        // statuses with an outgoing merge edge fire the bar.
        let yaml = r#"
name: w
statuses:
  - { name: Doing,  category: active,  owner: agent }
  - { name: Review, category: handoff, owner: user  }
  - { name: Done,   category: done,    owner: user  }
transitions:
  - { from: Doing,  to: Review, actions: [push_branch] }
  - { from: Review, to: Done,   actions: [merge] }
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        assert!(wf.fires_merge_bar("Review"));
        assert!(!wf.fires_merge_bar("Doing"));
        // Unknown statuses don't trip the bar.
        assert!(!wf.fires_merge_bar("Ghost"));
    }

    #[test]
    fn fires_merge_bar_falls_back_to_legacy_review_when_no_transitions() {
        // A workflow with no `transitions:` block (the default, what
        // existing projects have on disk) keeps the historic "Review →
        // Done" trigger. Lets existing zen users not have to edit YAML to
        // keep the bar firing.
        let wf = default_workflow();
        assert!(wf.transitions.is_none(), "default ships without transitions");
        assert!(wf.fires_merge_bar("Review"));
        assert!(!wf.fires_merge_bar("InProgress"));
        assert!(!wf.fires_merge_bar("Backlog"));
    }

    #[test]
    fn trunk_based_workflow_fires_bar_on_active_to_done() {
        // Worked example from `Plans/workflows.md` §8: a workflow that
        // skips `Review` and goes straight `InProgress -> Done` with a
        // merge action gets the same high bar — no special-casing.
        let yaml = r#"
name: trunk
statuses:
  - { name: Doing, category: active, owner: agent }
  - { name: Done,  category: done,   owner: user  }
transitions:
  - { from: Doing, to: Done, actions: [merge, delete_branch] }
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        assert!(wf.fires_merge_bar("Doing"));
    }

    #[test]
    fn rejects_status_with_unknown_category() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: pending, owner: agent }
"#;
        // serde rejects this at the type level — InvalidWorkflow is for
        // semantic errors, parse-time errors surface as Error::Yaml.
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::Yaml(_)), "got: {err}");
    }

    #[test]
    fn rejects_status_with_unknown_owner() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready, owner: robot }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::Yaml(_)), "got: {err}");
    }

    // ---------------------------------------------------------------------
    // git: block + {{var}} parameterization

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn workflow_git_block_parses_with_placeholders() {
        let yaml = r#"
name: feature-task
statuses:
  - { name: Todo, category: ready, owner: agent }
git:
  base_branch: feature/{{feature}}
"#;
        let wf = Workflow::from_yaml_str(yaml).expect("parse");
        let git = wf.git.expect("git block parsed");
        assert_eq!(git.base_branch.as_deref(), Some("feature/{{feature}}"));
    }

    #[test]
    fn workflow_with_no_git_block_resolves_to_none() {
        // `default` workflow has no `git:`, so callers fall back to the
        // project-level `Project::base_branch` / `merge_strategy` without
        // any per-task substitution.
        let wf = default_workflow();
        let out = wf.resolve_git(&params(&[])).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn workflow_git_omits_when_none_on_the_wire() {
        // A round trip of `default_workflow()` (no `git:`) must not emit
        // an empty `git:` key — the field has to be entirely absent so
        // legacy workflow YAMLs keep their existing shape.
        let wf = default_workflow();
        let y = serde_yaml::to_string(&wf).unwrap();
        assert!(!y.contains("git:"), "unexpected git: in {y}");
    }

    #[test]
    fn resolve_git_substitutes_placeholders_from_params() {
        let yaml = r#"
name: feature-task
statuses:
  - { name: Todo, category: ready, owner: agent }
git:
  base_branch: feature/{{feature}}
  merge_strategy: merge
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let resolved = wf
            .resolve_git(&params(&[("feature", "auth-rewrite")]))
            .unwrap()
            .expect("git block present");
        assert_eq!(resolved.base_branch.as_deref(), Some("feature/auth-rewrite"));
        assert_eq!(resolved.merge_strategy, MergeStrategy::Merge);
    }

    #[test]
    fn resolve_git_errors_with_actionable_message_when_param_missing() {
        // The error wording is the user contract — `Plans/workflows.md`
        // §12 quotes the message verbatim so the hint matches whatever
        // a confused user pastes into a search.
        let yaml = r#"
name: feature-task
statuses:
  - { name: Todo, category: ready, owner: agent }
git:
  base_branch: feature/{{feature}}
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let err = wf.resolve_git(&params(&[])).unwrap_err();
        match err {
            Error::MissingTaskParams {
                ref workflow,
                ref params,
            } => {
                assert_eq!(workflow, "feature-task");
                assert_eq!(params, &vec!["feature".to_string()]);
            }
            other => panic!("expected MissingTaskParams, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("workflow `feature-task`"), "msg: {msg}");
        assert!(msg.contains("parameter `feature`"), "msg: {msg}");
        assert!(msg.contains("`feature: <value>`"), "msg: {msg}");
        assert!(msg.contains("frontmatter"), "msg: {msg}");
    }

    #[test]
    fn resolve_git_lists_every_missing_param_in_one_error() {
        let yaml = r#"
name: stack
statuses:
  - { name: Todo, category: ready, owner: agent }
git:
  base_branch: feature/{{feature}}-{{region}}
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let err = wf
            .resolve_git(&params(&[("feature", "auth")]))
            .unwrap_err();
        match err {
            Error::MissingTaskParams { params, .. } => {
                assert_eq!(params, vec!["region".to_string()]);
            }
            other => panic!("expected MissingTaskParams, got {other:?}"),
        }
    }

    #[test]
    fn resolve_git_preserves_branches_without_placeholders() {
        // A plain (non-templated) `git:` block resolves to itself, so
        // the call site can use `resolve_git` unconditionally.
        let yaml = r#"
name: feature-release
statuses:
  - { name: Todo, category: ready, owner: agent }
git:
  base_branch: main
  merge_strategy: squash
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let resolved = wf.resolve_git(&params(&[])).unwrap().unwrap();
        assert_eq!(resolved.base_branch.as_deref(), Some("main"));
        assert_eq!(resolved.merge_strategy, MergeStrategy::Squash);
    }

    #[test]
    fn workflow_git_round_trips_through_yaml() {
        let yaml = r#"
name: feature-task
statuses:
  - { name: Todo, category: ready, owner: agent }
git:
  base_branch: feature/{{feature}}
  merge_strategy: merge
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let serialized = serde_yaml::to_string(&wf).unwrap();
        let back = Workflow::from_yaml_str(&serialized).unwrap();
        assert_eq!(wf, back);
    }

    // ---------------------------------------------------------------------
    // zen: block (per-workflow override of project zen config)

    #[test]
    fn workflow_zen_block_parses_all_three_overrides() {
        let yaml = r#"
name: research
statuses:
  - { name: Drafting, category: active, owner: agent }
zen:
  checks:
    local:
      - 'pytest -k research'
  ci_timeout: 600
  danger_paths:
    override:
      - 'fixtures/**'
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let z = wf.zen.as_ref().expect("zen block parsed");
        let checks = z.checks.as_ref().expect("checks set");
        assert_eq!(checks.local, vec!["pytest -k research".to_string()]);
        assert_eq!(z.ci_timeout, Some(Duration::from_secs(600)));
        assert!(matches!(
            z.danger_paths.as_ref().unwrap(),
            ZenDangerPaths::Override(v) if v == &vec!["fixtures/**".to_string()]
        ));
    }

    #[test]
    fn workflow_zen_subfields_independently_optional() {
        // A workflow can override just one dimension and leave the others
        // to fall back to the project default.
        let yaml = r#"
name: long-ci
statuses:
  - { name: Todo, category: ready, owner: agent }
zen:
  ci_timeout: 3600
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let z = wf.zen.expect("zen block parsed");
        assert!(z.checks.is_none());
        assert_eq!(z.ci_timeout, Some(Duration::from_secs(3600)));
        assert!(z.danger_paths.is_none());
        assert!(!z.is_empty());
    }

    #[test]
    fn workflow_zen_empty_block_is_recognized_as_empty() {
        // `zen: {}` parses and is structurally equivalent to omitting the
        // block — every override unset.
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready, owner: agent }
zen: {}
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let z = wf.zen.expect("zen block parsed");
        assert!(z.is_empty());
    }

    #[test]
    fn workflow_with_no_zen_block_omits_field_on_the_wire() {
        let wf = default_workflow();
        let y = serde_yaml::to_string(&wf).unwrap();
        assert!(!y.contains("zen:"), "unexpected zen: in {y}");
    }

    #[test]
    fn workflow_zen_round_trips_through_yaml() {
        let yaml = r#"
name: research
statuses:
  - { name: Drafting, category: active, owner: agent }
zen:
  checks:
    local:
      - 'pytest -k research'
  ci_timeout: 600
  danger_paths:
    extend:
      - 'fixtures/**'
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let serialized = serde_yaml::to_string(&wf).unwrap();
        let back = Workflow::from_yaml_str(&serialized).unwrap();
        assert_eq!(wf, back);
        // ci_timeout serializes as a bare integer (no struct form).
        assert!(
            serialized.contains("ci_timeout: 600"),
            "expected `ci_timeout: 600` in serialized form, got:\n{serialized}"
        );
    }
}
