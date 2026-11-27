//! The wiki's hub protocol: what a client (the CLI, an agent, the desktop
//! app, the TUI) asks the hub's wiki service, and what it answers.
//!
//! The hub is the wiki's single writer, so every read and write goes
//! through it, on this machine or, for a project on another machine, over
//! the relay. These ride the app's own request (`far::Request::Wiki` in a
//! control-socket `AppRequest`), which a remote project tab already sends
//! to its host. The hub side is `shelbi_orchestrator::wiki`.
//!
//! Plain data only: the storage and sync live in `shelbi-wiki`, which the
//! clients don't link.

use serde::{Deserialize, Serialize};

use crate::far::{Reply, Request};
use crate::source::ProjectLink;

/// A request to a project's wiki. Paths are relative to the wiki's root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WikiRequest {
    /// Set the wiki up if it isn't: the project's `wiki:` block, or the
    /// default orphan branch when it has none. Answers [`WikiReply::Status`].
    Init,
    /// Where the sync stands. Answers [`WikiReply::Status`] (with
    /// `set_up: false` for a project without a wiki).
    Status,
    /// The folders and documents under `path` (the root when `None`).
    /// Answers [`WikiReply::Tree`].
    Tree { path: Option<String> },
    /// A document, or one section of it (`## Heading`, as written). Answers
    /// [`WikiReply::Doc`].
    Read { path: String, section: Option<String> },
    /// Replace a document (creating it), or one section's body.
    Write {
        path: String,
        content: String,
        section: Option<String>,
        by: Author,
    },
    /// Add to the end of a document or section.
    Append {
        path: String,
        content: String,
        section: Option<String>,
        by: Author,
    },
    /// Add to the start of a document (after its frontmatter and title) or
    /// section.
    Prepend {
        path: String,
        content: String,
        section: Option<String>,
        by: Author,
    },
    /// Replace a document's text with `cstore write`'s rules: its
    /// frontmatter and H1 stay unless `content` brings its own, and `title`
    /// sets the H1. The document must exist. (A client's save of the whole
    /// text sends [`WikiRequest::Write`].)
    Overwrite {
        path: String,
        content: String,
        title: Option<String>,
        by: Author,
    },
    /// A new document (a path ending in `.md`) or folder (any other path).
    /// A document gets `# <title>` (else a title from its file name) unless
    /// `content` starts with an H1 of its own; a folder's `content` is its
    /// description.
    New {
        path: String,
        content: Option<String>,
        title: Option<String>,
        by: Author,
    },
    /// Move or rename a document or folder.
    Move { from: String, to: String, by: Author },
    /// Copy a document or folder.
    Copy { from: String, to: String, by: Author },
    /// Remove a document or folder, or one section of a document.
    Remove {
        path: String,
        section: Option<String>,
        by: Author,
    },
    /// One folder's entries (subfolders, then documents, with their
    /// counts), or a document's headings. Answers [`WikiReply::Entries`].
    List { path: Option<String> },
    /// The GFM tasks (`- [ ]`, `- [x]`) of a document, or of every document
    /// directly in a folder; only one section's with `section`, only the
    /// open ones with `pending`. Answers [`WikiReply::Tasks`].
    Tasks {
        path: String,
        section: Option<String>,
        pending: bool,
    },
    /// Check (or uncheck) the one task of a document whose text contains
    /// `text` (case-insensitive), looking only in `section` when given.
    SetTask {
        path: String,
        text: String,
        section: Option<String>,
        checked: bool,
        by: Author,
    },
    /// The uncommitted change under `path` (the whole wiki when `None`), as
    /// a unified diff. Answers [`WikiReply::Text`].
    Diff { path: Option<String> },
    /// The commits that touched `path` (the whole wiki when `None`), newest
    /// first, at most `limit`. Answers [`WikiReply::Log`].
    Log { path: Option<String>, limit: usize },
    /// Ask the clients to show the Wiki at `path` (its root when `None`):
    /// the hub announces it on the change bus.
    Open { path: Option<String> },
    /// Grep the wiki's documents. Answers [`WikiReply::Matches`].
    Search {
        query: String,
        folder: Option<String>,
        regex: bool,
    },
    /// Control the sync. Answers [`WikiReply::Status`].
    Sync(SyncOp),
    /// A held file's three sides. Answers [`WikiReply::Conflict`].
    Conflict { path: String },
    /// Resolve a held file with `content` (`None` deletes it), commit it
    /// as "Resolve merge conflicts", and sync.
    Resolve { path: String, content: Option<String> },
}

/// Who a write is from, for the `source` the change is announced with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Author {
    /// A person: the CLI in a shell, a client's save.
    Person,
    /// An agent (a Shelbi-managed pane).
    Agent,
}

impl Author {
    /// The author of a write from this process: an agent inside a
    /// Shelbi-managed pane, a person otherwise.
    pub fn of_this_process() -> Self {
        if std::env::var_os("SHELBI_MANAGED_CONTEXT").is_some() {
            Author::Agent
        } else {
            Author::Person
        }
    }
}

/// What [`WikiRequest::Sync`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncOp {
    /// Commit, pull and push now, and answer once it's done.
    Now,
    /// Stop syncing on the clock.
    Pause,
    Resume,
    /// A client shows the Wiki: pull every 30 s instead of every 5 min for
    /// the next minute and a half. Clients send it while the Wiki is open.
    Active,
}

/// A wiki's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WikiReply {
    Status(WikiStatus),
    Tree(Vec<WikiNode>),
    Doc { path: String, content: String },
    Matches(Vec<WikiMatch>),
    Entries(Vec<WikiEntry>),
    Tasks(Vec<WikiTask>),
    Log(Vec<WikiCommit>),
    /// Plain text (a diff).
    Text(String),
    Conflict(WikiConflict),
    /// A write (or move, copy, remove) was made.
    Done,
    Failed(WikiFailure),
}

/// The sync's state, as `shelbi wiki sync status` and the sidebars show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncState {
    Synced,
    Syncing,
    /// Changes wait to be committed or pushed.
    Pending,
    Offline,
    /// Some files couldn't be merged and are held.
    Conflict,
    Error(String),
}

impl std::fmt::Display for SyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncState::Synced => f.write_str("synced"),
            SyncState::Syncing => f.write_str("syncing"),
            SyncState::Pending => f.write_str("pending"),
            SyncState::Offline => f.write_str("offline"),
            SyncState::Conflict => f.write_str("conflict"),
            SyncState::Error(detail) => write!(f, "error {detail}"),
        }
    }
}

/// A wiki's sync, for [`WikiReply::Status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiStatus {
    /// Whether the project has a wiki yet. The rest is meaningless when not.
    pub set_up: bool,
    pub state: SyncState,
    /// Held files.
    pub conflicts: Vec<String>,
    pub paused: bool,
    /// `sync: auto`.
    pub auto: bool,
    /// `branch` or `folder`.
    pub source: String,
    /// The branch the wiki's worktree has checked out.
    pub branch: String,
    /// The wiki's root folder on the project's machine.
    pub root: String,
    /// When the last sync finished cleanly, RFC3339.
    pub last_synced: Option<String>,
    /// Folder mode: the rolling pull request's URL, once one is open.
    pub pull_request: Option<String>,
}

impl WikiStatus {
    /// The answer for a project without a wiki.
    pub fn not_set_up() -> Self {
        Self {
            set_up: false,
            state: SyncState::Synced,
            conflicts: Vec::new(),
            paused: false,
            auto: false,
            source: String::new(),
            branch: String::new(),
            root: String::new(),
            last_synced: None,
            pull_request: None,
        }
    }
}

/// One folder or document of [`WikiReply::Tree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiNode {
    pub path: String,
    /// The file or folder name on disk.
    pub name: String,
    /// A document's first H1 (else its file name); a folder's name.
    pub title: String,
    pub kind: NodeKind,
    /// A folder's subfolders (in `folderOrder`), then its documents (pinned
    /// first, then by title). Empty for a document.
    pub children: Vec<WikiNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Folder,
    Document,
}

/// One line of [`WikiReply::Matches`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiMatch {
    pub path: String,
    /// 1-based.
    pub line: usize,
    pub text: String,
    /// Byte range of the match within `text`.
    pub start: usize,
    pub end: usize,
}

/// One row of [`WikiReply::Entries`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiEntry {
    pub kind: EntryKind,
    /// A folder's or document's name on disk; a section's heading line as
    /// written (`## Goals`).
    pub name: String,
    /// Wiki-relative; a section's is its document's.
    pub path: String,
    /// A document's first H1 (else its file name); a folder's display name;
    /// a section's heading text.
    pub title: String,
    /// A section's heading level (1..=6); 0 otherwise.
    pub level: u8,
    /// A document's or section's lines and words; 0 for a folder.
    pub lines: usize,
    pub words: usize,
    /// A document's size; 0 otherwise.
    pub bytes: u64,
    /// A folder's documents (directly in it); 0 otherwise.
    pub documents: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    Folder,
    Document,
    Section,
}

/// One task of [`WikiReply::Tasks`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiTask {
    /// The document it's in.
    pub path: String,
    /// 1-based, in the document (also when asked for one section).
    pub line: usize,
    pub checked: bool,
    pub text: String,
}

/// One commit of [`WikiReply::Log`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiCommit {
    pub sha: String,
    pub author: String,
    /// RFC3339.
    pub date: String,
    pub subject: String,
    /// The wiki-relative files it touched.
    pub paths: Vec<String>,
}

/// A held file's sides, for [`WikiReply::Conflict`]. `None` is "deleted on
/// that side" (or, for `base`, "added on both").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiConflict {
    pub path: String,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    pub detected_at: String,
}

/// Why a wiki request failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiFailure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    NotFound,
    Invalid,
    Exists,
    /// The file is held for a conflict, or the request needs one that isn't.
    Conflict,
    /// The project has no wiki yet (`shelbi wiki init` sets one up).
    NotSetUp,
    Failed,
}

impl FailureKind {
    /// The exit code `shelbi wiki` ends with: 2 not found, 3 invalid, 4
    /// conflict, 1 anything else (as `cstore`).
    pub fn exit_code(self) -> i32 {
        match self {
            FailureKind::NotFound => 2,
            FailureKind::Invalid | FailureKind::Exists => 3,
            FailureKind::Conflict => 4,
            FailureKind::NotSetUp | FailureKind::Failed => 1,
        }
    }
}

impl WikiReply {
    /// A [`WikiReply::Failed`] of `kind`.
    pub fn failed(kind: FailureKind, message: impl Into<String>) -> Self {
        WikiReply::Failed(WikiFailure {
            kind,
            message: message.into(),
        })
    }
}

/// Ask `project`'s wiki on this machine's hub (started on demand).
/// Blocking.
pub fn ask_local(project: &str, req: WikiRequest) -> WikiReply {
    match crate::exec_daemon::app_request(project, &Request::Wiki(req)) {
        Ok(Reply::Wiki(reply)) => reply,
        Ok(other) => WikiReply::failed(FailureKind::Failed, format!("the hub answered something else ({other:?})")),
        Err(e) => WikiReply::failed(FailureKind::Failed, e.to_string()),
    }
}

/// Ask `link`'s project's wiki, where the project lives: this machine's hub,
/// or the far one over the relay. Blocking.
pub fn ask(link: &ProjectLink, req: WikiRequest) -> WikiReply {
    if !link.is_remote() {
        return ask_local(&link.key().name, req);
    }
    match link.far_request(&Request::Wiki(req)) {
        Ok(Reply::Wiki(reply)) => reply,
        Ok(other) => WikiReply::failed(FailureKind::Failed, format!("the far hub answered something else ({other:?})")),
        Err(e) => WikiReply::failed(FailureKind::Failed, e),
    }
}

/// The editor command for `file`: $EDITOR, else $VISUAL, else `vi`, split
/// on whitespace so `code --wait` works. `shelbi wiki edit` and the TUI's
/// Wiki `e` both run it, and wait for it to exit.
pub fn editor_argv(file: &std::path::Path, editor: Option<&str>, visual: Option<&str>) -> Vec<String> {
    let editor = editor
        .filter(|s| !s.trim().is_empty())
        .or_else(|| visual.filter(|s| !s.trim().is_empty()))
        .unwrap_or("vi");
    let mut argv: Vec<String> = editor.split_whitespace().map(String::from).collect();
    argv.push(file.to_string_lossy().into_owned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_as_json() {
        let req = Request::Wiki(WikiRequest::Write {
            path: "Plans/a.md".into(),
            content: "# A\n".into(),
            section: Some("## Goals".into()),
            by: Author::Agent,
        });
        let json = serde_json::to_value(&req).unwrap();
        let back: Request = serde_json::from_value(json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn failures_map_to_cstore_exit_codes() {
        assert_eq!(FailureKind::NotFound.exit_code(), 2);
        assert_eq!(FailureKind::Invalid.exit_code(), 3);
        assert_eq!(FailureKind::Conflict.exit_code(), 4);
        assert_eq!(FailureKind::Failed.exit_code(), 1);
    }

    #[test]
    fn sync_state_reads_as_its_wire_word() {
        assert_eq!(SyncState::Pending.to_string(), "pending");
        assert_eq!(SyncState::Error("no network".into()).to_string(), "error no network");
    }
}
