//! Review comments: a task's threads, where each one sits in a changeset, and
//! how they read when they leave the view.
//!
//! A thread anchors to a line or a range of lines on one side of a file, and
//! keeps a copy of the code it was written on. A refresh moves lines around,
//! so a thread is placed by that copy: at its line if the text still matches,
//! else at the nearest line with that text, else it's outdated (kept, shown at
//! the top of its file, and still sent, but no longer on a line). The stored
//! anchor isn't rewritten when a thread moves: each reading finds it again.
//!
//! Every change is a [`CommentOp`] carrying its own ids, so applying the same
//! ops to the on-disk document ([`super::store`]) and to the client's copy
//! lands the same thread, and two clients' ops merge.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::model::{Changeset, FileDiff, Side};

/// Whose words these are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorKind {
    #[default]
    Human,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    #[serde(default)]
    pub kind: AuthorKind,
}

impl Author {
    pub fn human(name: impl Into<String>) -> Author {
        Author { name: name.into(), kind: AuthorKind::Human }
    }

    pub fn agent(name: impl Into<String>) -> Author {
        Author { name: name.into(), kind: AuthorKind::Agent }
    }

    /// Whoever's at this machine: what a client's new comments are signed.
    pub fn local() -> Author {
        let name = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
        Author::human(if name.is_empty() { "you".to_string() } else { name })
    }
}

/// The lines a thread is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub side: Side,
    /// First and last line, on `side`, inclusive.
    pub start: u32,
    pub end: u32,
    /// The text of `start..=end` when the comment was written.
    pub code: Vec<String>,
}

impl Anchor {
    /// `src/x.rs:12` or `src/x.rs:12-15`, for a message.
    pub fn location(&self) -> String {
        if self.start == self.end {
            format!("{}:{}", self.path, self.end)
        } else {
            format!("{}:{}-{}", self.path, self.start, self.end)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentReply {
    pub id: String,
    pub author: Author,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// One thread: a comment and its replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub anchor: Anchor,
    pub author: Author,
    pub body: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<CommentReply>,
    /// Already sent to the agent: `gY` sends the rest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sent: bool,
}

/// A task's comments, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommentDoc {
    #[serde(default)]
    pub comments: Vec<Comment>,
}

/// One change to a [`CommentDoc`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentOp {
    Add(Comment),
    /// New text for a comment, or one of its replies.
    Edit { id: String, reply: Option<String>, body: String },
    Reply { id: String, reply: CommentReply },
    /// Remove a thread, or one reply.
    Delete { id: String, reply: Option<String> },
    Clear,
    MarkSent { ids: Vec<String> },
}

impl CommentDoc {
    pub fn get(&self, id: &str) -> Option<&Comment> {
        self.comments.iter().find(|c| c.id == id)
    }

    /// Apply `op`. An op on a thread that's gone (another client deleted it)
    /// does nothing; adding an id that's already here does nothing, so
    /// applying an op twice is harmless.
    pub fn apply(&mut self, op: &CommentOp) {
        match op {
            CommentOp::Add(comment) => {
                if self.get(&comment.id).is_none() {
                    self.comments.push(comment.clone());
                }
            }
            CommentOp::Edit { id, reply, body } => {
                let Some(comment) = self.comments.iter_mut().find(|c| c.id == *id) else { return };
                match reply {
                    Some(reply) => {
                        if let Some(r) = comment.replies.iter_mut().find(|r| r.id == *reply) {
                            r.body = body.clone();
                        }
                    }
                    None => comment.body = body.clone(),
                }
            }
            CommentOp::Reply { id, reply } => {
                let Some(comment) = self.comments.iter_mut().find(|c| c.id == *id) else { return };
                if !comment.replies.iter().any(|r| r.id == reply.id) {
                    comment.replies.push(reply.clone());
                    // A person's new reply hasn't been sent, whatever the thread
                    // had. An agent's is the agent's own answer: telling it back
                    // would only start a loop.
                    if reply.author.kind == AuthorKind::Human {
                        comment.sent = false;
                    }
                }
            }
            CommentOp::Delete { id, reply: None } => self.comments.retain(|c| c.id != *id),
            CommentOp::Delete { id, reply: Some(reply) } => {
                if let Some(comment) = self.comments.iter_mut().find(|c| c.id == *id) {
                    comment.replies.retain(|r| r.id != *reply);
                }
            }
            CommentOp::Clear => self.comments.clear(),
            CommentOp::MarkSent { ids } => {
                for comment in self.comments.iter_mut().filter(|c| ids.contains(&c.id)) {
                    comment.sent = true;
                }
            }
        }
    }
}

/// A new id: unique across the clients on a task without asking anyone.
pub fn fresh_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{n:x}", Utc::now().timestamp_millis(), std::process::id())
}

/// The lines of one side of a file: text and where it is.
pub(super) struct SideIndex<'a> {
    /// `(line number, hunk, line in hunk, text)`, ascending by line number.
    lines: Vec<(u32, u32, u32, &'a str)>,
}

impl<'a> SideIndex<'a> {
    pub(super) fn new(file: &'a FileDiff, side: Side) -> Self {
        let mut lines = Vec::new();
        for (h, hunk) in file.hunks.iter().enumerate() {
            for (l, line) in hunk.lines.iter().enumerate() {
                if let Some(number) = line.number(side) {
                    lines.push((number, h as u32, l as u32, line.text.as_str()));
                }
            }
        }
        SideIndex { lines }
    }

    /// Where the whole of `code` sits, with its last line at or nearest `end`.
    pub(super) fn find(&self, code: &[String], end: u32) -> Option<(u32, u32, u32, u32)> {
        let n = code.len().max(1);
        let mut best: Option<(u32, (u32, u32, u32, u32))> = None;
        for window in self.lines.windows(n) {
            let contiguous = window.windows(2).all(|w| w[1].0 == w[0].0 + 1);
            let same = code.is_empty() || window.iter().zip(code).all(|(a, b)| a.3 == b);
            if !contiguous || !same {
                continue;
            }
            let last = window[n - 1];
            let distance = last.0.abs_diff(end);
            if best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, (last.1, last.2, window[0].0, last.0)));
            }
        }
        best.map(|(_, at)| at)
    }
}

/// Where a thread sits in a changeset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub file: u32,
    /// The hunk and line it follows, when its lines are still there. `None`:
    /// outdated, shown at the top of its file.
    pub at: Option<(u32, u32)>,
    /// The range now, on the anchor's side (where `at` found it).
    pub lines: Option<(u32, u32)>,
}

/// Every thread's place. A thread whose file isn't in the changeset has none
/// (another commit's comment while reviewing one commit).
pub fn place(doc: &CommentDoc, changeset: &Changeset) -> HashMap<String, Placement> {
    let mut out = HashMap::with_capacity(doc.comments.len());
    for comment in &doc.comments {
        let Some(file) = changeset.file_index(&comment.anchor.path) else { continue };
        let index = SideIndex::new(&changeset.files[file], comment.anchor.side);
        let found = index.find(&comment.anchor.code, comment.anchor.end);
        out.insert(
            comment.id.clone(),
            Placement {
                file: file as u32,
                at: found.map(|(h, l, _, _)| (h, l)),
                lines: found.map(|(_, _, s, e)| (s, e)),
            },
        );
    }
    out
}

/// The anchor for `start..=end` on `side` of `file`, with its code copied.
/// `None` when a line in the range isn't in the diff.
pub fn anchor_for(file: &FileDiff, side: Side, start: u32, end: u32) -> Option<Anchor> {
    let index = SideIndex::new(file, side);
    let mut code = Vec::new();
    for number in start..=end {
        let line = index.lines.iter().find(|l| l.0 == number)?;
        code.push(line.3.to_string());
    }
    Some(Anchor { path: file.path.clone(), side, start, end, code })
}

/// One thread as text: where it is, the code quoted, the comment, the replies.
pub fn format_comment(comment: &Comment) -> String {
    let mut out = comment.anchor.location();
    if comment.anchor.side == Side::Old {
        out.push_str(" (old)");
    }
    out.push('\n');
    if !comment.anchor.code.is_empty() {
        out.push_str("```\n");
        for line in &comment.anchor.code {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("```\n");
    }
    out.push_str(&format!("{}: {}\n", comment.author.name, comment.body));
    for reply in &comment.replies {
        out.push_str(&format!("  ↳ {}: {}\n", reply.author.name, reply.body));
    }
    out
}

/// Several threads as one text.
pub fn format_comments<'a>(comments: impl IntoIterator<Item = &'a Comment>) -> String {
    let parts: Vec<String> = comments.into_iter().map(format_comment).collect();
    parts.join("\n")
}

/// One thread as an agent reads it: [`format_comment`] with the id a reply
/// names.
fn format_for_agent(comment: &Comment) -> String {
    format!("[thread {}] {}", comment.id, format_comment(comment))
}

/// The threads `gY` hasn't sent yet, with what a message about them says: the
/// quoted code, and how to answer each one inline. `project` and `key` (the
/// task's state key, [`super::backend::DiffTarget::state_key`]) make the reply
/// command work from any directory, on any machine.
pub fn agent_message(doc: &CommentDoc, project: &str, key: &str) -> Option<(String, Vec<String>)> {
    let unsent: Vec<&Comment> = doc.comments.iter().filter(|c| !c.sent).collect();
    if unsent.is_empty() {
        return None;
    }
    let ids = unsent.iter().map(|c| c.id.clone()).collect();
    let threads: Vec<String> = unsent.into_iter().map(format_for_agent).collect();
    let message = format!(
        "Review comments on this branch:\n\n{}\nTo answer a thread where the reviewer reads it, run: \
         shelbi --project {project} diff comment reply --task {key} -i <thread> \"your reply\"",
        threads.join("\n")
    );
    Some((message, ids))
}

/// Whether a reject's reason carries any threads at all, before anything reads
/// the store to find out which.
pub fn reason_carries_threads(reason: &str) -> bool {
    reason.contains("[thread ")
}

/// The threads `reason` carries: what's still marked unsent once a reject with
/// it lands, leaving any the reviewer cut from the reason out.
pub fn threads_in(reason: &str, doc: &CommentDoc) -> Vec<String> {
    doc.comments
        .iter()
        .filter(|c| !c.sent && reason.contains(&format_for_agent(c)))
        .map(|c| c.id.clone())
        .collect()
}

/// A reply from `author` to the thread `selector` names.
pub fn reply_to(doc: &CommentDoc, selector: &str, author: Author, body: &str) -> Result<CommentOp, String> {
    if body.trim().is_empty() {
        return Err("a reply needs some text".to_string());
    }
    let thread = find_thread(doc, selector)?;
    Ok(CommentOp::Reply {
        id: thread.id.clone(),
        reply: CommentReply { id: fresh_id(), author, body: body.trim_end().to_string(), created_at: Utc::now() },
    })
}

/// The thread `selector` names: its id, a prefix of it only one thread has, or
/// its place in the list from 1 (`shelbi diff comment list`).
pub fn find_thread<'a>(doc: &'a CommentDoc, selector: &str) -> Result<&'a Comment, String> {
    if let Some(comment) = doc.get(selector) {
        return Ok(comment);
    }
    if let Ok(at) = selector.parse::<usize>() {
        return at
            .checked_sub(1)
            .and_then(|i| doc.comments.get(i))
            .ok_or_else(|| format!("no thread {at}: this task has {}", doc.comments.len()));
    }
    let mut matching = doc.comments.iter().filter(|c| c.id.starts_with(selector));
    match (matching.next(), matching.next()) {
        (Some(comment), None) => Ok(comment),
        (Some(_), Some(_)) => Err(format!("`{selector}` matches more than one thread")),
        (None, _) => Err(format!("no thread `{selector}` on this task")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_patch;

    fn changeset(patch: &str) -> Changeset {
        Changeset { files: parse_patch(patch), ..Changeset::default() }
    }

    fn comment(id: &str, path: &str, side: Side, start: u32, end: u32, code: &[&str]) -> Comment {
        Comment {
            id: id.to_string(),
            anchor: Anchor {
                path: path.to_string(),
                side,
                start,
                end,
                code: code.iter().map(|c| (*c).to_string()).collect(),
            },
            author: Author::human("jlong"),
            body: "why?".into(),
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
            replies: Vec::new(),
            sent: false,
        }
    }

    const PATCH: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10,3 +10,4 @@\n one\n-two\n+TWO\n+three\n four\n";

    #[test]
    fn a_thread_sits_at_its_line() {
        let cs = changeset(PATCH);
        let doc = CommentDoc { comments: vec![comment("c", "a.rs", Side::New, 11, 11, &["TWO"])] };
        let p = place(&doc, &cs)["c"];
        assert_eq!((p.file, p.at, p.lines), (0, Some((0, 2)), Some((11, 11))));
    }

    #[test]
    fn a_thread_follows_its_text_when_lines_move() {
        // Everything moved down 90 lines.
        let moved = changeset("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -100,3 +100,4 @@\n one\n-two\n+TWO\n+three\n four\n");
        let doc = CommentDoc { comments: vec![comment("c", "a.rs", Side::New, 11, 11, &["TWO"])] };
        let p = place(&doc, &moved)["c"];
        assert_eq!(p.lines, Some((101, 101)));
    }

    #[test]
    fn a_range_needs_all_its_lines_in_a_row() {
        let cs = changeset(PATCH);
        let found = CommentDoc { comments: vec![comment("c", "a.rs", Side::New, 11, 12, &["TWO", "three"])] };
        assert_eq!(place(&found, &cs)["c"].lines, Some((11, 12)));
        let gone = CommentDoc { comments: vec![comment("c", "a.rs", Side::New, 11, 12, &["TWO", "gone"])] };
        assert_eq!(place(&gone, &cs)["c"].at, None);
    }

    #[test]
    fn when_the_line_is_gone_the_thread_is_outdated_not_lost() {
        let cs = changeset(PATCH);
        let doc = CommentDoc { comments: vec![comment("c", "a.rs", Side::New, 11, 11, &["rewritten"])] };
        let p = place(&doc, &cs)["c"];
        assert_eq!((p.file, p.at), (0, None));
    }

    #[test]
    fn a_thread_in_a_file_outside_the_changeset_has_no_place() {
        let cs = changeset(PATCH);
        let doc = CommentDoc { comments: vec![comment("c", "b.rs", Side::New, 1, 1, &["x"])] };
        assert!(place(&doc, &cs).is_empty());
    }

    #[test]
    fn the_nearest_copy_of_the_text_wins() {
        let cs = changeset("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,5 @@\n+x\n+y\n+x\n+y\n+x\n");
        let doc = CommentDoc { comments: vec![comment("c", "a.rs", Side::New, 4, 4, &["x"])] };
        // Lines 3 and 5 are both one away: the first found stays.
        assert_eq!(place(&doc, &cs)["c"].lines, Some((3, 3)));
    }

    #[test]
    fn old_side_anchors_read_the_old_numbers() {
        let cs = changeset(PATCH);
        let anchor = anchor_for(&cs.files[0], Side::Old, 11, 11).unwrap();
        assert_eq!(anchor.code, ["two"]);
        assert_eq!(anchor.location(), "a.rs:11");
        assert!(anchor_for(&cs.files[0], Side::Old, 11, 99).is_none());
    }

    #[test]
    fn a_message_names_each_thread_and_how_to_answer_it() {
        let mut sent = comment("old", "a.rs", Side::New, 1, 1, &["x"]);
        sent.sent = true;
        let doc = CommentDoc {
            comments: vec![sent, comment("c1", "a.rs", Side::New, 11, 12, &["TWO", "three"])],
        };
        let (message, ids) = agent_message(&doc, "proj", "my-task").unwrap();
        assert_eq!(ids, ["c1"]);
        assert!(message.contains("[thread c1] a.rs:11-12\n```\nTWO\nthree\n```\njlong: why?\n"), "{message}");
        assert!(!message.contains("[thread old]"), "sent threads stay out: {message}");
        assert!(message.contains("shelbi --project proj diff comment reply --task my-task -i <thread>"), "{message}");
        assert!(agent_message(&CommentDoc::default(), "p", "t").is_none());
    }

    #[test]
    fn a_reject_marks_only_the_threads_its_reason_still_holds() {
        let doc = CommentDoc {
            comments: vec![
                comment("kept", "a.rs", Side::New, 1, 1, &["x"]),
                comment("cut", "b.rs", Side::New, 2, 2, &["y"]),
            ],
        };
        let (message, _) = agent_message(&doc, "p", "t").unwrap();
        let start = message.find("[thread cut]").unwrap();
        let edited = format!("Please redo this.\n\n{}", &message[..start]);
        assert_eq!(threads_in(&edited, &doc), ["kept"]);
    }

    #[test]
    fn a_thread_is_found_by_id_prefix_or_place() {
        let doc = CommentDoc {
            comments: vec![
                comment("abc-1", "a.rs", Side::New, 1, 1, &["x"]),
                comment("abd-2", "a.rs", Side::New, 2, 2, &["y"]),
            ],
        };
        assert_eq!(find_thread(&doc, "abd-2").unwrap().id, "abd-2");
        assert_eq!(find_thread(&doc, "abc").unwrap().id, "abc-1");
        assert_eq!(find_thread(&doc, "2").unwrap().id, "abd-2");
        assert!(find_thread(&doc, "ab").unwrap_err().contains("more than one"));
        assert!(find_thread(&doc, "9").unwrap_err().contains("no thread 9"));
        assert!(find_thread(&doc, "zz").unwrap_err().contains("no thread"));
    }

    #[test]
    fn a_persons_reply_reopens_a_thread_and_an_agents_doesnt() {
        let reply = |id: &str, author: Author| CommentReply {
            id: id.into(),
            author,
            body: "ok".into(),
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
        };
        let mut doc = CommentDoc { comments: vec![comment("c", "a.rs", Side::New, 1, 1, &["x"])] };
        doc.apply(&CommentOp::MarkSent { ids: vec!["c".into()] });
        doc.apply(&CommentOp::Reply { id: "c".into(), reply: reply("r1", Author::agent("developer")) });
        assert!(doc.comments[0].sent, "the agent's own answer needs no sending back");
        doc.apply(&CommentOp::Reply { id: "c".into(), reply: reply("r2", Author::human("jlong")) });
        assert!(!doc.comments[0].sent, "the person's follow-up does");
    }

    #[test]
    fn ops_apply_and_repeat_harmlessly() {
        let mut doc = CommentDoc::default();
        let add = CommentOp::Add(comment("c", "a.rs", Side::New, 1, 1, &["x"]));
        doc.apply(&add);
        doc.apply(&add);
        assert_eq!(doc.comments.len(), 1);
        let reply = CommentReply {
            id: "r".into(),
            author: Author::human("jlong"),
            body: "fixed".into(),
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
        };
        doc.apply(&CommentOp::MarkSent { ids: vec!["c".into()] });
        assert!(doc.comments[0].sent);
        let op = CommentOp::Reply { id: "c".into(), reply };
        doc.apply(&op);
        doc.apply(&op);
        assert_eq!(doc.comments[0].replies.len(), 1);
        assert!(!doc.comments[0].sent, "a person's new reply hasn't been sent");
        doc.apply(&CommentOp::Edit { id: "c".into(), reply: Some("r".into()), body: "done".into() });
        assert_eq!(doc.comments[0].replies[0].body, "done");
        doc.apply(&CommentOp::Delete { id: "c".into(), reply: Some("r".into()) });
        assert!(doc.comments[0].replies.is_empty());
        doc.apply(&CommentOp::Delete { id: "c".into(), reply: None });
        assert!(doc.comments.is_empty());
        // On a thread that's gone: nothing.
        doc.apply(&CommentOp::Edit { id: "c".into(), reply: None, body: "x".into() });
    }

    #[test]
    fn the_message_quotes_the_code_and_skips_what_was_sent() {
        let mut first = comment("a", "x.rs", Side::New, 3, 4, &["let a;", "let b;"]);
        first.replies.push(CommentReply {
            id: "r".into(),
            author: Author::agent("developer"),
            body: "it drops the slot".into(),
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
        });
        let mut second = comment("b", "y.rs", Side::Old, 9, 9, &["gone"]);
        second.sent = true;
        let doc = CommentDoc { comments: vec![first, second] };
        let (message, ids) = agent_message(&doc, "p", "t").unwrap();
        assert_eq!(ids, ["a"]);
        assert_eq!(
            message,
            "Review comments on this branch:\n\n[thread a] x.rs:3-4\n```\nlet a;\nlet b;\n```\njlong: why?\n  ↳ developer: it drops the slot\n\n\
             To answer a thread where the reviewer reads it, run: \
             shelbi --project p diff comment reply --task t -i <thread> \"your reply\""
        );
        assert!(agent_message(&CommentDoc::default(), "p", "t").is_none());
    }

    #[test]
    fn the_doc_round_trips_as_json() {
        let doc = CommentDoc { comments: vec![comment("a", "x.rs", Side::Old, 3, 3, &["x"])] };
        let text = serde_json::to_string(&doc).unwrap();
        assert!(text.contains("\"side\":\"old\""), "{text}");
        assert_eq!(serde_json::from_str::<CommentDoc>(&text).unwrap(), doc);
    }
}
