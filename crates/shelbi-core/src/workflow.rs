//! Workflow definitions: the per-project, YAML-declared status set that
//! supersedes the hardcoded five-column [`crate::Column`] enum.
//!
//! A workflow is the structure described in `Plans/workflows.md`: a named
//! list of statuses, each carrying a [`StatusCategory`] (the semantic
//! vocabulary the rest of the system reasons in), an [`Owner`] (who is
//! expected to act when automation is off), and an optional **`agent:`**
//! (which agent the orchestrator dispatches to when automation is on).
//! Workflows live at `~/.shelbi/projects/<project>/workflows/<name>.yaml`
//! and are loaded through [`Workflow::from_yaml_str`].
//!
//! ## The two-field owner / agent split
//!
//! - `owner` (strict `user | agent`) — who is responsible when Zen is
//!   off. `user` waits for a human; `agent` is dispatchable.
//! - `agent` (optional, a directory name under `agents/`) — which agent
//!   is empowered to act when Zen is on. A `user`-owned status with an
//!   `agent:` value means "under Zen, this agent can do the work without
//!   me." A status with no `agent:` has no automation path — even Zen
//!   leaves it alone.
//!
//! See `Plans/agents-workspaces.md` §4 and `Plans/workflows.md` §1 for
//! the full design.
//!
//! ## Legacy migration
//!
//! Existing workflows authored before the split keep loading:
//!
//! - `owner: agent` (no `agent:` field) → fills in `agent:` from
//!   category (`ready` → `orchestrator`, `active` → `developer`). Any
//!   other category with bare `owner: agent` is a hard error.
//! - `owner: <name>` (a never-shipped named-owner design that may exist
//!   in test fixtures) → rewrites to `owner: agent, agent: <name>`.
//!
//! Either form causes [`Workflow::from_yaml_str_with_diagnostics`] to
//! return one summary deprecation diagnostic per workflow, which the
//! state-layer loader surfaces to stderr once per workflow per process.
//!
//! This module is the schema + validator. Wiring workflows into the
//! orchestrator, TUI, events log, or task frontmatter happens elsewhere.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

use crate::placeholders::substitute_placeholders;
use crate::{Error, GitConfig, ZenChecks, ZenDangerPaths};

// ---------------------------------------------------------------------------
// Workflow

/// A named workflow: the ordered list of statuses a task moves through,
/// plus the optional rules that constrain those moves. Round-trips through
/// YAML; call [`Workflow::validate`] (or the all-in-one
/// [`Workflow::from_yaml_str`]) before trusting the values.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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

    /// Stable id of the status a freshly created task lands in. When
    /// `None`, the first status in [`Workflow::statuses`] is used. Must
    /// reference a status declared in this workflow by [`Status::id`].
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
    ///
    /// Legacy single-field owner forms migrate silently. Use
    /// [`Workflow::from_yaml_str_with_diagnostics`] when you need the
    /// deprecation warning to surface (the state-layer loader does).
    pub fn from_yaml_str(s: &str) -> crate::Result<Self> {
        let (wf, _diags) = Self::from_yaml_str_with_diagnostics(s)?;
        Ok(wf)
    }

    /// Parse, migrate legacy forms, and validate — returning the workflow
    /// plus any migration warnings as human-readable diagnostic strings.
    ///
    /// At most one diagnostic per workflow: the loader bundles every
    /// migrated status into a single multiline warning so the user sees
    /// one "please update this YAML" message regardless of how many
    /// statuses are legacy.
    pub fn from_yaml_str_with_diagnostics(s: &str) -> crate::Result<(Self, Vec<String>)> {
        let raw: RawWorkflow = serde_yaml::from_str(s)?;
        let (wf, migrations) = convert_raw_workflow(raw)?;
        wf.validate()?;
        let diagnostics = if migrations.is_empty() {
            Vec::new()
        } else {
            vec![format_legacy_warning(&wf.name, &migrations)]
        };
        Ok((wf, diagnostics))
    }

    /// Resolved initial status id — explicit `initial_status` if set,
    /// otherwise the id of the first status in the list. Returns `None`
    /// only if [`Workflow::statuses`] is empty (which a validated
    /// workflow never is).
    pub fn resolved_initial_status(&self) -> Option<&str> {
        if let Some(s) = self.initial_status.as_deref() {
            return Some(s);
        }
        self.statuses.first().map(|s| s.id.as_str())
    }

    /// Look up a status by its stable [`Status::id`]. Linear scan —
    /// workflows are tiny (<10 statuses in practice) so a hash map isn't
    /// worth the allocation.
    pub fn status(&self, id: &str) -> Option<&Status> {
        self.statuses.iter().find(|s| s.id == id)
    }

    /// True iff `from -> to` is a legal status move. Both arguments are
    /// status ids ([`Status::id`]). Per `Plans/workflows.md` §11,
    /// transitions are any-to-any: both ids just have to be declared in
    /// this workflow. Listing an edge in [`Workflow::transitions`] only
    /// declares its side-effects; it does not restrict which moves are
    /// legal.
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
    /// fall back to the legacy convention: a task in the workflow's
    /// **handoff** status triggers the bar. This back-compat path keeps
    /// existing projects (whose migrated `default.yaml` carries no
    /// transitions block) on the same trigger they had before workflows
    /// landed; new projects that opt into the action-based bar by
    /// declaring transitions get the action-based semantic exclusively.
    ///
    /// The fallback keys off the [`StatusCategory::Handoff`] category, not
    /// the hardcoded `review` id — a project that renames its handoff
    /// status (e.g. `qa`) still trips the bar, consistent with the
    /// category-not-name contract every other generic consumer honors.
    pub fn fires_merge_bar(&self, from: &str) -> bool {
        match &self.transitions {
            Some(_) => !self.outgoing_merge_transitions(from).is_empty(),
            None => self
                .status(from)
                .map(|s| s.category == StatusCategory::Handoff)
                .unwrap_or(false),
        }
    }

    /// Full semantic check. Run after deserialization to catch the
    /// cross-reference errors that serde alone can't see: duplicate
    /// status ids, an `initial_status` pointing at nothing, a transition
    /// that references an id the workflow doesn't declare, and the
    /// two-field rule that `owner: agent` requires an explicit `agent:`.
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

        // Status ids: non-empty + unique. Linear-scan dup detection keeps
        // the error message deterministic (first dup wins). Names are the
        // user-facing display label — required non-empty but allowed to
        // collide between statuses since they aren't referenced by
        // anything stable.
        let mut seen: Vec<&str> = Vec::with_capacity(self.statuses.len());
        for st in &self.statuses {
            if st.id.trim().is_empty() {
                return Err(workflow_err(format!(
                    "workflow `{}`: status id must not be empty",
                    self.name
                )));
            }
            if st.name.trim().is_empty() {
                return Err(workflow_err(format!(
                    "workflow `{}`: status `{}`: name must not be empty",
                    self.name, st.id
                )));
            }
            if seen.contains(&st.id.as_str()) {
                return Err(workflow_err(format!(
                    "workflow `{}`: duplicate status id `{}`",
                    self.name, st.id
                )));
            }
            seen.push(st.id.as_str());

            // The two-field rule: a status owned by `agent` must name the
            // agent that runs it. Bare `owner: agent` is migrated by
            // [`convert_raw_workflow`] for the categories where a default
            // exists (`ready`, `active`) — anything that reaches `validate`
            // without an `agent:` set is an authoring bug.
            if matches!(st.owner, Owner::Agent) && st.agent.is_none() {
                return Err(workflow_err(format!(
                    "workflow `{}`: status `{}` has owner: agent but no agent: field \
                     — which agent should run here?",
                    self.name, st.id,
                )));
            }
        }

        if let Some(init) = self.initial_status.as_deref() {
            if self.status(init).is_none() {
                return Err(workflow_err(format!(
                    "workflow `{}`: initial_status `{}` does not match any declared status id",
                    self.name, init
                )));
            }
        }

        if let Some(tr) = &self.transitions {
            let mut seen: BTreeMap<(&str, &str), ()> = BTreeMap::new();
            for t in tr {
                if self.status(&t.from).is_none() {
                    return Err(workflow_err(format!(
                        "workflow `{}`: transition `{} -> {}` from undeclared status id `{}`",
                        self.name, t.from, t.to, t.from
                    )));
                }
                if self.status(&t.to).is_none() {
                    return Err(workflow_err(format!(
                        "workflow `{}`: transition `{} -> {}` targets undeclared status id `{}`",
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

    /// Resolve a parsed-but-unfilled workflow against a [`ProjectStatuses`]
    /// loaded from `workflows/statuses.yml`. Fills in each status's
    /// `name` + `category` from the project-wide source of truth, errors
    /// if a workflow declares an `id` the project doesn't know about, and
    /// runs [`Workflow::validate`] afterward.
    ///
    /// The error for an unknown id includes the full list of available
    /// ids — the user contract documented in `Plans/shared-statuses.md`
    /// (Loader validation rules).
    pub fn resolve_against(
        mut self,
        statuses: &crate::ProjectStatuses,
    ) -> crate::Result<Self> {
        for st in &mut self.statuses {
            let known = statuses.get(&st.id).ok_or_else(|| {
                let available = statuses
                    .ids()
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                workflow_err(format!(
                    "workflow `{wf}`: status id `{id}` is not declared in \
                     `workflows/statuses.yml` (available: {available})",
                    wf = self.name,
                    id = st.id,
                ))
            })?;
            st.name = known.name.clone();
            st.category = known.category;
        }
        self.validate()?;
        Ok(self)
    }

    /// Probe the raw YAML form of this workflow for inline `name:` or
    /// `category:` fields under any `statuses:` entry. Used by the
    /// loader to enforce "once `statuses.yml` is in place, workflow
    /// files must use the reference-only form."
    ///
    /// Returns the list of status ids that still carry inline identity
    /// fields, paired with which field(s) were present. Empty when the
    /// file is in the new (reference-only) form.
    pub fn inline_identity_fields(yaml: &str) -> crate::Result<Vec<InlineIdentityField>> {
        let value: serde_yaml::Value = serde_yaml::from_str(yaml)?;
        let mut out = Vec::new();
        let Some(statuses) = value
            .get(serde_yaml::Value::String("statuses".into()))
            .and_then(|v| v.as_sequence())
        else {
            return Ok(out);
        };
        for entry in statuses {
            let (has_name, has_category) = Status::parse_presence(entry);
            if !has_name && !has_category {
                continue;
            }
            let id = entry
                .get(serde_yaml::Value::String("id".into()))
                .and_then(|v| v.as_str())
                .or_else(|| {
                    entry
                        .get(serde_yaml::Value::String("name".into()))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("?")
                .to_string();
            out.push(InlineIdentityField {
                id,
                has_name,
                has_category,
            });
        }
        Ok(out)
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

impl<'de> Deserialize<'de> for Workflow {
    /// Custom deserializer: route through [`RawWorkflow`] so the lenient
    /// status schema (legacy `owner: <name>` rewrites, optional id/name
    /// fallback) applies whenever a Workflow is read from YAML —
    /// including the `serde_yaml::to_string` → `serde_yaml::from_str`
    /// round-trip the round-trip tests rely on. Direct callers that
    /// want the migration warning surfaced must use
    /// [`Workflow::from_yaml_str_with_diagnostics`]; this path runs the
    /// migration silently.
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = RawWorkflow::deserialize(d)?;
        let (wf, _diags) = convert_raw_workflow(raw).map_err(serde::de::Error::custom)?;
        Ok(wf)
    }
}

/// The canonical six-status default workflow shipped with every new
/// project. The constructor that drops `workflows/default.yaml` into a
/// fresh project should serialize this. Matches the table in
/// `Plans/agents-workspaces.md` §4 — Backlog/Review delegate to the
/// orchestrator agent when Zen is on, Todo always dispatches to the
/// orchestrator, InProgress hands off to the developer agent, and the
/// terminal Done/Canceled lanes have no automation path.
pub fn default_workflow() -> Workflow {
    Workflow {
        name: "default".to_string(),
        description: Some(
            "The standard one-track flow shipped with every project.".to_string(),
        ),
        statuses: vec![
            Status {
                id: "backlog".into(),
                name: "Backlog".into(),
                category: StatusCategory::Backlog,
                owner: Owner::User,
                agent: Some("orchestrator".into()),
            },
            Status {
                id: "todo".into(),
                name: "Todo".into(),
                category: StatusCategory::Ready,
                owner: Owner::Agent,
                agent: Some("orchestrator".into()),
            },
            Status {
                id: "in-progress".into(),
                name: "In Progress".into(),
                category: StatusCategory::Active,
                owner: Owner::Agent,
                agent: Some("developer".into()),
            },
            Status {
                id: "review".into(),
                name: "Review".into(),
                category: StatusCategory::Handoff,
                owner: Owner::User,
                agent: Some("orchestrator".into()),
            },
            Status {
                id: "done".into(),
                name: "Done".into(),
                category: StatusCategory::Done,
                owner: Owner::User,
                agent: None,
            },
            Status {
                id: "canceled".into(),
                name: "Canceled".into(),
                category: StatusCategory::Archived,
                owner: Owner::User,
                agent: None,
            },
        ],
        initial_status: None,
        // Default workflow ships *without* explicit transitions so the
        // on-disk `default.yaml` stays lean and matches what existing
        // projects already have after Phase 1's migration. Generic code
        // (Zen Mode, action-based confidence bar) treats a workflow with
        // no transitions declared as the legacy 5-status flow — see
        // [`Workflow::fires_merge_bar`].
        transitions: None,
        git: None,
        zen: None,
    }
}

// ---------------------------------------------------------------------------
// Status

/// One step in a workflow.
///
/// `id` is the stable identifier — referenced from task frontmatter and
/// from [`Workflow::transitions`]; conventional form is lowercase
/// kebab-case (`backlog`, `in-progress`). Renaming `id` invalidates
/// every transition and task that references it, so it's intended to be
/// frozen at workflow-creation time.
///
/// `name` is the user-facing display label rendered in CLI listings and
/// the Kanban column header. Free to change without invalidating any
/// references — display lives here, stable references live in `id`.
///
/// `owner` is whose responsibility this status is when automation is
/// off — strict `user | agent`. `agent` is the optional name of the
/// agent the orchestrator dispatches to when automation is on; it
/// references a directory under the project's `agents/` workspace. A
/// `user`-owned status may still set `agent:` to declare "under Zen,
/// this agent can do the work without me"; a terminal status (Done /
/// Canceled) leaves it `None` to declare "no automation here, period."
///
/// On the wire `id` and `name` are both first-class fields. For
/// backward compatibility with workflow YAMLs that pre-date the split,
/// the deserializer accepts either field alone and uses it for both —
/// see [`convert_raw_workflow`].
///
/// `agent` is the optional per-workflow agent assignment — when set,
/// the orchestrator dispatches a task in this status to a worker
/// running that agent's prompt. The same status id can carry different
/// `agent` values across workflows; identity (`name`, `category`,
/// ordering) is project-wide and lives in `workflows/statuses.yml`
/// (loaded via [`Workflow::resolve_against`] when the loader joins the
/// two files).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub id: String,
    pub name: String,
    pub category: StatusCategory,
    pub owner: Owner,
    /// Which agent runs this status when automation is on. `None` means
    /// no automation path even under Zen. See module-level docs for the
    /// owner / agent split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl serde::Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        // Post-migration on-disk shape: `id` + `owner` + optional
        // `agent` only. `name` and `category` belong to
        // `workflows/statuses.yml`; emitting them here would duplicate
        // the source of truth and re-introduce the conflict class the
        // split was designed to eliminate. The loader's
        // [`Workflow::resolve_against`] step fills both back in on
        // read.
        use serde::ser::SerializeMap;
        let len = 2 + usize::from(self.agent.is_some());
        let mut m = s.serialize_map(Some(len))?;
        m.serialize_entry("id", &self.id)?;
        // `owner` round-trips through the `Owner` enum's snake_case
        // representation — call the existing Serialize impl by
        // reference so we don't have to duplicate the wire form here.
        m.serialize_entry("owner", &self.owner)?;
        if let Some(agent) = &self.agent {
            m.serialize_entry("agent", agent)?;
        }
        m.end()
    }
}

impl Status {
    /// Re-parse the raw on-disk form of a single status entry so the
    /// loader can detect "this workflow file still carries inline `name:`
    /// or `category:` after migration" — see
    /// [`Workflow::inline_identity_fields`]. The check needs the
    /// presence bits the regular [`Deserialize`] impl throws away.
    pub(crate) fn parse_presence(value: &serde_yaml::Value) -> (bool, bool) {
        let m = match value {
            serde_yaml::Value::Mapping(m) => m,
            _ => return (false, false),
        };
        let has_name = m
            .get(serde_yaml::Value::String("name".into()))
            .map(|v| !v.is_null())
            .unwrap_or(false);
        let has_category = m
            .get(serde_yaml::Value::String("category".into()))
            .map(|v| !v.is_null())
            .unwrap_or(false);
        (has_name, has_category)
    }
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
    /// Terminal — closed without shipping (cancelled, won't fix, duplicate, etc.).
    Archived,
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
            StatusCategory::Archived => "archived",
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
            "archived" => Ok(StatusCategory::Archived),
            other => Err(crate::Error::Other(format!(
                "unknown status category: {other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Owner

/// Who is expected to act when a task sits in a given status — when
/// automation is *off*.
///
/// - `User` keeps the task waiting; the orchestrator does not dispatch.
/// - `Agent` makes the task eligible for auto-dispatch onto a free workspace.
///
/// The closed vocabulary is intentional. The orthogonal "which agent
/// runs this when Zen is on" question lives in [`Status::agent`] — see
/// module-level docs for the split.
///
/// `Plans/workflows.md` §6 explicitly rejects a third "either" value — a
/// task is either work for a workspace or work for the user. If the user
/// wants to grab an agent-owned task, they reassign it through the
/// normal CLI/TUI; no schema field is needed for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    User,
    Agent,
}

/// The canonical handoff status id in the shipped default workflow —
/// the `id:` of the `Review` status in [`default_workflow`]. Retained as
/// a named constant for callers that need the conventional spelling;
/// [`Workflow::fires_merge_bar`] no longer keys off it (it keys off the
/// [`StatusCategory::Handoff`] category so renamed handoff ids still
/// trip the bar).
pub const LEGACY_REVIEW_STATUS: &str = "review";

/// Default agent name dispatched for a status whose legacy YAML used
/// bare `owner: agent` on a `ready`-category status. Matches the
/// `agents/orchestrator/` workspace materialized by `shelbi init`.
const DEFAULT_READY_AGENT: &str = "orchestrator";

/// Same idea as [`DEFAULT_READY_AGENT`] but for `active`-category
/// statuses, where the developer agent does the work.
const DEFAULT_ACTIVE_AGENT: &str = "developer";

/// One status entry in a workflow file that still carries pre-migration
/// inline identity fields (`name:` and/or `category:`). Returned by
/// [`Workflow::inline_identity_fields`] so the loader can build a "you
/// haven't migrated this status yet" error with the specific fields
/// listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineIdentityField {
    /// Status id (or `name`, if `id` wasn't supplied — pre-split YAMLs
    /// used `name` as the stable identifier).
    pub id: String,
    /// True iff this entry carries a top-level `name:` key.
    pub has_name: bool,
    /// True iff this entry carries a top-level `category:` key.
    pub has_category: bool,
}

// ---------------------------------------------------------------------------
// Transition + TransitionAction

/// One edge in a workflow's action graph. Declares the side-effects that
/// fire when a task moves from `from` to `to`, plus an optional `target:`
/// override for `merge` and `open_pr` actions that should land somewhere
/// other than the workflow's resolved `git.base_branch`. See
/// `Plans/workflows.md` §12.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transition {
    /// Stable id ([`Status::id`]) a task moves out of. Must match a
    /// declared status in the enclosing workflow.
    pub from: String,

    /// Stable id ([`Status::id`]) a task moves into. Must match a
    /// declared status in the enclosing workflow.
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
// Raw parsing types + legacy migration

/// Lenient raw shape used for YAML deserialization. Every field that
/// went through a legacy form before the two-field split is widened
/// here, then narrowed in [`convert_raw_workflow`]. Direct callers must
/// not depend on this type — it's an internal staging buffer.
#[derive(Deserialize)]
struct RawWorkflow {
    name: String,
    #[serde(default)]
    description: Option<String>,
    statuses: Vec<RawStatus>,
    #[serde(default)]
    initial_status: Option<String>,
    #[serde(default)]
    transitions: Option<Vec<Transition>>,
    #[serde(default)]
    git: Option<GitConfig>,
    #[serde(default)]
    zen: Option<WorkflowZenConfig>,
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Optional at the wire layer so post-migration workflow YAMLs
    /// (which carry only `id` + `owner` + optional `agent`) parse
    /// without erroring. When missing, [`convert_raw_workflow`]
    /// leaves a `Backlog` sentinel; the loader replaces it with the
    /// real value from `workflows/statuses.yml` via
    /// [`Workflow::resolve_against`].
    #[serde(default)]
    category: Option<StatusCategory>,
    /// Accepted as any string so legacy named-owner YAMLs can migrate
    /// rather than fail at the type layer. Anything other than `user` /
    /// `agent` becomes `Owner::Agent` + `agent: <raw>` in the conversion
    /// step.
    owner: String,
    #[serde(default)]
    agent: Option<String>,
    /// Accepted-and-discarded for legacy YAML compatibility; serde reads
    /// it but the conversion step doesn't need it.
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
}

/// Per-status migration record captured during [`convert_raw_workflow`].
/// Used to format the single bundled deprecation diagnostic returned by
/// [`Workflow::from_yaml_str_with_diagnostics`].
struct StatusMigration {
    status_id: String,
    kind: MigrationKind,
}

enum MigrationKind {
    /// Bare `owner: agent` with no `agent:` field — derived from
    /// category. Only `ready` (→ orchestrator) and `active` (→ developer)
    /// have defaults; other categories error out.
    BareAgentOwner { derived_agent: String },
    /// Legacy `owner: <name>` where `<name>` is something other than
    /// `user` / `agent`. Rewrites to `owner: agent, agent: <name>`.
    NamedOwner { original: String },
}

/// Convert a [`RawWorkflow`] into the public [`Workflow`], applying
/// legacy migrations and collecting per-status diagnostics. The caller
/// is responsible for running [`Workflow::validate`] on the result —
/// `convert_raw_workflow` only handles the parse-layer reshaping, not
/// the cross-reference checks.
fn convert_raw_workflow(raw: RawWorkflow) -> crate::Result<(Workflow, Vec<StatusMigration>)> {
    let mut statuses = Vec::with_capacity(raw.statuses.len());
    let mut migrations = Vec::new();

    for st in raw.statuses {
        let (id, name) = resolve_id_name(st.id, st.name)?;
        // Sentinel when `category:` is absent on the wire — the loader
        // fills it in from `workflows/statuses.yml` via
        // [`Workflow::resolve_against`] before any validation that
        // depends on the real value runs.
        let category = st.category.unwrap_or(StatusCategory::Backlog);
        let (owner, agent, migration) = resolve_owner_agent(&id, &st.owner, st.agent, category)?;
        if let Some(m) = migration {
            migrations.push(m);
        }
        statuses.push(Status {
            id,
            name,
            category,
            owner,
            agent,
        });
    }

    Ok((
        Workflow {
            name: raw.name,
            description: raw.description,
            statuses,
            initial_status: raw.initial_status,
            transitions: raw.transitions,
            git: raw.git,
            zen: raw.zen,
        },
        migrations,
    ))
}

/// Fill in the id↔name fallback for legacy workflow YAMLs that only
/// carried one of the two before the split.
fn resolve_id_name(
    id: Option<String>,
    name: Option<String>,
) -> crate::Result<(String, String)> {
    match (id, name) {
        (Some(id), Some(name)) => Ok((id, name)),
        (Some(id), None) => {
            let n = id.clone();
            Ok((id, n))
        }
        (None, Some(name)) => {
            let i = name.clone();
            Ok((i, name))
        }
        (None, None) => Err(workflow_err(
            "status requires at least one of `id` or `name`",
        )),
    }
}

/// Classify `raw_owner` and decide the post-migration `(owner, agent,
/// migration?)` tuple. The status `id` and `category` are needed for
/// the category-default migration and for diagnostic messages.
fn resolve_owner_agent(
    status_id: &str,
    raw_owner: &str,
    raw_agent: Option<String>,
    category: StatusCategory,
) -> crate::Result<(Owner, Option<String>, Option<StatusMigration>)> {
    match raw_owner {
        "user" => Ok((Owner::User, raw_agent, None)),
        "agent" => match raw_agent {
            Some(agent) => Ok((Owner::Agent, Some(agent), None)),
            None => {
                // Legacy single-field design. Derive `agent:` from
                // category for the two categories where a default makes
                // sense; everything else is an authoring bug we surface
                // immediately.
                let derived = match category {
                    StatusCategory::Ready => DEFAULT_READY_AGENT,
                    StatusCategory::Active => DEFAULT_ACTIVE_AGENT,
                    other => {
                        return Err(workflow_err(format!(
                            "status `{status_id}` has owner: agent but no agent: field \
                             — which agent should run here? (no category default for `{other}`)",
                        )));
                    }
                };
                Ok((
                    Owner::Agent,
                    Some(derived.to_string()),
                    Some(StatusMigration {
                        status_id: status_id.to_string(),
                        kind: MigrationKind::BareAgentOwner {
                            derived_agent: derived.to_string(),
                        },
                    }),
                ))
            }
        },
        other => {
            // Legacy named-owner design (`owner: alice`). Rewrite to
            // `owner: agent, agent: <name>` so the in-memory
            // representation is strict. A conflicting explicit `agent:`
            // is an authoring bug — refuse to silently pick a winner.
            if let Some(explicit) = raw_agent {
                if explicit != other {
                    return Err(workflow_err(format!(
                        "status `{status_id}`: legacy named owner `{other}` conflicts with \
                         explicit `agent: {explicit}` — drop one",
                    )));
                }
            }
            Ok((
                Owner::Agent,
                Some(other.to_string()),
                Some(StatusMigration {
                    status_id: status_id.to_string(),
                    kind: MigrationKind::NamedOwner {
                        original: other.to_string(),
                    },
                }),
            ))
        }
    }
}

/// Bundle every per-status migration into one human-readable warning
/// the loader can drop straight onto stderr. One warning per workflow
/// — even if multiple statuses migrated — keeps the noise floor low.
fn format_legacy_warning(workflow_name: &str, migrations: &[StatusMigration]) -> String {
    let mut buf = format!(
        "workflow `{workflow_name}` uses the legacy single-field owner form; \
         update to the two-field form (`owner: <user|agent>` + optional `agent: <name>`):"
    );
    for m in migrations {
        match &m.kind {
            MigrationKind::BareAgentOwner { derived_agent } => {
                buf.push_str(&format!(
                    "\n  - status `{id}`: bare `owner: agent` migrated to `owner: agent, agent: {derived_agent}` (derived from category)",
                    id = m.status_id,
                ));
            }
            MigrationKind::NamedOwner { original } => {
                buf.push_str(&format!(
                    "\n  - status `{id}`: legacy `owner: {original}` rewrote to `owner: agent, agent: {original}`",
                    id = m.status_id,
                ));
            }
        }
    }
    buf
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
  Six-status default.
statuses:
  - { id: backlog,     name: Backlog,    category: backlog,  owner: user,  agent: orchestrator }
  - { id: todo,        name: Todo,       category: ready,    owner: agent, agent: orchestrator }
  - { id: in-progress, name: InProgress, category: active,   owner: agent, agent: developer    }
  - { id: review,      name: Review,     category: handoff,  owner: user,  agent: orchestrator }
  - { id: done,        name: Done,       category: done,     owner: user  }
  - { id: canceled,    name: Canceled,   category: archived, owner: user  }
"#;

    #[test]
    fn parses_the_default_workflow_yaml() {
        let wf = Workflow::from_yaml_str(DEFAULT_YAML).expect("parse default");
        assert_eq!(wf.name, "default");
        assert_eq!(wf.statuses.len(), 6);
        assert_eq!(wf.statuses[0].id, "backlog");
        assert_eq!(wf.statuses[0].name, "Backlog");
        assert_eq!(wf.statuses[0].category, StatusCategory::Backlog);
        assert_eq!(wf.statuses[0].owner, Owner::User);
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
        assert_eq!(wf.statuses[2].id, "in-progress");
        assert_eq!(wf.statuses[2].category, StatusCategory::Active);
        assert_eq!(wf.statuses[2].owner, Owner::Agent);
        assert_eq!(wf.statuses[2].agent.as_deref(), Some("developer"));
        assert_eq!(wf.statuses[5].id, "canceled");
        assert_eq!(wf.statuses[5].name, "Canceled");
        assert_eq!(wf.statuses[5].category, StatusCategory::Archived);
        assert_eq!(wf.statuses[5].owner, Owner::User);
        assert!(wf.statuses[5].agent.is_none());
        assert!(wf.initial_status.is_none());
        assert!(wf.transitions.is_none());
    }

    #[test]
    fn two_field_form_parses_without_diagnostics() {
        // The canonical new shape — every `agent: ...` declared explicitly —
        // is the silent path. No deprecation warning fires.
        let (_, diags) = Workflow::from_yaml_str_with_diagnostics(DEFAULT_YAML).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
    }

    #[test]
    fn user_owned_status_with_agent_field_parses() {
        // A user-owned status MAY name an agent: under Zen that agent can
        // act without the user's hand. The new schema permits this even
        // though `owner: user` doesn't require an agent.
        let yaml = r#"
name: w
statuses:
  - { id: review, name: Review, category: handoff, owner: user, agent: orchestrator }
  - { id: done,   name: Done,   category: done,    owner: user                       }
"#;
        let (wf, diags) = Workflow::from_yaml_str_with_diagnostics(yaml).unwrap();
        assert!(diags.is_empty());
        assert_eq!(wf.statuses[0].owner, Owner::User);
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
        assert_eq!(wf.statuses[1].owner, Owner::User);
        assert!(wf.statuses[1].agent.is_none());
    }

    #[test]
    fn terminal_statuses_with_no_agent_parse_cleanly() {
        // Acceptance test (e): Done / Canceled are terminal — they
        // legitimately have no automation path. The schema must accept
        // them without complaint.
        let yaml = r#"
name: w
statuses:
  - { id: done,     name: Done,     category: done,     owner: user }
  - { id: canceled, name: Canceled, category: archived, owner: user }
"#;
        let (wf, diags) = Workflow::from_yaml_str_with_diagnostics(yaml).unwrap();
        assert!(diags.is_empty());
        assert!(wf.statuses.iter().all(|s| s.agent.is_none()));
    }

    #[test]
    fn legacy_yaml_without_id_falls_back_to_name() {
        // Workflow YAML authored before the id/name split (and the
        // built-in fixtures the original tests relied on) only carries
        // `name:`. The deserializer must fill `id` from `name` so old
        // files keep loading — the alternative would be silently
        // rejecting every existing on-disk workflow.
        let yaml = r#"
name: legacy
statuses:
  - { name: Backlog, category: backlog, owner: user }
  - { name: Review,  category: handoff, owner: user }
"#;
        let wf = Workflow::from_yaml_str(yaml).expect("legacy yaml parses");
        assert_eq!(wf.statuses[0].id, "Backlog");
        assert_eq!(wf.statuses[0].name, "Backlog");
        assert_eq!(wf.statuses[1].id, "Review");
        assert_eq!(wf.statuses[1].name, "Review");
    }

    #[test]
    fn yaml_with_only_id_uses_id_for_name() {
        // The mirror case of the legacy fallback — a workflow that
        // declares `id:` but leaves `name:` off uses the id as the
        // display label until a user picks a better one.
        let yaml = r#"
name: w
statuses:
  - { id: backlog, category: backlog, owner: user }
"#;
        let wf = Workflow::from_yaml_str(yaml).expect("yaml parses");
        assert_eq!(wf.statuses[0].id, "backlog");
        assert_eq!(wf.statuses[0].name, "backlog");
    }

    #[test]
    fn rejects_status_with_neither_id_nor_name() {
        let yaml = r#"
name: w
statuses:
  - { category: backlog, owner: user }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("at least one of `id` or `name`")),
            "missing id/name should be an invalid-workflow error, got: {err}"
        );
    }

    #[test]
    fn default_workflow_helper_matches_documented_table() {
        let wf = default_workflow();
        wf.validate().expect("built-in default validates");

        let ids: Vec<_> = wf.statuses.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["backlog", "todo", "in-progress", "review", "done", "canceled"]
        );

        let names: Vec<_> = wf.statuses.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Backlog", "Todo", "In Progress", "Review", "Done", "Canceled"]
        );

        let cats: Vec<_> = wf.statuses.iter().map(|s| s.category).collect();
        assert_eq!(
            cats,
            vec![
                StatusCategory::Backlog,
                StatusCategory::Ready,
                StatusCategory::Active,
                StatusCategory::Handoff,
                StatusCategory::Done,
                StatusCategory::Archived,
            ]
        );

        let owners: Vec<_> = wf.statuses.iter().map(|s| s.owner).collect();
        assert_eq!(
            owners,
            vec![
                Owner::User,
                Owner::Agent,
                Owner::Agent,
                Owner::User,
                Owner::User,
                Owner::User,
            ]
        );

        // The two-field design: each non-terminal status names the agent
        // that runs it under Zen. Terminal Done / Canceled stay None.
        let agents: Vec<Option<&str>> =
            wf.statuses.iter().map(|s| s.agent.as_deref()).collect();
        assert_eq!(
            agents,
            vec![
                Some("orchestrator"),
                Some("orchestrator"),
                Some("developer"),
                Some("orchestrator"),
                None,
                None,
            ]
        );
    }

    #[test]
    fn round_trips_through_yaml_via_resolve_against() {
        // Post-migration round-trip: serialize the compact form, parse
        // it back, resolve against the canonical `statuses.yml`. The
        // wire form drops `name:`/`category:` from each status entry —
        // identity lives in `workflows/statuses.yml` after migration.
        let wf = default_workflow();
        let y = serde_yaml::to_string(&wf).unwrap();
        // Identity fields are absent on the wire — that's the contract.
        assert!(!y.contains("category:"), "unexpected category: in {y}");
        // `name:` only appears as the top-level workflow name, never on
        // a status entry.
        for line in y.lines() {
            assert!(
                !line.trim_start().starts_with("name:") || !line.starts_with("  -")
                    && !line.starts_with("    name:"),
                "unexpected per-status name: in {y}",
            );
        }
        let back = Workflow::from_yaml_str(&y)
            .unwrap()
            .resolve_against(&crate::default_project_statuses())
            .unwrap();
        assert_eq!(wf, back);
    }

    #[test]
    fn optional_fields_omitted_when_absent() {
        let wf = default_workflow();
        let y = serde_yaml::to_string(&wf).unwrap();
        // Nothing was set, so neither key should appear on the wire.
        assert!(!y.contains("initial_status"));
        assert!(!y.contains("transitions"));
    }

    #[test]
    fn archived_category_round_trips_through_yaml_and_from_str() {
        use std::str::FromStr;

        // YAML deserialization (serde rename_all = "snake_case") accepts
        // `archived` as a category — a workflow with a `Canceled`/`Won't
        // Fix` terminal status needs this to land on disk via
        // `workflows/statuses.yml`.
        let yaml = r#"
statuses:
  - { id: canceled, name: Canceled, category: archived }
"#;
        let ps = crate::ProjectStatuses::from_yaml_str(yaml).unwrap();
        assert_eq!(ps.statuses[0].category, StatusCategory::Archived);

        // Round-trip back out — wire form is the snake_case spelling.
        let y = serde_yaml::to_string(&ps).unwrap();
        assert!(y.contains("category: archived"));

        // FromStr (used by the events-log parser to lift
        // `from_category=`/`to_category=` tokens) accepts it too.
        assert_eq!(
            StatusCategory::from_str("archived").unwrap(),
            StatusCategory::Archived,
        );
        assert_eq!(StatusCategory::Archived.as_str(), "archived");
        assert_eq!(StatusCategory::Archived.to_string(), "archived");
    }

    #[test]
    fn either_owner_migrates_to_named_agent() {
        // Plans/workflows.md §6 closes the in-memory Owner vocabulary to
        // `user` / `agent`. A YAML that says `owner: either` (or any
        // other non-standard name) is the legacy named-owner design —
        // the loader rewrites it to `owner: agent, agent: either` and
        // surfaces a deprecation diagnostic. The downstream agent-
        // existence check (in the state-layer loader) then catches that
        // `either` isn't a real agent.
        let yaml = r#"
name: w
statuses:
  - { id: open, name: Open, category: ready, owner: either }
"#;
        let (wf, diags) = Workflow::from_yaml_str_with_diagnostics(yaml).unwrap();
        assert_eq!(wf.statuses[0].owner, Owner::Agent);
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("either"));
        assert_eq!(diags.len(), 1, "expected one bundled diagnostic, got {diags:?}");
        assert!(diags[0].contains("legacy"));
        assert!(diags[0].contains("either"));
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::InvalidWorkflow(ref m) if m.contains("name must not be empty")));
    }

    #[test]
    fn rejects_blank_status_id() {
        let yaml = r#"
name: w
statuses:
  - { id: "", name: Todo, category: ready, owner: agent, agent: orchestrator }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::InvalidWorkflow(ref m) if m.contains("status id must not be empty")));
    }

    #[test]
    fn rejects_blank_status_name_when_id_is_set() {
        // Both id and name are required non-empty after the split. A
        // workflow that nulls out name (`name: ""`) while keeping a real
        // id still has to render a header somewhere — surface the error
        // at validation time.
        let yaml = r#"
name: w
statuses:
  - { id: todo, name: "", category: ready, owner: agent, agent: orchestrator }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("name must not be empty")),
            "got: {err}",
        );
    }

    #[test]
    fn rejects_duplicate_status_ids() {
        let yaml = r#"
name: w
statuses:
  - { id: todo, name: Todo,    category: ready,  owner: agent, agent: orchestrator }
  - { id: todo, name: TodoTwo, category: active, owner: agent, agent: developer    }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("duplicate status id")),
            "got: {err}",
        );
    }

    #[test]
    fn duplicate_names_are_allowed_when_ids_differ() {
        // Names are display-only; two distinct ids may render the same
        // human-readable label without that being a validation failure.
        // (Likely unusual in practice — but it's the user's call, not
        // ours.)
        let yaml = r#"
name: w
statuses:
  - { id: review-a, name: Review, category: handoff, owner: user, agent: orchestrator }
  - { id: review-b, name: Review, category: handoff, owner: user, agent: orchestrator }
"#;
        Workflow::from_yaml_str(yaml).expect("distinct ids with same name validate");
    }

    #[test]
    fn rejects_unknown_initial_status() {
        let yaml = r#"
name: w
statuses:
  - { id: todo, name: Todo, category: ready, owner: agent, agent: orchestrator }
initial_status: backlog
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::InvalidWorkflow(ref m) if m.contains("initial_status")));
    }

    #[test]
    fn accepts_known_initial_status_and_resolves_it() {
        let yaml = r#"
name: w
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user                       }
  - { id: todo,    name: Todo,    category: ready,   owner: agent, agent: orchestrator }
initial_status: todo
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        assert_eq!(wf.resolved_initial_status(), Some("todo"));
    }

    #[test]
    fn resolved_initial_status_falls_back_to_first_status_id() {
        let wf = default_workflow();
        // First status's `id`, not `name` — the resolved value feeds
        // transition lookups and task frontmatter, both keyed by id.
        assert_eq!(wf.resolved_initial_status(), Some("backlog"));
    }

    #[test]
    fn rejects_transition_from_undeclared_status() {
        let yaml = r#"
name: w
statuses:
  - { id: todo, name: Todo, category: ready, owner: agent, agent: orchestrator }
transitions:
  - { from: bogus, to: todo, actions: [push_branch] }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("undeclared status id `bogus`")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_transition_to_undeclared_status() {
        let yaml = r#"
name: w
statuses:
  - { id: todo, name: Todo, category: ready, owner: agent, agent: orchestrator }
  - { id: done, name: Done, category: done,  owner: user                       }
transitions:
  - { from: todo, to: phantom, actions: [merge] }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidWorkflow(ref m) if m.contains("undeclared status id `phantom`")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_duplicate_transition_edge() {
        let yaml = r#"
name: w
statuses:
  - { id: todo, name: Todo, category: ready, owner: agent, agent: orchestrator }
  - { id: done, name: Done, category: done,  owner: user                       }
transitions:
  - { from: todo, to: done, actions: [merge] }
  - { from: todo, to: done, actions: [close_pr] }
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
        // Both endpoints exist → allowed, no whitelist semantic. The
        // lookup is keyed by `id`, so the kebab-case stable identifiers
        // are what cross the wire — the PascalCase display names are
        // not addressable here.
        assert!(wf.transition_allowed("backlog", "done"));
        assert!(wf.transition_allowed("done", "backlog"));
        // PascalCase names (the old hardcoded form) no longer resolve —
        // they aren't ids anymore.
        assert!(!wf.transition_allowed("Backlog", "Done"));
        // Unknown id is never reachable.
        assert!(!wf.transition_allowed("ghost", "done"));
        assert!(!wf.transition_allowed("backlog", "ghost"));
    }

    #[test]
    fn transition_allowed_does_not_restrict_when_transitions_declared() {
        // §11 in the workflows plan: declaring a `transitions:` block only
        // adds *side-effects* to edges. It does NOT restrict which moves
        // are legal — any declared status can move to any other.
        let yaml = r#"
name: w
statuses:
  - { name: Todo,    category: ready,  owner: agent, agent: orchestrator }
  - { name: Doing,   category: active, owner: agent, agent: developer    }
  - { name: Done,    category: done,   owner: user                       }
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
  - { name: Doing,  category: active,  owner: agent, agent: developer    }
  - { name: Review, category: handoff, owner: user                       }
  - { name: Done,   category: done,    owner: user                       }
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
  - { name: Doing,  category: active,  owner: agent, agent: developer    }
  - { name: Review, category: handoff, owner: user                       }
  - { name: Done,   category: done,    owner: user                       }
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
    fn fires_merge_bar_falls_back_to_handoff_category_when_no_transitions() {
        // A workflow with no `transitions:` block (the default, what
        // existing projects have on disk) keeps the historic "handoff →
        // Done" trigger. Lets existing zen users not have to edit YAML to
        // keep the bar firing. The trigger is keyed by the *category*, so
        // the canonical `review` id (category handoff) trips it — display
        // labels (`Review`) and non-handoff statuses never do.
        let wf = default_workflow();
        assert!(wf.transitions.is_none(), "default ships without transitions");
        assert!(wf.fires_merge_bar("review"));
        assert!(!wf.fires_merge_bar("Review"), "name is not an id");
        assert!(!wf.fires_merge_bar("in-progress"));
        assert!(!wf.fires_merge_bar("backlog"));
        // Unknown ids don't trip the bar.
        assert!(!wf.fires_merge_bar("ghost"));
    }

    #[test]
    fn fires_merge_bar_honors_renamed_handoff_id_when_no_transitions() {
        // A project renames its handoff status id from `review` to `qa`
        // and keeps a default-style workflow with no `transitions:` block.
        // The merge-bar fallback keys off the handoff *category*, not the
        // literal `review` id, so `qa` still trips the bar — the caller's
        // `category == Handoff` gate in the Zen dry-run probe is honored
        // rather than silently defeated.
        let statuses = crate::ProjectStatuses::from_yaml_str(
            "statuses:\n  \
             - { id: doing, name: Doing, category: active }\n  \
             - { id: qa,    name: QA,    category: handoff }\n  \
             - { id: done,  name: Done,  category: done }\n",
        )
        .unwrap();
        let wf = Workflow::from_yaml_str(
            "name: w\nstatuses:\n  \
             - { id: doing, owner: agent, agent: developer }\n  \
             - { id: qa,    owner: user }\n  \
             - { id: done,  owner: user }\n",
        )
        .unwrap()
        .resolve_against(&statuses)
        .unwrap();
        assert!(wf.transitions.is_none());
        assert!(wf.fires_merge_bar("qa"), "renamed handoff id trips the bar");
        assert!(!wf.fires_merge_bar("doing"));
        assert!(!wf.fires_merge_bar("done"));
    }

    #[test]
    fn trunk_based_workflow_fires_bar_on_active_to_done() {
        // Worked example from `Plans/workflows.md` §8: a workflow that
        // skips `Review` and goes straight `InProgress -> Done` with a
        // merge action gets the same high bar — no special-casing.
        let yaml = r#"
name: trunk
statuses:
  - { name: Doing, category: active, owner: agent, agent: developer }
  - { name: Done,  category: done,   owner: user                    }
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
  - { name: Todo, category: pending, owner: agent, agent: orchestrator }
"#;
        // serde rejects this at the type level — InvalidWorkflow is for
        // semantic errors, parse-time errors surface as Error::Yaml.
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        assert!(matches!(err, Error::Yaml(_)), "got: {err}");
    }

    // ---------------------------------------------------------------------
    // Two-field owner/agent design — strict + legacy migration

    #[test]
    fn owner_agent_with_explicit_agent_field_parses_without_warning() {
        let yaml = r#"
name: w
statuses:
  - { id: todo, name: Todo, category: ready, owner: agent, agent: orchestrator }
"#;
        let (wf, diags) = Workflow::from_yaml_str_with_diagnostics(yaml).unwrap();
        assert!(diags.is_empty(), "explicit two-field form should not warn: {diags:?}");
        assert_eq!(wf.statuses[0].owner, Owner::Agent);
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
    }

    #[test]
    fn owner_agent_without_agent_field_in_done_category_hard_errors() {
        // Acceptance test (b): bare `owner: agent` on a category that has
        // no default migration target must surface immediately with a
        // diagnostic that names the status and explains the rule.
        let yaml = r#"
name: w
statuses:
  - { id: ship, name: Ship, category: done, owner: agent }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        match &err {
            Error::InvalidWorkflow(msg) => {
                assert!(msg.contains("`ship`"), "msg: {msg}");
                assert!(msg.contains("owner: agent"), "msg: {msg}");
                assert!(msg.contains("agent:"), "msg: {msg}");
            }
            other => panic!("expected InvalidWorkflow, got {other:?}"),
        }
    }

    #[test]
    fn legacy_bare_owner_agent_migrates_by_category_with_one_warning() {
        // Acceptance test (d): bare `owner: agent` on the two categories
        // that have defaults (ready → orchestrator, active → developer)
        // migrates silently in-memory and surfaces a single bundled
        // deprecation diagnostic.
        let yaml = r#"
name: legacy
statuses:
  - { id: todo,     name: Todo,     category: ready,  owner: agent }
  - { id: doing,    name: Doing,    category: active, owner: agent }
"#;
        let (wf, diags) = Workflow::from_yaml_str_with_diagnostics(yaml).unwrap();
        assert_eq!(wf.statuses[0].owner, Owner::Agent);
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
        assert_eq!(wf.statuses[1].owner, Owner::Agent);
        assert_eq!(wf.statuses[1].agent.as_deref(), Some("developer"));
        // Exactly one diagnostic, even though two statuses migrated.
        assert_eq!(diags.len(), 1, "expected one bundled diagnostic, got: {diags:?}");
        let msg = &diags[0];
        assert!(msg.contains("legacy"), "msg: {msg}");
        assert!(msg.contains("`todo`"), "msg: {msg}");
        assert!(msg.contains("`doing`"), "msg: {msg}");
        assert!(msg.contains("orchestrator"), "msg: {msg}");
        assert!(msg.contains("developer"), "msg: {msg}");
    }

    #[test]
    fn legacy_named_owner_rewrites_to_owner_agent_plus_agent_field() {
        let yaml = r#"
name: legacy
statuses:
  - { id: design, name: Design, category: active, owner: alice }
"#;
        let (wf, diags) = Workflow::from_yaml_str_with_diagnostics(yaml).unwrap();
        assert_eq!(wf.statuses[0].owner, Owner::Agent);
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("alice"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].contains("alice"));
    }

    #[test]
    fn named_owner_conflicting_with_explicit_agent_errors() {
        // Authoring bug: someone wrote both `owner: alice` (legacy named
        // owner that would migrate to `agent: alice`) AND `agent: bob`
        // (explicit two-field form). The two disagree — refuse rather
        // than silently picking one.
        let yaml = r#"
name: w
statuses:
  - { id: design, name: Design, category: active, owner: alice, agent: bob }
"#;
        let err = Workflow::from_yaml_str(yaml).unwrap_err();
        match &err {
            Error::InvalidWorkflow(msg) => {
                assert!(msg.contains("alice"), "msg: {msg}");
                assert!(msg.contains("bob"), "msg: {msg}");
                assert!(msg.contains("conflict") || msg.contains("drop one"), "msg: {msg}");
            }
            other => panic!("expected InvalidWorkflow, got {other:?}"),
        }
    }

    #[test]
    fn named_owner_matching_explicit_agent_accepts_silently_via_migration() {
        // Redundant-but-consistent legacy form: same name on both fields.
        // The migration still rewrites owner to `agent`, keeping the agent
        // name; this is not an error.
        let yaml = r#"
name: w
statuses:
  - { id: design, name: Design, category: active, owner: alice, agent: alice }
"#;
        let (wf, _diags) = Workflow::from_yaml_str_with_diagnostics(yaml).unwrap();
        assert_eq!(wf.statuses[0].owner, Owner::Agent);
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("alice"));
    }

    #[test]
    fn validate_rejects_owner_agent_without_agent_field_for_directly_constructed_workflow() {
        // Direct construction (bypassing convert_raw_workflow) is the path
        // a programmatic builder might take. validate() is the safety net.
        let wf = Workflow {
            name: "w".into(),
            description: None,
            statuses: vec![Status {
                id: "todo".into(),
                name: "Todo".into(),
                category: StatusCategory::Ready,
                owner: Owner::Agent,
                agent: None,
            }],
            initial_status: None,
            transitions: None,
            git: None,
            zen: None,
        };
        let err = wf.validate().unwrap_err();
        match &err {
            Error::InvalidWorkflow(msg) => {
                assert!(msg.contains("`todo`"), "msg: {msg}");
                assert!(msg.contains("owner: agent"), "msg: {msg}");
            }
            other => panic!("expected InvalidWorkflow, got {other:?}"),
        }
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
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
        // Post-migration round-trip: identity fields (`name`, `category`)
        // live in `workflows/statuses.yml`, so the workflow YAML round-trip
        // goes through `resolve_against` to refill them.
        let yaml = r#"
name: feature-task
statuses:
  - { id: todo, name: Todo, category: ready, owner: agent, agent: orchestrator }
git:
  base_branch: feature/{{feature}}
  merge_strategy: merge
"#;
        let wf = Workflow::from_yaml_str(yaml).unwrap();
        let statuses = crate::ProjectStatuses::from_yaml_str(
            "statuses:\n  \
             - { id: todo, name: Todo, category: ready }\n  \
             - { id: done, name: Done, category: done }\n",
        )
        .unwrap();
        let serialized = serde_yaml::to_string(&wf).unwrap();
        let back = Workflow::from_yaml_str(&serialized)
            .unwrap()
            .resolve_against(&statuses)
            .unwrap();
        assert_eq!(wf, back);
    }

    // ---------------------------------------------------------------------
    // zen: block (per-workflow override of project zen config)

    #[test]
    fn workflow_zen_block_parses_all_three_overrides() {
        let yaml = r#"
name: research
statuses:
  - { name: Drafting, category: active, owner: agent, agent: developer }
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
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
  - { name: Todo, category: ready, owner: agent, agent: orchestrator }
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
  - { id: drafting, name: Drafting, category: active, owner: agent, agent: developer }
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
        let statuses = crate::ProjectStatuses::from_yaml_str(
            "statuses:\n  \
             - { id: drafting, name: Drafting, category: active }\n  \
             - { id: done, name: Done, category: done }\n",
        )
        .unwrap();
        let serialized = serde_yaml::to_string(&wf).unwrap();
        let back = Workflow::from_yaml_str(&serialized)
            .unwrap()
            .resolve_against(&statuses)
            .unwrap();
        assert_eq!(wf, back);
        // ci_timeout serializes as a bare integer (no struct form).
        assert!(
            serialized.contains("ci_timeout: 600"),
            "expected `ci_timeout: 600` in serialized form, got:\n{serialized}"
        );
    }

    // ---------------------------------------------------------------------
    // resolve_against + inline-identity probe (statuses.yml)

    #[test]
    fn resolve_against_fills_name_and_category_from_project_statuses() {
        // Workflow declares status ids only — identity comes from the
        // canonical `statuses.yml`. The resolver fills `name` +
        // `category` from there.
        let workflow_yaml = r#"
name: app
statuses:
  - { id: backlog, owner: user  }
  - { id: review,  owner: user, agent: orchestrator }
  - { id: done,    owner: user  }
"#;
        let wf = Workflow::from_yaml_str(workflow_yaml).unwrap();
        let resolved = wf
            .resolve_against(&crate::default_project_statuses())
            .unwrap();
        assert_eq!(resolved.statuses[0].name, "Backlog");
        assert_eq!(resolved.statuses[0].category, StatusCategory::Backlog);
        assert_eq!(resolved.statuses[1].name, "Review");
        assert_eq!(resolved.statuses[1].category, StatusCategory::Handoff);
        assert_eq!(resolved.statuses[1].agent.as_deref(), Some("orchestrator"));
        assert_eq!(resolved.statuses[2].name, "Done");
        assert_eq!(resolved.statuses[2].category, StatusCategory::Done);
    }

    #[test]
    fn resolve_against_rejects_unknown_status_id_and_lists_available() {
        // The error must echo the full available list — the contract in
        // `Plans/shared-statuses.md` (Loader validation rules) so the
        // user can immediately see what they probably meant to type.
        let workflow_yaml = r#"
name: app
statuses:
  - { id: ghost, owner: user }
"#;
        let wf = Workflow::from_yaml_str(workflow_yaml).unwrap();
        let err = wf
            .resolve_against(&crate::default_project_statuses())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("workflow `app`"), "msg: {msg}");
        assert!(msg.contains("status id `ghost`"), "msg: {msg}");
        assert!(msg.contains("`backlog`"), "msg: {msg}");
        assert!(msg.contains("`todo`"), "msg: {msg}");
        assert!(msg.contains("`in-progress`"), "msg: {msg}");
        assert!(msg.contains("`review`"), "msg: {msg}");
        assert!(msg.contains("`done`"), "msg: {msg}");
        assert!(msg.contains("`canceled`"), "msg: {msg}");
    }

    #[test]
    fn inline_identity_fields_detects_legacy_form() {
        // The loader uses this probe to refuse a workflow that still
        // carries inline `name:` / `category:` after `statuses.yml` is
        // already on disk — the post-migration "mixed forms" hard-fail.
        let yaml = r#"
name: w
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user }
  - { id: review,  owner: user }
"#;
        let found = Workflow::inline_identity_fields(yaml).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "backlog");
        assert!(found[0].has_name);
        assert!(found[0].has_category);
    }

    #[test]
    fn inline_identity_fields_returns_empty_for_reference_form() {
        let yaml = r#"
name: w
statuses:
  - { id: backlog, owner: user }
  - { id: review,  owner: user, agent: orchestrator }
"#;
        let found = Workflow::inline_identity_fields(yaml).unwrap();
        assert!(found.is_empty(), "expected empty, got: {found:?}");
    }

    #[test]
    fn status_serializes_only_id_owner_and_agent() {
        // Post-migration on-disk shape contract: never emit `name:` or
        // `category:` inside a status entry. Anything that does would
        // re-introduce the conflict class the split eliminated.
        let s = Status {
            id: "review".into(),
            name: "Review".into(),
            category: StatusCategory::Handoff,
            owner: Owner::User,
            agent: Some("orchestrator".into()),
        };
        let y = serde_yaml::to_string(&s).unwrap();
        assert!(y.contains("id: review"), "got: {y}");
        assert!(y.contains("owner: user"), "got: {y}");
        assert!(y.contains("agent: orchestrator"), "got: {y}");
        assert!(!y.contains("name:"), "unexpected name: in {y}");
        assert!(!y.contains("category:"), "unexpected category: in {y}");
    }

    #[test]
    fn status_serializes_without_agent_when_none() {
        let s = Status {
            id: "todo".into(),
            name: "Todo".into(),
            category: StatusCategory::Ready,
            owner: Owner::Agent,
            agent: None,
        };
        let y = serde_yaml::to_string(&s).unwrap();
        assert!(!y.contains("agent:"), "unexpected agent: in {y}");
    }
}
