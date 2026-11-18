//! `LinearStore` against a small stateful Linear simulator: `Issues` is
//! answered by evaluating the request's filter over an in-memory team, and
//! the mutations apply to it, so every trait method round-trips for real.
//! Workspace reads (viewer, teams, states, members) come from the bundled
//! fixtures (`tests/fixtures/linear/`).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use serde_json::{json, Value};
use shelbi_core::{Column, IssuePr, LinearConnection};

use super::*;
use crate::issue_store::{IssueFields, NewIssue, PrioMove};
use crate::linear_client::{
    fixture, test_home, LinearRequest, LinearResponse, LinearRunner, RetryPolicy,
};
use crate::ownership::{OwnershipGate, TrackerOwner};
use crate::SecretToken;

// --- the simulator -------------------------------------------------------------

/// What the runner does with the next request for an operation.
enum Fault {
    /// Answer with this response instead.
    Reply(LinearResponse),
    /// Apply the request, then fail as a timeout (the write landed, the
    /// response was lost).
    ApplyThenTimeout,
    /// Fail as a timeout without applying anything.
    Timeout,
}

#[derive(Default)]
struct World {
    issues: Vec<Value>,
    comments: BTreeMap<String, Vec<Value>>,
    calls: Vec<LinearRequest>,
    faults: BTreeMap<String, VecDeque<Fault>>,
    next_number: i64,
    clock: i64,
}

#[derive(Clone)]
struct Sim(Arc<Mutex<World>>);

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap()
}

fn states() -> Vec<Value> {
    let v: Value = serde_json::from_str(fixture("TeamStates").unwrap()).unwrap();
    v["data"]["team"]["states"]["nodes"].as_array().unwrap().clone()
}

fn state(id: &str) -> Value {
    let s = states().into_iter().find(|s| s["id"] == id).expect("known state");
    json!({ "id": s["id"], "name": s["name"], "type": s["type"] })
}

fn user(id: &str) -> Value {
    let v: Value = serde_json::from_str(fixture("TeamMembers").unwrap()).unwrap();
    let u = v["data"]["team"]["members"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == id)
        .cloned()
        .expect("known member");
    json!({ "id": u["id"], "name": u["name"], "displayName": u["displayName"], "email": u["email"] })
}

impl Sim {
    fn new() -> Self {
        let world = World {
            next_number: 1,
            ..Default::default()
        };
        Sim(Arc::new(Mutex::new(world)))
    }

    fn w(&self) -> std::sync::MutexGuard<'_, World> {
        self.0.lock().unwrap()
    }

    fn client(&self) -> LinearClient {
        let sim = self.clone();
        let runner: LinearRunner = Arc::new(move |req, _| sim.answer(req));
        LinearClient::with_runner(
            SecretToken::new("lin_api_sim".into(), crate::issue_auth::TokenSource::Keychain),
            runner,
        )
        .retry_policy(RetryPolicy {
            attempts: 2,
            base_delay: std::time::Duration::ZERO,
        })
    }

    fn fault(&self, op: &str, fault: Fault) {
        self.w().faults.entry(op.into()).or_default().push_back(fault);
    }

    fn calls(&self, op: &str) -> Vec<LinearRequest> {
        self.w().calls.iter().filter(|c| c.operation == op).cloned().collect()
    }

    fn tick(w: &mut World) -> String {
        w.clock += 1;
        (t0() + ChronoDuration::seconds(w.clock)).to_rfc3339()
    }

    /// Seed an issue straight into the team (as if made in Linear).
    fn seed(&self, title: &str, state_id: &str, sort: f64, extra: Value) -> String {
        let mut w = self.w();
        let n = w.next_number;
        w.next_number += 1;
        let at = Self::tick(&mut w);
        let mut issue = json!({
            "id": format!("uuid-{n}"),
            "identifier": format!("ENG-{n}"),
            "number": n,
            "title": title,
            "description": "",
            "sortOrder": sort,
            "url": format!("https://linear.app/acme/issue/ENG-{n}"),
            "createdAt": at,
            "updatedAt": at,
            "archivedAt": null,
            "state": state(state_id),
            "assignee": user("user-ada"),
            "labels": { "nodes": [] },
            "project": null,
            "attachments": { "nodes": [] },
            "team": { "id": "team-eng" },
        });
        if let Value::Object(extra) = extra {
            for (k, v) in extra {
                issue[k] = v;
            }
        }
        let id = issue["id"].as_str().unwrap().to_string();
        w.issues.push(issue);
        id
    }

    /// Change an issue in place (as if edited in Linear), bumping updatedAt.
    fn edit(&self, uuid: &str, f: impl FnOnce(&mut Value)) {
        let mut w = self.w();
        let at = Self::tick(&mut w);
        let issue = w.issues.iter_mut().find(|i| i["id"] == uuid).unwrap();
        f(issue);
        issue["updatedAt"] = json!(at);
    }

    fn issue(&self, uuid: &str) -> Value {
        self.w().issues.iter().find(|i| i["id"] == uuid).cloned().unwrap()
    }

    fn answer(&self, req: &LinearRequest) -> std::result::Result<LinearResponse, TransportFailure> {
        let fault = {
            let mut w = self.w();
            w.calls.push(req.clone());
            w.faults.get_mut(&req.operation).and_then(VecDeque::pop_front)
        };
        match fault {
            Some(Fault::Reply(r)) => return Ok(r),
            Some(Fault::Timeout) => return Err(TransportFailure::Timeout("sim".into())),
            Some(Fault::ApplyThenTimeout) => {
                let _ = self.apply(req);
                return Err(TransportFailure::Timeout("sim".into()));
            }
            None => {}
        }
        Ok(LinearResponse::ok(self.apply(req).to_string()))
    }

    fn apply(&self, req: &LinearRequest) -> Value {
        let v = &req.variables;
        let data = match req.operation.as_str() {
            "Issues" => self.issues(v),
            "IssueCreate" => self.create(v),
            "IssueUpdate" => {
                let issue = self.update(v["id"].as_str().unwrap(), &v["input"]);
                json!({ "issueUpdate": { "success": true, "issue": issue } })
            }
            "AttachmentCreate" => {
                let att = self.attach(&v["input"]);
                json!({ "attachmentCreate": { "success": true, "attachment": att } })
            }
            "IssueComments" => self.comments(v),
            "CommentCreate" => self.comment(&v["input"]),
            op => {
                let body: Value = serde_json::from_str(fixture(op).expect("fixture")).unwrap();
                return body;
            }
        };
        json!({ "data": data })
    }

    fn issues(&self, v: &Value) -> Value {
        let w = self.w();
        let mut hits: Vec<&Value> = w
            .issues
            .iter()
            .filter(|i| v["includeArchived"] == true || i["archivedAt"].is_null())
            .filter(|i| v["filter"].is_null() || eval(i, &v["filter"]))
            .collect();
        let key = if v["orderBy"] == "updatedAt" { "updatedAt" } else { "createdAt" };
        hits.sort_by(|a, b| b[key].as_str().cmp(&a[key].as_str()).then(b["number"].as_i64().cmp(&a["number"].as_i64())));
        let start: usize = v["after"].as_str().map(|c| c.parse().unwrap()).unwrap_or(0);
        let first = v["first"].as_u64().unwrap_or(50) as usize;
        let page: Vec<Value> = hits.iter().skip(start).take(first).map(|i| (*i).clone()).collect();
        let end = start + page.len();
        json!({ "issues": { "nodes": page, "pageInfo": {
            "hasNextPage": end < hits.len(), "endCursor": end.to_string() } } })
    }

    fn create(&self, v: &Value) -> Value {
        let input = &v["input"];
        let mut w = self.w();
        let n = w.next_number;
        w.next_number += 1;
        let at = Self::tick(&mut w);
        let labels: Vec<Value> = input["labelIds"]
            .as_array()
            .map(|ls| ls.iter().map(|l| json!({ "id": l })).collect())
            .unwrap_or_default();
        let issue = json!({
            "id": input["id"],
            "identifier": format!("ENG-{n}"),
            "number": n,
            "title": input["title"],
            "description": input["description"],
            "sortOrder": input["sortOrder"],
            "url": format!("https://linear.app/acme/issue/ENG-{n}"),
            "createdAt": at,
            "updatedAt": at,
            "archivedAt": null,
            "state": state(input["stateId"].as_str().unwrap()),
            "assignee": input["assigneeId"].as_str().map(user),
            "labels": { "nodes": labels },
            "project": input["projectId"].as_str().map(|p| json!({ "id": p })),
            "attachments": { "nodes": [] },
            "team": { "id": input["teamId"] },
        });
        w.issues.push(issue.clone());
        drop(w);
        let mut data = json!({ "issueCreate": { "success": true, "issue": issue } });
        if !v["attachment"].is_null() {
            data["attachmentCreate"] = json!({ "success": true, "attachment": self.attach(&v["attachment"]) });
        }
        data
    }

    fn update(&self, id: &str, input: &Value) -> Value {
        let mut w = self.w();
        let at = Self::tick(&mut w);
        let issue = w
            .issues
            .iter_mut()
            .find(|i| i["id"] == id || i["identifier"] == id)
            .expect("issue to update");
        for (k, val) in input.as_object().unwrap() {
            match k.as_str() {
                "stateId" => issue["state"] = state(val.as_str().unwrap()),
                "assigneeId" => issue["assignee"] = user(val.as_str().unwrap()),
                other => issue[other] = val.clone(),
            }
        }
        issue["updatedAt"] = json!(at);
        issue.clone()
    }

    fn attach(&self, input: &Value) -> Value {
        let mut w = self.w();
        let issue = w
            .issues
            .iter_mut()
            .find(|i| i["id"] == input["issueId"])
            .expect("issue to attach to");
        let att = json!({
            "id": format!("att-{}", input["url"].as_str().unwrap()),
            "url": input["url"],
            "metadata": input["metadata"],
        });
        let nodes = issue["attachments"]["nodes"].as_array_mut().unwrap();
        nodes.retain(|a| a["url"] != input["url"]);
        nodes.push(att.clone());
        att
    }

    fn comments(&self, v: &Value) -> Value {
        let w = self.w();
        let nodes: Vec<Value> = w
            .comments
            .get(v["id"].as_str().unwrap())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| v["filter"].is_null() || eval(c, &v["filter"]))
            .collect();
        json!({ "issue": { "comments": { "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null } } } })
    }

    fn comment(&self, input: &Value) -> Value {
        let mut w = self.w();
        let at = Self::tick(&mut w);
        let c = json!({ "id": input["id"], "body": input["body"], "createdAt": at, "user": user("user-ada") });
        let issue_id = input["issueId"].as_str().unwrap().to_string();
        w.comments.entry(issue_id.clone()).or_default().push(c.clone());
        if let Some(i) = w.issues.iter_mut().find(|i| i["id"] == issue_id) {
            i["updatedAt"] = json!(at);
        }
        json!({ "commentCreate": { "success": true, "comment": c } })
    }
}

/// Evaluate the subset of Linear's filter language the store sends.
fn eval(value: &Value, filter: &Value) -> bool {
    let Some(conds) = filter.as_object() else {
        return true;
    };
    conds.iter().all(|(k, cond)| match k.as_str() {
        "and" => cond.as_array().unwrap().iter().all(|c| eval(value, c)),
        "eq" => same(value, cond),
        "in" => cond.as_array().unwrap().iter().any(|c| same(value, c)),
        "nin" => !cond.as_array().unwrap().iter().any(|c| same(value, c)),
        "gt" => match (value.as_f64(), cond.as_f64()) {
            (Some(a), Some(b)) => a > b,
            _ => date(value) > date(cond),
        },
        "startsWith" => value.as_str().is_some_and(|s| s.starts_with(cond.as_str().unwrap())),
        "endsWith" => value.as_str().is_some_and(|s| s.ends_with(cond.as_str().unwrap())),
        "some" => value.as_array().is_some_and(|a| a.iter().any(|e| eval(e, cond))),
        "labels" | "attachments" => eval(&value[k]["nodes"], cond),
        field => !value[field].is_null() && eval(&value[field], cond),
    })
}

fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn date(v: &Value) -> DateTime<Utc> {
    v.as_str().unwrap().parse().unwrap()
}

// --- fixtures -----------------------------------------------------------------

struct Env {
    _home: test_home::Home,
    sim: Sim,
}

fn env() -> Env {
    let home = test_home::fresh();
    std::fs::create_dir_all(home.path.join("projects/p")).unwrap();
    Env {
        _home: home,
        sim: Sim::new(),
    }
}

impl Env {
    fn store(&self, conn: LinearConnection) -> LinearStore {
        LinearStore::with_client("p", conn, self.sim.client())
    }

    fn eng(&self) -> LinearStore {
        self.store(LinearConnection::for_team("ENG"))
    }
}

fn ids(board: &[IssueFile]) -> Vec<String> {
    board.iter().map(|tf| tf.task.id.clone()).collect()
}

fn spec(id: &str, column: Column) -> NewIssue {
    NewIssue::new(id, format!("Title {id}"), column, format!("Body of {id}."))
}

// --- reads ----------------------------------------------------------------------

#[test]
fn the_board_is_exactly_the_issues_matching_the_filters() {
    let env = env();
    let sim = &env.sim;
    let both = json!({ "project": { "id": "proj-shelbi" }, "labels": { "nodes": [{ "id": "label-shelbi" }] } });
    sim.seed("match", "state-todo", 1.0, both.clone());
    sim.seed("other project", "state-todo", 2.0, json!({ "project": { "id": "proj-mobile" }, "labels": { "nodes": [{ "id": "label-shelbi" }] } }));
    sim.seed("no label", "state-todo", 3.0, json!({ "project": { "id": "proj-shelbi" } }));
    let mut grace = both.clone();
    grace["assignee"] = user("user-grace");
    sim.seed("grace's", "state-todo", 4.0, grace);
    sim.seed("other team", "state-todo", 5.0, json!({ "team": { "id": "team-des" } }));
    sim.seed("done", "state-done", 6.0, both);

    let conn = LinearConnection {
        team: "ENG".into(),
        project: Some("Shelbi".into()),
        label: Some("shelbi".into()),
        assignee: Some("me".into()),
    };
    let store = env.store(conn);
    let all = store.list().unwrap();
    assert_eq!(ids(&all), ["eng-1", "eng-6"], "open match + its done history");
    assert_eq!(all[1].task.column, Column::done());
    assert_eq!(ids(&store.list_open().unwrap()), ["eng-1"]);
    assert_eq!(ids(&store.list_closed().unwrap()), ["eng-6"]);

    let call = sim.calls("Issues").into_iter().next().unwrap();
    let filter = call.var("filter");
    assert_eq!(filter["team"]["id"]["eq"], "team-eng");
    assert_eq!(filter["project"]["id"]["eq"], "proj-shelbi");
    assert_eq!(filter["labels"]["some"]["id"]["eq"], "label-shelbi");
    assert_eq!(filter["assignee"]["id"]["eq"], "user-ada");
    assert_eq!(call.var("includeArchived"), false);

    // Outside the filters is absent, even by id.
    assert!(store.get("eng-2").unwrap().is_none());
    assert!(store.get("eng-1").unwrap().is_some());
}

#[test]
fn the_board_paginates_and_ranks_by_sort_order() {
    let env = env();
    for n in 0..130 {
        // Descending sort orders: the last seeded issue sorts first.
        env.sim.seed(&format!("i{n}"), "state-todo", 1000.0 - n as f64, json!({}));
    }
    let board = env.eng().list_open().unwrap();
    assert_eq!(board.len(), 130);
    assert_eq!(env.sim.calls("Issues").len(), 2, "two pages of 100");
    assert_eq!(env.sim.calls("Issues")[1].var("after"), "100");
    assert_eq!(board[0].task.id, "eng-130");
    assert_eq!(board[0].task.priority, 0);
    assert_eq!(board[129].task.id, "eng-1");
    assert_eq!(board[129].task.priority, 129);
}

#[test]
fn states_map_to_statuses_and_issues_carry_their_assignee() {
    let env = env();
    let sim = &env.sim;
    sim.seed("a", "state-review", 1.0, json!({}));
    sim.seed("b", "state-progress", 1.0, json!({ "assignee": null }));
    sim.seed("c", "state-canceled", 1.0, json!({}));
    sim.seed("d", "state-triage", 1.0, json!({}));
    let store = env.eng();
    let by_id: BTreeMap<String, IssueFile> =
        store.list().unwrap().into_iter().map(|tf| (tf.task.id.clone(), tf)).collect();
    assert_eq!(by_id["eng-1"].task.column.as_str(), "in-review");
    assert_eq!(by_id["eng-1"].tracker_assignees, ["ada"]);
    assert_eq!(by_id["eng-2"].task.column, Column::in_progress());
    assert!(by_id["eng-2"].tracker_assignees.is_empty());
    assert_eq!(by_id["eng-3"].task.column, Column::canceled());
    assert_eq!(by_id["eng-4"].task.column.as_str(), "triage");
    assert_eq!(
        ids(&store.list_in_status(&Column::from_status_id("in-review")).unwrap()),
        ["eng-1"]
    );
}

#[test]
fn a_synced_catalog_maps_by_external_id_and_closed_follows_the_state_type() {
    let env = env();
    let path = crate::workflows::statuses_path("p").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "managed_by: linear\nstatuses:\n\
         - { id: todo, name: Todo, category: ready, external_id: state-todo }\n\
         - { id: review, name: In Review, category: handoff, external_id: state-review }\n\
         - { id: shipped, name: Done, category: done, external_id: state-done }\n",
    )
    .unwrap();
    let a = env.sim.seed("a", "state-review", 1.0, json!({}));
    env.sim.seed("b", "state-done", 1.0, json!({}));
    let store = env.eng();
    let open = store.list_open().unwrap();
    assert_eq!(ids(&open), ["eng-1"]);
    assert_eq!(open[0].task.column, Column::review(), "by external_id, not `in-review`");
    let closed = store.list_in_status(&Column::from_status_id("shipped")).unwrap();
    assert_eq!(ids(&closed), ["eng-2"], "a synced completed status reads from history");

    store.move_status("eng-1", &Column::todo(), "").unwrap();
    store.move_status("eng-1", &Column::review(), "").unwrap();
    assert_eq!(env.sim.issue(&a)["state"]["id"], "state-review");
    let cold = store.refresh_board(None, &[]).unwrap();
    let since = t0() + ChronoDuration::seconds(env.sim.w().clock);
    store.move_status("eng-1", &Column::from_status_id("shipped"), "").unwrap();
    assert_eq!(env.sim.issue(&a)["state"]["id"], "state-done");
    let warm = store.refresh_board(Some(since), &cold.board).unwrap();
    assert!(warm.board.is_empty(), "a closed card leaves the open board");
}

#[test]
fn a_rate_limit_surfaces_from_reads_and_writes() {
    let env = env();
    env.sim.seed("a", "state-todo", 1.0, json!({}));
    let store = env.eng();
    let limited = || {
        Fault::Reply(
            LinearResponse::ok(
                r#"{"errors":[{"message":"Rate limit exceeded","extensions":{"code":"RATELIMITED"}}]}"#,
            )
            .with_status(400)
            .with_header("X-RateLimit-Requests-Reset", ((Utc::now().timestamp() + 600) * 1000).to_string()),
        )
    };
    env.sim.fault("Issues", limited());
    let err = store.list().unwrap_err().to_string();
    assert!(err.contains("rate limit"), "{err}");
    // Parked: later calls fail fast without reaching Linear.
    let before = env.sim.calls("Issues").len();
    let err = store.move_status("eng-1", &Column::in_progress(), "").unwrap_err().to_string();
    assert!(err.contains("rate limit") && err.contains("paused"), "{err}");
    assert_eq!(env.sim.calls("Issues").len(), before);
}

// --- create ---------------------------------------------------------------------

#[test]
fn add_applies_every_filter_and_shelbi_fields_survive_a_reread() {
    let env = env();
    let conn = LinearConnection {
        team: "ENG".into(),
        project: Some("Shelbi".into()),
        label: Some("shelbi".into()),
        assignee: Some("grace@example.com".into()),
    };
    let store = env.store(conn);
    let mut s = spec("linear-store", Column::todo());
    s.workflow = Some("app".into());
    s.branch = Some("jlong/linear-store".into());
    s.depends_on = vec!["eng-9".into()];
    s.pr = Some(IssuePr::new(42, Some("https://github.com/o/r/pull/42".into())));
    s.params.insert("estimate".into(), yaml_serde::Value::from("2h"));
    let (issue, assignees) = store.add_with_assignees(s).unwrap();
    assert_eq!(issue.id, "linear-store");
    assert_eq!(issue.column, Column::todo());
    assert_eq!(assignees, ["grace"]);

    let create = &env.sim.calls("IssueCreate")[0];
    let input = create.var("input");
    assert_eq!(input["teamId"], "team-eng");
    assert_eq!(input["projectId"], "proj-shelbi");
    assert_eq!(input["labelIds"], json!(["label-shelbi"]));
    assert_eq!(input["assigneeId"], "user-grace");
    assert_eq!(input["stateId"], "state-todo");
    assert_eq!(input["description"], "Body of linear-store.");
    let att = create.var("attachment");
    assert_eq!(att["url"], "https://shelbi.dev/i/ENG/linear-store");
    assert_eq!(att["issueId"], input["id"]);
    assert_eq!(att["title"], "Shelbi");

    // A fresh store (no process cache) finds it by its attachment.
    let fresh = env.store(store.connection().clone());
    let tf = fresh.get("linear-store").unwrap().expect("found by attachment");
    assert_eq!(tf.task.title, "Title linear-store");
    assert_eq!(tf.body, "Body of linear-store.");
    assert_eq!(tf.task.workflow.as_deref(), Some("app"));
    assert_eq!(tf.task.branch.as_deref(), Some("jlong/linear-store"));
    assert_eq!(tf.task.depends_on, ["eng-9"]);
    assert_eq!(tf.task.pr.as_ref().map(|p| p.number), Some(42));
    assert_eq!(tf.task.params["estimate"], yaml_serde::Value::from("2h"));
    assert_eq!(ids(&fresh.list().unwrap()), ["linear-store"]);
}

#[test]
fn add_assigns_the_viewer_without_an_assignee_filter_and_refuses_duplicates() {
    let env = env();
    let store = env.eng();
    let (_, assignees) = store.add_with_assignees(spec("x", Column::backlog())).unwrap();
    assert_eq!(assignees, ["ada"]);
    let input = env.sim.calls("IssueCreate")[0].var("input").clone();
    assert_eq!(input["assigneeId"], "user-ada");
    assert!(input.get("projectId").is_none() && input.get("labelIds").is_none());

    let err = store.add(spec("x", Column::backlog())).unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    // A Linear-authored issue's id is taken too.
    env.sim.seed("authored", "state-todo", 1.0, json!({}));
    assert!(store.add(spec("eng-2", Column::todo())).is_err());
    assert_eq!(env.sim.calls("IssueCreate").len(), 1);
}

#[test]
fn add_places_the_card_by_priority_and_appends_by_default() {
    let env = env();
    let store = env.eng();
    store.add(spec("a", Column::todo())).unwrap();
    store.add(spec("b", Column::todo())).unwrap();
    let mut top = spec("c", Column::todo());
    top.priority = Some(0);
    let issue = store.add(top).unwrap();
    assert_eq!(issue.priority, 0);
    let col = store.list_in_status(&Column::todo()).unwrap();
    assert_eq!(ids(&col), ["c", "a", "b"]);
    assert_eq!(col.iter().map(|t| t.task.priority).collect::<Vec<_>>(), [0, 1, 2]);
    let sorts: Vec<f64> = env
        .sim
        .calls("IssueCreate")
        .iter()
        .map(|c| c.var("input")["sortOrder"].as_f64().unwrap())
        .collect();
    assert_eq!(sorts, [0.0, 1000.0, -1000.0]);
}

#[test]
fn an_unknown_outcome_create_is_settled_by_its_id() {
    let env = env();
    let store = env.eng();
    // Landed, response lost: finished, never duplicated.
    env.sim.fault("IssueCreate", Fault::ApplyThenTimeout);
    let issue = store.add(spec("landed", Column::todo())).unwrap();
    assert_eq!(issue.id, "landed");
    assert_eq!(env.sim.w().issues.len(), 1);
    assert!(store.get("landed").unwrap().is_some());

    // Never landed: a typed unknown-outcome error naming the check.
    env.sim.fault("IssueCreate", Fault::Timeout);
    let err = store.add(spec("lost", Column::todo())).unwrap_err();
    assert!(err.is_unknown_write_outcome(), "{err:?}");
    assert!(err.to_string().contains("shelbi issue show lost"), "{err}");
    assert_eq!(env.sim.w().issues.len(), 1);
}

#[test]
fn a_create_whose_attachment_failed_is_repaired() {
    let env = env();
    // The issue lands but the same request's attachmentCreate errors.
    let sim = env.sim.clone();
    let runner_sim = sim.clone();
    let runner: LinearRunner = Arc::new(move |req, _| {
        if req.operation == "IssueCreate" {
            let mut only_issue = req.clone();
            only_issue.variables["attachment"] = Value::Null;
            let _ = runner_sim.answer(&only_issue);
            return Ok(LinearResponse::ok(
                r#"{"data":{"issueCreate":{"success":true}},"errors":[{"message":"attachment failed"}]}"#,
            ));
        }
        runner_sim.answer(req)
    });
    let client = LinearClient::with_runner(
        SecretToken::new("k".into(), crate::issue_auth::TokenSource::Keychain),
        runner,
    );
    let store = LinearStore::with_client("p", LinearConnection::for_team("ENG"), client);
    store.add(spec("repaired", Column::todo())).unwrap();
    let issue = sim.w().issues[0].clone();
    assert_eq!(issue["attachments"]["nodes"][0]["url"], "https://shelbi.dev/i/ENG/repaired");
    assert_eq!(sim.calls("AttachmentCreate").len(), 1);
}

// --- moves, priority, fields ------------------------------------------------------

#[test]
fn moves_cancel_and_done_round_trip_through_linear_states() {
    let env = env();
    let store = env.eng();
    store.add(spec("m", Column::todo())).unwrap();
    crate::set_task_assignment("p", "m", Some("alpha")).unwrap();

    let mv = store.move_status("m", &Column::in_progress(), "").unwrap().unwrap();
    assert_eq!((mv.from, mv.to.clone()), (Column::todo(), Column::in_progress()));
    assert_eq!(mv.workflow, "default");
    assert_eq!(env.sim.calls("IssueUpdate")[0].var("input")["stateId"], "state-progress");
    assert!(store.move_status("m", &Column::in_progress(), "").unwrap().is_none());

    let review = Column::from_status_id("in-review");
    store.move_status("m", &review, "").unwrap();
    assert_eq!(store.get("m").unwrap().unwrap().task.column, review);

    store.move_status("m", &Column::done(), "").unwrap();
    let uuid = env.sim.w().issues[0]["id"].as_str().unwrap().to_string();
    assert_eq!(env.sim.issue(&uuid)["state"]["type"], "completed");
    assert_eq!(ids(&store.list_closed().unwrap()), ["m"]);

    let mv = store.cancel("m", "nope").unwrap().unwrap();
    assert_eq!(mv.to, Column::canceled());
    assert_eq!(env.sim.issue(&uuid)["state"]["id"], "state-canceled");
    assert_eq!(crate::get_task_assignment("p", "m").unwrap(), None, "cancel unassigns");

    let err = store.move_status("m", &Column::review(), "").unwrap_err().to_string();
    assert!(err.contains("no Linear workflow state") && err.contains("`review`"), "{err}");
}

#[test]
fn set_priority_reorders_by_sort_order() {
    let env = env();
    let store = env.eng();
    for id in ["a", "b", "c", "d"] {
        store.add(spec(id, Column::todo())).unwrap();
    }
    let order = |s: &LinearStore| ids(&s.list_in_status(&Column::todo()).unwrap());
    store.set_priority("c", PrioMove::Top).unwrap();
    assert_eq!(order(&store), ["c", "a", "b", "d"]);
    store.set_priority("c", PrioMove::Bottom).unwrap();
    assert_eq!(order(&store), ["a", "b", "d", "c"]);
    store.set_priority("d", PrioMove::Up).unwrap();
    assert_eq!(order(&store), ["a", "d", "b", "c"]);
    store.set_priority("a", PrioMove::Down).unwrap();
    assert_eq!(order(&store), ["d", "a", "b", "c"]);
    store.set_priority("c", PrioMove::Set(1)).unwrap();
    assert_eq!(order(&store), ["d", "c", "a", "b"]);
    let updates = env.sim.calls("IssueUpdate").len();
    store.set_priority("d", PrioMove::Top).unwrap();
    assert_eq!(env.sim.calls("IssueUpdate").len(), updates, "a no-op move writes nothing");
    store.renumber(&Column::todo()).unwrap();
    assert_eq!(env.sim.calls("IssueUpdate").len(), updates, "renumber has nothing to do");
}

#[test]
fn set_priority_respaces_a_column_whose_neighbours_collide() {
    let env = env();
    for id in ["x", "y", "z"] {
        env.sim.seed(id, "state-todo", 5.0, json!({}));
    }
    let store = env.eng();
    store.set_priority("eng-3", PrioMove::Set(1)).unwrap();
    let col = ids(&store.list_in_status(&Column::todo()).unwrap());
    assert_eq!(col, ["eng-1", "eng-3", "eng-2"]);
}

#[test]
fn set_fields_edits_linear_fields_and_the_attachment() {
    let env = env();
    let uuid = env.sim.seed("authored", "state-todo", 1.0, json!({ "description": "Prose." }));
    let store = env.eng();

    let fields = IssueFields {
        title: Some("Renamed".into()),
        body: Some("New prose.".into()),
        workflow: Some(Some("app".into())),
        branch: Some(Some("b".into())),
        ..Default::default()
    };
    store.set_fields("eng-1", fields).unwrap();
    let raw = env.sim.issue(&uuid);
    assert_eq!(raw["title"], "Renamed");
    assert_eq!(raw["description"], "New prose.");
    let att = &raw["attachments"]["nodes"][0];
    assert_eq!(att["url"], "https://shelbi.dev/i/ENG/eng-1", "keeps the id it is known by");
    assert_eq!(att["metadata"]["shelbi"]["id"], "eng-1");
    assert_eq!(att["metadata"]["shelbi"]["workflow"], "app");

    let mut params = BTreeMap::new();
    params.insert("k".to_string(), Some(yaml_serde::Value::from(1)));
    store
        .set_fields("eng-1", IssueFields { params, depends_on: Some(vec!["z".into()]), ..Default::default() })
        .unwrap();
    let tf = env.eng().get("eng-1").unwrap().unwrap();
    assert_eq!(tf.task.title, "Renamed");
    assert_eq!(tf.task.workflow.as_deref(), Some("app"));
    assert_eq!(tf.task.branch.as_deref(), Some("b"));
    assert_eq!(tf.task.depends_on, ["z"]);
    assert_eq!(tf.task.params["k"], yaml_serde::Value::from(1));

    // Unchanged values write nothing.
    let writes = env.sim.calls("IssueUpdate").len() + env.sim.calls("AttachmentCreate").len();
    store
        .set_fields("eng-1", IssueFields { title: Some("Renamed".into()), ..Default::default() })
        .unwrap();
    assert_eq!(env.sim.calls("IssueUpdate").len() + env.sim.calls("AttachmentCreate").len(), writes);

    // Workspace routing stays local.
    store
        .set_fields("eng-1", IssueFields { assigned_to: Some(Some("alpha".into())), ..Default::default() })
        .unwrap();
    assert_eq!(env.sim.calls("IssueUpdate").len() + env.sim.calls("AttachmentCreate").len(), writes);
    assert_eq!(store.get("eng-1").unwrap().unwrap().task.assigned_to.as_deref(), Some("alpha"));
}

#[test]
fn reject_review_appends_feedback_moves_to_ready_and_unassigns() {
    let env = env();
    let uuid = env.sim.seed("r", "state-review", 1.0, json!({ "description": "Spec." }));
    crate::set_task_assignment("p", "eng-1", Some("review-1")).unwrap();
    let store = env.eng();
    let mv = store.reject_review("eng-1", &Column::todo(), "Needs tests", "2026-11-18").unwrap();
    assert_eq!(mv.unwrap().to, Column::todo());
    let desc = env.sim.issue(&uuid)["description"].as_str().unwrap().to_string();
    assert!(desc.starts_with("Spec.") && desc.contains("Needs tests"), "{desc}");
    assert_eq!(crate::get_task_assignment("p", "eng-1").unwrap(), None);
}

#[test]
fn delete_cancels_and_park_clear_use_local_state() {
    let env = env();
    let uuid = env.sim.seed("d", "state-review", 1.0, json!({}));
    crate::set_task_assignment("p", "eng-1", Some("review-1")).unwrap();
    let store = env.eng();
    assert_eq!(store.park_review("eng-1").unwrap().as_deref(), Some("review-1"));
    store.clear_parked("eng-1").unwrap();
    store.delete("eng-1").unwrap();
    assert_eq!(env.sim.issue(&uuid)["state"]["type"], "canceled");
    store.delete("eng-1").unwrap();
    store.delete("missing").unwrap();
}

// --- comments -------------------------------------------------------------------

#[test]
fn comments_round_trip_with_the_marker_stripped() {
    let env = env();
    env.sim.seed("c", "state-todo", 1.0, json!({}));
    let store = env.eng();
    let posted = store.add_comment("eng-1", "Hello from Shelbi").unwrap();
    assert_eq!(posted.body, "Hello from Shelbi");
    assert_eq!(posted.author.as_deref(), Some("ada"));
    let sent = env.sim.calls("CommentCreate")[0].var("input")["body"].as_str().unwrap().to_string();
    assert!(sent.contains("<!-- shelbi:comment/"), "{sent}");
    let listed = store.list_comments("eng-1").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].body, "Hello from Shelbi");
    assert!(store.list_comments("missing").unwrap().is_empty());

    // A lost response whose comment landed is found by its id.
    env.sim.fault("CommentCreate", Fault::ApplyThenTimeout);
    assert_eq!(store.add_comment("eng-1", "again").unwrap().body, "again");
    env.sim.fault("CommentCreate", Fault::Timeout);
    assert!(store.add_comment("eng-1", "lost").unwrap_err().is_unknown_write_outcome());
    assert_eq!(store.list_comments("eng-1").unwrap().len(), 2);
}

// --- ownership --------------------------------------------------------------------

#[test]
fn ownership_tokens_and_claim_work_on_linear_issues() {
    let env = env();
    env.sim.seed("mine", "state-todo", 1.0, json!({}));
    env.sim.seed("grace's", "state-todo", 2.0, json!({ "assignee": user("user-grace") }));
    env.sim.seed("nobody's", "state-todo", 3.0, json!({ "assignee": null }));
    let store = env.eng();
    assert_eq!(store.viewer_login().as_deref(), Some("ada"));
    let gate = OwnershipGate::RequireAssignee(store.viewer_login().unwrap());
    let board = store.list_open().unwrap();
    let owners: Vec<String> = board
        .iter()
        .map(|tf| gate.owner(&tf.tracker_assignees).unwrap().token())
        .collect();
    assert_eq!(owners, ["me", "other:grace", "unassigned"]);

    let after = store.claim("eng-2").unwrap();
    assert_eq!(after, ["ada"]);
    assert_eq!(env.sim.calls("IssueUpdate")[0].var("input")["assigneeId"], "user-ada");
    let tf = store.get("eng-2").unwrap().unwrap();
    assert_eq!(gate.owner(&tf.tracker_assignees), Some(TrackerOwner::Me));
}

// --- changes ----------------------------------------------------------------------

#[test]
fn refresh_board_reads_cold_then_folds_deltas_from_linear() {
    let env = env();
    let sim = &env.sim;
    let a = sim.seed("a", "state-todo", 1.0, json!({}));
    let b = sim.seed("b", "state-todo", 2.0, json!({}));
    let c = sim.seed("c", "state-progress", 1.0, json!({}));
    sim.seed("done", "state-done", 1.0, json!({}));
    let conn = LinearConnection { assignee: Some("me".into()), ..LinearConnection::for_team("ENG") };
    let store = env.store(conn);

    let cold = store.refresh_board(None, &[]).unwrap();
    assert_eq!(ids(&cold.board), ["eng-1", "eng-2", "eng-3"]);
    assert_eq!(cold.numbers, [("eng-1".to_string(), 1), ("eng-2".to_string(), 2), ("eng-3".to_string(), 3)]);
    assert_eq!(cold.viewer_login.as_deref(), Some("ada"));
    let since = t0() + ChronoDuration::seconds(sim.w().clock);

    // Quiet: one team-scoped delta read, same board.
    let quiet = store.refresh_board(Some(since), &cold.board).unwrap();
    assert_eq!(ids(&quiet.board), ids(&cold.board));
    let delta = sim.calls("Issues").last().unwrap().clone();
    assert_eq!(delta.var("includeArchived"), true);
    assert!(delta.var("filter").get("assignee").is_none(), "deltas are team-scoped");

    // Moved in Linear, reordered, reassigned away, and closed.
    sim.edit(&a, |i| i["state"] = state("state-progress"));
    sim.edit(&c, |i| i["sortOrder"] = json!(5.0));
    sim.edit(&b, |i| i["assignee"] = user("user-grace"));
    let warm = store.refresh_board(Some(since), &cold.board).unwrap();
    assert_eq!(ids(&warm.board), ["eng-1", "eng-3"]);
    assert_eq!(warm.board[0].task.column, Column::in_progress());
    assert_eq!(warm.board[0].task.priority, 0, "sort 1.0 before 5.0");
    assert_eq!(warm.board[1].task.priority, 1);

    let since2 = t0() + ChronoDuration::seconds(sim.w().clock);
    sim.edit(&a, |i| i["state"] = state("state-done"));
    sim.edit(&c, |i| i["archivedAt"] = json!("2026-11-11T00:00:00Z"));
    let closed = store.refresh_board(Some(since2), &warm.board).unwrap();
    assert!(closed.board.is_empty(), "{:?}", ids(&closed.board));
}

#[test]
fn a_warm_tick_in_a_fresh_process_reads_the_board_whole() {
    let env = env();
    env.sim.seed("a", "state-todo", 1.0, json!({}));
    let cold = env.eng().refresh_board(None, &[]).unwrap();
    let since = t0() + ChronoDuration::seconds(env.sim.w().clock);
    let uuid = env.sim.seed("b", "state-todo", 2.0, json!({}));
    env.sim.edit(&uuid, |_| {});
    // A new store clears the process caches (a restarted daemon).
    let read = env.eng().refresh_board(Some(since), &cold.board).unwrap();
    assert_eq!(ids(&read.board), ["eng-1", "eng-2"]);
}

#[test]
fn poll_changes_reports_a_card_moved_in_linear_and_new_comments() {
    let env = env();
    let sim = &env.sim;
    let a = sim.seed("a", "state-todo", 1.0, json!({}));
    let store = env.eng();
    let (none, cursor) = store.poll_changes(&Cursor::start()).unwrap();
    assert!(none.is_empty());
    assert!(cursor.watermark().is_some());

    let (quiet, same) = store.poll_changes(&cursor).unwrap();
    assert!(quiet.is_empty());
    assert_eq!(same, cursor);

    sim.edit(&a, |i| i["state"] = state("state-review"));
    store.add_comment("eng-1", "from shelbi").unwrap();
    let (changes, next) = store.poll_changes(&cursor).unwrap();
    assert!(next.watermark() > cursor.watermark());
    let moved = changes.iter().find_map(|c| match c {
        IssueChange::Upserted(tf) => Some(tf.task.column.clone()),
        _ => None,
    });
    assert_eq!(moved.map(|c| c.as_str().to_string()).as_deref(), Some("in-review"));
    let comment = changes.iter().find_map(|c| match c {
        IssueChange::CommentAdded { issue_id, comment } => Some((issue_id.clone(), comment.body.clone())),
        _ => None,
    });
    assert_eq!(comment, Some(("eng-1".to_string(), "from shelbi".to_string())));
    let (after, _) = store.poll_changes(&next).unwrap();
    assert!(after.is_empty(), "nothing is reported twice");
}

#[test]
fn closed_page_reads_history_a_page_at_a_time() {
    let env = env();
    for n in 0..60 {
        env.sim.seed(&format!("d{n}"), "state-done", 1.0, json!({}));
    }
    env.sim.seed("open", "state-todo", 1.0, json!({}));
    let store = env.eng();
    let first = store.closed_page(None).unwrap();
    assert_eq!(first.issues.len(), 50);
    assert_eq!(first.issues[0].task.id, "eng-60", "newest first");
    let cursor = first.next_cursor.expect("more");
    let second = store.closed_page(Some(&cursor)).unwrap();
    assert_eq!(second.issues.len(), 10);
    assert!(second.next_cursor.is_none());
    let call = env.sim.calls("Issues").last().unwrap().clone();
    assert_eq!(call.var("orderBy"), "updatedAt");
    assert_eq!(call.var("filter")["state"]["type"]["in"], json!(["completed", "canceled"]));
}

#[test]
fn fetch_many_reads_known_numbers_in_one_request() {
    let env = env();
    env.sim.seed("a", "state-todo", 1.0, json!({}));
    env.sim.seed("b", "state-todo", 2.0, json!({}));
    let store = env.eng();
    store.add(spec("named", Column::todo())).unwrap();
    let before = env.sim.calls("Issues").len();
    let mut got = ids(&store.fetch_many(&["eng-1", "eng-2", "named", "ghost"]).unwrap());
    got.sort();
    assert_eq!(got, ["eng-1", "eng-2", "named"]);
    let numbered = &env.sim.calls("Issues")[before];
    assert!(numbered.var("filter")["number"]["in"].as_array().unwrap().len() >= 2);
    let (found, failed) = store.fetch_many_partial(&["eng-1", "ghost"]).unwrap();
    assert_eq!(ids(&found), ["eng-1"]);
    assert!(failed.is_empty());
}

#[test]
fn the_index_identity_follows_the_filters() {
    let a = crate::board_index::linear_board_repo(&LinearConnection::for_team("ENG"));
    assert_eq!(a, "linear:eng");
    let b = crate::board_index::linear_board_repo(&LinearConnection {
        team: " ENG ".into(),
        project: Some("Shelbi".into()),
        label: Some("".into()),
        assignee: Some("me".into()),
    });
    assert_eq!(b, "linear:eng/project:shelbi/assignee:me");
    let cfg = shelbi_core::IssueTrackerConfig {
        backend: shelbi_core::IssueTrackerBackend::Linear,
        linear: Some(LinearConnection::for_team("ENG")),
        ..Default::default()
    };
    assert_eq!(crate::expected_board_repo(&cfg).as_deref(), Some("linear:eng"));
}

#[test]
fn a_cached_store_from_config_reaches_the_installed_fake() {
    let _home = test_home::fresh();
    std::fs::create_dir_all(_home.path.join("projects/p")).unwrap();
    let fake = crate::linear_client::FakeLinear::new();
    let _guard = fake.install();
    let cfg = shelbi_core::IssueTrackerConfig {
        backend: shelbi_core::IssueTrackerBackend::Linear,
        linear: Some(LinearConnection::for_team("ENG")),
        ..Default::default()
    };
    let store = crate::issue_store::build_store("p", &cfg).unwrap();
    let board = store.list().unwrap();
    assert_eq!(ids(&board), ["eng-1", "linear-store"]);
    assert_eq!(board[1].task.depends_on, ["eng-1"]);
}
