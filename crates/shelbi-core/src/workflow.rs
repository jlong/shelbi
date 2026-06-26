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

use serde::{Deserialize, Serialize};

use crate::placeholders::substitute_placeholders;
use crate::{Error, GitConfig};

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

    /// Optional whitelist of allowed status transitions. Key = from-status
    /// name; value = list of statuses the task may move to from there. A
    /// key whose value is `[]` declares a terminal status (no outgoing
    /// moves). When the entire field is `None`, transitions are
    /// unrestricted (any-to-any) — matching today's freedom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitions: Option<BTreeMap<String, Vec<String>>>,

    /// Per-workflow override of the project-level `git:` defaults. When
    /// `None`, callers inherit `Project::base_branch` and
    /// `Project::merge_strategy` unchanged. Field values may contain
    /// `{{var}}` placeholders that are resolved against the task's
    /// params at task-load time — see [`Workflow::resolve_git`] and
    /// `Plans/workflows.md` §12 "Parameterization".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitConfig>,
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

    /// True iff `from -> to` is permitted under this workflow's
    /// transition rules. When [`Workflow::transitions`] is unset, every
    /// move between declared statuses is allowed; an unknown status is
    /// never reachable. Self-loops are allowed (`Todo -> Todo`) so
    /// callers don't have to special-case "no change."
    pub fn transition_allowed(&self, from: &str, to: &str) -> bool {
        if self.status(from).is_none() || self.status(to).is_none() {
            return false;
        }
        match &self.transitions {
            None => true,
            Some(map) => map
                .get(from)
                .map(|v| v.iter().any(|s| s == to))
                .unwrap_or(false),
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
            for (from, tos) in tr {
                if self.status(from).is_none() {
                    return Err(workflow_err(format!(
                        "workflow `{}`: transitions key `{}` is not a declared status",
                        self.name, from
                    )));
                }
                for to in tos {
                    if self.status(to).is_none() {
                        return Err(workflow_err(format!(
                            "workflow `{}`: transition `{}` -> `{}` targets undeclared status `{}`",
                            self.name, from, to, to
                        )));
                    }
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
        transitions: None,
        git: None,
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
    fn rejects_transitions_key_pointing_at_undeclared_status() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready, owner: agent }
transitions:
  Bogus: [Todo]
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("transitions key `Bogus`")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_transitions_value_pointing_at_undeclared_status() {
        let yaml = r#"
name: w
statuses:
  - { name: Todo, category: ready,  owner: agent }
  - { name: Done, category: done,   owner: user  }
transitions:
  Todo: [Done, Phantom]
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("undeclared status `Phantom`")),
            "got: {err}"
        );
    }

    #[test]
    fn transition_allowed_is_any_to_any_when_unset() {
        let wf = default_workflow();
        assert!(wf.transition_allowed("Backlog", "Done"));
        assert!(wf.transition_allowed("Done", "Backlog"));
        // Unknown status is never reachable.
        assert!(!wf.transition_allowed("Ghost", "Done"));
        assert!(!wf.transition_allowed("Backlog", "Ghost"));
    }

    #[test]
    fn transition_allowed_honors_restricted_map() {
        let yaml = r#"
name: w
statuses:
  - { name: Backlog,    category: backlog, owner: user  }
  - { name: Todo,       category: ready,   owner: agent }
  - { name: InProgress, category: active,  owner: agent }
  - { name: Review,     category: handoff, owner: user  }
  - { name: Done,       category: done,    owner: user  }
transitions:
  Backlog:    [Todo]
  Todo:       [InProgress, Backlog]
  InProgress: [Review, Todo]
  Review:     [Done, Todo, InProgress]
  Done:       []
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        assert!(wf.transition_allowed("Backlog", "Todo"));
        assert!(wf.transition_allowed("Review", "Done"));
        // Not in the whitelist:
        assert!(!wf.transition_allowed("Backlog", "Done"));
        // Terminal state: nothing leaves.
        assert!(!wf.transition_allowed("Done", "Backlog"));
        // Self-loops require an explicit listing under restricted mode.
        assert!(!wf.transition_allowed("Todo", "Todo"));
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
}
