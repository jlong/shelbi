//! A remote project source answers exactly what the local one does.
//!
//! One fixture project in a temp Shelbi home is read twice: by this
//! machine's `LocalSource`, and by a `RemoteSource` whose every query crosses
//! an in-process relay (`serve_relay_with` over a socket pair) to the far
//! side's handler, `QueryAnswers`, which reads through its own `LocalSource`
//! on the same disk. Every `ProjectSource` query's answer must be equal.
//! No SSH, no daemon, nothing of the real `~/.shelbi` or the real hub.

#![cfg(unix)]

use std::fmt::Debug;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use shelbi_app::settings::agents::fixture::Home;
use shelbi_app::source::remote::{ChannelLink, RemoteSource};
use shelbi_app::source::{ActivityWindow, ProjectKey, ProjectSource};
use shelbi_client::{RelayChannel, RelayServices};
use shelbi_orchestrator::source::{LocalSource, QueryAnswers};

const PROJECT: &str = "far-project";

/// This test binary's lock over `SHELBI_HOME` (one test, but the fixture
/// asks for a guard).
static ENV: Mutex<()> = Mutex::new(());

/// The project as both sources read it: the shared agents/workflows fixture
/// plus an error, a changelog entry, a keys.yaml with a section for the
/// project, and a few event-log lines.
fn fixture() -> Home {
    // The real hub's sockets must stay out of it: a read that probes the
    // daemon goes to the fixture home's own (absent) sockets instead.
    for var in ["SHELBI_HUB_SOCK", "SHELBI_CONTROL_SOCK", "SHELBI_HUB_ADDR"] {
        std::env::remove_var(var);
    }
    let lock = ENV.lock().unwrap_or_else(|p| p.into_inner());
    let home = Home::new("remote-source", PROJECT, lock);
    shelbi_state::append_error(PROJECT, "the board didn't refresh", Some("poller")).unwrap();
    shelbi_state::changelog::add_entry(
        PROJECT,
        shelbi_state::changelog::NewEntry {
            title: "Remote windows read over the relay".into(),
            date: shelbi_state::changelog::now(),
            author: "orchestrator".into(),
            issues: vec!["t-1".into()],
            prs: Vec::new(),
            urls: Vec::new(),
            description: "Shipped.".into(),
        },
    )
    .unwrap();
    std::fs::write(
        home.root.join("keys.yaml"),
        format!("defaults:\n  global:\n    zen_toggle: alt-z\nprojects:\n  {PROJECT}:\n    kanban:\n      nav_up: [alt-k]\n"),
    )
    .unwrap();
    std::fs::write(
        home.root.join("events.log"),
        format!(
            "2026-11-02T10:00:00Z project={PROJECT} workspace=alpha task=t-1 -> working\n\
             2026-11-02T10:05:00Z project={PROJECT} workspace=alpha task=t-1 -> awaiting_input\n\
             2026-11-02T10:06:00Z project=other workspace=beta note=elsewhere\n"
        ),
    )
    .unwrap();
    home
}

/// Equal as the wire sees them (most answers have no `PartialEq`).
#[track_caller]
fn same<T: Serialize>(what: &str, local: T, remote: T) {
    let local = serde_json::to_value(&local).unwrap();
    let remote = serde_json::to_value(&remote).unwrap();
    assert_eq!(local, remote, "`{what}` answered differently over the relay");
}

/// Equal as `Debug` prints them, for the config models (no serde, no
/// `PartialEq`), whose `Debug` shows every field including the files they
/// were built from.
#[track_caller]
fn same_debug<T: Debug>(what: &str, local: T, remote: T) {
    assert_eq!(format!("{local:#?}"), format!("{remote:#?}"), "`{what}` built differently");
}

#[test]
fn every_query_answers_over_the_relay_what_the_local_source_does() {
    let _home = fixture();
    let local = LocalSource::new(PROJECT);
    let remote = remote_for(PROJECT);
    assert_eq!(remote.key(), &ProjectKey::remote("devbox", PROJECT));

    // The fixture reads as a real project, so the comparisons below compare
    // something.
    assert!(local.project_exists());
    assert!(local.sidebar().is_some());
    assert!(local.workflow_config().is_ok());
    assert!(local.agents_config().is_ok());
    assert!(!local.errors().unwrap().is_empty());
    assert!(local.changelog(None).is_some_and(|c| !c.entries.is_empty()));

    // --- sidebar and project
    same("sidebar", local.sidebar(), remote.sidebar());
    same("config_skeleton", local.config_skeleton(), remote.config_skeleton());
    same("project_exists", local.project_exists(), remote.project_exists());
    same("project_summary", local.project_summary(), remote.project_summary());
    same("edit_targets", local.edit_targets(), remote.edit_targets());

    // --- board
    same("open_board", local.open_board(), remote.open_board());
    same("closed_page", local.closed_page(), remote.closed_page());
    same("more_closed", local.more_closed("cursor-1"), remote.more_closed("cursor-1"));
    same("search", local.search("remote", 1), remote.search("remote", 1));
    same("issue_state", local.issue_state("t-1"), remote.issue_state("t-1"));

    // --- activity and history (the remote source keeps its own title cache
    // far side, like the local one does here: both start cold)
    let staged = |source: &dyn ProjectSource| {
        let mut early: Vec<ActivityWindow> = Vec::new();
        let full = source.activity_window(50, &mut |read| early.push(read));
        (early, full)
    };
    assert!(!staged(&local).1.data.events.is_empty(), "the fixture's events read");
    same("activity_window", staged(&local), staged(&remote));
    same("activity_window (warm)", staged(&local), staged(&remote));
    same("changelog", local.changelog(None), remote.changelog(None));
    let stamp = local.changelog(None).unwrap().stamp;
    same("changelog (unchanged)", local.changelog(Some(&stamp)), remote.changelog(Some(&stamp)));
    same("errors", local.errors(), remote.errors());
    same("zen_state", local.zen_state(), remote.zen_state());

    // --- workspaces and machines
    same("workspace_details", local.workspace_details("alpha"), remote.workspace_details("alpha"));
    same("workspace_details (unknown)", local.workspace_details("nope"), remote.workspace_details("nope"));
    same("review_open_target", local.review_open_target("t-1"), remote.review_open_target("t-1"));
    same("review_slots", local.review_slots(), remote.review_slots());
    same("machines", local.machines(), remote.machines());
    {
        // The probe runs `ssh` from the project's machine. With no `ssh` on
        // the PATH it fails at once, the same way on both sides, and nothing
        // reaches a network or the real hub.
        let path = std::env::var_os("PATH");
        std::env::set_var("PATH", "/nonexistent");
        let answers = (local.machine_reachability("devbox-2"), remote.machine_reachability("devbox-2"));
        match path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        same("machine_reachability", answers.0, answers.1);
    }

    // --- config
    same_debug("workflow_config", local.workflow_config(), remote.workflow_config());
    same("project_git", local.project_git(), remote.project_git());
    same_debug("agents_config", local.agents_config(), remote.agents_config());
    same("workspaces_config", local.workspaces_config(), remote.workspaces_config());
    let (local_keys, remote_keys) = (local.project_keymap(), remote.project_keymap());
    assert_eq!(local_keys, remote_keys, "`project_keymap` merged differently");
    // The project's section applied (it differs from the keys without it),
    // and cleanly.
    assert!(local_keys.1.is_empty(), "{:?}", local_keys.1);
    assert_ne!(remote_keys.0, shelbi_state::keymap::load_keymaps(None).0);

    // A project the far machine doesn't have reads as nothing on both sides.
    let missing = LocalSource::new("not-registered");
    let channel_less = remote_for("not-registered");
    same("sidebar (missing)", missing.sidebar(), channel_less.sidebar());
    same("project_exists (missing)", missing.project_exists(), channel_less.project_exists());
    same("machines (missing)", missing.machines(), channel_less.machines());
}

/// A remote source for `project` over an in-process relay answering with
/// the far side's real handler.
fn remote_for(project: &str) -> RemoteSource {
    let (near, far) = UnixStream::pair().unwrap();
    let far_read = far.try_clone().unwrap();
    let services = RelayServices {
        query: Some(QueryAnswers::default().handler()),
        ..RelayServices::default()
    };
    let root = std::env::temp_dir().join(format!("rs-{}-no-sessions", std::process::id()));
    std::thread::spawn(move || {
        let _ = shelbi_client::serve_relay_with(Box::new(far_read), Box::new(far), &root, services);
    });
    let near_read = near.try_clone().unwrap();
    let channel = RelayChannel::new(Box::new(near_read), Box::new(near)).unwrap();
    RemoteSource::new(project, Arc::new(ChannelLink::new("devbox", channel)))
}
