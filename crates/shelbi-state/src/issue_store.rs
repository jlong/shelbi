//! The `IssueStore` seam: the backend-agnostic contract every consumer
//! (CLI, TUI, orchestrator) uses to reach a project's issue board.
//!
//! Today the only backend is [`FileSystemStore`] — a thin facade over the
//! markdown-on-disk board that already lives in this crate. It exists so the
//! GitHub / Jira / Linear backends the pluggable-issue-trackers plan calls for
//! can slot in behind the same trait without any consumer learning which
//! backend is live. Every method speaks the shared domain types ([`Issue`],
//! status [`Column`] ids, issue ids) so the workflow / category layer stays
//! backend-agnostic.
//!
//! ## What routes through the trait vs. what stays in the module
//!
//! The trait captures the *semantic* board operations from the plan (list /
//! get / add / move-status / set-priority / set-fields / cancel / poll-changes
//! / comments). The lower-level filesystem primitives it is built on
//! (`load_task` / `save_task` / `renumber_column`, the per-project task lock,
//! parked-review markers, welcome-card scaffolding) remain module functions:
//! they are the *mechanism* the [`FileSystemStore`] happens to use, not part of
//! the cross-backend contract, and a remote backend will satisfy the same trait
//! with an entirely different mechanism. `FileSystemStore` therefore delegates
//! to those functions, which keeps this a behavior-preserving refactor.
//!
//! Comments and `poll_changes` are the two capabilities the plan adds that the
//! filesystem board did not have before; both are implemented natively here
//! (comments as frontmatter files under `<project>/comments/<id>/`, change
//! detection as an `updated_at` watermark) rather than delegated.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::{
    Column, Error, Issue, IssueLaunchConfig, IssueTrackerBackend, IssueTrackerConfig, IssueZenConfig,
    Result,
};

use crate::IssueFile;

/// Resolve a project's `issue_tracker` config into a live [`IssueStore`].
///
/// The config is validated first, so a `github` backend with a missing or
/// malformed `repo` surfaces a field-named [`Error::InvalidIssueTracker`]
/// before we ever try to build a store. `file_system` (the markdown board) and
/// `github` (issues in `owner/repo`) both have live backends; `jira` / `linear`
/// validate but resolve to a typed [`Error::IssueTrackerUnimplemented`] so a
/// caller can tell "not built yet" apart from "you configured it wrong". See
/// `Plans/pluggable-task-stores.md` §2 + D1.
pub fn resolve_issue_store(
    project: &str,
    cfg: &IssueTrackerConfig,
) -> Result<Box<dyn IssueStore>> {
    cfg.validate()?;
    match cfg.backend {
        IssueTrackerBackend::FileSystem => Ok(Box::new(FileSystemStore::new(project))),
        IssueTrackerBackend::Github => {
            // `validate()` above guarantees a well-formed `github.repo`, so the
            // unwrap is unreachable — kept as a typed error rather than a panic.
            let repo = cfg
                .github
                .as_ref()
                .map(|g| g.repo.clone())
                .ok_or_else(|| {
                    Error::InvalidIssueTracker("issue_tracker.github.repo is required".into())
                })?;
            Ok(Box::new(crate::GitHubStore::new(project, repo)))
        }
        other => Err(Error::IssueTrackerUnimplemented(other.to_string())),
    }
}

/// Resolve the live [`IssueStore`] for `project` from its on-disk YAML — the
/// name-only convenience over [`resolve_issue_store`] for the many call-sites
/// (CLI subcommands, daemon loops) that hold a project name but not a loaded
/// [`shelbi_core::Project`]. Loading the project config is the same read those
/// paths already do elsewhere per operation.
///
/// Call-sites that already hold a `&Project` should call [`resolve_issue_store`]
/// with `&project.issue_tracker` directly instead, to avoid re-reading the YAML.
pub fn issue_store_for(project: &str) -> Result<Box<dyn IssueStore>> {
    let cfg = crate::load_project(project)?.issue_tracker;
    resolve_issue_store(project, &cfg)
}

/// Creation spec handed to [`IssueStore::add`]. Carries the durable issue
/// definition without the fields a store assigns itself (priority within a
/// column, timestamps). For the filesystem backend the `id` is client-chosen;
/// remote backends may ignore it and mint their own, returning the created
/// [`Issue`] with the authoritative id.
#[derive(Debug, Clone)]
pub struct NewIssue {
    pub id: String,
    pub title: String,
    pub column: Column,
    pub body: String,
    pub workflow: Option<String>,
    pub branch: Option<String>,
    pub depends_on: Vec<String>,
    pub prefers_machine: Option<String>,
    pub zen: Option<IssueZenConfig>,
    pub launch: Option<IssueLaunchConfig>,
    pub params: BTreeMap<String, serde_yaml::Value>,
    /// Explicit position within the destination column. `None` appends to the
    /// end (priority = current column length), matching `shelbi task add`.
    pub priority: Option<u32>,
}

impl NewIssue {
    /// A spec with only the required fields set and everything optional
    /// defaulted (no workflow / branch / deps, appended to its column).
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        column: Column,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            column,
            body: body.into(),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: BTreeMap::new(),
            priority: None,
        }
    }
}

/// Partial field update for [`IssueStore::set_fields`]. Each field is a nested
/// option: the outer `Some` means "touch this field", the inner value is what
/// to set it to (`None` clears it). An all-`None` `IssueFields` is a no-op.
/// This is the general-purpose sibling of the old single-field `set_branch`.
#[derive(Debug, Clone, Default)]
pub struct IssueFields {
    pub branch: Option<Option<String>>,
    pub depends_on: Option<Vec<String>>,
    pub prefers_machine: Option<Option<String>>,
    pub assigned_to: Option<Option<String>>,
    /// The issue's display title. `Some` replaces it (an empty string is a
    /// legal, if unusual, title — the store does not police that).
    pub title: Option<String>,
    /// The issue's workflow name. Outer `Some` touches it, inner `None` clears
    /// it back to the project default (the `edit --workflow` surface).
    pub workflow: Option<Option<String>>,
    /// The issue's markdown body (the prose under the frontmatter). `Some`
    /// replaces it wholesale — this is the one non-frontmatter field
    /// [`IssueStore::set_fields`] touches, so `shelbi issue edit` can rewrite
    /// the body through the same seam as its frontmatter edits.
    pub body: Option<String>,
}

impl IssueFields {
    /// True when no field is set — [`IssueStore::set_fields`] short-circuits on
    /// this so a caller "updating" nothing never bumps `updated_at`.
    pub fn is_empty(&self) -> bool {
        self.branch.is_none()
            && self.depends_on.is_none()
            && self.prefers_machine.is_none()
            && self.assigned_to.is_none()
            && self.title.is_none()
            && self.workflow.is_none()
            && self.body.is_none()
    }
}

/// Relative or absolute priority move within an issue's current column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrioMove {
    /// Priority 0 (top of the column).
    Top,
    /// Last position in the column.
    Bottom,
    /// One position toward the top (no-op at the top).
    Up,
    /// One position toward the bottom (no-op at the bottom).
    Down,
    /// An explicit position, clamped into range (the classic
    /// `set_task_priority` behavior).
    Set(u32),
}

/// The result of a status move that actually changed something: where the
/// issue came from, where it landed, and the workflow it runs under (so the
/// caller can append a move event). Mirrors the `(from, to, workflow)` triple
/// the filesystem move functions have always returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMove {
    pub from: Column,
    pub to: Column,
    pub workflow: String,
}

/// A comment on an issue. For the filesystem backend the `id` is the
/// zero-padded sequence number of the comment file; remote backends carry
/// their native comment id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueComment {
    pub id: String,
    pub author: Option<String>,
    pub created_at: DateTime<Utc>,
    pub body: String,
}

/// One change surfaced by [`IssueStore::poll_changes`], to be reconciled into
/// the event log by the orchestrator's heartbeat.
///
/// The filesystem backend detects *upserts* (an issue whose `updated_at` moved
/// past the cursor) and *new comments* (a comment file created past the
/// cursor). Detecting *removals* needs a record of the prior id set, which is
/// the reconciliation subtask's job — the enum leaves room for it, but the
/// filesystem backend does not emit it yet.
#[derive(Debug, Clone)]
pub enum IssueChange {
    /// An issue was created or modified since the cursor.
    Upserted(Box<IssueFile>),
    /// A comment was added to an issue since the cursor.
    CommentAdded {
        issue_id: String,
        comment: IssueComment,
    },
}

/// Opaque poll watermark threaded through successive [`IssueStore::poll_changes`]
/// calls. The plan's "lightweight sync cursor, not a content cache": for the
/// filesystem backend it is simply the high-water `updated_at` timestamp seen
/// on the last pass. [`Cursor::start`] begins a fresh watch (reports nothing as
/// changed on the first call, only the current high-water mark).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    watermark: Option<DateTime<Utc>>,
}

impl Cursor {
    /// The initial cursor for a watcher that has never polled. The first
    /// `poll_changes(&Cursor::start())` returns no changes and a cursor set to
    /// the current board high-water mark, so only genuinely later edits surface.
    pub fn start() -> Self {
        Self { watermark: None }
    }

    /// Construct a cursor pinned at an explicit watermark (persisted across
    /// process restarts by the caller).
    pub fn at(watermark: DateTime<Utc>) -> Self {
        Self {
            watermark: Some(watermark),
        }
    }

    /// The watermark timestamp, if this cursor has advanced past `start`.
    pub fn watermark(&self) -> Option<DateTime<Utc>> {
        self.watermark
    }
}

/// The board seam. Every consumer that reads or mutates a project's issues
/// does so through this trait; [`FileSystemStore`] is the only implementor
/// today, with GitHub / Jira / Linear backends to follow behind the same shape.
pub trait IssueStore {
    /// The whole board, in the canonical column-then-priority order.
    fn list(&self) -> Result<Vec<IssueFile>>;

    /// Every issue in a single status column, in priority order.
    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>>;

    /// One issue by id, or `None` when no such issue exists.
    fn get(&self, id: &str) -> Result<Option<IssueFile>>;

    /// Create an issue from `spec`, returning it as stored (with its assigned
    /// priority and timestamps). Errors if the id already exists.
    fn add(&self, spec: NewIssue) -> Result<Issue>;

    /// Move an issue to status `to`. Returns `Some(StatusMove)` when the status
    /// actually changed, `None` when the issue was already there. `reason` is
    /// carried for backends that persist it (e.g. a GitHub comment); the
    /// filesystem backend records the move in the event log via the caller.
    fn move_status(&self, id: &str, to: &Column, reason: &str) -> Result<Option<StatusMove>>;

    /// Reposition an issue within its current column.
    fn set_priority(&self, id: &str, pos: PrioMove) -> Result<()>;

    /// Apply a partial field update. A no-op (and no `updated_at` bump) when
    /// `fields` is empty or leaves every value unchanged.
    fn set_fields(&self, id: &str, fields: IssueFields) -> Result<()>;

    /// Cancel an issue: move it to the terminal `canceled` status and drop any
    /// workspace assignment. Returns the move like [`IssueStore::move_status`].
    fn cancel(&self, id: &str, reason: &str) -> Result<Option<StatusMove>>;

    /// Move an issue to status `to` **and** clear its workspace assignment in a
    /// single write. The generalization of [`IssueStore::cancel`] (which is this
    /// with `to = canceled`): an agent-driven transition can land the card in an
    /// *active* status, where a stale `assigned_to` would make the closed
    /// workspace's supervisor relaunch it. Returns `Some(StatusMove)` when the
    /// status changed (the owner is still cleared when it did not).
    fn move_status_and_unassign(
        &self,
        id: &str,
        to: &Column,
        reason: &str,
    ) -> Result<Option<StatusMove>>;

    /// Delete an issue outright. Idempotent — deleting a missing issue is `Ok`.
    /// Callers that keep a column contiguous follow this with
    /// [`IssueStore::renumber`].
    fn delete(&self, id: &str) -> Result<()>;

    /// Renumber a status column so its priorities are the contiguous
    /// `0..len` again — the repair a delete or an out-of-band edit leaves for.
    fn renumber(&self, status: &Column) -> Result<()>;

    /// Unload a review-status issue from its slot at the operator's request:
    /// clear `assigned_to` and set a durable "parked" marker so the review
    /// auto-loader and the stranded-slot resume both leave it alone until it is
    /// explicitly loaded again. Returns the workspace it was unassigned from, if
    /// any. See [`crate::park_review_task`].
    fn park_review(&self, id: &str) -> Result<Option<String>>;

    /// Clear an issue's parked marker if present (idempotent). Called on any
    /// fresh dispatch/assignment so a re-served or reworked issue stops being
    /// skipped by the auto-loader. See [`crate::clear_task_parked`].
    fn clear_parked(&self, id: &str) -> Result<()>;

    /// **Reject** a review-status issue: append the reviewer's `reason` to the
    /// issue body as a dated feedback section and bounce the card back to the
    /// `ready` status, clearing its workspace assignment — all as one atomic
    /// operation, so a crash can't leave the card edited-but-not-moved or
    /// unowned-but-still-in-review. `date` is the pre-formatted rejection date
    /// stamped into the section header (passed in for testability). The caller
    /// resolves `ready` from the issue's workflow (the same way it resolves the
    /// accept target for [`IssueStore::move_status`]), keeping the workflow layer
    /// out of the store. Returns `Some(StatusMove)` when the status changed; the
    /// body edit and owner-clear still land (returning `None`) when the card was
    /// already in `ready`. See [`crate::reject_review_task_to`].
    fn reject_review(
        &self,
        id: &str,
        ready: &Column,
        reason: &str,
        date: &str,
    ) -> Result<Option<StatusMove>>;

    /// Everything that changed since `since`, plus the cursor to pass next time.
    fn poll_changes(&self, since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)>;

    /// An issue's comments, oldest first.
    fn list_comments(&self, id: &str) -> Result<Vec<IssueComment>>;

    /// Append a comment to an issue and return it as stored.
    fn add_comment(&self, id: &str, body: &str) -> Result<IssueComment>;
}

/// The markdown-on-disk board backend: today's `~/.shelbi/projects/<name>/tasks/`
/// files, behind the [`IssueStore`] trait. A cheap handle — it holds only the
/// project name and resolves paths per call, so it is fine to construct one per
/// operation.
#[derive(Debug, Clone)]
pub struct FileSystemStore {
    project: String,
}

impl FileSystemStore {
    /// A store bound to `project` (the registry alias / on-disk directory name).
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
        }
    }

    /// The project name this store is bound to.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// `<project_dir>/comments/<id>/` — where an issue's comment files live.
    /// A sibling of `tasks/`, so the `*.md` task scan never sees comments.
    fn comments_dir(&self, id: &str) -> Result<PathBuf> {
        shelbi_core::validate_task_id(id)?;
        Ok(crate::project_dir(&self.project)?
            .join("comments")
            .join(id))
    }
}

impl IssueStore for FileSystemStore {
    fn list(&self) -> Result<Vec<IssueFile>> {
        crate::list_tasks(&self.project)
    }

    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
        crate::list_column(&self.project, status.clone())
    }

    fn get(&self, id: &str) -> Result<Option<IssueFile>> {
        // Missing file -> None; a genuine parse / id-mismatch error propagates.
        if !crate::task_path(&self.project, id)?.exists() {
            return Ok(None);
        }
        Ok(Some(crate::load_task(&self.project, id)?))
    }

    fn add(&self, spec: NewIssue) -> Result<Issue> {
        let now = Utc::now();
        // Append to the destination column unless an explicit slot was given —
        // the same rule `shelbi task add` has always used.
        let priority = match spec.priority {
            Some(p) => p,
            None => crate::list_column(&self.project, spec.column.clone())?.len() as u32,
        };
        let issue = Issue {
            id: spec.id,
            title: spec.title,
            column: spec.column,
            priority,
            assigned_to: None,
            workflow: spec.workflow,
            branch: spec.branch,
            depends_on: spec.depends_on,
            prefers_machine: spec.prefers_machine,
            zen: spec.zen,
            launch: spec.launch,
            created_at: now,
            updated_at: now,
            params: spec.params,
        };
        // Reject self-references, unknown dep ids, and cycles before the card
        // lands — the same guard `shelbi task add` ran, now the store's job so
        // every `add` caller (and every backend) enforces it.
        if !issue.depends_on.is_empty() {
            let existing = crate::list_tasks(&self.project)?;
            crate::validate_depends_on(&issue, &existing)?;
        }
        crate::create_task(&self.project, &issue, &spec.body)?;
        Ok(issue)
    }

    fn move_status(&self, id: &str, to: &Column, _reason: &str) -> Result<Option<StatusMove>> {
        Ok(crate::move_task(&self.project, id, to.clone())?.map(Into::into))
    }

    fn set_priority(&self, id: &str, pos: PrioMove) -> Result<()> {
        let target = match pos {
            PrioMove::Set(n) => n,
            _ => {
                // Top/Bottom/Up/Down resolve against the issue's current index
                // in its column. Reads outside the write lock are fine: the
                // absolute `set_task_priority` re-derives the index under the
                // lock and clamps, so a concurrent reorder can only cost this
                // move a slot, never corrupt the column.
                let issue = crate::load_task(&self.project, id)?;
                let col = crate::list_column(&self.project, issue.task.column)?;
                let idx = col
                    .iter()
                    .position(|tf| tf.task.id == id)
                    .ok_or_else(|| Error::Other(format!("issue `{id}` not in its own column?")))?;
                let last = col.len().saturating_sub(1);
                match pos {
                    PrioMove::Top => 0,
                    PrioMove::Bottom => last as u32,
                    PrioMove::Up => idx.saturating_sub(1) as u32,
                    PrioMove::Down => (idx + 1).min(last) as u32,
                    PrioMove::Set(_) => unreachable!(),
                }
            }
        };
        crate::set_task_priority(&self.project, id, target)
    }

    fn set_fields(&self, id: &str, fields: IssueFields) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        // Load -> mutate -> save under the per-project task lock, so a
        // concurrent writer touching a *different* field on the same card
        // can't be clobbered by a stale whole-issue write (the same guarantee
        // the single-field `set_task_branch` provides). Skip the write and the
        // `updated_at` bump when nothing actually changed.
        let _lock = crate::lock_tasks(&self.project)?;
        let mut tf = crate::load_task(&self.project, id)?;
        let mut changed = false;
        if let Some(branch) = fields.branch {
            if tf.task.branch != branch {
                tf.task.branch = branch;
                changed = true;
            }
        }
        if let Some(depends_on) = fields.depends_on {
            if tf.task.depends_on != depends_on {
                tf.task.depends_on = depends_on;
                changed = true;
            }
        }
        if let Some(prefers_machine) = fields.prefers_machine {
            if tf.task.prefers_machine != prefers_machine {
                tf.task.prefers_machine = prefers_machine;
                changed = true;
            }
        }
        if let Some(assigned_to) = fields.assigned_to {
            if tf.task.assigned_to != assigned_to {
                tf.task.assigned_to = assigned_to;
                changed = true;
            }
        }
        if let Some(title) = fields.title {
            if tf.task.title != title {
                tf.task.title = title;
                changed = true;
            }
        }
        if let Some(workflow) = fields.workflow {
            if tf.task.workflow != workflow {
                tf.task.workflow = workflow;
                changed = true;
            }
        }
        // The body is not frontmatter, so it is compared and swapped on `tf`
        // directly rather than on `tf.task`.
        if let Some(body) = fields.body {
            if tf.body != body {
                tf.body = body;
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        tf.task.updated_at = Utc::now();
        crate::save_task_unlocked(&self.project, &tf.task, &tf.body)
    }

    fn cancel(&self, id: &str, reason: &str) -> Result<Option<StatusMove>> {
        // Cancel is move-and-unassign to the terminal `canceled` status: drop
        // any owner so the pane supervisor doesn't relaunch a canceled card on
        // its old workspace (same reasoning as the release-to-todo path).
        self.move_status_and_unassign(id, &Column::canceled(), reason)
    }

    fn move_status_and_unassign(
        &self,
        id: &str,
        to: &Column,
        _reason: &str,
    ) -> Result<Option<StatusMove>> {
        Ok(crate::move_task_and_unassign(&self.project, id, to.clone())?.map(Into::into))
    }

    fn delete(&self, id: &str) -> Result<()> {
        crate::delete_task(&self.project, id)
    }

    fn renumber(&self, status: &Column) -> Result<()> {
        crate::renumber_column(&self.project, status.clone())
    }

    fn park_review(&self, id: &str) -> Result<Option<String>> {
        crate::park_review_task(&self.project, id)
    }

    fn clear_parked(&self, id: &str) -> Result<()> {
        crate::clear_task_parked(&self.project, id)
    }

    fn reject_review(
        &self,
        id: &str,
        ready: &Column,
        reason: &str,
        date: &str,
    ) -> Result<Option<StatusMove>> {
        Ok(crate::reject_review_task_to(&self.project, id, ready, reason, date)?.map(Into::into))
    }

    fn poll_changes(&self, since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
        let issues = crate::list_tasks(&self.project)?;
        let mut changes = Vec::new();
        let mut high = since.watermark();

        for tf in issues {
            let updated = tf.task.updated_at;
            high = Some(high.map_or(updated, |h| h.max(updated)));
            // Only surface changes once the cursor has advanced past `start`;
            // the first poll just establishes the high-water mark.
            if since.watermark().is_some_and(|w| updated > w) {
                changes.push(IssueChange::Upserted(Box::new(tf)));
            }
        }

        // New comments across all issues (created strictly after the cursor).
        // Only meaningful once the cursor has advanced past `start`.
        if let Some(w) = since.watermark() {
            for (issue_id, comment) in self.all_comments()? {
                if comment.created_at > w {
                    high = Some(high.map_or(comment.created_at, |h| h.max(comment.created_at)));
                    changes.push(IssueChange::CommentAdded { issue_id, comment });
                }
            }
        }

        Ok((changes, Cursor { watermark: high }))
    }

    fn list_comments(&self, id: &str) -> Result<Vec<IssueComment>> {
        let dir = self.comments_dir(id)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let text = crate::read_to_string_at(&path)?;
            let (front, body) = crate::split_frontmatter(&text)
                .ok_or_else(|| Error::Other(format!("comment {} missing frontmatter", path.display())))?;
            let meta: CommentMeta = serde_yaml::from_str(front)?;
            out.push(IssueComment {
                id: stem,
                author: meta.author,
                created_at: meta.created_at,
                // The frontmatter format guarantees exactly one trailing
                // newline on the body; strip it so a comment round-trips to
                // the text that was passed to `add_comment`.
                body: strip_one_trailing_newline(body),
            });
        }
        // Sequence-numbered filenames sort lexically into creation order
        // because they are zero-padded.
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn add_comment(&self, id: &str, body: &str) -> Result<IssueComment> {
        // The issue must exist — a comment on a missing card would be
        // orphaned and never surfaced. `get` also validates the id.
        if self.get(id)?.is_none() {
            return Err(Error::Other(format!("issue `{id}` not found")));
        }
        let dir = self.comments_dir(id)?;
        crate::ensure_dir(&dir)?;
        // Next sequence number = one past the current max. Bounded fan-out
        // (comment counts are small) and no separate counter file to keep in
        // sync. Comments are single-writer in practice (the orchestrator), so
        // the read-then-write is not locked; if that changes, wrap it in the
        // task lock like the column mutations.
        let next = self
            .list_comments(id)?
            .iter()
            .filter_map(|c| c.id.parse::<u64>().ok())
            .max()
            .map_or(1, |m| m + 1);
        let seq = format!("{next:04}");
        let created_at = Utc::now();
        let meta = CommentMeta {
            author: None,
            created_at,
        };
        let path = dir.join(format!("{seq}.md"));
        crate::write_frontmatter_file(&path, &meta, body)?;
        Ok(IssueComment {
            id: seq,
            author: None,
            created_at,
            // Match what `list_comments` will read back for this comment.
            body: strip_one_trailing_newline(body),
        })
    }
}

/// Drop a single trailing `\n` — the one the frontmatter file format appends
/// — so a stored body round-trips to the text a caller passed in.
fn strip_one_trailing_newline(s: &str) -> String {
    s.strip_suffix('\n').unwrap_or(s).to_string()
}

impl FileSystemStore {
    /// Flatten every issue's comments into `(issue_id, comment)` pairs — the
    /// scan `poll_changes` walks to spot newly added comments.
    fn all_comments(&self) -> Result<Vec<(String, IssueComment)>> {
        let base = crate::project_dir(&self.project)?.join("comments");
        if !base.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&base)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let Some(issue_id) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            for comment in self.list_comments(issue_id)? {
                out.push((issue_id.to_string(), comment));
            }
        }
        Ok(out)
    }
}

impl From<(Column, Column, String)> for StatusMove {
    fn from((from, to, workflow): (Column, Column, String)) -> Self {
        StatusMove { from, to, workflow }
    }
}

/// Frontmatter of a stored comment file. The body is the comment prose.
#[derive(Debug, Serialize, Deserialize)]
struct CommentMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-issue-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn spec(id: &str, column: Column) -> NewIssue {
        NewIssue::new(id, id.replace('-', " "), column, "# Task\n\nbody\n")
    }

    #[test]
    fn add_get_list_round_trip() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        let issue = store.add(spec("a", Column::todo())).unwrap();
        assert_eq!(issue.id, "a");
        assert_eq!(issue.priority, 0); // appended to an empty column

        let got = store.get("a").unwrap().expect("issue exists");
        assert_eq!(got.task.id, "a");
        assert_eq!(got.body, "# Task\n\nbody\n");
        assert!(store.get("missing").unwrap().is_none());

        // A second add appends after the first.
        let b = store.add(spec("b", Column::todo())).unwrap();
        assert_eq!(b.priority, 1);
        assert_eq!(store.list().unwrap().len(), 2);
        assert_eq!(store.list_in_status(&Column::todo()).unwrap().len(), 2);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn add_rejects_duplicate_id() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::todo())).unwrap();
        assert!(store.add(spec("a", Column::todo())).is_err());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn add_validates_depends_on() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        let with_ghost_dep = || {
            let mut s = spec("a", Column::todo());
            s.depends_on = vec!["ghost".into()];
            s
        };
        // A dependency on an id that doesn't exist is rejected.
        assert!(store.add(with_ghost_dep()).is_err());

        // With the dependency present, the add succeeds.
        store.add(spec("ghost", Column::todo())).unwrap();
        assert!(store.add(with_ghost_dep()).is_ok());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn move_status_reports_move_then_none_when_already_there() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::todo())).unwrap();
        let mv = store
            .move_status("a", &Column::in_progress(), "start")
            .unwrap()
            .expect("status changed");
        assert_eq!(mv.from, Column::todo());
        assert_eq!(mv.to, Column::in_progress());

        assert!(store
            .move_status("a", &Column::in_progress(), "again")
            .unwrap()
            .is_none());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_priority_top_bottom_and_set() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        for id in ["a", "b", "c"] {
            store.add(spec(id, Column::todo())).unwrap();
        }
        // c starts last; send it to the top.
        store.set_priority("c", PrioMove::Top).unwrap();
        let ids: Vec<_> = store
            .list_in_status(&Column::todo())
            .unwrap()
            .into_iter()
            .map(|tf| tf.task.id)
            .collect();
        assert_eq!(ids, vec!["c", "a", "b"]);

        // Now push c back to the bottom via an absolute Set past the end.
        store.set_priority("c", PrioMove::Set(99)).unwrap();
        let ids: Vec<_> = store
            .list_in_status(&Column::todo())
            .unwrap()
            .into_iter()
            .map(|tf| tf.task.id)
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);

        // Up moves one slot toward the top.
        store.set_priority("c", PrioMove::Up).unwrap();
        let ids: Vec<_> = store
            .list_in_status(&Column::todo())
            .unwrap()
            .into_iter()
            .map(|tf| tf.task.id)
            .collect();
        assert_eq!(ids, vec!["a", "c", "b"]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_fields_updates_and_is_noop_when_unchanged() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::in_progress())).unwrap();
        let before = store.get("a").unwrap().unwrap().task.updated_at;

        store
            .set_fields(
                "a",
                IssueFields {
                    branch: Some(Some("jlong/a".into())),
                    assigned_to: Some(Some("alpha".into())),
                    depends_on: Some(vec!["b".into()]),
                    ..Default::default()
                },
            )
            .unwrap();
        let after = store.get("a").unwrap().unwrap().task;
        assert_eq!(after.branch.as_deref(), Some("jlong/a"));
        assert_eq!(after.assigned_to.as_deref(), Some("alpha"));
        assert_eq!(after.depends_on, vec!["b".to_string()]);
        assert!(after.updated_at >= before);

        // Setting the identical values again is a no-op: updated_at unchanged.
        let stamp = after.updated_at;
        store
            .set_fields(
                "a",
                IssueFields {
                    branch: Some(Some("jlong/a".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.get("a").unwrap().unwrap().task.updated_at, stamp);

        // Empty update: also a no-op.
        store.set_fields("a", IssueFields::default()).unwrap();
        assert_eq!(store.get("a").unwrap().unwrap().task.updated_at, stamp);

        // Clearing a field.
        store
            .set_fields(
                "a",
                IssueFields {
                    assigned_to: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.get("a").unwrap().unwrap().task.assigned_to, None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cancel_moves_to_canceled_and_clears_owner() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::in_progress())).unwrap();
        store
            .set_fields(
                "a",
                IssueFields {
                    assigned_to: Some(Some("alpha".into())),
                    ..Default::default()
                },
            )
            .unwrap();

        let mv = store.cancel("a", "obsolete").unwrap().expect("moved");
        assert_eq!(mv.to, Column::canceled());
        let after = store.get("a").unwrap().unwrap().task;
        assert_eq!(after.column, Column::canceled());
        assert_eq!(after.assigned_to, None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_fields_edits_title_workflow_and_body() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::todo())).unwrap();
        store
            .set_fields(
                "a",
                IssueFields {
                    title: Some("New title".into()),
                    workflow: Some(Some("app".into())),
                    body: Some("# Rewritten\n\ncontent\n".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let after = store.get("a").unwrap().unwrap();
        assert_eq!(after.task.title, "New title");
        assert_eq!(after.task.workflow.as_deref(), Some("app"));
        assert_eq!(after.body, "# Rewritten\n\ncontent\n");

        // Clearing the workflow back to the project default.
        store
            .set_fields(
                "a",
                IssueFields {
                    workflow: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.get("a").unwrap().unwrap().task.workflow, None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn delete_then_renumber_keeps_column_contiguous() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::todo())).unwrap();
        store.add(spec("b", Column::todo())).unwrap();
        store.add(spec("c", Column::todo())).unwrap();

        store.delete("b").unwrap();
        assert!(store.get("b").unwrap().is_none());
        store.renumber(&Column::todo()).unwrap();

        let col = store.list_in_status(&Column::todo()).unwrap();
        let ids: Vec<_> = col.iter().map(|tf| tf.task.id.clone()).collect();
        let prios: Vec<_> = col.iter().map(|tf| tf.task.priority).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert_eq!(prios, vec![0, 1]);

        // Delete is idempotent.
        store.delete("b").unwrap();

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn move_status_and_unassign_clears_owner() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::review())).unwrap();
        store
            .set_fields(
                "a",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();

        let mv = store
            .move_status_and_unassign("a", &Column::todo(), "workspace:stop")
            .unwrap()
            .expect("moved");
        assert_eq!(mv.to, Column::todo());
        let after = store.get("a").unwrap().unwrap().task;
        assert_eq!(after.column, Column::todo());
        assert_eq!(after.assigned_to, None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn park_review_sets_marker_and_clear_parked_removes_it() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::review())).unwrap();
        store
            .set_fields(
                "a",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();

        let was = store.park_review("a").unwrap();
        assert_eq!(was.as_deref(), Some("review-1"));
        assert!(crate::is_task_parked("p", "a").unwrap());
        // Parking clears the assignment.
        assert_eq!(store.get("a").unwrap().unwrap().task.assigned_to, None);

        store.clear_parked("a").unwrap();
        assert!(!crate::is_task_parked("p", "a").unwrap());
        // Clearing again is idempotent.
        store.clear_parked("a").unwrap();

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn reject_review_appends_feedback_moves_to_ready_and_unassigns() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::review())).unwrap();
        store
            .set_fields(
                "a",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();

        let mv = store
            .reject_review("a", &Column::todo(), "restore the handler", "2026-09-03")
            .unwrap()
            .expect("status changed");
        assert_eq!(mv.from, Column::review());
        assert_eq!(mv.to, Column::todo());

        let after = store.get("a").unwrap().unwrap();
        assert_eq!(after.task.column, Column::todo());
        assert_eq!(after.task.assigned_to, None);
        assert!(after.body.contains("Review feedback (rejected 2026-09-03)"));
        assert!(after.body.contains("restore the handler"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn comments_round_trip_and_order() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::todo())).unwrap();
        assert!(store.list_comments("a").unwrap().is_empty());

        let c1 = store.add_comment("a", "first").unwrap();
        let c2 = store.add_comment("a", "second").unwrap();
        assert_eq!(c1.id, "0001");
        assert_eq!(c2.id, "0002");

        let comments = store.list_comments("a").unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].body, "first");
        assert_eq!(comments[1].body, "second");

        // A comment on a missing issue is refused.
        assert!(store.add_comment("missing", "x").is_err());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn resolve_file_system_returns_a_working_store() {
        use shelbi_core::{GithubConnection, IssueTrackerConfig};

        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Default (and explicit file_system) resolves to a live store.
        let store = resolve_issue_store("p", &IssueTrackerConfig::default()).unwrap();
        store.add(spec("a", Column::todo())).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);

        // A staged github block alongside file_system does not block resolution.
        let cfg = IssueTrackerConfig {
            backend: IssueTrackerBackend::FileSystem,
            github: Some(GithubConnection {
                repo: "owner/repo".into(),
            }),
            ..Default::default()
        };
        assert!(resolve_issue_store("p", &cfg).is_ok());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn resolve_github_returns_a_live_store() {
        use shelbi_core::{GithubConnection, IssueTrackerConfig};

        // A valid github config now resolves to a live [`GitHubStore`] (the
        // write path landed) rather than a typed "unimplemented" error.
        let cfg = IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection {
                repo: "owner/repo".into(),
            }),
            ..Default::default()
        };
        // No network / gh call happens at construction — the store is a cheap
        // handle that resolves auth lazily per API call.
        assert!(resolve_issue_store("p", &cfg).is_ok());
    }

    #[test]
    fn resolve_jira_is_unimplemented_but_valid_config_is_typed() {
        use shelbi_core::{IssueTrackerConfig, JiraConnection};

        let cfg = IssueTrackerConfig {
            backend: IssueTrackerBackend::Jira,
            jira: Some(JiraConnection {
                project: "PROJ".into(),
            }),
            ..Default::default()
        };
        match resolve_issue_store("p", &cfg) {
            Err(Error::IssueTrackerUnimplemented(b)) => assert_eq!(b, "jira"),
            Err(other) => panic!("expected IssueTrackerUnimplemented, got {other:?}"),
            Ok(_) => panic!("expected an error, got a live store"),
        }
    }

    #[test]
    fn resolve_github_missing_repo_is_a_config_error_not_unimplemented() {
        use shelbi_core::IssueTrackerConfig;

        // Validation runs before the unimplemented check, so a malformed
        // github backend surfaces the field-named config error.
        let cfg = IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            ..Default::default()
        };
        match resolve_issue_store("p", &cfg) {
            Err(Error::InvalidIssueTracker(msg)) => {
                assert!(msg.contains("issue_tracker.github.repo"), "msg: {msg}")
            }
            Err(other) => panic!("expected InvalidIssueTracker, got {other:?}"),
            Ok(_) => panic!("expected an error, got a live store"),
        }
    }

    #[test]
    fn poll_changes_watermark_surfaces_later_edits_and_comments() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let store = FileSystemStore::new("p");

        store.add(spec("a", Column::todo())).unwrap();

        // First poll from `start` establishes the high-water mark and reports
        // nothing as changed.
        let (changes, cursor) = store.poll_changes(&Cursor::start()).unwrap();
        assert!(changes.is_empty());
        assert!(cursor.watermark().is_some());

        // A later field edit surfaces as an upsert.
        std::thread::sleep(std::time::Duration::from_millis(5));
        store
            .set_fields(
                "a",
                IssueFields {
                    branch: Some(Some("jlong/a".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let (changes, cursor) = store.poll_changes(&cursor).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], IssueChange::Upserted(_)));

        // A later comment surfaces as CommentAdded.
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.add_comment("a", "hello").unwrap();
        let (changes, _cursor) = store.poll_changes(&cursor).unwrap();
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            IssueChange::CommentAdded { issue_id, comment } => {
                assert_eq!(issue_id, "a");
                assert_eq!(comment.body, "hello");
            }
            other => panic!("expected CommentAdded, got {other:?}"),
        }

        std::env::remove_var("SHELBI_HOME");
    }
}
