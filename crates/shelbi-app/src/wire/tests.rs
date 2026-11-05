//! One JSON round trip per read model. Each test pins the model's wire shape
//! against a literal, decodes it back and compares, and decodes `{}` to show
//! every field defaults. A failure here means the wire form changed: update
//! the literal on purpose (and, once the relay ships, its protocol version).

use std::collections::{BTreeSet, HashMap};
use std::fmt::Debug;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use shelbi_core::{Column, MachineKind, ProjectStatus, ProjectStatuses, Reachability, StatusCategory};
use shelbi_state::changelog::{ChangelogStamp, Link};
use shelbi_state::machine_state::MachineRecord;
use shelbi_state::{IssueFile, OwnershipGate, WorkspaceState, ZenModeState};

use crate::activity::{ActivityData, Event, SystemEvent, SystemKind, TaskCacheEntry, TaskMeta};
use crate::board::{BoardOutcome, BoardProject, BoardRead, ClosedPage};
use crate::changelog::{ChangelogData, ChangelogEntryView, ChangelogLink};
use crate::command::EditItem;
use crate::exec::EditTarget;
use crate::nav::View;
use crate::overlay::palette_search::{IssueDoc, SearchCorpus};
use crate::settings::summary::ProjectSummary;
use crate::view::{
    MachineRow, MachineWorkspace, MachinesModel, NavItem, ReviewRow, ReviewSlot, ReviewState,
    SidebarModel, SlotAssignment, WorkspaceBadge, WorkspaceRow,
};

/// `value` serializes to exactly `expected`, and decodes back equal.
fn assert_round_trip<T>(value: &T, expected: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    assert_wire_shape(value, &expected);
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "decoded value differs from the original");
}

/// `value` serializes to exactly `expected` (pretty-printed on failure).
fn assert_wire_shape<T: Serialize>(value: &T, expected: &Value) {
    let actual = serde_json::to_value(value).unwrap();
    assert!(
        &actual == expected,
        "wire shape changed\n--- expected\n{}\n--- actual\n{}",
        serde_json::to_string_pretty(expected).unwrap(),
        serde_json::to_string_pretty(&actual).unwrap(),
    );
}

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// An issue with its optional fields set, plus its JSON.
fn issue() -> (IssueFile, Value) {
    let json = json!({
        "task": {
            "id": "t-1",
            "title": "Fix it",
            "column": "in_progress",
            "priority": 2,
            "assigned_to": "alpha",
            "branch": "jlong/t-1",
            "created_at": "2026-11-02T10:00:00Z",
            "updated_at": "2026-11-02T11:00:00Z",
            "feature": "remote-window"
        },
        "body": "The body.",
        "tracker_assignees": ["jlong"]
    });
    (serde_json::from_value(json.clone()).unwrap(), json)
}

#[test]
fn sidebar_model_round_trips() {
    let model = SidebarModel {
        project_label: "Shelbi".into(),
        nav: vec![
            NavItem {
                label: "Issues".into(),
                view: View::Issues,
            },
            NavItem {
                label: "Orchestrator".into(),
                view: View::Session("orch".into()),
            },
        ],
        workspaces: vec![WorkspaceRow {
            name: "alpha".into(),
            machine: "devbox".into(),
            is_remote: true,
            current_task: Some("t-1".into()),
            agent: Some("developer".into()),
            badge: WorkspaceBadge::AwaitingPermission,
        }],
        reviews: vec![ReviewRow {
            task_id: "t-2".into(),
            title: "Review me".into(),
            branch: "jlong/t-2".into(),
            location: Some("devbox:3001".into()),
            workspace: Some("review-1".into()),
            state: ReviewState::Serving,
            number: Some(401),
            pr: None,
        }],
        config_error: Some("bad yaml".into()),
        board_loading: true,
        collapsed_machines: BTreeSet::from(["devbox".to_string()]),
        board_banner: Some("board 12s".into()),
        daemon_version_line: Some("daemon 0.13.1".into()),
        daemon_version_mismatch: true,
        status_line: "2 startup warnings".into(),
        zen_mode: ZenModeState::On,
        unread_errors: 3,
        session_lost: Some("logged out".into()),
        orchestrator_status: Some("supervisor gave up: crash loop".into()),
    };
    assert_round_trip(
        &model,
        json!({
            "project_label": "Shelbi",
            "nav": [
                { "label": "Issues", "view": "issues" },
                { "label": "Orchestrator", "view": { "session": "orch" } }
            ],
            "workspaces": [{
                "name": "alpha",
                "machine": "devbox",
                "is_remote": true,
                "current_task": "t-1",
                "agent": "developer",
                "badge": "awaiting_permission"
            }],
            "reviews": [{
                "task_id": "t-2",
                "title": "Review me",
                "branch": "jlong/t-2",
                "location": "devbox:3001",
                "workspace": "review-1",
                "state": "serving",
                "number": 401
            }],
            "config_error": "bad yaml",
            "board_loading": true,
            "collapsed_machines": ["devbox"],
            "board_banner": "board 12s",
            "daemon_version_line": "daemon 0.13.1",
            "daemon_version_mismatch": true,
            "status_line": "2 startup warnings",
            "zen_mode": "on",
            "unread_errors": 3,
            "session_lost": "logged out",
            "orchestrator_status": "supervisor gave up: crash loop"
        }),
    );

    let empty: SidebarModel = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, SidebarModel::default());
    // A row missing its badge reads as not loaded yet, never a made-up state.
    let row: WorkspaceRow = serde_json::from_str(r#"{"name":"alpha"}"#).unwrap();
    assert_eq!(row.badge, WorkspaceBadge::Loading);
}

#[test]
fn board_read_round_trips() {
    let (issue, issue_json) = issue();
    let workflow = shelbi_core::default_workflow();
    let read = BoardRead {
        project: Some(BoardProject {
            default_workflow_name: "default".into(),
            display_name: Some("Shelbi".into()),
            workspaces: vec!["alpha".into(), "beta".into()],
            ownership: OwnershipGate::RequireAssignee("jlong".into()),
            remote_search: true,
        }),
        state_snapshot: Some(shelbi_state::State {
            workspace_filter: Some("alpha".into()),
            ..Default::default()
        }),
        workflows: Ok(vec![workflow.clone()]),
        project_statuses: Err("statuses.yaml load failed: bad".into()),
        board: BoardOutcome::Warm {
            banner: Some("board 4m stale".into()),
            base_tasks: vec![issue],
        },
    };
    // The workflow's own form is its on-disk YAML shape, pinned by
    // shelbi-core's workflow round-trip tests; this pins the envelope
    // around it.
    let workflow_json = serde_json::to_value(&workflow).unwrap();
    let identities: Vec<Value> = workflow
        .statuses
        .iter()
        .map(|st| json!({ "id": st.id, "name": st.name, "category": st.category }))
        .collect();
    assert_round_trip(
        &read,
        json!({
            "project": {
                "default_workflow_name": "default",
                "display_name": "Shelbi",
                "workspaces": ["alpha", "beta"],
                "ownership": { "require_assignee": "jlong" },
                "remote_search": true
            },
            "state_snapshot": { "zen_mode": "off", "workspace_filter": "alpha" },
            "workflows": { "Ok": [{ "workflow": workflow_json, "statuses": identities }] },
            "project_statuses": { "Err": "statuses.yaml load failed: bad" },
            "board": {
                "warm": { "banner": "board 4m stale", "base_tasks": [issue_json] }
            }
        }),
    );
    // The identities are what carry the names: the workflow's own form
    // leaves them to `statuses.yaml`.
    assert!(workflow_json["statuses"][2].get("name").is_none());
    assert_eq!(identities[2], json!({ "id": "in-progress", "name": "In Progress", "category": "active" }));

    // Missing fields: no project, no workflows, and the loading state, never
    // an empty board.
    let empty: BoardRead = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.project, None);
    assert_eq!(empty.workflows, Ok(vec![]));
    assert_eq!(empty.project_statuses, Ok(ProjectStatuses::default()));
    assert_eq!(empty.board, BoardOutcome::Cold { banner: None });
    let project: BoardProject = serde_json::from_str("{}").unwrap();
    assert_eq!(project.ownership, OwnershipGate::FailClosed);

    let cold = BoardRead {
        project: None,
        state_snapshot: None,
        workflows: Err("workflow load failed: x".into()),
        project_statuses: Ok(ProjectStatuses {
            statuses: vec![ProjectStatus {
                id: "todo".into(),
                name: "Todo".into(),
                category: StatusCategory::Ready,
            }],
        }),
        board: BoardOutcome::Failed("refresh failed: x".into()),
    };
    assert_round_trip(
        &cold,
        json!({
            "project": null,
            "state_snapshot": null,
            "workflows": { "Err": "workflow load failed: x" },
            "project_statuses": { "Ok": { "statuses": [{ "id": "todo", "name": "Todo", "category": "ready" }] } },
            "board": { "failed": "refresh failed: x" }
        }),
    );
}

#[test]
fn closed_page_round_trips() {
    let (issue, issue_json) = issue();
    let page = ClosedPage {
        issues: vec![issue],
        next_cursor: Some("cursor-2".into()),
    };
    assert_round_trip(
        &page,
        json!({ "issues": [issue_json], "next_cursor": "cursor-2" }),
    );
    let empty: ClosedPage = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, ClosedPage::default());
}

#[test]
fn machines_model_round_trips() {
    let model = MachinesModel {
        display_name: Some("Shelbi".into()),
        machines: vec![MachineRow {
            name: "devbox".into(),
            kind: MachineKind::Ssh,
            host: Some("jlong@devbox".into()),
            is_local: false,
            tags: vec!["gpu".into()],
            remote: Some(MachineRecord {
                path: "/usr/local/bin/shelbi".into(),
                version: "0.13.1".into(),
                compatible: true,
                source: "path".into(),
                checked_at: ts("2026-11-02T09:00:00Z"),
                host: None,
            }),
            reachability: Reachability::Unreachable {
                error: "ssh timed out".into(),
            },
            workspaces: vec![
                MachineWorkspace {
                    name: "alpha".into(),
                    is_review_slot: false,
                    state: Some(WorkspaceState::Working),
                    current_task: Some("t-1".into()),
                    assignment: SlotAssignment::InProgress(vec!["t-1".into()]),
                    agent: Some("developer".into()),
                    runner: Some("Claude".into()),
                    task_title: Some("Fix it".into()),
                    branch: Some("jlong/t-1".into()),
                    review: None,
                },
                MachineWorkspace {
                    name: "review-1".into(),
                    is_review_slot: true,
                    state: Some(WorkspaceState::Serving),
                    current_task: Some("t-2".into()),
                    assignment: SlotAssignment::Review("t-2".into()),
                    agent: Some("review".into()),
                    runner: None,
                    task_title: None,
                    branch: None,
                    review: Some(ReviewSlot {
                        task_id: "t-2".into(),
                        state: ReviewState::Loading,
                        url: Some("http://devbox:3001".into()),
                    }),
                },
                MachineWorkspace {
                    name: "beta".into(),
                    ..Default::default()
                },
            ],
        }],
    };
    assert_round_trip(
        &model,
        json!({
            "display_name": "Shelbi",
            "machines": [{
                "name": "devbox",
                "kind": "ssh",
                "host": "jlong@devbox",
                "is_local": false,
                "tags": ["gpu"],
                "remote": {
                    "path": "/usr/local/bin/shelbi",
                    "version": "0.13.1",
                    "compatible": true,
                    "source": "path",
                    "checked_at": "2026-11-02T09:00:00Z"
                },
                "reachability": { "unreachable": { "error": "ssh timed out" } },
                "workspaces": [
                    {
                        "name": "alpha",
                        "is_review_slot": false,
                        "state": "working",
                        "current_task": "t-1",
                        "assignment": { "in_progress": ["t-1"] },
                        "agent": "developer",
                        "runner": "Claude",
                        "task_title": "Fix it",
                        "branch": "jlong/t-1",
                        "review": null
                    },
                    {
                        "name": "review-1",
                        "is_review_slot": true,
                        "state": "serving",
                        "current_task": "t-2",
                        "assignment": { "review": "t-2" },
                        "agent": "review",
                        "runner": null,
                        "task_title": null,
                        "branch": null,
                        "review": { "task_id": "t-2", "state": "loading", "url": "http://devbox:3001" }
                    },
                    {
                        "name": "beta",
                        "is_review_slot": false,
                        "state": null,
                        "current_task": null,
                        "assignment": "idle",
                        "agent": null,
                        "runner": null,
                        "task_title": null,
                        "branch": null,
                        "review": null
                    }
                ]
            }]
        }),
    );

    let empty: MachinesModel = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, MachinesModel::default());
    let row: MachineRow = serde_json::from_str("{}").unwrap();
    assert_eq!(row.kind, MachineKind::Local);
    assert_eq!(row.reachability, Reachability::Unknown);
}

#[test]
fn activity_data_round_trips() {
    let mut task_cache = HashMap::new();
    task_cache.insert(
        "t-1".to_string(),
        TaskCacheEntry {
            meta: Some(TaskMeta {
                title: "Fix it".into(),
                branch: Some("jlong/t-1".into()),
                assigned_to: Some("alpha".into()),
            }),
            mtime: Some(UNIX_EPOCH + Duration::from_secs(1_793_000_000)),
            fetched: Instant::now(),
            terminal: true,
        },
    );
    let data = ActivityData {
        events: vec![
            Event::Issue {
                ts: ts("2026-11-02T10:00:00Z"),
                project: Some("shelbi".into()),
                id: "t-1".into(),
                workflow: "default".into(),
                from: Column::from_status_id("todo"),
                to: Column::from_status_id("in-progress"),
                reason: "dispatch".into(),
                agent: Some("developer".into()),
                from_category: StatusCategory::Ready,
                to_category: StatusCategory::Active,
                raw: "raw issue line".into(),
            },
            Event::Workspace {
                ts: ts("2026-11-02T10:01:00Z"),
                project: None,
                name: "alpha".into(),
                prev: Some(WorkspaceState::Working),
                new: WorkspaceState::AwaitingInput,
                raw: "raw workspace line".into(),
            },
            Event::ZenDryRun {
                ts: ts("2026-11-02T10:02:00Z"),
                project: None,
                task_id: "t-3".into(),
                action: "promote".into(),
                detail: "would promote".into(),
                raw: "raw zen line".into(),
            },
            Event::Heartbeat {
                ts: ts("2026-11-02T10:03:00Z"),
                project: "shelbi".into(),
                raw: "raw heartbeat".into(),
            },
            Event::System(SystemEvent {
                ts: ts("2026-11-02T10:04:00Z"),
                kind: SystemKind::Push,
                project: Some("shelbi".into()),
                target: Some("t-1".into()),
                status: Some("pushed".into()),
                detail: Some("to origin".into()),
                raw: "raw push line".into(),
            }),
            Event::Unknown {
                ts: None,
                raw: "garbage".into(),
            },
        ],
        task_cache,
        status: Some("read failed".into()),
        has_older: true,
        window: Some(500),
    };
    let expected = json!({
        "events": [
            { "issue": {
                "ts": "2026-11-02T10:00:00Z",
                "project": "shelbi",
                "id": "t-1",
                "workflow": "default",
                "from": "todo",
                "to": "in_progress",
                "reason": "dispatch",
                "agent": "developer",
                "from_category": "ready",
                "to_category": "active",
                "raw": "raw issue line"
            } },
            { "workspace": {
                "ts": "2026-11-02T10:01:00Z",
                "project": null,
                "name": "alpha",
                "prev": "working",
                "new": "awaiting_input",
                "raw": "raw workspace line"
            } },
            { "zen_dry_run": {
                "ts": "2026-11-02T10:02:00Z",
                "project": null,
                "task_id": "t-3",
                "action": "promote",
                "detail": "would promote",
                "raw": "raw zen line"
            } },
            { "heartbeat": {
                "ts": "2026-11-02T10:03:00Z",
                "project": "shelbi",
                "raw": "raw heartbeat"
            } },
            { "system": {
                "ts": "2026-11-02T10:04:00Z",
                "kind": "push",
                "project": "shelbi",
                "target": "t-1",
                "status": "pushed",
                "detail": "to origin",
                "raw": "raw push line"
            } },
            { "unknown": { "ts": null, "raw": "garbage" } }
        ],
        "task_cache": {
            "t-1": {
                "meta": { "title": "Fix it", "branch": "jlong/t-1", "assigned_to": "alpha" },
                "mtime": { "secs_since_epoch": 1_793_000_000u64, "nanos_since_epoch": 0 },
                "terminal": true
            }
        },
        "status": "read failed",
        "has_older": true,
        "window": 500
    });
    assert_wire_shape(&data, &expected);

    // `ActivityData` has no `PartialEq` (a cache entry's `fetched` is an
    // `Instant`, which stays off the wire), so compare field by field.
    let back: ActivityData = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(back.events, data.events);
    assert_eq!(back.status, data.status);
    assert_eq!(back.has_older, data.has_older);
    assert_eq!(back.window, data.window);
    let (entry, original) = (&back.task_cache["t-1"], &data.task_cache["t-1"]);
    assert_eq!(entry.meta, original.meta);
    assert_eq!(entry.mtime, original.mtime);
    assert_eq!(entry.terminal, original.terminal);
    // A decoded entry counts as resolved just now.
    assert!(entry.fetched.elapsed() < Duration::from_secs(60));
    assert_wire_shape(&back, &expected);

    let empty: ActivityData = serde_json::from_str("{}").unwrap();
    assert!(empty.events.is_empty() && empty.task_cache.is_empty());
    assert_eq!((empty.status, empty.has_older, empty.window), (None, false, None));
    // An event missing its fields still decodes.
    let event: Event = serde_json::from_str(r#"{"workspace":{}}"#).unwrap();
    assert!(matches!(event, Event::Workspace { new: WorkspaceState::AwaitingInput, .. }));
}

fn changelog_entry() -> (ChangelogEntryView, Value) {
    let pr = ChangelogLink::from(Link::Pr {
        number: Some(2076),
        url: Some("https://github.com/32pixels/shelbi/pull/2076".into()),
    });
    let issue = ChangelogLink::from(Link::Issue { id: "t-1".into() });
    let entry = ChangelogEntryView {
        id: "retro-racing-s".into(),
        title: "Retro Racing S".into(),
        date: "2026-11-01T09:30:00-07:00".parse().unwrap(),
        author: "orchestrator".into(),
        description: "A fourth app icon.".into(),
        links: vec![pr.clone(), issue],
        extra_links: vec![pr],
    };
    let pr_json = json!({ "kind": "pr", "number": 2076, "url": "https://github.com/32pixels/shelbi/pull/2076" });
    let json = json!({
        "id": "retro-racing-s",
        "title": "Retro Racing S",
        "date": "2026-11-01T09:30:00-07:00",
        "author": "orchestrator",
        "description": "A fourth app icon.",
        "links": [pr_json, { "kind": "issue", "id": "t-1" }],
        "extra_links": [pr_json]
    });
    (entry, json)
}

#[test]
fn changelog_data_round_trips() {
    let (entry, entry_json) = changelog_entry();
    let data = ChangelogData {
        project: "shelbi".into(),
        entries: vec![entry],
        skipped: 1,
        error: Some("unreadable entry".into()),
        stamp: ChangelogStamp {
            entries: 4,
            modified: Some(UNIX_EPOCH + Duration::new(1_793_000_000, 5)),
        },
    };
    assert_round_trip(
        &data,
        json!({
            "project": "shelbi",
            "entries": [entry_json],
            "skipped": 1,
            "error": "unreadable entry",
            "stamp": {
                "entries": 4,
                "modified": { "secs_since_epoch": 1_793_000_000u64, "nanos_since_epoch": 5 }
            }
        }),
    );
    // The links' labels and targets are rebuilt on decode, not sent.
    let back: ChangelogData = serde_json::to_string(&data)
        .and_then(|s| serde_json::from_str(&s))
        .unwrap();
    assert_eq!(back.entries[0].links[0].label, "#2076");

    let empty: ChangelogData = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, ChangelogData::default());
}

#[test]
fn project_summary_round_trips() {
    let summary = ProjectSummary {
        project: "shelbi".into(),
        label: "Shelbi".into(),
        settings_dir: PathBuf::from("/home/jlong/shelbi/.shelbi"),
        in_repo: true,
        workflows: Ok((3, 7)),
        agents: Err("agents/ unreadable".into()),
        machines: 2,
        edit_targets: vec![
            EditItem {
                target: EditTarget::Project,
                title: "Project settings".into(),
            },
            EditItem {
                target: EditTarget::Agent("developer".into()),
                title: "Developer instructions".into(),
            },
        ],
    };
    assert_round_trip(
        &summary,
        json!({
            "project": "shelbi",
            "label": "Shelbi",
            "settings_dir": "/home/jlong/shelbi/.shelbi",
            "in_repo": true,
            "workflows": { "Ok": [3, 7] },
            "agents": { "Err": "agents/ unreadable" },
            "machines": 2,
            "edit_targets": [
                { "target": "project", "title": "Project settings" },
                { "target": { "agent": "developer" }, "title": "Developer instructions" }
            ]
        }),
    );

    let empty: ProjectSummary = serde_json::from_str("{}").unwrap();
    assert_eq!((empty.workflows, empty.agents), (Ok((0, 0)), Ok(0)));
    let item: EditItem = serde_json::from_str("{}").unwrap();
    assert_eq!(item.target, EditTarget::Project);
}

#[test]
fn search_corpus_round_trips() {
    let (entry, entry_json) = changelog_entry();
    let corpus = SearchCorpus::new(
        vec![IssueDoc::new(
            "1931",
            "Remote window",
            "Native UI OVER SSH",
            "In Progress",
            StatusCategory::Active,
        )],
        vec![entry],
    );
    assert_round_trip(
        &corpus,
        json!({
            "issues": [{
                "id": "1931",
                "title": "Remote window",
                "body": "Native UI OVER SSH",
                "status": "In Progress",
                "category": "active"
            }],
            "changelog": [entry_json]
        }),
    );
    // The lowercased body is rebuilt on decode (the round trip's equality
    // covers it), so a decoded corpus searches like a local one.
    let issue: IssueDoc = serde_json::from_str(r#"{"body":"ABC"}"#).unwrap();
    assert_eq!(issue, IssueDoc::new("", "", "ABC", "", StatusCategory::Backlog));

    let empty: SearchCorpus = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, SearchCorpus::default());
}

/// A sanity check on the helpers above: a changed field fails the shape pin.
#[test]
#[should_panic(expected = "wire shape changed")]
fn a_changed_shape_fails_the_pin() {
    let model = SidebarModel::default();
    let mut expected = serde_json::to_value(&model).unwrap();
    expected["renamed_field"] = json!(true);
    assert_round_trip(&model, expected);
}

#[test]
fn now_is_not_on_the_wire() {
    // `SystemTime` (an mtime, a stamp) is wall-clock and travels; `Instant`
    // (a cache entry's `fetched`) is monotonic and doesn't.
    let entry = TaskCacheEntry {
        meta: None,
        mtime: Some(SystemTime::UNIX_EPOCH),
        fetched: Instant::now(),
        terminal: false,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert!(json.get("fetched").is_none());
}
