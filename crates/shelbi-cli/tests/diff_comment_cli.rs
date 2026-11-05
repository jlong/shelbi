//! `shelbi diff comment`, as an agent runs it: straight to the store on the hub,
//! and through the hub's socket from a pane that has its address.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};

use shelbi_app::diff::comments::{Anchor, Author, AuthorKind, Comment, CommentOp};
use shelbi_app::diff::model::Side;
use shelbi_app::diff::store::DiffStore;

fn shelbi(home: &std::path::Path, hub: Option<&std::path::Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_shelbi"));
    cmd.args(args)
        .env("SHELBI_HOME", home)
        .env("SHELBI_AGENT", "developer")
        .env_remove("SHELBI_HUB_ADDR")
        .env_remove("SHELBI_HUB_SOCK")
        .env_remove("SHELBI_PROJECT");
    if let Some(hub) = hub {
        cmd.env("SHELBI_HUB_SOCK", hub);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A thread on task `t` of project `demo`, in `home`'s state directory.
fn seed(home: &std::path::Path) -> DiffStore {
    let store = DiffStore::at(home.join("projects/demo/diff/t"));
    store
        .apply_comment(&CommentOp::Add(Comment {
            id: "abc-1".into(),
            anchor: Anchor { path: "a.rs".into(), side: Side::New, start: 3, end: 3, code: vec!["let x;".into()] },
            author: Author::human("jlong"),
            body: "why?".into(),
            created_at: chrono::Utc::now(),
            replies: Vec::new(),
            sent: true,
        }))
        .unwrap();
    store
}

#[test]
fn on_the_hub_a_reply_goes_straight_to_the_store() {
    let home = tempfile::tempdir().unwrap();
    let store = seed(home.path());

    let listed = stdout(&shelbi(home.path(), None, &["-p", "demo", "diff", "comment", "list", "--task", "t"]));
    assert!(listed.starts_with("1. [thread abc-1] a.rs:3\n"), "{listed}");

    let replied = shelbi(home.path(), None, &["-p", "demo", "diff", "comment", "reply", "--task", "t", "-i", "1", "it drops the slot"]);
    assert!(stdout(&replied).contains("replied as developer"));
    let doc = store.comments().unwrap();
    let reply = &doc.comments[0].replies[0];
    assert_eq!((reply.author.name.as_str(), reply.author.kind), ("developer", AuthorKind::Agent));
    assert_eq!(reply.body, "it drops the slot");
    assert!(doc.comments[0].sent, "it isn't sent back to the agent");

    let named = shelbi(home.path(), None, &["-p", "demo", "diff", "comment", "reply", "--task", "t", "-i", "abc", "-a", "claude", "more"]);
    assert!(stdout(&named).contains("replied as claude"));

    let json = stdout(&shelbi(home.path(), None, &["-p", "demo", "diff", "comment", "list", "--task", "t", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["comments"][0]["replies"].as_array().unwrap().len(), 2);
}

#[test]
fn a_bad_thread_says_so_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let store = seed(home.path());
    let out = shelbi(home.path(), None, &["-p", "demo", "diff", "comment", "reply", "--task", "t", "-i", "zz", "hi"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no thread"), "{out:?}");
    assert!(store.comments().unwrap().comments[0].replies.is_empty());
}

#[test]
fn a_machine_without_the_project_and_with_no_hub_refuses_rather_than_writing_nowhere() {
    let home = tempfile::tempdir().unwrap();
    let out = shelbi(home.path(), None, &["-p", "demo", "diff", "comment", "reply", "--task", "t", "-i", "1", "hi"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't on this machine"), "{out:?}");
    assert!(!home.path().join("projects/demo/diff").exists());
}

#[test]
fn from_a_pane_with_a_hub_address_it_asks_the_hub_and_reports_its_refusal() {
    let home = tempfile::tempdir().unwrap();
    let sock = home.path().join("hub.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let hub = std::thread::spawn(move || {
        let mut heard = Vec::new();
        for answer in ["{\"ok\":true}", "{\"ok\":false,\"error\":\"no thread `zz` on this task\"}"] {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            heard.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
            // A hello first, as the daemon may.
            (&stream).write_all(format!("{{\"daemon\":\"hello\"}}\n{answer}\n").as_bytes()).unwrap();
        }
        heard
    });
    let ok = shelbi(home.path(), Some(&sock), &["-p", "demo", "diff", "comment", "reply", "--task", "t", "-i", "abc", "fixed"]);
    assert!(stdout(&ok).contains("replied as developer"));
    let refused = shelbi(home.path(), Some(&sock), &["-p", "demo", "diff", "comment", "reply", "--task", "t", "-i", "zz", "fixed"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no thread `zz`"), "{refused:?}");
    let heard = hub.join().unwrap();
    assert_eq!(heard[0]["verb"], "diff-comment");
    assert_eq!(heard[0]["project"], "demo");
    assert_eq!(heard[0]["task_id"], "t");
    assert_eq!(heard[0]["op"], "reply");
    assert_eq!(heard[0]["thread"], "abc");
    assert_eq!(heard[0]["author"], "developer");
    assert_eq!(heard[0]["body"], "fixed");
}
