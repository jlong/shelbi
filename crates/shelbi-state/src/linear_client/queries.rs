//! Typed Linear objects, pagination, and the read queries the backend needs.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::client::LinearClient;
use super::error::{LinearError, LinearResult};
use super::transport::LinearRequest;

/// Nodes requested per page.
pub const PAGE_SIZE: u32 = 100;

/// A hard stop on pages per [`LinearClient::paginate`] call (100 × 100 nodes),
/// so a server that keeps answering `hasNextPage` can't loop forever.
const MAX_PAGES: usize = 100;

/// A Linear user (the viewer, or a team member).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearUser {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default = "yes")]
    pub active: bool,
}

fn yes() -> bool {
    true
}

impl LinearUser {
    /// `Display Name <email>` (or the bits that exist).
    pub fn label(&self) -> String {
        let name = if self.display_name.is_empty() { &self.name } else { &self.display_name };
        if self.email.is_empty() {
            name.clone()
        } else {
            format!("{name} <{}>", self.email)
        }
    }
}

/// A Linear team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearTeam {
    pub id: String,
    pub key: String,
    pub name: String,
}

/// A workflow state's category. Linear's six fixed types; any type a newer
/// Linear adds reads as [`LinearStateType::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinearStateType {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
    #[serde(other)]
    Unknown,
}

impl LinearStateType {
    /// The wire token (`started`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            LinearStateType::Triage => "triage",
            LinearStateType::Backlog => "backlog",
            LinearStateType::Unstarted => "unstarted",
            LinearStateType::Started => "started",
            LinearStateType::Completed => "completed",
            LinearStateType::Canceled => "canceled",
            LinearStateType::Unknown => "unknown",
        }
    }
}

/// A team's workflow state (a board column). `position` orders states within
/// a team.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinearState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: LinearStateType,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub position: f64,
}

/// A project's status (`Planned`, `In Progress`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearProjectStatus {
    pub name: String,
    /// `backlog` | `planned` | `started` | `paused` | `completed` | `canceled`.
    #[serde(rename = "type", default)]
    pub kind: String,
}

/// A Linear project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearProject {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub status: Option<LinearProjectStatus>,
}

impl LinearProject {
    /// The status name, or empty. (Linear deprecated `Project.state` for
    /// `status`; this is its replacement.)
    pub fn state(&self) -> &str {
        self.status.as_ref().map(|s| s.name.as_str()).unwrap_or("")
    }
}

/// An issue label: a team label or a workspace label (shared by all teams).
/// Group labels (`is_group`) hold other labels and can't be put on an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearLabel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub is_group: bool,
}

/// GraphQL connection paging info.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    #[serde(default)]
    pub end_cursor: Option<String>,
}

/// A GraphQL connection: one page of `nodes` plus its [`PageInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection<T> {
    pub nodes: Vec<T>,
    #[serde(default)]
    pub page_info: PageInfo,
}

const PAGE_INFO: &str = "pageInfo { hasNextPage endCursor }";
const USER_FIELDS: &str = "id name displayName email active";

impl LinearClient {
    /// Collect every node of a paginated connection.
    ///
    /// `query` must declare `$first: Int` and `$after: String` and pass them to
    /// the connection; `path` is the field path from `data` to the connection
    /// (e.g. `["team", "states"]`). `variables` holds the query's other
    /// variables; `first` / `after` are set per page.
    pub fn paginate<T: DeserializeOwned>(
        &self,
        operation: &str,
        query: &str,
        variables: Value,
        path: &[&str],
    ) -> LinearResult<Vec<T>> {
        let mut vars = match variables {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                return Err(LinearError::Decode {
                    detail: format!("paginate variables must be an object, got {other}"),
                })
            }
        };
        vars.insert("first".into(), json!(PAGE_SIZE));
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            vars.insert("after".into(), after.clone().map(Value::String).unwrap_or(Value::Null));
            let req = LinearRequest::new(operation, query, Value::Object(vars.clone()));
            let mut data = self.query_value(&req)?;
            let mut node = &mut data;
            for key in path {
                node = node.get_mut(*key).ok_or_else(|| LinearError::Decode {
                    detail: format!("{operation}: no `{key}` in the response"),
                })?;
            }
            let page: Connection<T> = serde_json::from_value(node.take()).map_err(|e| {
                LinearError::Decode {
                    detail: format!("{operation}: {e}"),
                }
            })?;
            out.extend(page.nodes);
            match page.page_info.end_cursor {
                Some(cursor) if page.page_info.has_next_page && after.as_ref() != Some(&cursor) => {
                    after = Some(cursor)
                }
                _ => return Ok(out),
            }
        }
        Ok(out)
    }

    /// The user the API key belongs to.
    pub fn viewer(&self) -> LinearResult<LinearUser> {
        #[derive(Deserialize)]
        struct Data {
            viewer: LinearUser,
        }
        let req = LinearRequest::new(
            "Viewer",
            format!("query Viewer {{ viewer {{ {USER_FIELDS} }} }}"),
            json!({}),
        );
        Ok(self.query::<Data>(&req)?.viewer)
    }

    /// Every team the key can see, sorted by key.
    pub fn teams(&self) -> LinearResult<Vec<LinearTeam>> {
        let query = format!(
            "query Teams($first: Int, $after: String) {{ teams(first: $first, after: $after) \
             {{ nodes {{ id key name }} {PAGE_INFO} }} }}"
        );
        let mut teams: Vec<LinearTeam> = self.paginate("Teams", &query, json!({}), &["teams"])?;
        teams.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(teams)
    }

    /// A team's workflow states, in board order (`position`).
    pub fn team_states(&self, team_id: &str) -> LinearResult<Vec<LinearState>> {
        let query = format!(
            "query TeamStates($teamId: String!, $first: Int, $after: String) {{ team(id: $teamId) \
             {{ states(first: $first, after: $after) {{ nodes {{ id name type color position }} \
             {PAGE_INFO} }} }} }}"
        );
        let mut states: Vec<LinearState> =
            self.paginate("TeamStates", &query, json!({ "teamId": team_id }), &["team", "states"])?;
        states.sort_by(|a, b| a.position.total_cmp(&b.position));
        Ok(states)
    }

    /// The projects a team belongs to, sorted by name.
    pub fn team_projects(&self, team_id: &str) -> LinearResult<Vec<LinearProject>> {
        let query = format!(
            "query TeamProjects($teamId: String!, $first: Int, $after: String) {{ team(id: $teamId) \
             {{ projects(first: $first, after: $after) {{ nodes {{ id name status {{ name type }} }} \
             {PAGE_INFO} }} }} }}"
        );
        let mut projects: Vec<LinearProject> = self.paginate(
            "TeamProjects",
            &query,
            json!({ "teamId": team_id }),
            &["team", "projects"],
        )?;
        projects.sort_by_key(|p| p.name.to_lowercase());
        Ok(projects)
    }

    /// The labels usable on a team's issues: the team's own plus workspace
    /// labels (no team), sorted by name.
    pub fn labels(&self, team_id: &str) -> LinearResult<Vec<LinearLabel>> {
        let query = format!(
            "query Labels($teamId: ID, $first: Int, $after: String) {{ issueLabels(first: $first, \
             after: $after, filter: {{ or: [{{ team: {{ id: {{ eq: $teamId }} }} }}, \
             {{ team: {{ null: true }} }}] }}) {{ nodes {{ id name color isGroup }} {PAGE_INFO} }} }}"
        );
        let mut labels: Vec<LinearLabel> =
            self.paginate("Labels", &query, json!({ "teamId": team_id }), &["issueLabels"])?;
        labels.sort_by_key(|l| l.name.to_lowercase());
        Ok(labels)
    }

    /// A team's members, sorted by name.
    pub fn team_members(&self, team_id: &str) -> LinearResult<Vec<LinearUser>> {
        let query = format!(
            "query TeamMembers($teamId: String!, $first: Int, $after: String) {{ team(id: $teamId) \
             {{ members(first: $first, after: $after) {{ nodes {{ {USER_FIELDS} }} {PAGE_INFO} }} }} }}"
        );
        let mut users: Vec<LinearUser> = self.paginate(
            "TeamMembers",
            &query,
            json!({ "teamId": team_id }),
            &["team", "members"],
        )?;
        users.sort_by_key(|u| u.name.to_lowercase());
        Ok(users)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_client::{test_home, FakeLinear};

    #[test]
    fn viewer_and_teams_decode_from_fixtures() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let client = fake.client();
        let me = client.viewer().unwrap();
        assert_eq!(me.email, "ada@example.com");
        assert_eq!(me.label(), "ada <ada@example.com>");
        let keys: Vec<_> = client.teams().unwrap().into_iter().map(|t| t.key).collect();
        assert_eq!(keys, ["DES", "ENG"]);
    }

    #[test]
    fn states_come_back_in_position_order_with_typed_categories() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let states = fake.client().team_states("team-eng").unwrap();
        let names: Vec<_> = states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Triage", "Backlog", "Todo", "In Progress", "In Review", "Done", "Canceled"]);
        let kinds: Vec<_> = states.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, ["triage", "backlog", "unstarted", "started", "started", "completed", "canceled"]);
        assert_eq!(fake.calls_for("TeamStates")[0].var("teamId"), "team-eng");
    }

    #[test]
    fn projects_labels_and_members_decode() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let client = fake.client();
        let projects = client.team_projects("team-eng").unwrap();
        assert_eq!(projects[0].name, "Mobile");
        assert_eq!(projects[1].state(), "In Progress");
        let labels = client.labels("team-eng").unwrap();
        assert!(labels.iter().any(|l| l.name == "Feature" && l.is_group));
        let users = client.team_members("team-eng").unwrap();
        assert!(users.iter().any(|u| !u.active), "an inactive member is listed");
    }

    #[test]
    fn unknown_state_types_do_not_fail_decoding() {
        let s: LinearState =
            serde_json::from_value(json!({"id": "s", "name": "Odd", "type": "someday"})).unwrap();
        assert_eq!(s.kind, LinearStateType::Unknown);
    }

    #[test]
    fn paginate_follows_cursors_until_the_last_page() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        fake.queue_json(
            "Teams",
            r#"{"data":{"teams":{"nodes":[{"id":"t1","key":"A","name":"A"}],
                "pageInfo":{"hasNextPage":true,"endCursor":"c1"}}}}"#,
        );
        fake.queue_json(
            "Teams",
            r#"{"data":{"teams":{"nodes":[{"id":"t2","key":"B","name":"B"}],
                "pageInfo":{"hasNextPage":false,"endCursor":"c2"}}}}"#,
        );
        let teams = fake.client().teams().unwrap();
        assert_eq!(teams.len(), 2);
        let calls = fake.calls_for("Teams");
        assert_eq!(calls[0].var("after"), &Value::Null);
        assert_eq!(calls[1].var("after"), "c1");
        assert_eq!(calls[1].var("first"), PAGE_SIZE);
    }

    #[test]
    fn paginate_stops_on_a_repeated_cursor() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        fake.on(
            "Teams",
            r#"{"data":{"teams":{"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"same"}}}}"#,
        );
        fake.client().teams().unwrap();
        assert_eq!(fake.calls_for("Teams").len(), 2);
    }
}
