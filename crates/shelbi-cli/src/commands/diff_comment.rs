//! `shelbi diff comment list|reply`: an agent's side of the diff view's review
//! threads, in the shape of Skim's `skim session comment`.
//!
//! The threads live in the project's state directory on the hub, and a worker
//! may be on another machine, so a pane with a hub address goes through the hub
//! daemon (the `diff-comment` verb). Anywhere else (the user's own shell on the
//! hub, no daemon running) it's the store directly: it takes the same lock.
//!
//! A reply is signed `$SHELBI_AGENT` unless `-a` says otherwise, and shows in
//! both clients with no refresh: an open view looks at the store every couple of
//! seconds.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{anyhow, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use shelbi_app::diff::comments::{format_comment, Author, CommentDoc};
use shelbi_app::diff::store::DiffStore;

use super::require_project;

/// How long a CLI call waits for the hub's answer.
const HUB_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Subcommand)]
pub enum DiffCmd {
    /// Review threads on a task's diff.
    Comment {
        #[command(subcommand)]
        cmd: CommentCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum CommentCmd {
    /// List the threads, each with the number `reply -i` takes.
    List {
        /// The task (or `ws-<workspace>` for a workspace with no task).
        #[arg(long)]
        task: String,
        /// The threads as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Answer a thread, under the comment in the diff view.
    Reply {
        /// The task (or `ws-<workspace>` for a workspace with no task).
        #[arg(long)]
        task: String,
        /// The thread: its id, a prefix of it, or its number in `list`.
        #[arg(short = 'i', long = "index", value_name = "THREAD")]
        index: String,
        /// Who it's from. Defaults to `$SHELBI_AGENT`.
        #[arg(short = 'a', long = "author", value_name = "NAME")]
        author: Option<String>,
        /// The reply.
        text: String,
    },
}

pub fn run(project: Option<String>, cmd: DiffCmd) -> Result<()> {
    let project = require_project(project)?;
    let DiffCmd::Comment { cmd } = cmd;
    match cmd {
        CommentCmd::List { task, json } => {
            let doc = list(&project, &task)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&doc)?);
            } else {
                print!("{}", list_text(&doc));
            }
        }
        CommentCmd::Reply { task, index, author, text } => {
            let author = author
                .or_else(|| std::env::var("SHELBI_AGENT").ok())
                .filter(|a| !a.trim().is_empty())
                .unwrap_or_else(|| "agent".to_string());
            reply(&project, &task, &index, &author, &text)?;
            println!("✓ replied as {author}");
        }
    }
    Ok(())
}

/// The threads, numbered from 1.
fn list_text(doc: &CommentDoc) -> String {
    if doc.comments.is_empty() {
        return "No comments on this task.\n".to_string();
    }
    let parts: Vec<String> = doc
        .comments
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. [thread {}] {}", i + 1, c.id, format_comment(c)))
        .collect();
    parts.join("\n")
}

fn list(project: &str, task: &str) -> Result<CommentDoc> {
    let asked = json!({"verb": "diff-comment", "project": project, "task_id": task, "op": "list"});
    if let Some(answer) = ask_hub(&asked) {
        let answer = hub_result(answer?)?;
        return serde_json::from_value(answer["doc"].clone()).map_err(|e| anyhow!("the hub's answer doesn't parse: {e}"));
    }
    local_store(project, task)?.comments().map_err(|e| anyhow!(e))
}

fn reply(project: &str, task: &str, thread: &str, author: &str, text: &str) -> Result<()> {
    let asked = json!({
        "verb": "diff-comment",
        "project": project,
        "task_id": task,
        "op": "reply",
        "thread": thread,
        "author": author,
        "body": text,
    });
    if let Some(answer) = ask_hub(&asked) {
        hub_result(answer?)?;
        return Ok(());
    }
    local_store(project, task)?
        .reply(thread, Author::agent(author), text)
        .map(|_| ())
        .map_err(|e| anyhow!(e))
}

/// This machine's store, when this machine has the project. Elsewhere a write
/// would land in a file nobody reads.
fn local_store(project: &str, task: &str) -> Result<DiffStore> {
    let root = shelbi_state::project_dir(project).map_err(|e| anyhow!(e))?;
    if !root.is_dir() {
        return Err(anyhow!("project `{project}` isn't on this machine, and the hub can't be reached"));
    }
    DiffStore::for_project(project, task).map_err(|e| anyhow!(e))
}

/// The hub's answer: its `doc` on success, its reason otherwise.
fn hub_result(answer: Value) -> Result<Value> {
    if answer["ok"].as_bool() == Some(true) {
        return Ok(answer);
    }
    Err(anyhow!("{}", answer["error"].as_str().unwrap_or("the hub refused it")))
}

/// Ask the hub, when this pane knows where it is. `None`: nobody to ask.
fn ask_hub(message: &Value) -> Option<Result<Value>> {
    let addr = std::env::var("SHELBI_HUB_ADDR")
        .ok()
        .filter(|a| !a.is_empty())
        .or_else(|| std::env::var("SHELBI_HUB_SOCK").ok().filter(|s| !s.is_empty()).map(|s| format!("unix:{s}")))?;
    let mut line = message.to_string();
    line.push('\n');
    let answer = if let Some(hostport) = addr.strip_prefix("tcp:") {
        let stream = TcpStream::connect(hostport).ok()?;
        stream.set_read_timeout(Some(HUB_WAIT)).ok()?;
        converse(&stream, line.as_bytes(), |s| s.shutdown(Shutdown::Write))
    } else {
        let stream = UnixStream::connect(addr.strip_prefix("unix:").unwrap_or(&addr)).ok()?;
        stream.set_read_timeout(Some(HUB_WAIT)).ok()?;
        converse(&stream, line.as_bytes(), |s| s.shutdown(Shutdown::Write))
    };
    Some(answer)
}

/// One line out, then the answer line. The daemon may lead with a hello.
fn converse<'a, S>(stream: &'a S, line: &[u8], half_close: impl Fn(&S) -> std::io::Result<()>) -> Result<Value>
where
    &'a S: Read + Write,
{
    let mut writer: &S = stream;
    writer.write_all(line)?;
    half_close(stream)?;
    let mut text = String::new();
    let mut reader: &S = stream;
    reader.read_to_string(&mut text)?;
    parse_answer(&text)
}

fn parse_answer(text: &str) -> Result<Value> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v.get("ok").is_some())
        .ok_or_else(|| anyhow!("the hub didn't answer: it may be older than this shelbi (run `shelbi reload`)"))
}

/// The `diff-comment` verb, on the hub. Always a line of JSON, so the worker
/// hears why a reply was refused: a daemon log nobody reads wouldn't help.
pub fn answer_hub(
    project: Option<&str>,
    task: Option<&str>,
    op: Option<&str>,
    thread: Option<&str>,
    author: Option<&str>,
    body: Option<&str>,
) -> String {
    answer_with(project, task, op, thread, author, body, &|project, task| DiffStore::for_project(project, task))
}

fn answer_with(
    project: Option<&str>,
    task: Option<&str>,
    op: Option<&str>,
    thread: Option<&str>,
    author: Option<&str>,
    body: Option<&str>,
    store: &dyn Fn(&str, &str) -> Result<DiffStore, String>,
) -> String {
    let result = (|| -> Result<Value, String> {
        let project = project.filter(|p| !p.is_empty()).ok_or("diff-comment needs a project")?;
        let task = task.filter(|t| !t.is_empty()).ok_or("diff-comment needs a task")?;
        let store = store(project, task)?;
        match op {
            Some("list") => Ok(json!({"ok": true, "doc": store.comments()?})),
            Some("reply") => {
                let thread = thread.ok_or("a reply needs a thread")?;
                let author = author.filter(|a| !a.is_empty()).unwrap_or("agent");
                store.reply(thread, Author::agent(author), body.unwrap_or(""))?;
                Ok(json!({"ok": true}))
            }
            other => Err(format!("unknown diff-comment op {other:?}")),
        }
    })();
    result.unwrap_or_else(|error| json!({"ok": false, "error": error})).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use shelbi_app::diff::comments::{Anchor, AuthorKind, Comment, CommentOp};
    use shelbi_app::diff::model::Side;

    fn store_with_a_thread(dir: &std::path::Path) -> DiffStore {
        let store = DiffStore::at(dir);
        store
            .apply_comment(&CommentOp::Add(Comment {
                id: "abc-1".into(),
                anchor: Anchor { path: "a.rs".into(), side: Side::New, start: 3, end: 3, code: vec!["x".into()] },
                author: Author::human("jlong"),
                body: "why?".into(),
                created_at: DateTime::from_timestamp(0, 0).unwrap(),
                replies: Vec::new(),
                sent: true,
            }))
            .unwrap();
        store
    }

    fn call(dir: &std::path::Path, op: &str, thread: Option<&str>, body: Option<&str>) -> Value {
        let dir = dir.to_path_buf();
        let store = move |_: &str, _: &str| Ok(DiffStore::at(&dir));
        let text = answer_with(Some("p"), Some("t"), Some(op), thread, Some("developer"), body, &store);
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn a_reply_through_the_hub_lands_with_its_author_and_leaves_the_thread_sent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_a_thread(dir.path());
        assert_eq!(call(dir.path(), "reply", Some("abc"), Some("it drops the slot on close"))["ok"], true);
        let doc = store.comments().unwrap();
        let reply = &doc.comments[0].replies[0];
        assert_eq!((reply.author.name.as_str(), reply.author.kind), ("developer", AuthorKind::Agent));
        assert_eq!(reply.body, "it drops the slot on close");
        assert!(doc.comments[0].sent, "an agent's answer doesn't go back to it");
    }

    #[test]
    fn the_hub_says_why_it_refused() {
        let dir = tempfile::tempdir().unwrap();
        store_with_a_thread(dir.path());
        let answer = call(dir.path(), "reply", Some("zz"), Some("hi"));
        assert_eq!(answer["ok"], false);
        assert!(answer["error"].as_str().unwrap().contains("no thread"), "{answer}");
        assert!(hub_result(answer).unwrap_err().to_string().contains("no thread"));
        let unknown = call(dir.path(), "shrug", None, None);
        assert!(unknown["error"].as_str().unwrap().contains("unknown diff-comment op"));
    }

    #[test]
    fn a_list_comes_back_numbered() {
        let dir = tempfile::tempdir().unwrap();
        store_with_a_thread(dir.path());
        let answer = hub_result(call(dir.path(), "list", None, None)).unwrap();
        let doc: CommentDoc = serde_json::from_value(answer["doc"].clone()).unwrap();
        assert_eq!(doc.comments[0].id, "abc-1");
        let text = list_text(&doc);
        assert!(text.starts_with("1. [thread abc-1] a.rs:3\n"), "{text}");
        assert_eq!(list_text(&CommentDoc::default()), "No comments on this task.\n");
    }

    #[test]
    fn the_answer_is_found_past_a_hello_line() {
        let answer = parse_answer("{\"daemon\":\"hello\"}\n{\"ok\":true}\n").unwrap();
        assert_eq!(answer["ok"], true);
        assert!(parse_answer("ok\n").unwrap_err().to_string().contains("didn't answer"));
    }
}
