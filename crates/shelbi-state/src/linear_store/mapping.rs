//! The pure rules that map a Linear issue onto a Shelbi [`IssueFile`] and
//! back: status mapping, Shelbi ids, the attachment metadata, the filters,
//! and board order. No I/O here, so every rule is unit-testable on its own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use shelbi_core::{Column, IssueLaunchConfig, IssuePr, IssueZenConfig, ProjectStatuses};

use crate::linear_client::{
    LinearIssue, LinearState, LinearStateType, LinearUser, ResolvedLinearFilters,
    SHELBI_ATTACHMENT_PREFIX,
};

/// The title of Shelbi's attachment on an issue.
pub(crate) const ATTACHMENT_TITLE: &str = "Shelbi";

/// The key under the attachment's `metadata` holding [`LinearMeta`].
const META_KEY: &str = "shelbi";

// --- status mapping -----------------------------------------------------------

/// `In Review` → `in-review`: lowercase, every run of non-alphanumerics
/// collapsed to one `-`, trimmed.
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// The Shelbi status a Linear workflow state maps to. **The one place** the
/// state → status rule lives.
///
/// A project whose status catalog is synced from Linear (`managed_by:
/// linear`, [`crate::linear_statuses`]) maps each state to the status
/// carrying its uuid as `external_id`: ids are assigned once per state and
/// survive renames. Otherwise (no synced catalog yet, or a state added since
/// the last sync) the rule is by name: Linear's two terminal categories land
/// in Shelbi's two terminal statuses (`completed` → `done`, `canceled` →
/// `canceled`, whatever the state is called), and every other state maps by
/// its slugged name (`In Review` → `in-review`).
pub(crate) fn column_for_state(state: &LinearState, catalog: Option<&ProjectStatuses>) -> Column {
    if let Some(st) = catalog.and_then(|c| c.by_external_id(&state.id)) {
        return Column::from_status_id(&st.id);
    }
    match state.kind {
        LinearStateType::Completed => Column::done(),
        LinearStateType::Canceled | LinearStateType::Duplicate => Column::canceled(),
        _ => Column::from_status_id(slug(&state.name)),
    }
}

/// Whether a state is closed in Linear (completed, canceled or duplicate). The store
/// decides open vs. history by this, never by the status id, so a synced
/// status like `shipped` or `duplicate` still leaves the open board.
pub(crate) fn is_closed_state(state: &LinearState) -> bool {
    matches!(state.kind, LinearStateType::Completed | LinearStateType::Canceled | LinearStateType::Duplicate)
}

/// The team state a move to `col` writes: among the states that map to
/// `col` (in board order), the one whose own slug is `col` if any, else the
/// first. So `done` prefers a completed state named "Done" over "Shipped".
pub(crate) fn state_for_column<'a>(
    states: &'a [LinearState],
    col: &Column,
    catalog: Option<&ProjectStatuses>,
) -> Option<&'a LinearState> {
    let matches: Vec<&LinearState> =
        states.iter().filter(|s| column_for_state(s, catalog) == *col).collect();
    matches
        .iter()
        .find(|s| slug(&s.name) == col.as_str())
        .or_else(|| matches.first())
        .copied()
}

/// The ids of every team state that maps to `col`.
pub(crate) fn state_ids_for_column(
    states: &[LinearState],
    col: &Column,
    catalog: Option<&ProjectStatuses>,
) -> Vec<String> {
    states
        .iter()
        .filter(|s| column_for_state(s, catalog) == *col)
        .map(|s| s.id.clone())
        .collect()
}

// --- identity -----------------------------------------------------------------

/// The token Shelbi uses for a Linear user in `tracker_assignees`,
/// `viewer_login` and comment authors: the display name, which Linear keeps
/// unique within a workspace (so `owner=other:grace` and `@grace` read like a
/// GitHub login), falling back to the email, then the id.
pub(crate) fn user_token(user: &LinearUser) -> String {
    [&user.display_name, &user.email, &user.id]
        .into_iter()
        .find(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

/// The Shelbi id of a Linear issue: the id stored in its Shelbi attachment,
/// else its lowercased identifier (`ENG-123` → `eng-123`).
pub(crate) fn shelbi_id(issue: &LinearIssue, meta: &LinearMeta) -> String {
    meta.id
        .as_deref()
        .filter(|id| shelbi_core::validate_task_id(id).is_ok())
        .map(str::to_string)
        .unwrap_or_else(|| issue.identifier.to_ascii_lowercase())
}

/// The issue number a Shelbi id names when it is a lowercased identifier of
/// this team (`eng-123` with key `ENG` → 123).
pub(crate) fn number_from_id(id: &str, team_key: &str) -> Option<i64> {
    let rest = id.strip_prefix(&format!("{}-", team_key.to_ascii_lowercase()))?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// The URL of the Shelbi attachment for `id` in `team_key`.
pub(crate) fn attachment_url(team_key: &str, id: &str) -> String {
    format!("{SHELBI_ATTACHMENT_PREFIX}{team_key}/{id}")
}

// --- attachment metadata ------------------------------------------------------

/// The Shelbi-only fields of an issue, kept in its Shelbi attachment's
/// `metadata.shelbi` (what GitHub keeps in the fenced body block). Unknown
/// keys flatten into `params`, so a newer binary's fields survive an older
/// read. Board order is not here: it is Linear's own `sortOrder`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct LinearMeta {
    /// The Shelbi id (always written).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "shelbi_core::deserialize_issue_pr"
    )]
    pub pr: Option<IssuePr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefers_machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zen: Option<IssueZenConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<IssueLaunchConfig>,
    #[serde(flatten, default)]
    pub params: BTreeMap<String, yaml_serde::Value>,
}

/// Read an issue's [`LinearMeta`]: default when it has no Shelbi attachment,
/// `Err` when the attachment's metadata doesn't parse.
pub(crate) fn read_meta(issue: &LinearIssue) -> Result<LinearMeta, String> {
    let Some(att) = issue.shelbi_attachment() else {
        return Ok(LinearMeta::default());
    };
    match att.metadata.get(META_KEY) {
        None | Some(Value::Null) => Ok(LinearMeta::default()),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| e.to_string()),
    }
}

/// The attachment `metadata` for `meta`: the fields under `shelbi`, plus a
/// few `attributes` Linear shows on the attachment.
pub(crate) fn meta_value(meta: &LinearMeta) -> Result<Value, String> {
    let shelbi = serde_json::to_value(meta).map_err(|e| e.to_string())?;
    let mut attributes = Vec::new();
    if let Some(id) = &meta.id {
        attributes.push(json!({ "name": "Shelbi id", "value": id }));
    }
    if let Some(w) = &meta.workflow {
        attributes.push(json!({ "name": "Workflow", "value": w }));
    }
    if let Some(b) = &meta.branch {
        attributes.push(json!({ "name": "Branch", "value": b }));
    }
    if let Some(pr) = &meta.pr {
        let value = pr.url.clone().unwrap_or_else(|| pr.number_label());
        attributes.push(json!({ "name": "Pull request", "value": value }));
    }
    Ok(json!({ META_KEY: shelbi, "attributes": attributes }))
}

/// The `attachmentCreate` input that writes `meta` onto issue `issue_id`.
pub(crate) fn attachment_input(
    issue_id: &str,
    team_key: &str,
    shelbi_id: &str,
    meta: &LinearMeta,
) -> Result<Value, String> {
    let mut meta = meta.clone();
    meta.id = Some(shelbi_id.to_string());
    Ok(json!({
        "issueId": issue_id,
        "url": attachment_url(team_key, shelbi_id),
        "title": ATTACHMENT_TITLE,
        "subtitle": meta.workflow.clone().unwrap_or_else(|| "Shelbi issue".into()),
        "metadata": meta_value(&meta)?,
    }))
}

// --- filters ------------------------------------------------------------------

/// The `IssueFilter` for the project's visible set: team, plus the project,
/// label and assignee filters that are set.
pub(crate) fn visible_filter(f: &ResolvedLinearFilters) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("team".into(), json!({ "id": { "eq": f.team_id } }));
    if let Some(p) = &f.project_id {
        m.insert("project".into(), json!({ "id": { "eq": p } }));
    }
    if let Some(l) = &f.label_id {
        m.insert("labels".into(), json!({ "some": { "id": { "eq": l } } }));
    }
    if let Some(a) = &f.assignee_id {
        m.insert("assignee".into(), json!({ "id": { "eq": a } }));
    }
    m
}

/// The team-only filter (a delta reads every touched team issue, then
/// [`matches_filters`] decides which are on the board, so an issue that
/// leaves the filters leaves the board).
pub(crate) fn team_filter(f: &ResolvedLinearFilters) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("team".into(), json!({ "id": { "eq": f.team_id } }));
    m
}

/// `base` plus `extra`'s keys.
pub(crate) fn with(mut base: Map<String, Value>, extra: Value) -> Value {
    if let Value::Object(extra) = extra {
        base.extend(extra);
    }
    Value::Object(base)
}

/// Linear's closed state types: a state of one of these leaves the open board.
pub(crate) const CLOSED_STATE_TYPES: [&str; 3] = ["completed", "canceled", "duplicate"];

/// The `state` filter for the open (non-terminal) or closed board.
pub(crate) fn terminal_filter(closed: bool) -> Value {
    let op = if closed { "in" } else { "nin" };
    json!({ "state": { "type": { op: CLOSED_STATE_TYPES } } })
}

/// The filter matching the Shelbi attachment of `id` (by URL suffix, so a
/// renamed team key still finds it).
pub(crate) fn attachment_filter(id: &str) -> Value {
    json!({ "attachments": { "some": { "and": [
        { "url": { "startsWith": SHELBI_ATTACHMENT_PREFIX } },
        { "url": { "endsWith": format!("/{id}") } },
    ] } } })
}

/// Whether `issue` passes the project's optional filters (the team is
/// assumed: every read is team-scoped). Mirrors [`visible_filter`].
pub(crate) fn matches_filters(issue: &LinearIssue, f: &ResolvedLinearFilters) -> bool {
    let project_ok = f.project_id.as_ref().is_none_or(|p| {
        issue.project.as_ref().is_some_and(|ip| ip.id == *p)
    });
    let label_ok = f
        .label_id
        .as_ref()
        .is_none_or(|l| issue.label_ids().any(|id| id == l));
    let assignee_ok = f.assignee_id.as_ref().is_none_or(|a| {
        issue.assignee.as_ref().is_some_and(|u| u.id == *a)
    });
    project_ok && label_ok && assignee_ok
}

// --- board order --------------------------------------------------------------

/// A `sortOrder` that places an issue between `before` and `after` (either
/// may be absent: the top or bottom of the column, or an empty column).
pub(crate) fn sort_between(before: Option<f64>, after: Option<f64>) -> f64 {
    match (before, after) {
        (None, None) => 0.0,
        (Some(a), None) => a + 1000.0,
        (None, Some(b)) => b - 1000.0,
        (Some(a), Some(b)) => (a + b) / 2.0,
    }
}

/// Whether `v` sits strictly between its neighbours (float midpoints run out
/// eventually; the caller then respaces the column).
pub(crate) fn strictly_between(v: f64, before: Option<f64>, after: Option<f64>) -> bool {
    before.is_none_or(|a| v > a) && after.is_none_or(|b| v < b)
}

/// A fresh v4 UUID, for the ids Shelbi chooses for the issues and comments it
/// creates (so an unknown-outcome create can be looked up by id).
pub(crate) fn new_uuid() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    if !filled {
        let a = crate::github_store::comment_marker_token().to_le_bytes();
        let b = crate::github_store::comment_marker_token().to_le_bytes();
        bytes[..8].copy_from_slice(&a);
        bytes[8..].copy_from_slice(&b);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: &str, name: &str, kind: LinearStateType, position: f64) -> LinearState {
        LinearState {
            id: id.into(),
            name: name.into(),
            kind,
            color: String::new(),
            position,
        }
    }

    #[test]
    fn slugs_state_names() {
        assert_eq!(slug("In Review"), "in-review");
        assert_eq!(slug("  QA / Verify!  "), "qa-verify");
        assert_eq!(slug("Todo"), "todo");
    }

    #[test]
    fn states_map_by_name_and_terminal_types_always_land_terminal() {
        use LinearStateType::*;
        let cases = [
            (state("a", "In Progress", Started, 1.0), "in-progress"),
            (state("b", "In Review", Started, 2.0), "in-review"),
            (state("c", "Todo", Unstarted, 0.0), "todo"),
            (state("d", "Triage", Triage, 0.0), "triage"),
            (state("e", "Shipped", Completed, 3.0), "done"),
            (state("f", "Duplicate", Canceled, 4.0), "canceled"),
            // Linear gives a team's Duplicate state its own closed type.
            (state("g", "Duplicate", Duplicate, 5.0), "canceled"),
        ];
        for (s, want) in cases {
            assert_eq!(column_for_state(&s, None).as_str(), want, "{}", s.name);
        }
        assert!(is_closed_state(&state("g", "Duplicate", Duplicate, 5.0)));
    }

    #[test]
    fn moves_prefer_the_state_named_like_the_status() {
        use LinearStateType::*;
        let states = vec![
            state("shipped", "Shipped", Completed, 5.0),
            state("done", "Done", Completed, 6.0),
            state("dup", "Duplicate", Canceled, 7.0),
            state("review", "In Review", Started, 4.0),
        ];
        assert_eq!(state_for_column(&states, &Column::done(), None).unwrap().id, "done");
        assert_eq!(state_for_column(&states, &Column::canceled(), None).unwrap().id, "dup");
        assert_eq!(
            state_for_column(&states, &Column::from_status_id("in-review"), None).unwrap().id,
            "review"
        );
        assert!(state_for_column(&states, &Column::todo(), None).is_none());
        assert_eq!(state_ids_for_column(&states, &Column::done(), None), ["shipped", "done"]);
    }

    #[test]
    fn a_synced_catalog_maps_states_by_external_id() {
        use LinearStateType::*;
        let catalog: ProjectStatuses = yaml_serde::from_str(
            "managed_by: linear\nstatuses:\n\
             - { id: shipped, name: Shipped, category: done, external_id: shipped-uuid }\n\
             - { id: doing, name: In Progress, category: active, external_id: progress-uuid }\n",
        )
        .unwrap();
        let states = vec![
            state("progress-uuid", "In Progress", Started, 1.0),
            state("shipped-uuid", "Shipped", Completed, 2.0),
            state("new-uuid", "Brand New", Unstarted, 0.0),
        ];
        // A renamed state keeps the id its uuid was assigned.
        assert_eq!(column_for_state(&states[0], Some(&catalog)).as_str(), "doing");
        assert_eq!(column_for_state(&states[1], Some(&catalog)).as_str(), "shipped");
        assert!(is_closed_state(&states[1]));
        // A state the catalog hasn't synced yet falls back to the name rule.
        assert_eq!(column_for_state(&states[2], Some(&catalog)).as_str(), "brand-new");
        let col = Column::from_status_id("shipped");
        assert_eq!(state_for_column(&states, &col, Some(&catalog)).unwrap().id, "shipped-uuid");
    }

    #[test]
    fn identifier_ids_round_trip_to_numbers() {
        assert_eq!(number_from_id("eng-123", "ENG"), Some(123));
        assert_eq!(number_from_id("eng-", "ENG"), None);
        assert_eq!(number_from_id("eng-12a", "ENG"), None);
        assert_eq!(number_from_id("des-4", "ENG"), None);
        assert_eq!(number_from_id("linear-store", "ENG"), None);
    }

    #[test]
    fn user_tokens_prefer_the_display_name() {
        let mut u = LinearUser {
            id: "u1".into(),
            name: "Grace Hopper".into(),
            display_name: "grace".into(),
            email: "grace@example.com".into(),
            active: true,
        };
        assert_eq!(user_token(&u), "grace");
        u.display_name.clear();
        assert_eq!(user_token(&u), "grace@example.com");
        u.email.clear();
        assert_eq!(user_token(&u), "u1");
    }

    #[test]
    fn meta_round_trips_through_the_attachment_metadata() {
        let mut params = BTreeMap::new();
        params.insert("estimate".to_string(), yaml_serde::Value::from("2h"));
        let meta = LinearMeta {
            id: Some("x".into()),
            workflow: Some("app".into()),
            branch: Some("jlong/x".into()),
            pr: Some(IssuePr::new(7, Some("https://github.com/o/r/pull/7".into()))),
            depends_on: vec!["eng-1".into()],
            params,
            ..Default::default()
        };
        let value = meta_value(&meta).unwrap();
        assert_eq!(value["attributes"][1]["value"], "app");
        let back: LinearMeta = serde_json::from_value(value["shelbi"].clone()).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn sort_orders_land_between_neighbours() {
        assert_eq!(sort_between(Some(1.0), Some(2.0)), 1.5);
        assert_eq!(sort_between(None, Some(2.0)), -998.0);
        assert_eq!(sort_between(Some(2.0), None), 1002.0);
        assert!(strictly_between(1.5, Some(1.0), Some(2.0)));
        assert!(!strictly_between(1.0, Some(1.0), Some(1.0)));
    }

    #[test]
    fn uuids_are_v4_shaped_and_distinct() {
        let a = new_uuid();
        let b = new_uuid();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"), "{a}");
    }
}
