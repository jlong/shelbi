//! Resolve a project's [`LinearConnection`] filter references (`team: ENG`,
//! `label: shelbi`, `assignee: me`) into Linear ids, with a per-project cache.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::LinearConnection;

use super::client::LinearClient;
use super::error::{LinearError, LinearResult};
use super::queries::LinearTeam;

/// Basename of the per-project filter cache, under
/// `~/.shelbi/projects/<name>/`.
pub const FILTER_CACHE_FILE: &str = "linear.json";

/// A project's Linear filters resolved to ids (plus display names for
/// `status` output). Every issue the board shows matches all of the set
/// filters, and every issue Shelbi creates is created with them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLinearFilters {
    pub team_id: String,
    pub team_key: String,
    pub team_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_name: Option<String>,
}

/// Find the one candidate matching `wanted` (case-insensitive) on any of its
/// keys, preferring an exact id. `label` renders a candidate for errors.
fn pick<'a, T>(
    what: &'static str,
    wanted: &str,
    candidates: &'a [T],
    id: impl Fn(&T) -> &str,
    keys: impl Fn(&T) -> Vec<&str>,
    label: impl Fn(&T) -> String,
) -> LinearResult<&'a T> {
    if let Some(hit) = candidates.iter().find(|c| id(c) == wanted) {
        return Ok(hit);
    }
    let matches: Vec<&T> = candidates
        .iter()
        .filter(|c| {
            id(c).eq_ignore_ascii_case(wanted)
                || keys(c).iter().any(|k| !k.is_empty() && k.eq_ignore_ascii_case(wanted))
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(LinearError::Resolve {
            what,
            wanted: wanted.to_string(),
            available: candidates.iter().map(&label).collect(),
            ambiguous: false,
        }),
        many => Err(LinearError::Resolve {
            what,
            wanted: wanted.to_string(),
            available: many.iter().map(|c| format!("{} [{}]", label(c), id(c))).collect(),
            ambiguous: true,
        }),
    }
}

/// Find one team by key, name or id (case-insensitive), with an error listing
/// the teams when nothing matches.
pub fn resolve_team(client: &LinearClient, team_ref: &str) -> LinearResult<LinearTeam> {
    let teams = client.teams()?;
    pick_team(&teams, team_ref).cloned()
}

fn pick_team<'a>(teams: &'a [LinearTeam], team_ref: &str) -> LinearResult<&'a LinearTeam> {
    pick(
        "team",
        team_ref.trim(),
        teams,
        |t| &t.id,
        |t| vec![&t.key, &t.name],
        |t| format!("{} ({})", t.key, t.name),
    )
}

/// Resolve `conn`'s references to ids against Linear (a few small reads:
/// teams, then only the lists an optional filter needs).
///
/// * `team`: key, name or id.
/// * `project`: name or id, among the team's projects.
/// * `label`: name or id, among the team's and the workspace's labels.
/// * `assignee`: `me` (the key's owner), an email, a name / display name, or
///   an id, among the team's members.
pub fn resolve_filters(
    client: &LinearClient,
    conn: &LinearConnection,
) -> LinearResult<ResolvedLinearFilters> {
    let team_ref = conn.team_ref().ok_or_else(|| {
        LinearError::Local(shelbi_core::Error::InvalidIssueTracker(
            "issue_tracker.linear.team is required".into(),
        ))
    })?;
    let teams = client.teams()?;
    let team = pick_team(&teams, team_ref)?;
    let mut out = ResolvedLinearFilters {
        team_id: team.id.clone(),
        team_key: team.key.clone(),
        team_name: team.name.clone(),
        project_id: None,
        project_name: None,
        label_id: None,
        label_name: None,
        assignee_id: None,
        assignee_name: None,
    };

    if let Some(wanted) = conn.project_ref() {
        let projects = client.team_projects(&team.id)?;
        let p = pick("project", wanted, &projects, |p| &p.id, |p| vec![&p.name], |p| p.name.clone())?;
        out.project_id = Some(p.id.clone());
        out.project_name = Some(p.name.clone());
    }

    if let Some(wanted) = conn.label_ref() {
        let labels = client.labels(&team.id)?;
        let l = pick("label", wanted, &labels, |l| &l.id, |l| vec![&l.name], |l| l.name.clone())?;
        out.label_id = Some(l.id.clone());
        out.label_name = Some(l.name.clone());
    }

    if let Some(wanted) = conn.assignee_ref() {
        let user = if wanted.eq_ignore_ascii_case("me") {
            client.viewer()?
        } else {
            let members = client.team_members(&team.id)?;
            pick(
                "assignee",
                wanted,
                &members,
                |u| &u.id,
                |u| vec![&u.email, &u.name, &u.display_name],
                |u| u.label(),
            )?
            .clone()
        };
        out.assignee_id = Some(user.id.clone());
        out.assignee_name = Some(user.label());
    }
    Ok(out)
}

/// The on-disk cache: the filters as written plus what they resolved to.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FilterCache {
    filters: LinearConnection,
    resolved: ResolvedLinearFilters,
    resolved_at: DateTime<Utc>,
}

/// `~/.shelbi/projects/<project>/linear.json`.
pub fn filter_cache_path(project: &str) -> shelbi_core::Result<PathBuf> {
    Ok(crate::project_dir(project)?.join(FILTER_CACHE_FILE))
}

/// [`resolve_filters`] behind the per-project cache. The cache is keyed on the
/// filters as written: a hit needs zero API calls; editing any filter in the
/// project config misses and re-resolves. Linear ids are stable, so a hit
/// stays valid across renames on the Linear side. A failed cache write is
/// ignored (the next call just resolves again).
pub fn resolve_filters_cached(
    client: &LinearClient,
    project: &str,
    conn: &LinearConnection,
) -> LinearResult<ResolvedLinearFilters> {
    let path = filter_cache_path(project)?;
    if let Some(cache) = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<FilterCache>(&b).ok())
    {
        if cache.filters == *conn {
            return Ok(cache.resolved);
        }
    }
    let resolved = resolve_filters(client, conn)?;
    let cache = FilterCache {
        filters: conn.clone(),
        resolved: resolved.clone(),
        resolved_at: Utc::now(),
    };
    if let Ok(serde_json::Value::Object(fresh)) = serde_json::to_value(&cache) {
        // Other keys in the file (the status catalog's sync stamp) are kept.
        let mut doc = read_cache_doc(&path);
        doc.extend(fresh);
        let _ = write_cache_doc(&path, &doc);
    }
    Ok(resolved)
}

/// The keys the filter cache owns in `linear.json`; anything else in the
/// file belongs to another reader (`crate::linear_statuses`).
const FILTER_CACHE_KEYS: &[&str] = &["filters", "resolved", "resolved_at"];

/// `linear.json` as a JSON object; empty when it's missing or unreadable.
pub(crate) fn read_cache_doc(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// Write `doc` as `linear.json` (atomically), or remove the file when `doc`
/// is empty.
pub(crate) fn write_cache_doc(
    path: &std::path::Path,
    doc: &serde_json::Map<String, serde_json::Value>,
) -> shelbi_core::Result<()> {
    if doc.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec_pretty(doc)
        .map_err(|e| shelbi_core::Error::Other(format!("encoding linear.json: {e}")))?;
    if let Some(parent) = path.parent() {
        crate::ensure_dir(parent)?;
    }
    crate::atomic_write(path, &bytes)
}

/// Record `resolved` as what `conn` resolves to for `project`, so the next
/// [`resolve_filters_cached`] needs no API call (connecting a project
/// resolves its filters once, in its preflight, then stores them here).
/// `linear.json` is shared with other Linear state, so only the filter
/// cache's own keys are replaced; every other key in the file is kept.
pub fn store_filter_cache(
    project: &str,
    conn: &LinearConnection,
    resolved: &ResolvedLinearFilters,
) -> shelbi_core::Result<()> {
    let path = filter_cache_path(project)?;
    let mut doc = read_cache_doc(&path);
    let cache = serde_json::to_value(FilterCache {
        filters: conn.clone(),
        resolved: resolved.clone(),
        resolved_at: Utc::now(),
    })
    .map_err(|e| shelbi_core::Error::Other(format!("encoding linear.json: {e}")))?;
    if let serde_json::Value::Object(cache) = cache {
        doc.extend(cache);
    }
    write_cache_doc(&path, &doc)
}

/// Drop a project's filter cache so the next resolve asks Linear again (e.g.
/// after a project or label was deleted and recreated).
pub fn invalidate_filter_cache(project: &str) -> shelbi_core::Result<()> {
    let path = filter_cache_path(project)?;
    let mut doc = read_cache_doc(&path);
    for key in FILTER_CACHE_KEYS {
        doc.remove(*key);
    }
    write_cache_doc(&path, &doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_client::{test_home, FakeLinear};

    fn conn(team: &str) -> LinearConnection {
        LinearConnection::for_team(team)
    }

    #[test]
    fn team_matches_key_name_or_id_case_insensitively() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let client = fake.client();
        for wanted in ["ENG", "eng", "Engineering", "team-eng"] {
            let r = resolve_filters(&client, &conn(wanted)).unwrap();
            assert_eq!((r.team_id.as_str(), r.team_key.as_str()), ("team-eng", "ENG"), "{wanted}");
            assert_eq!(r.project_id, None);
        }
        // A team-only resolve reads nothing but the team list.
        assert!(fake.calls().iter().all(|c| c.operation == "Teams"));
    }

    #[test]
    fn a_missing_team_lists_the_available_ones() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let err = resolve_filters(&fake.client(), &conn("OPS")).unwrap_err().to_string();
        assert_eq!(err, "no Linear team `OPS`; available: DES (Design), ENG (Engineering)");
    }

    #[test]
    fn every_filter_resolves() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let c = LinearConnection {
            team: "ENG".into(),
            project: Some("shelbi".into()),
            label: Some("BUG".into()),
            assignee: Some("grace@example.com".into()),
        };
        let r = resolve_filters(&fake.client(), &c).unwrap();
        assert_eq!(r.project_id.as_deref(), Some("proj-shelbi"));
        assert_eq!(r.project_name.as_deref(), Some("Shelbi"));
        assert_eq!(r.label_id.as_deref(), Some("label-bug"));
        assert_eq!(r.assignee_id.as_deref(), Some("user-grace"));
        assert_eq!(r.assignee_name.as_deref(), Some("grace <grace@example.com>"));
        assert_eq!(fake.calls_for("TeamProjects")[0].var("teamId"), "team-eng");
    }

    #[test]
    fn assignee_me_is_the_viewer() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let mut c = conn("ENG");
        c.assignee = Some("Me".into());
        let r = resolve_filters(&fake.client(), &c).unwrap();
        assert_eq!(r.assignee_id.as_deref(), Some("user-ada"));
        assert!(fake.calls_for("TeamMembers").is_empty());
    }

    #[test]
    fn ambiguous_and_missing_filters_are_clear_errors() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let mut c = conn("ENG");
        c.label = Some("nope".into());
        let err = resolve_filters(&fake.client(), &c).unwrap_err().to_string();
        assert!(err.starts_with("no Linear label `nope`; available: Bug,"), "{err}");

        // Two members share the name `sam`: ambiguous, and the error names ids.
        let mut c = conn("ENG");
        c.assignee = Some("sam".into());
        let err = resolve_filters(&fake.client(), &c).unwrap_err().to_string();
        assert!(err.contains("ambiguous") && err.contains("[user-sam-1]") && err.contains("[user-sam-2]"), "{err}");
    }

    #[test]
    fn the_cache_skips_linear_until_the_filters_change() {
        let home = test_home::fresh();
        std::fs::create_dir_all(home.path.join("projects/p")).unwrap();
        let fake = FakeLinear::new();
        let client = fake.client();
        let mut c = conn("ENG");
        let first = resolve_filters_cached(&client, "p", &c).unwrap();
        let calls = fake.calls().len();
        assert_eq!(resolve_filters_cached(&client, "p", &c).unwrap(), first);
        assert_eq!(fake.calls().len(), calls, "a cache hit makes no request");
        assert!(filter_cache_path("p").unwrap().exists());

        c.label = Some("Bug".into());
        let second = resolve_filters_cached(&client, "p", &c).unwrap();
        assert_eq!(second.label_id.as_deref(), Some("label-bug"));
        assert!(fake.calls().len() > calls);

        invalidate_filter_cache("p").unwrap();
        invalidate_filter_cache("p").unwrap();
        assert!(!filter_cache_path("p").unwrap().exists());
    }

    #[test]
    fn storing_the_cache_keeps_other_keys_and_serves_the_next_resolve() {
        let _home = test_home::fresh();
        let path = filter_cache_path("lin").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"states_hash":"abc"}"#).unwrap();
        let fake = FakeLinear::new();
        let resolved = resolve_filters(&fake.client(), &conn("ENG")).unwrap();
        store_filter_cache("lin", &conn("ENG"), &resolved).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(doc["states_hash"], "abc", "another feature's key survives");
        assert_eq!(doc["resolved"]["team_id"], "team-eng");
        let calls = fake.calls().len();
        assert_eq!(resolve_filters_cached(&fake.client(), "lin", &conn("ENG")).unwrap(), resolved);
        assert_eq!(fake.calls().len(), calls, "a cache hit asks Linear nothing");
    }
}
