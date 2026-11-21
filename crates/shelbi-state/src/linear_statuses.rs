//! A Linear project's status catalog: `workflows/statuses.yaml` synced from
//! the team's workflow states.
//!
//! When a project's tracker is Linear, its statuses come from Linear. The
//! catalog is written with `managed_by: linear`, and each status carries the
//! Linear state's uuid as `external_id`. Settings then shows the set, names
//! and order read-only ([`crate::workflow_config::WorkflowConfig::statuses_read_only`]);
//! only categories stay editable. Workflows keep referencing status ids
//! exactly as they do for a hand-written catalog.
//!
//! ## Sync rules
//!
//! * One status per state, in Linear's `position` order.
//! * **Ids are assigned once per state and never change.** A new state gets
//!   the kebab-case of its name (`In Review` -> `in-review`, made unique with
//!   `-2`, `-3`, ...). A state renamed in Linear keeps its id and only its
//!   `name` changes, so no issue or workflow that names the id is orphaned.
//!   (The alternative, re-slugging and migrating every reference, would have
//!   to reach issues in the tracker and every workflow; a stable id needs
//!   neither.)
//! * Category defaults by state type: triage / backlog -> backlog,
//!   unstarted -> ready, started -> active (handoff when the name matches
//!   `review`, `qa`, `verif` or `test`), completed -> done, canceled and
//!   duplicate -> archived.
//! * A category the user changed is kept across re-syncs. A status's
//!   category counts as changed when it differs from the default for its
//!   type and its name *as the catalog had it*; then a rename in Linear
//!   updates the name and keeps the category. An unchanged one follows the
//!   new name (renaming `Doing` to `In Review` moves it to handoff).
//! * A state removed in Linear leaves the catalog. Its id and category are
//!   remembered in `linear.json` (`retired_statuses`) so
//!   [`plan_workflow_rewrites`] can still map a workflow that names it.
//!
//! ## Triggers
//!
//! * [`plan_connect_to`] then [`connect`]: when a project is pointed at
//!   Linear (the connect flows). The plan reads the team's states and maps
//!   the project's workflows onto them without writing anything, so a status
//!   no team status can take ([`ConnectPlan::refusal`]) refuses the
//!   connection before it's written. [`connect`] then syncs and applies the
//!   rewrites, so the project loads right after.
//! * [`sync`]: `shelbi tracker linear sync`.
//! * [`refresh_if_changed`]: the daemon's board refresh. It reads the states
//!   (one small query, at most every [`STATES_CHECK_INTERVAL`]), compares a
//!   hash of their ids, names, types and positions with the one cached in
//!   `~/.shelbi/projects/<name>/linear.json`, and syncs only when they differ.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use shelbi_core::{
    default_project_statuses, Error, IssueTrackerBackend, ProjectStatus, ProjectStatuses, Result,
    StatusCategory, TrackerName, Workflow,
};

use crate::linear_client::{
    self, filter_cache_path, LinearClient, LinearState, LinearStateType,
};
use crate::workflow_config::{kebab, raw_status_ids, workflow_files};

/// The caller name sync requests carry in `linear-requests.log`.
const CALLER: &str = "statuses";

/// How often [`refresh_if_changed`] reads the team's states at most.
pub const STATES_CHECK_INTERVAL: Duration = Duration::from_secs(300);

/// `linear.json` keys this module owns (beside the filter cache's).
const STATES_HASH_KEY: &str = "states_hash";
const STATES_CHECKED_KEY: &str = "states_checked_at";
const RETIRED_KEY: &str = "retired_statuses";

/// The header a synced `statuses.yaml` starts with.
fn managed_header(team: Option<&str>) -> String {
    let team = team.map(|t| format!(" {t}")).unwrap_or_default();
    format!(
        "# Statuses synced from the Linear team{team}.\n\
         #\n\
         # Linear owns the set, the names and the order: change them in Linear and\n\
         # Shelbi re-syncs on the next board refresh (or run\n\
         # `shelbi tracker linear sync`). Each status's `category` is Shelbi's and\n\
         # stays yours to edit; a re-sync keeps it. `external_id` is the Linear\n\
         # state's id; `id` is assigned once and never changes.\n"
    )
}

// ---------------------------------------------------------------------------
// The pure rules

/// The category a Linear state gets unless the user picks another.
pub fn default_category(kind: LinearStateType, name: &str) -> StatusCategory {
    match kind {
        LinearStateType::Triage | LinearStateType::Backlog => StatusCategory::Backlog,
        LinearStateType::Unstarted => StatusCategory::Ready,
        LinearStateType::Started if is_review_name(name) => StatusCategory::Handoff,
        LinearStateType::Started => StatusCategory::Active,
        LinearStateType::Completed => StatusCategory::Done,
        LinearStateType::Canceled | LinearStateType::Duplicate => StatusCategory::Archived,
        // A type a newer Linear added: park it where nothing dispatches.
        LinearStateType::Unknown => StatusCategory::Backlog,
    }
}

/// Whether `category` is what an older Shelbi gave a state of `kind` by
/// default, so a re-sync treats it as the default rather than the user's
/// pick. Before Shelbi knew Linear's `duplicate` type it read as unknown and
/// landed in backlog.
fn legacy_default(kind: LinearStateType, category: StatusCategory) -> bool {
    kind == LinearStateType::Duplicate && category == StatusCategory::Backlog
}

/// A started state whose name reads as a review step (`/review|qa|verif|test/i`).
fn is_review_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["review", "qa", "verif", "test"].iter().any(|w| lower.contains(w))
}

/// The id a new status named `name` gets: its kebab-case (`status` when the
/// name has no letters or digits).
pub fn slug(name: &str) -> String {
    let s = kebab(name);
    if s.is_empty() {
        "status".to_string()
    } else {
        s
    }
}

/// The catalog `states` make from `current` (see the module docs for the
/// rules). Pure: reads and writes nothing.
pub fn plan_catalog(current: &ProjectStatuses, states: &[LinearState]) -> Result<ProjectStatuses> {
    let mut states: Vec<&LinearState> = states.iter().collect();
    states.sort_by(|a, b| a.position.total_cmp(&b.position));

    // Only a managed catalog's entries are matched: a hand-written catalog's
    // statuses have no external id, and the first sync starts from defaults.
    let known: HashMap<&str, &ProjectStatus> = current
        .statuses
        .iter()
        .filter_map(|s| s.external_id.as_deref().map(|e| (e, s)))
        .collect();

    // Ids kept by matched states are taken first, so a new state can never
    // claim the id of one that was renamed away from it.
    let mut taken: HashSet<String> = states
        .iter()
        .filter_map(|st| known.get(st.id.as_str()).map(|s| s.id.clone()))
        .collect();

    let mut out = Vec::with_capacity(states.len());
    for st in states {
        let default = default_category(st.kind, &st.name);
        let status = match known.get(st.id.as_str()) {
            Some(prev) => {
                let overridden = prev.category != default_category(st.kind, &prev.name)
                    && !legacy_default(st.kind, prev.category);
                ProjectStatus {
                    id: prev.id.clone(),
                    name: st.name.clone(),
                    category: if overridden { prev.category } else { default },
                    external_id: Some(st.id.clone()),
                }
            }
            None => {
                let base = slug(&st.name);
                let mut id = base.clone();
                let mut n = 2;
                while taken.contains(&id) {
                    id = format!("{base}-{n}");
                    n += 1;
                }
                taken.insert(id.clone());
                ProjectStatus {
                    id,
                    name: st.name.clone(),
                    category: default,
                    external_id: Some(st.id.clone()),
                }
            }
        };
        out.push(status);
    }
    let catalog = ProjectStatuses {
        managed_by: Some(TrackerName::Linear),
        statuses: out,
    };
    catalog.validate().map_err(|e| {
        Error::InvalidProjectStatuses(format!("the Linear team's states don't make a usable status catalog: {e}"))
    })?;
    Ok(catalog)
}

/// One difference between two catalogs, as `sync` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogChange {
    /// The catalog is now (or no longer) synced from a tracker.
    Managed { tracker: Option<TrackerName> },
    Added { id: String, name: String, category: StatusCategory },
    Renamed { id: String, from: String, to: String },
    Category { id: String, from: StatusCategory, to: StatusCategory },
    Removed { id: String, name: String },
    /// The statuses both catalogs have are in a different order; `order` is
    /// the new one.
    Reordered { order: Vec<String> },
}

impl std::fmt::Display for CatalogChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogChange::Managed { tracker: Some(t) } => write!(f, "~ statuses are now managed by {t}"),
            CatalogChange::Managed { tracker: None } => write!(f, "~ statuses are no longer managed by a tracker"),
            CatalogChange::Added { id, name, category } => write!(f, "+ {id} ({name}, {category})"),
            CatalogChange::Renamed { id, from, to } => write!(f, "~ {id}: renamed {from:?} -> {to:?}"),
            CatalogChange::Category { id, from, to } => write!(f, "~ {id}: category {from} -> {to}"),
            CatalogChange::Removed { id, name } => write!(f, "- {id} ({name})"),
            CatalogChange::Reordered { order } => write!(f, "~ order: {}", order.join(", ")),
        }
    }
}

/// What changed from `before` to `after`. Statuses match by external id
/// when both have one, else by id.
pub fn diff_catalogs(before: &ProjectStatuses, after: &ProjectStatuses) -> Vec<CatalogChange> {
    let mut out = Vec::new();
    if before.managed_by != after.managed_by {
        out.push(CatalogChange::Managed { tracker: after.managed_by });
    }
    let find = |s: &ProjectStatus| -> Option<usize> {
        s.external_id
            .as_deref()
            .and_then(|e| before.statuses.iter().position(|b| b.external_id.as_deref() == Some(e)))
            .or_else(|| before.statuses.iter().position(|b| b.id == s.id))
    };
    let mut matched: Vec<usize> = Vec::new();
    let mut changes = Vec::new();
    for s in &after.statuses {
        match find(s) {
            Some(i) => {
                let b = &before.statuses[i];
                matched.push(i);
                if b.name != s.name {
                    changes.push(CatalogChange::Renamed { id: s.id.clone(), from: b.name.clone(), to: s.name.clone() });
                }
                if b.category != s.category {
                    changes.push(CatalogChange::Category { id: s.id.clone(), from: b.category, to: s.category });
                }
            }
            None => changes.push(CatalogChange::Added {
                id: s.id.clone(),
                name: s.name.clone(),
                category: s.category,
            }),
        }
    }
    for (i, b) in before.statuses.iter().enumerate() {
        if !matched.contains(&i) {
            changes.push(CatalogChange::Removed { id: b.id.clone(), name: b.name.clone() });
        }
    }
    if matched.windows(2).any(|w| w[0] > w[1]) {
        changes.push(CatalogChange::Reordered { order: after.ids().iter().map(|s| s.to_string()).collect() });
    }
    out.extend(changes);
    out
}

/// The hash `refresh_if_changed` compares: the states' ids, names, types and
/// positions, in position order.
pub fn states_hash(states: &[LinearState]) -> String {
    let mut sorted: Vec<&LinearState> = states.iter().collect();
    sorted.sort_by(|a, b| a.position.total_cmp(&b.position));
    let mut canon = String::new();
    for s in sorted {
        canon.push_str(&format!("{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1e}", s.id, s.name, s.kind.as_str(), s.position));
    }
    crate::config_write::content_hash(canon.as_bytes())
}

// ---------------------------------------------------------------------------
// Sync

/// A workflow that names a status the catalog doesn't have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenRef {
    pub workflow: String,
    pub status: String,
}

/// What a sync did (or, dry, would do).
#[derive(Debug, Clone)]
pub struct StatusSync {
    pub before: ProjectStatuses,
    pub after: ProjectStatuses,
    pub changes: Vec<CatalogChange>,
    /// Whether `statuses.yaml` was written. False on a dry run and when
    /// nothing changed.
    pub written: bool,
    /// Workflow references `after` no longer satisfies: these workflows fail
    /// validation until they're pointed at other statuses
    /// ([`plan_workflow_rewrites`]).
    pub broken: Vec<BrokenRef>,
}

/// Sync `project`'s catalog from its Linear team: read the team's states,
/// then [`sync_with_states`]. `dry_run` writes nothing.
pub fn sync(project: &str, dry_run: bool) -> Result<StatusSync> {
    let (team, states) = fetch_states(project)?;
    sync_with_states(project, Some(&team), &states, dry_run)
}

/// Sync `project`'s catalog from `states` (already read from Linear).
/// `team` names the team in the file's header.
pub fn sync_with_states(
    project: &str,
    team: Option<&str>,
    states: &[LinearState],
    dry_run: bool,
) -> Result<StatusSync> {
    let path = crate::statuses_path(project)?;
    let exists = crate::workflows::statuses_read_path(project)?.exists();
    let before = crate::load_project_statuses(project)?;
    let after = plan_catalog(&before, states)?;
    let changes = diff_catalogs(&before, &after);
    let broken = broken_refs(project, &after)?;
    let mut written = false;
    if !dry_run {
        if !changes.is_empty() || !exists {
            let text = format!("{}{}", managed_header(team), yaml_serde::to_string(&after)?);
            crate::atomic_write(&path, text.as_bytes())?;
            written = true;
            remember_retired(project, &before, &after)?;
        }
        update_doc(project, |doc| {
            doc.insert(STATES_HASH_KEY.into(), Value::String(states_hash(states)));
            doc.insert(STATES_CHECKED_KEY.into(), Value::String(Utc::now().to_rfc3339()));
        })?;
    }
    Ok(StatusSync { before, after, changes, written, broken })
}

/// What [`connect`] did.
#[derive(Debug, Clone)]
pub struct Connected {
    pub sync: StatusSync,
    /// The rewrites that mapped the project's workflows onto the synced
    /// catalog. Those that change something were written.
    pub rewrites: Vec<WorkflowRewrite>,
    /// How many workflow files were written.
    pub rewritten: usize,
    /// Workflow statuses no team status could take: those workflows still
    /// fail to load. Empty when [`plan_connect_to`] was asked first and
    /// passed.
    pub unmappable: Vec<Unmappable>,
}

/// A project was just pointed at Linear: sync its catalog and map its
/// workflows onto it by category (what `shelbi tracker linear sync
/// --rewrite-workflows --yes` does), so the project loads right after. Ask
/// [`plan_connect_to`] before writing the connection, so a status that can't
/// be mapped refuses it instead.
pub fn connect(project: &str) -> Result<Connected> {
    let (team, states) = fetch_states(project)?;
    connect_with_states(project, Some(&team), &states)
}

/// [`connect`] with the team's states already read.
pub fn connect_with_states(project: &str, team: Option<&str>, states: &[LinearState]) -> Result<Connected> {
    let sync = sync_with_states(project, team, states, false)?;
    let (rewrites, unmappable) = plan_rewrites(project, &sync.after, &sync.before)?;
    let rewritten = apply_workflow_rewrites(&rewrites)?;
    Ok(Connected { sync, rewrites, rewritten, unmappable })
}

/// A workflow status that no status of the team has the category of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmappable {
    pub workflow: String,
    pub status: String,
    /// Its category, when Shelbi knows it (from the catalog, a status an
    /// earlier sync retired, or Shelbi's defaults).
    pub category: Option<StatusCategory>,
}

/// What connecting `project` to a team would write, planned before anything
/// is: the catalog sync and the workflow rewrites onto it.
#[derive(Debug, Clone)]
pub struct ConnectPlan {
    /// The team, `KEY (Name)`.
    pub team: String,
    /// The team's key alone (`ENG`), for messages.
    pub team_key: String,
    /// The sync, dry: [`StatusSync::written`] is false.
    pub sync: StatusSync,
    pub rewrites: Vec<WorkflowRewrite>,
    pub unmappable: Vec<Unmappable>,
}

impl ConnectPlan {
    /// Why the connection must not be written, if it mustn't: some workflow
    /// names a status whose category no team status has, so the project
    /// wouldn't load. Says what to add in Linear (or change in the workflow).
    pub fn refusal(&self) -> Option<String> {
        if self.unmappable.is_empty() {
            return None;
        }
        let team = &self.team_key;
        let mut msg = format!(
            "The Linear team {team} has no status to map {} onto, so the project wouldn't load after \
             connecting. Nothing was written.",
            if self.unmappable.len() == 1 { "this workflow status" } else { "these workflow statuses" }
        );
        for u in &self.unmappable {
            let what = match u.category {
                Some(c) => format!("a {c} status; {team} needs {}", linear_state_for(c)),
                None => "a status Shelbi has no category for".to_string(),
            };
            msg.push_str(&format!("\n  workflow `{}`: `{}` ({what})", u.workflow, u.status));
        }
        let ids: Vec<String> = self.sync.after.statuses.iter().map(|s| format!("`{}`", s.id)).collect();
        msg.push_str(&format!(
            "\nAdd that state to the team in Linear, or point the workflow at one of {team}'s \
             statuses ({}), then connect again.",
            ids.join(", ")
        ));
        Some(msg)
    }
}

/// The kind of Linear state that syncs into `category` by default.
fn linear_state_for(category: StatusCategory) -> &'static str {
    match category {
        StatusCategory::Backlog => "a Backlog or Triage state",
        StatusCategory::Ready => "an Unstarted state (like Todo)",
        StatusCategory::Active => "a Started state (like In Progress)",
        StatusCategory::Handoff => {
            "a Started state named for review, QA, verify or test (like In Review)"
        }
        StatusCategory::Done => "a Completed state (like Done)",
        StatusCategory::Archived => "a Canceled state",
    }
}

/// Plan connecting `project` to the team `filters` resolved to: read the
/// team's states and plan the sync and the workflow rewrites onto them.
/// Writes nothing, and works before the project is on Linear (the key is
/// the project's or the global one).
pub fn plan_connect_to(project: &str, filters: &linear_client::ResolvedLinearFilters) -> Result<ConnectPlan> {
    let client = LinearClient::for_project(Some(project))?.caller(CALLER);
    let states = client.team_states(&filters.team_id)?;
    plan_connect(project, &team_label(filters), &filters.team_key, &states)
}

/// [`plan_connect_to`] with the team's states already read.
pub fn plan_connect(project: &str, team: &str, team_key: &str, states: &[LinearState]) -> Result<ConnectPlan> {
    let sync = sync_with_states(project, Some(team), states, true)?;
    let (rewrites, unmappable) = plan_rewrites(project, &sync.after, &sync.before)?;
    Ok(ConnectPlan { team: team.to_string(), team_key: team_key.to_string(), sync, rewrites, unmappable })
}

/// The board-refresh trigger: when `project` is on Linear, read its team's
/// states (at most every `min_interval`; pass zero to force) and sync if they
/// changed since the last sync. `None` when the project isn't on Linear, the
/// check was skipped, or nothing changed.
pub fn refresh_if_changed(project: &str, min_interval: Duration) -> Result<Option<StatusSync>> {
    if crate::load_project_tracker(project)?.backend != IssueTrackerBackend::Linear {
        return Ok(None);
    }
    let doc = read_doc(project)?;
    let checked = doc
        .get(STATES_CHECKED_KEY)
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok());
    if let Some(at) = checked {
        let age = Utc::now().signed_duration_since(at.with_timezone(&Utc));
        if age.to_std().is_ok_and(|a| a < min_interval) {
            return Ok(None);
        }
    }
    let (team, states) = fetch_states(project)?;
    let hash = states_hash(&states);
    let managed = crate::load_project_statuses(project)?.managed_by == Some(TrackerName::Linear);
    if managed && doc.get(STATES_HASH_KEY).and_then(Value::as_str) == Some(hash.as_str()) {
        update_doc(project, |doc| {
            doc.insert(STATES_CHECKED_KEY.into(), Value::String(Utc::now().to_rfc3339()));
        })?;
        return Ok(None);
    }
    let sync = sync_with_states(project, Some(&team), &states, false)?;
    Ok(sync.written.then_some(sync))
}

/// The project's Linear team (`KEY (Name)`) and its states.
///
/// The connection is read without loading the project's workflows: a sync
/// is how a workflow naming a status the team lacks gets repaired, so it
/// can't need that workflow to load first.
fn fetch_states(project: &str) -> Result<(String, Vec<LinearState>)> {
    let tracker = crate::load_project_tracker(project)?;
    let conn = match (tracker.backend, tracker.linear.as_ref()) {
        (IssueTrackerBackend::Linear, Some(conn)) => conn.clone(),
        _ => {
            return Err(Error::InvalidIssueTracker(format!(
                "project `{project}` isn't connected to Linear (issue_tracker.backend: linear with \
                 a linear.team)"
            )))
        }
    };
    let client = LinearClient::for_project(Some(project))?.caller(CALLER);
    let filters = linear_client::resolve_filters_cached(&client, project, &conn)?;
    let states = client.team_states(&filters.team_id)?;
    Ok((team_label(&filters), states))
}

/// `KEY (Name)`, as a synced catalog's header names the team.
fn team_label(filters: &linear_client::ResolvedLinearFilters) -> String {
    format!("{} ({})", filters.team_key, filters.team_name)
}

/// The workflow files' references `catalog` lacks.
fn broken_refs(project: &str, catalog: &ProjectStatuses) -> Result<Vec<BrokenRef>> {
    let mut out = Vec::new();
    for (stem, path) in workflow_files(&crate::workflows_dir(project)?).map_err(config_err)? {
        let text = crate::read_to_string_at(&path)?;
        for id in raw_status_ids(&text) {
            if catalog.get(&id).is_none() {
                out.push(BrokenRef { workflow: stem.clone(), status: id });
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// linear.json

fn read_doc(project: &str) -> Result<serde_json::Map<String, Value>> {
    Ok(linear_client::read_cache_doc(&filter_cache_path(project)?))
}

fn update_doc(project: &str, edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<()> {
    let path = filter_cache_path(project)?;
    let mut doc = linear_client::read_cache_doc(&path);
    edit(&mut doc);
    linear_client::write_cache_doc(&path, &doc)
}

/// Record the statuses `after` dropped, so a later workflow rewrite still
/// knows their categories. An id that comes back is forgotten again.
fn remember_retired(project: &str, before: &ProjectStatuses, after: &ProjectStatuses) -> Result<()> {
    update_doc(project, |doc| {
        let mut retired: BTreeMap<String, Value> = doc
            .get(RETIRED_KEY)
            .and_then(Value::as_object)
            .map(|m| m.clone().into_iter().collect())
            .unwrap_or_default();
        for b in &before.statuses {
            if after.get(&b.id).is_none() {
                retired.insert(b.id.clone(), Value::String(b.category.to_string()));
            }
        }
        for a in &after.statuses {
            retired.remove(&a.id);
        }
        if retired.is_empty() {
            doc.remove(RETIRED_KEY);
        } else {
            doc.insert(RETIRED_KEY.into(), Value::Object(retired.into_iter().collect()));
        }
    })
}

fn retired_categories(project: &str) -> Result<HashMap<String, StatusCategory>> {
    let doc = read_doc(project)?;
    Ok(doc
        .get(RETIRED_KEY)
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(id, v)| {
                    let cat = serde_json::from_value::<StatusCategory>(v.clone()).ok()?;
                    Some((id.clone(), cat))
                })
                .collect()
        })
        .unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Workflow rewrites

/// One workflow file mapped onto a new catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRewrite {
    pub workflow: String,
    pub path: PathBuf,
    /// The file as read; [`apply_workflow_rewrites`] refuses if it changed.
    pub before: String,
    pub after: String,
    /// `(old id, new id)` for each status pointed elsewhere.
    pub remapped: Vec<(String, String)>,
    /// Old ids folded into a status the workflow already lists (the entry
    /// was dropped, its transitions moved to the kept one).
    pub merged: Vec<String>,
    /// Old ids with no status of their category in the catalog: left as
    /// they are, so the workflow still fails validation until the user picks
    /// one (or gives a status that category).
    pub unmapped: Vec<String>,
    /// Why the rewritten file still doesn't validate against the catalog,
    /// if it doesn't.
    pub error: Option<String>,
}

impl WorkflowRewrite {
    /// Whether applying it writes anything.
    pub fn changes(&self) -> bool {
        self.before != self.after
    }
}

/// Map every workflow file of `project` onto `catalog`: each status id the
/// catalog lacks becomes the catalog's first status of the same category,
/// keeping its owner, agent and tags; `initial_status` and transitions
/// follow. The old id's category comes from `previous` (the catalog before
/// the sync), then from the ids earlier syncs retired, then from Shelbi's
/// default catalog. Returns only the workflows that need a change (or can't
/// be fully mapped). Reads, never writes.
pub fn plan_workflow_rewrites(
    project: &str,
    catalog: &ProjectStatuses,
    previous: &ProjectStatuses,
) -> Result<Vec<WorkflowRewrite>> {
    Ok(plan_rewrites(project, catalog, previous)?.0)
}

/// [`plan_workflow_rewrites`], with the statuses left unmapped and their
/// categories.
fn plan_rewrites(
    project: &str,
    catalog: &ProjectStatuses,
    previous: &ProjectStatuses,
) -> Result<(Vec<WorkflowRewrite>, Vec<Unmappable>)> {
    let retired = retired_categories(project)?;
    let defaults = default_project_statuses();
    let category_of = |id: &str| -> Option<StatusCategory> {
        previous
            .get(id)
            .map(|s| s.category)
            .or_else(|| retired.get(id).copied())
            .or_else(|| defaults.get(id).map(|s| s.category))
    };
    let mut out = Vec::new();
    let mut unmappable = Vec::new();
    for (stem, path) in workflow_files(&crate::workflows_dir(project)?).map_err(config_err)? {
        let text = crate::read_to_string_at(&path)?;
        let rewrite = rewrite_workflow_text(&stem, path, &text, catalog, &category_of)?;
        unmappable.extend(rewrite.unmapped.iter().map(|id| Unmappable {
            workflow: stem.clone(),
            status: id.clone(),
            category: category_of(id),
        }));
        if rewrite.changes() || !rewrite.unmapped.is_empty() {
            out.push(rewrite);
        }
    }
    Ok((out, unmappable))
}

/// Rewrite one workflow file's text (see [`plan_workflow_rewrites`]).
/// Comments and formatting survive, through [`crate::yaml_edit`].
pub fn rewrite_workflow_text(
    workflow: &str,
    path: PathBuf,
    text: &str,
    catalog: &ProjectStatuses,
    category_of: &dyn Fn(&str) -> Option<StatusCategory>,
) -> Result<WorkflowRewrite> {
    let patch_err = |e: crate::yaml_edit::PatchError| {
        Error::Other(format!("{}: {e}", path.display()))
    };
    let raw = crate::yaml_edit::parse(text).map_err(patch_err)?;
    let mut target = raw.clone();
    let mut remapped = Vec::new();
    let mut merged = Vec::new();
    let mut unmapped = Vec::new();
    // old id -> new id, for every id the file's statuses list.
    let mut map: HashMap<String, String> = HashMap::new();

    if let Some(seq) = target.get_mut("statuses").and_then(|v| v.as_sequence_mut()) {
        let ids: Vec<Option<String>> = seq
            .iter()
            .map(|e| e.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        // Ids the file already lists that the catalog has: those stay put
        // and win over a remapped duplicate.
        let mut kept: HashSet<String> = ids
            .iter()
            .flatten()
            .filter(|id| catalog.get(id).is_some())
            .cloned()
            .collect();
        let mut drop = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            let Some(id) = id else { continue };
            if catalog.get(id).is_some() {
                map.insert(id.clone(), id.clone());
                continue;
            }
            let Some(new) = category_of(id).and_then(|c| catalog.first_of(c)).map(|s| s.id.clone()) else {
                unmapped.push(id.clone());
                map.insert(id.clone(), id.clone());
                continue;
            };
            map.insert(id.clone(), new.clone());
            if kept.contains(&new) {
                merged.push(id.clone());
                drop.push(i);
            } else {
                kept.insert(new.clone());
                remapped.push((id.clone(), new.clone()));
                if let Some(entry) = seq[i].as_mapping_mut() {
                    entry.insert("id".into(), new.into());
                }
            }
        }
        for i in drop.into_iter().rev() {
            seq.remove(i);
        }
    }

    if let Some(map_tbl) = target.as_mapping_mut() {
        if let Some(initial) = map_tbl.get_mut("initial_status") {
            if let Some(new) = initial.as_str().and_then(|s| map.get(s)) {
                *initial = new.clone().into();
            }
        }
    }

    if let Some(seq) = target.get_mut("transitions").and_then(|v| v.as_sequence_mut()) {
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut keep = Vec::with_capacity(seq.len());
        for t in seq.drain(..) {
            let mut t = t;
            let from = t.get("from").and_then(|v| v.as_str()).map(str::to_string);
            let to = t.get("to").and_then(|v| v.as_str()).map(str::to_string);
            let (Some(from), Some(to)) = (from, to) else {
                keep.push(t);
                continue;
            };
            let nf = map.get(&from).cloned().unwrap_or(from.clone());
            let nt = map.get(&to).cloned().unwrap_or(to.clone());
            // An edge whose two ends were folded into one status, or that
            // now duplicates an earlier edge, has nothing left to say.
            if (nf == nt && from != to) || !seen.insert((nf.clone(), nt.clone())) {
                continue;
            }
            if let Some(m) = t.as_mapping_mut() {
                m.insert("from".into(), nf.into());
                m.insert("to".into(), nt.into());
            }
            keep.push(t);
        }
        *seq = keep;
    }

    let after = if target == raw {
        text.to_string()
    } else {
        crate::yaml_edit::patch(text, &target).map_err(patch_err)?.text
    };
    let error = Workflow::from_yaml_str(&after)
        .and_then(|wf| wf.resolve_against(catalog))
        .err()
        .map(|e| e.to_string());
    Ok(WorkflowRewrite {
        workflow: workflow.to_string(),
        path,
        before: text.to_string(),
        after,
        remapped,
        merged,
        unmapped,
        error,
    })
}

/// Write the rewrites that change something. Refuses, writing nothing, when
/// any file changed on disk since it was planned. Returns how many files
/// were written.
pub fn apply_workflow_rewrites(rewrites: &[WorkflowRewrite]) -> Result<usize> {
    let pending: Vec<&WorkflowRewrite> = rewrites.iter().filter(|r| r.changes()).collect();
    for r in &pending {
        let now = crate::read_to_string_at(&r.path)?;
        if now != r.before {
            return Err(Error::Other(format!(
                "{} changed on disk since the rewrite was planned; nothing was written",
                r.path.display()
            )));
        }
    }
    for r in &pending {
        crate::atomic_write(&r.path, r.after.as_bytes())?;
    }
    Ok(pending.len())
}

/// A line of a [`line_diff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Same(String),
    Removed(String),
    Added(String),
}

/// A line diff of `before` -> `after` (longest common subsequence; workflow
/// files are small).
pub fn line_diff(before: &str, after: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            out.push(DiffLine::Same(a[i].to_string()));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Removals first, so a changed line reads `- old` then `+ new`.
            out.push(DiffLine::Removed(a[i].to_string()));
            i += 1;
        } else {
            out.push(DiffLine::Added(b[j].to_string()));
            j += 1;
        }
    }
    out
}

fn config_err(e: crate::workflow_config::ConfigError) -> Error {
    Error::Other(e.to_string())
}

#[cfg(test)]
mod tests;
