//! The GitHub issues backend for the [`IssueStore`] seam.
//!
//! A project whose `issue_tracker.backend` is `github` keeps its board *as*
//! GitHub issues in a single repo (`owner/repo`). This module implements the
//! full contract — the read half (`list` / `list_in_status` / `get` /
//! `list_comments`, plus the `poll_changes` watermark) and the write half
//! (`add` / `move_status` / `set_priority` / `set_fields` / `cancel` /
//! `add_comment`) — by driving the GitHub REST API live through the `gh` CLI.
//!
//! ## Writing a shelbi mutation back onto a GitHub issue (plan §3/§5)
//!
//! The write path is the inverse of the read mapping:
//!
//! * **`add`** creates an issue carrying a `shelbi:id/<slug>` label, a
//!   `shelbi:status/<column>` label, and the fenced `<!-- shelbi:begin -->`
//!   metadata block (workflow / branch / depends_on / prefers_machine /
//!   priority / zen / launch / params). Creating into a terminal status closes
//!   the issue.
//! * **`move_status`** swaps the single `shelbi:status/*` label (keeping every
//!   other label) and opens / closes the issue when the target status is
//!   terminal — `done` closes as `completed`, `canceled` as `not_planned`, so
//!   the read path recovers which terminal even from GitHub state alone.
//! * **`set_fields` / `set_priority`** rewrite only the fenced metadata block in
//!   the issue body, never the human prose around it (the round-trip-safety
//!   contract). Priority is a plain integer in that block, renumbered
//!   client-side across a column exactly like the filesystem board.
//! * **`add_comment`** posts a native issue comment.
//!
//! Workspace assignment (`assigned_to`) is deliberately *not* written to GitHub
//! (plan §3): it is ephemeral local routing, not board state a github.com viewer
//! should see. Instead it is persisted to the **local assignment overlay**
//! (`crate::set_task_assignment` / `crate::get_task_assignment`, marker files
//! under `<project_dir>/assignments/`). [`GitHubStore::set_fields`] writes the
//! overlay for that one field; every read ([`GitHubStore::get`] /
//! [`GitHubStore::list`]) folds the overlay back onto the reconstructed issue,
//! so the active-workspace, conflict, and supervision scans still recover which
//! workspace owns a card even though the tracker stores no assignment. The
//! unassigning transitions ([`GitHubStore::move_status_and_unassign`] /
//! [`GitHubStore::cancel`] / [`GitHubStore::park_review`]) clear the overlay.
//!
//! ## Label hygiene — auto-create on first use (plan §6)
//!
//! Every write that applies a `shelbi:status/*` (or `shelbi:id/*`) label first
//! ensures the label exists in the repo, creating any that are missing. The
//! creation is idempotent: a label that already exists is left untouched
//! (GitHub's "already_exists" is swallowed), so first-use bootstrap and steady
//! state both work without a separate provisioning step.
//!
//! ## No content cache — live reads only (plan D3)
//!
//! Deliberately, nothing is mirrored to disk. Every read hits the API through
//! `gh`; when the tracker is unreachable the call surfaces a clear
//! [`Error::Command`] rather than rendering stale data. The only cross-call
//! state is the [`Cursor`] watermark threaded through [`poll_changes`] for
//! change detection — a last-seen `updated_at`, not a copy of any issue's
//! content. The caller (the orchestrator heartbeat) owns and persists that
//! cursor; the store holds no board state of its own.
//!
//! ## Mapping a GitHub issue onto an [`Issue`] (plan §3)
//!
//! One shelbi issue ⇔ one GitHub issue:
//!
//! * **id** — the `shelbi:id/<slug>` label (the round-trip anchor). An issue
//!   with no such label falls back to its issue number as the id, so a repo
//!   that has not been migrated still renders.
//! * **status** — the `shelbi:status/<id>` label. A *closed* issue always maps
//!   to a terminal status (`done`, or `canceled` when GitHub's `state_reason`
//!   is `not_planned`), so closing an issue on github.com reads as a terminal
//!   card regardless of a stale non-terminal label.
//! * **shelbi-only fields** (`workflow` / `branch` / `depends_on` /
//!   `prefers_machine` / `priority` / `zen` / `launch`) — parsed from the fenced
//!   `<!-- shelbi:begin -->` … `<!-- shelbi:end -->` YAML block in the issue
//!   body. The block is stripped from [`IssueFile::body`] so the human prose and
//!   the shelbi metadata never clobber each other.
//! * **assignment** (`assigned_to`) — never read from GitHub. The raw mapping
//!   ([`GhIssue::into_issue_file`]) always yields `None`; the [`IssueStore`]
//!   read methods then fold in the local assignment overlay (see above), so a
//!   consumer sees the owning workspace without the tracker storing it.
//!
//! ## Testability
//!
//! The `gh` invocation is a single injectable closure ([`GhRunner`]). The
//! production constructor ([`GitHubStore::new`]) builds a runner that resolves
//! auth via [`crate::resolve_github_token`] and shells out to `gh api`; unit
//! tests inject a closure that returns canned JSON, so every mapping rule is
//! exercised without a network or a real repo.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::{
    Column, Error, IssueLaunchConfig, IssueZenConfig, Result, DEFAULT_WORKFLOW_NAME,
};

use crate::issue_store::{Cursor, IssueChange, IssueComment, IssueFields, IssueStore, NewIssue, PrioMove, StatusMove};
use crate::{resolve_github_token_by_name, IssueFile};
use shelbi_core::Issue;

/// Label prefix carrying an issue's stable shelbi id (`shelbi:id/<slug>`).
const ID_LABEL_PREFIX: &str = "shelbi:id/";
/// Label prefix carrying an issue's shelbi status (`shelbi:status/<id>`).
const STATUS_LABEL_PREFIX: &str = "shelbi:status/";
/// Opening marker of the fenced shelbi-metadata block in an issue body.
const META_BEGIN: &str = "<!-- shelbi:begin -->";
/// Closing marker of the fenced shelbi-metadata block in an issue body.
const META_END: &str = "<!-- shelbi:end -->";

/// A `gh` invocation: given the args that follow `gh`, yield stdout on success
/// or a typed [`Error`] on failure. Boxed so the production runner (real `gh`
/// with resolved auth) and a test runner (canned JSON) are interchangeable.
type GhRunner = Arc<dyn Fn(&[&str]) -> Result<String> + Send + Sync>;

/// The GitHub issues backend. A cheap handle: it holds the `owner/repo`
/// selector and the `gh` runner closure, and resolves everything else per call.
#[derive(Clone)]
pub struct GitHubStore {
    /// The project *name* (registry alias / on-disk dir). GitHub is the source
    /// of truth for board state, but the review-slot parked markers are local
    /// daemon state under `<project_dir>/parked-review/`, so park/clear still
    /// need the project name to reach them.
    project: String,
    repo: String,
    gh: GhRunner,
}

impl std::fmt::Debug for GitHubStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubStore")
            .field("repo", &self.repo)
            .finish_non_exhaustive()
    }
}

impl GitHubStore {
    /// A store bound to `repo` (`owner/repo`) for the project named `project`,
    /// using the real `gh` CLI. Auth is resolved lazily per call via
    /// [`crate::resolve_github_token_by_name`] and handed to the `gh` subprocess
    /// as `GH_TOKEN`, so all three token sources (env / keychain / out-of-repo
    /// `tokens.yml`) funnel through one place. The project *name* is enough to
    /// locate the out-of-repo `tokens.yml`, so the store never has to hold (or
    /// re-load) a full `Project`. Nothing secret is written down.
    pub fn new(project: impl Into<String>, repo: impl Into<String>) -> Self {
        let project = project.into();
        let repo = repo.into();
        let project_for_gh = project.clone();
        let gh: GhRunner = Arc::new(move |args: &[&str]| run_gh(&project_for_gh, args));
        Self { project, repo, gh }
    }

    /// Construct a store over an arbitrary `gh` runner. The production seam for
    /// tests: inject a closure returning canned JSON instead of shelling out.
    #[cfg(test)]
    fn with_runner(
        repo: impl Into<String>,
        runner: impl Fn(&[&str]) -> Result<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            project: "test-project".to_string(),
            repo: repo.into(),
            gh: Arc::new(runner),
        }
    }

    /// The `owner/repo` this store reads.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Run `gh api` for a REST endpoint under this repo, streaming each element
    /// of the returned JSON array as one object per line (`--jq '.[]'`, with
    /// `--paginate` following `Link` headers). Returns the parsed issue objects.
    /// `extra` carries additional `-f key=value` query params (e.g. a label
    /// filter or a `since=` watermark).
    fn api_issues(&self, path: &str, extra: &[&str]) -> Result<Vec<GhIssue>> {
        let mut args: Vec<&str> = vec!["api", "-X", "GET", path, "--paginate"];
        args.extend_from_slice(extra);
        // One JSON object per line, so paginated arrays never concatenate into
        // invalid JSON — parse line by line.
        args.extend_from_slice(&["--jq", ".[]"]);
        let out = (self.gh)(&args)?;
        parse_jsonl(&out)
    }
}

impl IssueStore for GitHubStore {
    fn list(&self) -> Result<Vec<IssueFile>> {
        let path = format!("repos/{}/issues", self.repo);
        let issues = self.api_issues(&path, &["-f", "state=all", "-f", "per_page=100"])?;
        // Fold the local assignment overlay onto every issue in one read, rather
        // than statting a marker file per card.
        let assignments = crate::task_assignments(&self.project)?;
        let mut out: Vec<IssueFile> = issues
            .into_iter()
            .filter(|gh| !gh.is_pull_request())
            .map(|gh| {
                let mut tf = gh.into_issue_file();
                tf.task.assigned_to = assignments.get(&tf.task.id).cloned();
                tf
            })
            .collect();
        sort_board(&mut out);
        Ok(out)
    }

    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
        // Client-side filter of the full board: the status lives in a label we
        // already parse, and this keeps a single mapping path (no second query
        // shape to keep in sync). `list` has already applied the assignment
        // overlay, so the conflict/active-workspace scans see the owner.
        Ok(self
            .list()?
            .into_iter()
            .filter(|tf| tf.task.column == *status)
            .collect())
    }

    fn get(&self, id: &str) -> Result<Option<IssueFile>> {
        shelbi_core::validate_task_id(id)?;
        let path = format!("repos/{}/issues", self.repo);
        let label = format!("labels={ID_LABEL_PREFIX}{id}");
        let issues = self.api_issues(
            &path,
            &["-f", "state=all", "-f", &label, "-f", "per_page=100"],
        )?;
        let Some(gh) = issues.into_iter().find(|gh| !gh.is_pull_request()) else {
            return Ok(None);
        };
        let mut tf = gh.into_issue_file();
        // Fold in the local assignment overlay so a caller reads the owning
        // workspace even though the tracker stores no assignment.
        tf.task.assigned_to = crate::get_task_assignment(&self.project, id)?;
        Ok(Some(tf))
    }

    fn add(&self, spec: NewIssue) -> Result<Issue> {
        shelbi_core::validate_task_id(&spec.id)?;
        // A card that already exists (same shelbi id) must not be silently
        // duplicated into a second issue — `add` is create-exclusive, matching
        // the filesystem backend's no-overwrite guarantee.
        if self.get_raw(&spec.id)?.is_some() {
            return Err(Error::Other(format!(
                "issue `{}` already exists in {}",
                spec.id, self.repo
            )));
        }
        // Priority: an explicit slot wins; otherwise append to the destination
        // status (priority = current count), the same rule the filesystem
        // backend uses. Priority is a plain int in the metadata block.
        let priority = match spec.priority {
            Some(p) => p,
            None => self.list_in_status(&spec.column)?.len() as u32,
        };

        let id_label = format!("{ID_LABEL_PREFIX}{}", spec.id);
        let status_label = status_label_name(&spec.column);
        // Auto-create every label this write applies (id + the status set) so a
        // fresh repo bootstraps without a separate provisioning step.
        self.ensure_labels(&self.bootstrap_labels(&[id_label.clone(), status_label.clone()]))?;

        let meta = meta_from_new(&spec, priority);
        let body = build_body(&spec.body, &meta);

        let fields = vec![
            ("title", spec.title.clone()),
            ("body", body),
            ("labels[]", id_label),
            ("labels[]", status_label),
        ];
        let out = self.api_send("POST", &format!("repos/{}/issues", self.repo), &fields)?;
        let created: GhIssue = parse_json_object(&out)?;

        // Creating straight into a terminal status closes the issue, so the
        // board and GitHub agree the moment the card exists.
        if is_terminal(&spec.column) {
            self.set_state(created.number, "closed", terminal_reason(&spec.column))?;
        }

        Ok(Issue {
            id: spec.id,
            title: spec.title,
            column: spec.column,
            priority,
            // Assignment is ephemeral local routing, never stored on GitHub.
            assigned_to: None,
            workflow: spec.workflow,
            branch: spec.branch,
            depends_on: spec.depends_on,
            prefers_machine: spec.prefers_machine,
            zen: spec.zen,
            launch: spec.launch,
            created_at: created.created_at,
            updated_at: created.updated_at,
            params: spec.params,
        })
    }

    fn move_status(&self, id: &str, to: &Column, _reason: &str) -> Result<Option<StatusMove>> {
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let from = gh.column();
        if from == *to {
            // Already there — no label swap, no event. Mirrors the filesystem
            // backend returning `None` for a no-op move.
            return Ok(None);
        }
        let workflow = gh.workflow_name();

        // Ensure the destination status label exists before applying it.
        let to_label = status_label_name(to);
        self.ensure_labels(std::slice::from_ref(&to_label))?;

        // Swap the status label: keep every non-status label (the id anchor and
        // any human-added labels), replace the single `shelbi:status/*` one.
        let mut labels = gh.non_status_labels();
        labels.push(to_label);
        self.set_labels(gh.number, &labels)?;

        // Terminal target closes the issue (recording which terminal via
        // `state_reason`); a non-terminal target reopens a closed issue so a
        // reopened card leaves the terminal lane.
        if is_terminal(to) {
            self.set_state(gh.number, "closed", terminal_reason(to))?;
        } else if gh.state == "closed" {
            self.set_state(gh.number, "open", None)?;
        }

        Ok(Some(StatusMove {
            from,
            to: to.clone(),
            workflow,
        }))
    }

    fn set_priority(&self, id: &str, pos: PrioMove) -> Result<()> {
        let Some(target) = self.get(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        // Reorder within the issue's current status, then renumber the column to
        // contiguous 0..N — the same client-side ordering the read path sorts
        // by and the filesystem backend maintains on disk.
        let mut column = self.list_in_status(&target.task.column)?;
        let Some(idx) = column.iter().position(|tf| tf.task.id == id) else {
            return Err(Error::Other(format!("issue `{id}` not in its own status?")));
        };
        let last = column.len().saturating_sub(1);
        let dest = match pos {
            PrioMove::Top => 0,
            PrioMove::Bottom => last,
            PrioMove::Up => idx.saturating_sub(1),
            PrioMove::Down => (idx + 1).min(last),
            PrioMove::Set(n) => (n as usize).min(last),
        };
        if dest == idx {
            return Ok(());
        }
        let moved = column.remove(idx);
        column.insert(dest, moved);
        // Only rewrite the issues whose integer priority actually changed.
        for (new_prio, tf) in column.iter().enumerate() {
            if tf.task.priority != new_prio as u32 {
                self.rewrite_priority(&tf.task.id, new_prio as u32)?;
            }
        }
        Ok(())
    }

    fn set_fields(&self, id: &str, fields: IssueFields) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        // `assigned_to` is ephemeral local routing (plan §3) — never stored on
        // GitHub. It is persisted to the local assignment overlay instead, so an
        // assign/start/resume through this seam is recoverable by the daemon's
        // ownership scans. Apply it first so an `assigned_to`-only update still
        // lands (it just touches nothing on the remote).
        if let Some(assigned_to) = &fields.assigned_to {
            crate::set_task_assignment(&self.project, id, assigned_to.as_deref())?;
        }
        if fields.branch.is_none()
            && fields.depends_on.is_none()
            && fields.prefers_machine.is_none()
            && fields.title.is_none()
            && fields.workflow.is_none()
            && fields.body.is_none()
        {
            // `assigned_to` was the only field — the overlay write above is the
            // whole operation; nothing to PATCH on the remote.
            return Ok(());
        }
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let body_raw = gh.body.clone().unwrap_or_default();
        let (mut prose, mut meta) = split_shelbi_meta(&body_raw);
        // The GitHub issue title and the shelbi body prose are stored natively;
        // branch / depends_on / prefers_machine / workflow live in the meta
        // block folded into the issue body. Accumulate a single PATCH.
        let mut params: Vec<(&str, String)> = Vec::new();
        let mut body_changed = false;
        if let Some(branch) = fields.branch {
            if meta.branch != branch {
                meta.branch = branch;
                body_changed = true;
            }
        }
        if let Some(depends_on) = fields.depends_on {
            if meta.depends_on != depends_on {
                meta.depends_on = depends_on;
                body_changed = true;
            }
        }
        if let Some(prefers_machine) = fields.prefers_machine {
            if meta.prefers_machine != prefers_machine {
                meta.prefers_machine = prefers_machine;
                body_changed = true;
            }
        }
        if let Some(workflow) = fields.workflow {
            if meta.workflow != workflow {
                meta.workflow = workflow;
                body_changed = true;
            }
        }
        if let Some(new_prose) = fields.body {
            if prose != new_prose {
                prose = new_prose;
                body_changed = true;
            }
        }
        if let Some(title) = fields.title {
            if gh.title != title {
                params.push(("title", title));
            }
        }
        if body_changed {
            params.push(("body", build_body(&prose, &meta)));
        }
        if params.is_empty() {
            return Ok(());
        }
        self.api_send(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repo, gh.number),
            &params,
        )?;
        Ok(())
    }

    fn cancel(&self, id: &str, reason: &str) -> Result<Option<StatusMove>> {
        // Cancel = move-and-unassign to the terminal `canceled` status (closes
        // the issue as `not_planned`), clearing the local assignment overlay so a
        // canceled card isn't relaunched on its old workspace — the same
        // reasoning as the filesystem backend.
        self.move_status_and_unassign(id, &Column::canceled(), reason)
    }

    fn move_status_and_unassign(
        &self,
        id: &str,
        to: &Column,
        reason: &str,
    ) -> Result<Option<StatusMove>> {
        let mv = self.move_status(id, to, reason)?;
        // Drop the local assignment overlay regardless of whether the status
        // actually changed (matching the filesystem backend's move-and-unassign,
        // which clears the owner even on a no-op move).
        crate::set_task_assignment(&self.project, id, None)?;
        Ok(mv)
    }

    fn delete(&self, id: &str) -> Result<()> {
        // GitHub's REST API cannot hard-delete an issue, so `rm` maps to the
        // nearest durable effect: close it as `not_planned` (the same terminal
        // state a cancel reaches). Idempotent — a missing/closed issue is a
        // no-op. GraphQL `deleteIssue` exists but needs elevated repo scope, so
        // the REST close is the portable choice for the experimental backend.
        let Some(gh) = self.get_raw(id)? else {
            // Still drop any stale local assignment for a card GitHub no longer
            // knows about, so the overlay doesn't leak owners.
            crate::set_task_assignment(&self.project, id, None)?;
            return Ok(());
        };
        if gh.state != "closed" {
            self.set_state(gh.number, "closed", Some("not_planned"))?;
        }
        // A deleted card has no owner — clear the overlay.
        crate::set_task_assignment(&self.project, id, None)?;
        Ok(())
    }

    fn renumber(&self, status: &Column) -> Result<()> {
        // Rewrite the column's stored priorities to contiguous 0..N — the same
        // repair the filesystem backend does, expressed as the client-side
        // reorder GitHub priorities are maintained by.
        let column = self.list_in_status(status)?;
        for (idx, tf) in column.iter().enumerate() {
            if tf.task.priority != idx as u32 {
                self.rewrite_priority(&tf.task.id, idx as u32)?;
            }
        }
        Ok(())
    }

    fn park_review(&self, id: &str) -> Result<Option<String>> {
        // Parked markers are local daemon state for the review-slot auto-loader.
        // The prior owner lives in the local assignment overlay (GitHub stores
        // none), so report and clear it, then set the parked marker — mirroring
        // the filesystem backend's park (clear owner + mark parked in one step).
        let was = crate::get_task_assignment(&self.project, id)?;
        crate::set_task_assignment(&self.project, id, None)?;
        crate::set_task_parked(&self.project, id)?;
        Ok(was)
    }

    fn clear_parked(&self, id: &str) -> Result<()> {
        crate::clear_task_parked(&self.project, id)
    }

    fn poll_changes(&self, since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
        // Live read + watermark, no content cache (plan D3). The first poll
        // from `Cursor::start()` just establishes the high-water `updated_at`
        // and reports nothing; later edits (issue `updated_at` moved past the
        // cursor) surface as upserts, and comments on those same freshly-touched
        // issues (created past the cursor) surface as CommentAdded.
        let path = format!("repos/{}/issues", self.repo);
        // On a warm cursor, scope the list to issues touched at/after the
        // watermark (`since=`, plan D3 — "GitHub supports this cheaply"). On a
        // quiescent board this transfers next to nothing, so the pass stays well
        // inside the `gh` rate budget instead of re-listing the whole repo every
        // tick. The cold first poll has no watermark and lists the board once to
        // seed the high-water mark. `since=` is *inclusive* (updated_at >= w),
        // but the strictly-greater filter below drops an issue sitting exactly at
        // the watermark, so a change is never surfaced twice across polls.
        let since_param = since.watermark().map(|w| format!("since={}", w.to_rfc3339()));
        let mut extra: Vec<&str> = vec!["-f", "state=all", "-f", "per_page=100"];
        if let Some(param) = since_param.as_deref() {
            extra.push("-f");
            extra.push(param);
        }
        let issues = self.api_issues(&path, &extra)?;

        let mut changes = Vec::new();
        let mut high = since.watermark();
        let mut touched: Vec<(String, i64)> = Vec::new();

        for gh in issues {
            if gh.is_pull_request() {
                continue;
            }
            let updated = gh.updated_at;
            high = Some(high.map_or(updated, |h| h.max(updated)));
            if since.watermark().is_some_and(|w| updated > w) {
                let number = gh.number;
                let tf = gh.into_issue_file();
                touched.push((tf.task.id.clone(), number));
                changes.push(IssueChange::Upserted(Box::new(tf)));
            }
        }

        // Comment detection is scoped to issues whose `updated_at` advanced —
        // adding a comment bumps the issue's `updated_at`, so a new comment can
        // only be on one of those. Only meaningful once past `start`.
        if let Some(w) = since.watermark() {
            for (issue_id, number) in touched {
                // Scope the per-issue comment fetch with the same `since=`
                // watermark so we only pull comments GitHub touched since the
                // last poll (plan D3 "comments `since=`"), then keep only the
                // ones genuinely *created* past the cursor — an edit to a
                // pre-cursor comment is returned by `since=` but is not a new
                // comment, so the `created_at > w` filter drops it.
                for comment in self.comments_for_number_since(number, Some(w))? {
                    if comment.created_at > w {
                        high = Some(high.map_or(comment.created_at, |h| h.max(comment.created_at)));
                        changes.push(IssueChange::CommentAdded {
                            issue_id: issue_id.clone(),
                            comment,
                        });
                    }
                }
            }
        }

        Ok((changes, high.map_or_else(Cursor::start, Cursor::at)))
    }

    fn list_comments(&self, id: &str) -> Result<Vec<IssueComment>> {
        // Resolve the shelbi id to an issue number, then read its comments live.
        let Some(gh) = self.get_raw(id)? else {
            return Ok(Vec::new());
        };
        self.comments_for_number(gh.number)
    }

    fn add_comment(&self, id: &str, body: &str) -> Result<IssueComment> {
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let out = self.api_send(
            "POST",
            &format!("repos/{}/issues/{}/comments", self.repo, gh.number),
            &[("body", body.to_string())],
        )?;
        let created: GhComment = parse_json_object(&out)?;
        Ok(created.into_comment())
    }
}

impl GitHubStore {
    /// Resolve a shelbi id to the raw GitHub issue (number + fields), or `None`.
    fn get_raw(&self, id: &str) -> Result<Option<GhIssue>> {
        shelbi_core::validate_task_id(id)?;
        let path = format!("repos/{}/issues", self.repo);
        let label = format!("labels={ID_LABEL_PREFIX}{id}");
        let issues = self.api_issues(
            &path,
            &["-f", "state=all", "-f", &label, "-f", "per_page=100"],
        )?;
        Ok(issues.into_iter().find(|gh| !gh.is_pull_request()))
    }

    /// Live-read the comments on a GitHub issue by its number, oldest first.
    fn comments_for_number(&self, number: i64) -> Result<Vec<IssueComment>> {
        self.comments_for_number_since(number, None)
    }

    /// Like [`GitHubStore::comments_for_number`] but, when `since` is set, scopes
    /// the fetch to comments GitHub touched at/after that watermark (`since=`).
    /// The change-detection path passes the poll watermark so a quiescent issue
    /// transfers no comment bodies; `list_comments` passes `None` for the full
    /// history. `since=` filters on the comment's `updated_at`, so an edited
    /// pre-watermark comment can still come back — the caller keeps only those
    /// whose `created_at` is genuinely past the cursor.
    fn comments_for_number_since(
        &self,
        number: i64,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<IssueComment>> {
        let path = format!("repos/{}/issues/{number}/comments", self.repo);
        let since_param = since.map(|w| format!("since={}", w.to_rfc3339()));
        let mut args: Vec<&str> = vec!["api", "-X", "GET", &path, "--paginate"];
        args.extend_from_slice(&["-f", "per_page=100"]);
        if let Some(param) = since_param.as_deref() {
            args.push("-f");
            args.push(param);
        }
        args.extend_from_slice(&["--jq", ".[]"]);
        let out = (self.gh)(&args)?;
        let raw: Vec<GhComment> = parse_jsonl(&out)?;
        let mut comments: Vec<IssueComment> = raw.into_iter().map(GhComment::into_comment).collect();
        // The API returns comments in creation order already; sort defensively
        // on the id so the ordering contract holds regardless.
        comments.sort_by_key(|c| c.created_at);
        Ok(comments)
    }

    // --- write helpers -------------------------------------------------------

    /// Run a mutating `gh api` call (`POST` / `PATCH` / `PUT`) with a set of
    /// `-f key=value` form fields, returning the raw response body. Repeated
    /// keys (e.g. `labels[]`) build an array, which is how GitHub expects label
    /// lists. Every value is passed as a distinct argv entry, so newlines and
    /// shell metacharacters in a title / body / comment are never interpreted.
    fn api_send(&self, method: &str, path: &str, fields: &[(&str, String)]) -> Result<String> {
        let mut args: Vec<String> = vec!["api".into(), "-X".into(), method.into(), path.into()];
        for (k, v) in fields {
            args.push("-f".into());
            args.push(format!("{k}={v}"));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        (self.gh)(&refs)
    }

    /// Replace an issue's entire label set (`PUT .../labels`). The caller
    /// computes the full desired set — keeping the id anchor and any human
    /// labels — so this is the atomic "these are the labels now" primitive the
    /// status swap builds on.
    fn set_labels(&self, number: i64, labels: &[String]) -> Result<()> {
        let fields: Vec<(&str, String)> =
            labels.iter().map(|l| ("labels[]", l.clone())).collect();
        self.api_send(
            "PUT",
            &format!("repos/{}/issues/{number}/labels", self.repo),
            &fields,
        )?;
        Ok(())
    }

    /// Open or close an issue, optionally recording a close `state_reason`
    /// (`completed` for `done`, `not_planned` for `canceled`).
    fn set_state(&self, number: i64, state: &str, reason: Option<&str>) -> Result<()> {
        let mut fields = vec![("state", state.to_string())];
        if let Some(r) = reason {
            fields.push(("state_reason", r.to_string()));
        }
        self.api_send(
            "PATCH",
            &format!("repos/{}/issues/{number}", self.repo),
            &fields,
        )?;
        Ok(())
    }

    /// Rewrite a single issue's stored priority integer inside its fenced
    /// metadata block, leaving the human prose (and every other metadata field)
    /// untouched. Reads the live body so a concurrent human prose edit is
    /// preserved.
    fn rewrite_priority(&self, id: &str, priority: u32) -> Result<()> {
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let body_raw = gh.body.clone().unwrap_or_default();
        let (prose, mut meta) = split_shelbi_meta(&body_raw);
        meta.priority = Some(priority);
        let body = build_body(&prose, &meta);
        self.api_send(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repo, gh.number),
            &[("body", body)],
        )?;
        Ok(())
    }

    /// The full label set to ensure on first use: the caller's specific labels
    /// (id + the status being applied) plus the stock `shelbi:status/*` set, so
    /// a fresh repo lands every status label the board can move a card into.
    fn bootstrap_labels(&self, specific: &[String]) -> Vec<String> {
        let mut out: Vec<String> = specific.to_vec();
        for col in Column::core() {
            out.push(status_label_name(&col));
        }
        out
    }

    /// Ensure every named label exists in the repo, creating the missing ones.
    /// Idempotent: existing labels are left as-is (one list call snapshots the
    /// repo, and [`GitHubStore::create_label`] additionally swallows GitHub's
    /// "already_exists" so a label that raced into existence is not an error).
    fn ensure_labels(&self, names: &[String]) -> Result<()> {
        let existing = self.list_label_names()?;
        let mut seen: std::collections::HashSet<&str> =
            existing.iter().map(String::as_str).collect();
        for name in names {
            if seen.insert(name.as_str()) {
                self.create_label(name)?;
            }
        }
        Ok(())
    }

    /// Every label name defined in the repo.
    fn list_label_names(&self) -> Result<Vec<String>> {
        let path = format!("repos/{}/labels", self.repo);
        let args = ["api", "-X", "GET", &path, "--paginate", "--jq", ".[]"];
        let out = (self.gh)(&args)?;
        let labels: Vec<GhLabel> = parse_jsonl(&out)?;
        Ok(labels.into_iter().map(|l| l.name).collect())
    }

    /// Create one repo label, tolerating a concurrent creation: GitHub answers a
    /// duplicate `POST /labels` with `422 already_exists`, which we treat as
    /// success so label bootstrap stays idempotent even against a stale list.
    fn create_label(&self, name: &str) -> Result<()> {
        let fields = vec![
            ("name", name.to_string()),
            ("color", label_color(name).to_string()),
            ("description", "Managed by shelbi".to_string()),
        ];
        match self.api_send("POST", &format!("repos/{}/labels", self.repo), &fields) {
            Ok(_) => Ok(()),
            Err(Error::Command { stderr, .. }) if stderr.contains("already_exists") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Run the real `gh` CLI with resolved auth. The token is resolved through the
/// full chain (env → `gh` keychain → out-of-repo `tokens.yml`) and handed to
/// the child as `GH_TOKEN`; a failure to resolve surfaces the actionable
/// [`Error::MissingIssueTrackerAuth`]. A non-zero exit (network down, repo not
/// found, not authed) becomes an [`Error::Command`] — never a stale render.
fn run_gh(project: &str, args: &[&str]) -> Result<String> {
    let token = resolve_github_token_by_name(project)?;
    let output = std::process::Command::new("gh")
        .args(args)
        .env("GH_TOKEN", token.expose())
        .output()
        .map_err(|e| Error::Command {
            cmd: format!("gh {}", args.join(" ")),
            status: "failed to spawn".to_string(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(Error::Command {
            cmd: format!("gh {}", args.join(" ")),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse a `--jq '.[]'` stream (one JSON object per line) into a vec, skipping
/// blank lines. A malformed line is a hard error — a partial read must never be
/// silently rendered as a truncated board.
fn parse_jsonl<T: for<'de> Deserialize<'de>>(text: &str) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .map_err(|e| Error::Other(format!("gh returned unparseable JSON: {e}")))?,
        );
    }
    Ok(out)
}

/// Canonical board order: column order first, then priority, then id — the same
/// ordering [`crate::list_tasks`] gives the filesystem board.
fn sort_board(issues: &mut [IssueFile]) {
    issues.sort_by(|a, b| {
        a.task
            .column
            .board_order()
            .cmp(&b.task.column.board_order())
            .then(a.task.priority.cmp(&b.task.priority))
            .then(a.task.id.cmp(&b.task.id))
    });
}

// --- GitHub REST wire shapes -------------------------------------------------

/// One issue object from `GET /repos/{owner}/{repo}/issues`. Only the fields
/// the mapping needs are named; the rest are ignored.
#[derive(Debug, Deserialize)]
struct GhIssue {
    number: i64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    /// `"open"` | `"closed"`.
    state: String,
    /// `"completed"` | `"not_planned"` | null — only set when closed.
    #[serde(default)]
    state_reason: Option<String>,
    #[serde(default)]
    labels: Vec<GhLabel>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// Present only on pull requests, which the issues endpoint also returns;
    /// its presence is how we tell a PR apart from an issue.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

impl GhIssue {
    /// True when this "issue" is really a pull request (the issues endpoint
    /// returns both; a PR carries a `pull_request` object).
    fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }

    /// The stable shelbi id: the `shelbi:id/<slug>` label if present, else the
    /// issue number as a string (so an un-migrated repo still renders).
    fn shelbi_id(&self) -> String {
        self.labels
            .iter()
            .find_map(|l| l.name.strip_prefix(ID_LABEL_PREFIX))
            .map(str::to_string)
            .unwrap_or_else(|| self.number.to_string())
    }

    /// The `shelbi:status/<id>` label value, if any.
    fn status_label(&self) -> Option<&str> {
        self.labels
            .iter()
            .find_map(|l| l.name.strip_prefix(STATUS_LABEL_PREFIX))
    }

    /// Every label name that is NOT a `shelbi:status/*` label — the set to keep
    /// when swapping the status label on a move.
    fn non_status_labels(&self) -> Vec<String> {
        self.labels
            .iter()
            .filter(|l| !l.name.starts_with(STATUS_LABEL_PREFIX))
            .map(|l| l.name.clone())
            .collect()
    }

    /// The workflow this issue runs under, parsed from its metadata block, or
    /// the canonical default when unset — the value a [`StatusMove`] carries.
    fn workflow_name(&self) -> String {
        let body = self.body.clone().unwrap_or_default();
        split_shelbi_meta(&body)
            .1
            .workflow
            .unwrap_or_else(|| DEFAULT_WORKFLOW_NAME.to_string())
    }

    /// Map GitHub state + labels onto a shelbi [`Column`]. A closed issue is
    /// always terminal; an open issue takes its status label, defaulting to
    /// `todo` when unlabeled.
    fn column(&self) -> Column {
        let closed = self.state == "closed";
        match self.status_label() {
            Some(id) => {
                let col = Column::from_status_id(id);
                if closed && !is_terminal(&col) {
                    // A closed issue overrides a stale non-terminal label so it
                    // can never render in an active lane.
                    terminal_from_reason(self.state_reason.as_deref())
                } else {
                    col
                }
            }
            None if closed => terminal_from_reason(self.state_reason.as_deref()),
            None => Column::todo(),
        }
    }

    /// Full mapping onto an [`IssueFile`]: native fields plus the parsed fenced
    /// metadata block, with that block stripped from the body prose.
    fn into_issue_file(self) -> IssueFile {
        let body_raw = self.body.clone().unwrap_or_default();
        let (prose, meta) = split_shelbi_meta(&body_raw);
        let column = self.column();
        let id = self.shelbi_id();

        let task = Issue {
            id,
            title: self.title,
            column,
            priority: meta.priority.unwrap_or(0),
            // Workspace routing is ephemeral local state, never read from GitHub.
            assigned_to: None,
            workflow: meta.workflow,
            branch: meta.branch,
            depends_on: meta.depends_on,
            prefers_machine: meta.prefers_machine,
            zen: meta.zen,
            launch: meta.launch,
            created_at: self.created_at,
            updated_at: self.updated_at,
            params: meta.params,
        };
        IssueFile { task, body: prose }
    }
}

/// One comment object from `GET /repos/{owner}/{repo}/issues/{n}/comments`.
#[derive(Debug, Deserialize)]
struct GhComment {
    id: i64,
    #[serde(default)]
    body: Option<String>,
    created_at: DateTime<Utc>,
    #[serde(default)]
    user: Option<GhUser>,
}

#[derive(Debug, Deserialize)]
struct GhUser {
    login: String,
}

impl GhComment {
    fn into_comment(self) -> IssueComment {
        IssueComment {
            id: self.id.to_string(),
            author: self.user.map(|u| u.login),
            created_at: self.created_at,
            body: self.body.unwrap_or_default(),
        }
    }
}

/// True for the terminal columns (`done` / `canceled`).
fn is_terminal(col: &Column) -> bool {
    *col == Column::done() || *col == Column::canceled()
}

/// The terminal column implied by a closed issue's `state_reason`:
/// `not_planned` → `canceled`, everything else → `done`.
fn terminal_from_reason(reason: Option<&str>) -> Column {
    match reason {
        Some("not_planned") => Column::canceled(),
        _ => Column::done(),
    }
}

/// The GitHub `state_reason` to close an issue with for a terminal target:
/// `canceled` → `not_planned`, `done` → `completed`. `None` for a non-terminal
/// column (which never closes the issue). Round-trips with
/// [`terminal_from_reason`] so a closed issue reads back to the same terminal.
fn terminal_reason(col: &Column) -> Option<&'static str> {
    if *col == Column::canceled() {
        Some("not_planned")
    } else if *col == Column::done() {
        Some("completed")
    } else {
        None
    }
}

/// The full `shelbi:status/<id>` label name for a column.
fn status_label_name(col: &Column) -> String {
    format!("{STATUS_LABEL_PREFIX}{}", col.as_str())
}

/// A stable label colour (6 hex digits, no `#`) for a managed shelbi label. The
/// stock status labels get distinct hues so the GitHub label list reads as a
/// board; the id anchor and any custom status share a neutral grey. Cosmetic
/// only — nothing keys off the colour.
fn label_color(name: &str) -> &'static str {
    match name {
        "shelbi:status/backlog" => "c5def5",
        "shelbi:status/todo" => "1d76db",
        "shelbi:status/in-progress" => "fbca04",
        "shelbi:status/review" => "d93f0b",
        "shelbi:status/done" => "0e8a16",
        "shelbi:status/canceled" => "6a737d",
        _ => "ededed",
    }
}

/// Build a [`ShelbiMeta`] from a creation spec, stamping the resolved priority.
fn meta_from_new(spec: &NewIssue, priority: u32) -> ShelbiMeta {
    ShelbiMeta {
        workflow: spec.workflow.clone(),
        branch: spec.branch.clone(),
        depends_on: spec.depends_on.clone(),
        prefers_machine: spec.prefers_machine.clone(),
        priority: Some(priority),
        zen: spec.zen.clone(),
        launch: spec.launch.clone(),
        params: spec.params.clone(),
    }
}

/// Compose an issue body from human `prose` and shelbi `meta`, emitting the
/// fenced `<!-- shelbi:begin -->` … `<!-- shelbi:end -->` block that
/// [`split_shelbi_meta`] reads back. The inverse of that split, so a
/// read-modify-write round-trips: prose is preserved verbatim (trimmed), and an
/// all-empty `meta` emits no block at all rather than an empty fence.
fn build_body(prose: &str, meta: &ShelbiMeta) -> String {
    let prose = prose.trim();
    let yaml = serde_yaml::to_string(meta).unwrap_or_default();
    let yaml = yaml.trim();
    // serde_yaml renders a struct with every field skipped as `{}`.
    if yaml.is_empty() || yaml == "{}" {
        return if prose.is_empty() {
            String::new()
        } else {
            format!("{prose}\n")
        };
    }
    let block = format!("{META_BEGIN}\n```yaml\n{yaml}\n```\n{META_END}\n");
    if prose.is_empty() {
        block
    } else {
        format!("{prose}\n\n{block}")
    }
}

/// Parse a single JSON object (a `gh api` write response) into `T`. Unlike
/// [`parse_jsonl`], the write endpoints return one object, not a `--jq '.[]'`
/// stream.
fn parse_json_object<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T> {
    serde_json::from_str(text.trim())
        .map_err(|e| Error::Other(format!("gh returned unparseable JSON: {e}")))
}

/// The shelbi-only fields carried in the fenced `<!-- shelbi:begin -->` block.
/// Every field is optional; unknown keys flatten into `params`, mirroring
/// [`Issue::params`] so a newer binary's fields survive an older read.
#[derive(Debug, Default, Deserialize, Serialize)]
struct ShelbiMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefers_machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zen: Option<IssueZenConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launch: Option<IssueLaunchConfig>,
    #[serde(flatten, default)]
    params: BTreeMap<String, serde_yaml::Value>,
}

/// Split an issue body into `(prose, metadata)`: the fenced shelbi block is
/// removed from the returned prose and parsed as YAML into [`ShelbiMeta`]. A
/// body with no block yields the whole body as prose and a default (empty)
/// metadata. A malformed block is treated as absent — read must never fail on a
/// hand-edited body — so its fields simply default.
fn split_shelbi_meta(body: &str) -> (String, ShelbiMeta) {
    let Some(begin) = body.find(META_BEGIN) else {
        return (body.trim().to_string(), ShelbiMeta::default());
    };
    let after_begin = begin + META_BEGIN.len();
    let Some(end_rel) = body[after_begin..].find(META_END) else {
        // Unterminated marker: leave the body untouched, no metadata.
        return (body.trim().to_string(), ShelbiMeta::default());
    };
    let inner = &body[after_begin..after_begin + end_rel];
    let after_end = after_begin + end_rel + META_END.len();

    // Prose = everything before the marker + everything after it, with the
    // seam's surrounding blank lines collapsed so a stripped block doesn't
    // leave a double blank gap.
    let prose = format!("{}{}", &body[..begin], &body[after_end..]);
    let prose = prose.trim().to_string();

    let meta = parse_meta_yaml(inner).unwrap_or_default();
    (prose, meta)
}

/// Parse the inner text of a fenced shelbi block into [`ShelbiMeta`]. The inner
/// text is a fenced ```` ```yaml ```` code block; strip the fence lines and
/// deserialize the YAML. Returns `None` on any parse failure so a malformed
/// block reads as absent metadata rather than erroring the whole board.
fn parse_meta_yaml(inner: &str) -> Option<ShelbiMeta> {
    let yaml = strip_code_fence(inner);
    if yaml.trim().is_empty() {
        return Some(ShelbiMeta::default());
    }
    serde_yaml::from_str(&yaml).ok()
}

/// Strip a leading ```` ```yaml ```` / ```` ``` ```` fence and its closing
/// ```` ``` ```` from a block, returning the inner YAML. Lines outside a fence
/// are kept, so a block written without a fence still parses as raw YAML.
fn strip_code_fence(inner: &str) -> String {
    let trimmed = inner.trim();
    let mut lines: Vec<&str> = trimmed.lines().collect();
    if lines.first().is_some_and(|l| l.trim_start().starts_with("```")) {
        lines.remove(0);
    }
    if lines.last().is_some_and(|l| l.trim() == "```") {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a store whose `gh` runner dispatches on the endpoint path in the
    /// args, returning canned JSONL. `issues_json` answers the issues endpoint;
    /// `comments_json` answers any `/comments` endpoint.
    fn store_with(
        issues_json: &'static str,
        comments_json: &'static str,
    ) -> GitHubStore {
        GitHubStore::with_runner("owner/repo", move |args| {
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if path.contains("/comments") {
                Ok(comments_json.to_string())
            } else {
                Ok(issues_json.to_string())
            }
        })
    }

    #[test]
    fn list_maps_labels_body_and_orders_the_board() {
        // Two issues: one in-progress with a full metadata block, one closed.
        let issues = r#"{"number":7,"title":"Do the thing","body":"Prose here.\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\nbranch: jlong/do-thing\ndepends_on: [other]\nprefers_machine: hub\npriority: 3\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/do-thing"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}
{"number":4,"title":"Old task","body":"done body","state":"closed","state_reason":"completed","labels":[{"name":"shelbi:id/old-task"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");

        let board = store.list().unwrap();
        assert_eq!(board.len(), 2);

        // Board order: in-progress sorts before done.
        assert_eq!(board[0].task.id, "do-thing");
        assert_eq!(board[1].task.id, "old-task");

        let first = &board[0];
        assert_eq!(first.task.column, Column::in_progress());
        assert_eq!(first.task.title, "Do the thing");
        assert_eq!(first.task.workflow.as_deref(), Some("app"));
        assert_eq!(first.task.branch.as_deref(), Some("jlong/do-thing"));
        assert_eq!(first.task.depends_on, vec!["other".to_string()]);
        assert_eq!(first.task.prefers_machine.as_deref(), Some("hub"));
        assert_eq!(first.task.priority, 3);
        // Assignment is never read from GitHub.
        assert_eq!(first.task.assigned_to, None);
        // The fenced block is stripped from the prose.
        assert_eq!(first.body, "Prose here.");
        assert!(!first.body.contains("shelbi:begin"));

        // Closed + completed → done.
        assert_eq!(board[1].task.column, Column::done());
    }

    #[test]
    fn closed_not_planned_is_canceled_and_overrides_a_stale_label() {
        // Closed with a stale non-terminal status label → terminal wins.
        let issues = r#"{"number":9,"title":"Abandoned","body":"","state":"closed","state_reason":"not_planned","labels":[{"name":"shelbi:id/abandoned"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board[0].task.column, Column::canceled());
    }

    #[test]
    fn open_issue_without_status_label_defaults_to_todo() {
        let issues = r#"{"number":1,"title":"Fresh","body":"hi","state":"open","labels":[{"name":"shelbi:id/fresh"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board[0].task.column, Column::todo());
    }

    #[test]
    fn issue_without_id_label_falls_back_to_number() {
        let issues = r#"{"number":42,"title":"Unmigrated","body":"body","state":"open","labels":[],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board[0].task.id, "42");
    }

    #[test]
    fn pull_requests_are_filtered_out() {
        let issues = r#"{"number":1,"title":"A real issue","body":"","state":"open","labels":[{"name":"shelbi:id/real"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}
{"number":2,"title":"A PR","body":"","state":"open","labels":[],"pull_request":{"url":"https://x"},"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].task.id, "real");
    }

    #[test]
    fn get_returns_the_matching_issue_and_none_for_missing() {
        let issues = r#"{"number":7,"title":"Do the thing","body":"prose","state":"open","labels":[{"name":"shelbi:id/do-thing"},{"name":"shelbi:status/review"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        // The runner returns the issue for any issues query; for the "missing"
        // case we return an empty result.
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            let has_missing = args.iter().any(|a| a.contains("shelbi:id/missing"));
            if has_missing {
                Ok(String::new())
            } else {
                Ok(issues.to_string())
            }
        });

        let got = store.get("do-thing").unwrap().expect("issue exists");
        assert_eq!(got.task.id, "do-thing");
        assert_eq!(got.task.column, Column::review());
        assert_eq!(got.body, "prose");

        assert!(store.get("missing").unwrap().is_none());
    }

    #[test]
    fn list_comments_reads_live_and_orders_by_creation() {
        let issues = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let comments = r#"{"id":100,"body":"first","created_at":"2026-08-01T01:00:00Z","user":{"login":"alice"}}
{"id":101,"body":"second","created_at":"2026-08-01T02:00:00Z","user":{"login":"bob"}}"#;
        let store = store_with(issues, comments);

        let got = store.list_comments("t").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].body, "first");
        assert_eq!(got[0].author.as_deref(), Some("alice"));
        assert_eq!(got[0].id, "100");
        assert_eq!(got[1].body, "second");
        assert_eq!(got[1].author.as_deref(), Some("bob"));
    }

    #[test]
    fn list_comments_for_missing_issue_is_empty() {
        let store = GitHubStore::with_runner("owner/repo", |_args| Ok(String::new()));
        assert!(store.list_comments("nope").unwrap().is_empty());
    }

    #[test]
    fn unreachable_tracker_surfaces_a_command_error_not_stale_data() {
        let store = GitHubStore::with_runner("owner/repo", |args| {
            Err(Error::Command {
                cmd: format!("gh {}", args.join(" ")),
                status: "exit status: 1".to_string(),
                stderr: "could not resolve host: api.github.com".to_string(),
            })
        });
        let err = store.list().unwrap_err();
        match err {
            Error::Command { stderr, .. } => assert!(stderr.contains("could not resolve host")),
            other => panic!("expected Error::Command, got {other:?}"),
        }
    }

    #[test]
    fn body_without_a_meta_block_is_all_prose() {
        let (prose, meta) = split_shelbi_meta("Just a plain description.\n");
        assert_eq!(prose, "Just a plain description.");
        assert!(meta.workflow.is_none());
        assert_eq!(meta.priority, None);
    }

    #[test]
    fn malformed_meta_block_reads_as_absent_metadata() {
        // An unterminated block leaves the body intact and yields no metadata.
        let body = "Prose\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\n";
        let (prose, meta) = split_shelbi_meta(body);
        assert!(prose.contains("Prose"));
        assert!(meta.workflow.is_none());
    }

    #[test]
    fn meta_block_carries_unknown_keys_into_params() {
        let body = "P\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\nfeature: auth-rewrite\n```\n<!-- shelbi:end -->";
        let (_prose, meta) = split_shelbi_meta(body);
        assert_eq!(meta.workflow.as_deref(), Some("app"));
        assert_eq!(
            meta.params.get("feature").and_then(|v| v.as_str()),
            Some("auth-rewrite")
        );
    }

    #[test]
    fn poll_changes_watermark_surfaces_later_edits_and_comments() {
        // Issue updated_at 2026-08-02; a first poll from start reports nothing
        // and sets the high-water mark there.
        let issues = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");

        let (changes, cursor) = store.poll_changes(&Cursor::start()).unwrap();
        assert!(changes.is_empty());
        assert_eq!(
            cursor.watermark(),
            Some("2026-08-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap())
        );

        // Now poll against an *older* cursor: the issue's updated_at is past it,
        // so it surfaces as an upsert, and its post-cursor comment as CommentAdded.
        let older = Cursor::at("2026-08-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap());
        let comments = r#"{"id":100,"body":"new comment","created_at":"2026-08-01T18:00:00Z","user":{"login":"alice"}}"#;
        let store = store_with(issues, comments);
        let (changes, _cursor) = store.poll_changes(&older).unwrap();

        let upserts = changes
            .iter()
            .filter(|c| matches!(c, IssueChange::Upserted(_)))
            .count();
        let comment_adds: Vec<_> = changes
            .iter()
            .filter_map(|c| match c {
                IssueChange::CommentAdded { issue_id, comment } => Some((issue_id, comment)),
                _ => None,
            })
            .collect();
        assert_eq!(upserts, 1);
        assert_eq!(comment_adds.len(), 1);
        assert_eq!(comment_adds[0].0, "t");
        assert_eq!(comment_adds[0].1.body, "new comment");
    }

    #[test]
    fn poll_changes_scopes_the_query_with_the_since_watermark() {
        // A recording runner captures every `gh` call so we can assert the
        // `since=` watermark is (a) absent on a cold first poll and (b) present
        // on both the issues query and the per-touched-issue comments query on a
        // warm poll — the rate-limit-respecting `since=` scoping (plan D3).
        let issues = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let comments = r#"{"id":100,"body":"new comment","created_at":"2026-08-01T18:00:00Z","user":{"login":"alice"}}"#;
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            Ok(if path.contains("/comments") { comments } else { issues }.to_string())
        });

        // Cold poll: no watermark, so no `since=` scoping — the whole board is
        // listed once to seed the high-water mark.
        let (_changes, cursor) = store.poll_changes(&Cursor::start()).unwrap();
        {
            let calls = calls.lock().unwrap();
            let issues_call = calls
                .iter()
                .find(|c| c.contains("repos/owner/repo/issues") && !c.contains("/comments"))
                .expect("issues list call");
            assert!(!issues_call.contains("since="), "cold poll must not scope: {issues_call}");
        }

        // Warm poll against an older cursor: the issue's updated_at (2026-08-02)
        // is past it, so it surfaces — and both the issues query and the comments
        // query carry the exact `since=<watermark>` param.
        calls.lock().unwrap().clear();
        let older = Cursor::at("2026-08-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap());
        let (_changes, _next) = store.poll_changes(&older).unwrap();
        let calls = calls.lock().unwrap();
        let issues_call = calls
            .iter()
            .find(|c| c.contains("repos/owner/repo/issues") && !c.contains("/comments"))
            .expect("issues list call");
        assert!(
            issues_call.contains("since=2026-08-01T12:00:00+00:00"),
            "warm poll must scope issues: {issues_call}"
        );
        let comments_call = calls
            .iter()
            .find(|c| c.contains("/comments"))
            .expect("comments call");
        assert!(
            comments_call.contains("since=2026-08-01T12:00:00+00:00"),
            "warm poll must scope comments: {comments_call}"
        );

        // The cursor advanced to the issue's updated_at high-water mark.
        assert_eq!(
            cursor.watermark(),
            Some("2026-08-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap())
        );
    }

    // --- write path ----------------------------------------------------------

    type Calls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A store whose `gh` runner records every call (space-joined) and answers
    /// reads from canned JSON: `by_id_json` for a `get`-by-id lookup (an issues
    /// GET carrying a `labels=shelbi:id/…` filter), `list_json` for a plain
    /// issues list, `labels_json` for a `GET .../labels`, `[]` for comments.
    /// Every mutating call (`POST` / `PATCH` / `PUT`) echoes `write_json`.
    fn recording_store(
        by_id_json: &'static str,
        list_json: &'static str,
        labels_json: &'static str,
        write_json: &'static str,
    ) -> (GitHubStore, Calls) {
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            if method != "GET" {
                return Ok(write_json.to_string());
            }
            let path = args
                .iter()
                .find(|a| a.contains("repos/"))
                .copied()
                .unwrap_or("");
            if path.ends_with("/labels") {
                return Ok(labels_json.to_string());
            }
            if path.contains("/comments") {
                return Ok(String::new());
            }
            let by_id = args.iter().any(|a| a.contains("labels=shelbi:id/"));
            Ok(if by_id { by_id_json } else { list_json }.to_string())
        });
        (store, calls)
    }

    /// Every call that swaps a status label or opens/closes the issue. Used by
    /// the terminal / reopen assertions.
    fn call_containing<'a>(calls: &'a [String], needles: &[&str]) -> Option<&'a String> {
        calls
            .iter()
            .find(|c| needles.iter().all(|n| c.contains(n)))
    }

    #[test]
    fn add_creates_issue_with_anchor_labels_and_meta_block() {
        // Fresh repo: the id lookup and the column list are both empty, and no
        // labels exist yet.
        let created = r#"{"number":10,"title":"Do the thing","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let (store, calls) = recording_store("", "", "", created);

        let mut spec = NewIssue::new("do-thing", "Do the thing", Column::todo(), "Prose body");
        spec.workflow = Some("app".into());
        spec.branch = Some("jlong/do-thing".into());
        let issue = store.add(spec).unwrap();
        assert_eq!(issue.id, "do-thing");
        assert_eq!(issue.priority, 0); // appended to an empty column
        assert_eq!(
            issue.created_at,
            "2026-08-03T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );

        let calls = calls.lock().unwrap();
        // The status label set is auto-created (todo among the stock set).
        assert!(call_containing(
            &calls,
            &["-X POST", "repos/owner/repo/labels", "name=shelbi:status/todo"]
        )
        .is_some());
        // The create carries both anchor labels and a metadata block.
        let create = call_containing(&calls, &["-X POST", "repos/owner/repo/issues", "title=Do the thing"])
            .expect("issue create POST");
        assert!(create.contains("labels[]=shelbi:id/do-thing"));
        assert!(create.contains("labels[]=shelbi:status/todo"));
        assert!(create.contains("workflow: app"));
        assert!(create.contains("branch: jlong/do-thing"));
        assert!(create.contains("priority: 0"));
        assert!(create.contains("Prose body"));
        assert!(create.contains(META_BEGIN));
    }

    #[test]
    fn add_rejects_a_duplicate_id() {
        let existing = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/do-thing"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, _calls) = recording_store(existing, existing, "", "{}");
        assert!(store
            .add(NewIssue::new("do-thing", "T", Column::todo(), "b"))
            .is_err());
    }

    #[test]
    fn add_into_a_terminal_status_closes_the_issue() {
        let created = r#"{"number":11,"title":"Done thing","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let (store, calls) = recording_store("", "", "", created);
        store
            .add(NewIssue::new("done-thing", "Done thing", Column::done(), "b"))
            .unwrap();
        let calls = calls.lock().unwrap();
        assert!(call_containing(
            &calls,
            &["-X PATCH", "repos/owner/repo/issues/11", "state=closed", "state_reason=completed"]
        )
        .is_some());
    }

    #[test]
    fn move_status_swaps_the_label_and_closes_on_a_terminal_target() {
        // In-progress issue with a workflow in its meta block.
        let issue = r#"{"number":7,"title":"T","body":"P\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");

        let mv = store
            .move_status("t", &Column::done(), "accept")
            .unwrap()
            .expect("status changed");
        assert_eq!(mv.from, Column::in_progress());
        assert_eq!(mv.to, Column::done());
        assert_eq!(mv.workflow, "app");

        let calls = calls.lock().unwrap();
        let put = call_containing(&calls, &["-X PUT", "repos/owner/repo/issues/7/labels"])
            .expect("PUT labels");
        // New status applied, old status dropped, id anchor preserved.
        assert!(put.contains("labels[]=shelbi:status/done"));
        assert!(put.contains("labels[]=shelbi:id/t"));
        assert!(!put.contains("shelbi:status/in-progress"));
        // Terminal target closes the issue as completed.
        assert!(call_containing(
            &calls,
            &["-X PATCH", "state=closed", "state_reason=completed"]
        )
        .is_some());
    }

    #[test]
    fn move_status_is_a_noop_when_already_in_the_target() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/review"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        assert!(store.move_status("t", &Column::review(), "again").unwrap().is_none());
        // No label swap issued.
        assert!(call_containing(&calls.lock().unwrap(), &["-X PUT", "/labels"]).is_none());
    }

    #[test]
    fn move_status_reopens_a_closed_issue_for_a_non_terminal_target() {
        // A closed (done) issue moved back into an active lane must reopen.
        let issue = r#"{"number":7,"title":"T","body":"","state":"closed","state_reason":"completed","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/done"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        let mv = store
            .move_status("t", &Column::in_progress(), "reopen")
            .unwrap()
            .expect("moved");
        assert_eq!(mv.from, Column::done());
        assert_eq!(mv.to, Column::in_progress());
        assert!(call_containing(&calls.lock().unwrap(), &["-X PATCH", "state=open"]).is_some());
    }

    #[test]
    fn cancel_closes_the_issue_as_not_planned() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        let mv = store.cancel("t", "obsolete").unwrap().expect("moved");
        assert_eq!(mv.to, Column::canceled());
        assert!(call_containing(
            &calls.lock().unwrap(),
            &["-X PATCH", "state=closed", "state_reason=not_planned"]
        )
        .is_some());
    }

    #[test]
    fn set_fields_rewrites_only_the_meta_block() {
        let issue = r#"{"number":7,"title":"T","body":"Prose stays.\n\n<!-- shelbi:begin -->\n```yaml\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store
            .set_fields(
                "t",
                IssueFields {
                    branch: Some(Some("jlong/t".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let calls = calls.lock().unwrap();
        let patch = call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/7"])
            .expect("body PATCH");
        assert!(patch.contains("branch: jlong/t"));
        // Human prose is preserved around the block.
        assert!(patch.contains("Prose stays."));
    }

    #[test]
    fn set_fields_patches_title_and_body_prose() {
        let issue = r#"{"number":7,"title":"Old","body":"Old prose.\n\n<!-- shelbi:begin -->\n```yaml\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store
            .set_fields(
                "t",
                IssueFields {
                    title: Some("New title".into()),
                    body: Some("Fresh prose.".into()),
                    workflow: Some(Some("app".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let calls = calls.lock().unwrap();
        let patch = call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/7"])
            .expect("title/body PATCH");
        assert!(patch.contains("title=New title"));
        assert!(patch.contains("Fresh prose."));
        assert!(patch.contains("workflow: app"));
    }

    #[test]
    fn delete_closes_the_issue_as_not_planned() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store.delete("t").unwrap();
        assert!(call_containing(
            &calls.lock().unwrap(),
            &["-X PATCH", "state=closed", "state_reason=not_planned"]
        )
        .is_some());
    }

    #[test]
    fn set_fields_with_only_assigned_to_touches_the_overlay_not_the_api() {
        // Assignment is ephemeral local routing; GitHub never stores it, so an
        // `assigned_to`-only update must not touch the API at all — it lands in
        // the local overlay instead, and a subsequent read folds it back on.
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(Some("alpha".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        // No gh API call — the only reads/writes were the local overlay.
        assert!(calls.lock().unwrap().is_empty());

        // The overlay is folded onto reads.
        assert_eq!(
            store.get("t").unwrap().unwrap().task.assigned_to.as_deref(),
            Some("alpha")
        );
        assert_eq!(
            store.list().unwrap()[0].task.assigned_to.as_deref(),
            Some("alpha")
        );

        // Clearing it removes the overlay again.
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.get("t").unwrap().unwrap().task.assigned_to, None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn move_and_unassign_and_park_clear_the_assignment_overlay() {
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let review = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/review"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, _calls) = recording_store(review, review, "", "{}");

        // Assign, then move-and-unassign back to todo: the overlay is cleared.
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .move_status_and_unassign("t", &Column::todo(), "stop")
            .unwrap();
        assert_eq!(crate::get_task_assignment("test-project", "t").unwrap(), None);

        // Re-assign, then park: park reports the prior owner, clears the overlay,
        // and sets the local parked marker.
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let was = store.park_review("t").unwrap();
        assert_eq!(was.as_deref(), Some("review-1"));
        assert_eq!(crate::get_task_assignment("test-project", "t").unwrap(), None);
        assert!(crate::is_task_parked("test-project", "t").unwrap());

        std::env::remove_var("SHELBI_HOME");
    }

    /// A throwaway `SHELBI_HOME` for tests that exercise the local assignment
    /// overlay (marker files under `<project_dir>/assignments/`).
    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-github-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn set_priority_renumbers_the_column_contiguously() {
        // Three issues a(0) b(1) c(2) in todo; send c to the top.
        let list = r#"{"number":1,"title":"A","body":"<!-- shelbi:begin -->\n```yaml\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/a"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}
{"number":2,"title":"B","body":"<!-- shelbi:begin -->\n```yaml\npriority: 1\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/b"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}
{"number":3,"title":"C","body":"<!-- shelbi:begin -->\n```yaml\npriority: 2\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/c"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;

        // A by-id GET returns just the matching issue so each rewrite patches the
        // right number; a plain list returns all three.
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            if method != "GET" {
                return Ok("{}".to_string());
            }
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if path.ends_with("/labels") {
                return Ok(String::new());
            }
            for (id, num, prio) in [("a", 1, 0), ("b", 2, 1), ("c", 3, 2)] {
                if args.iter().any(|a| *a == format!("labels=shelbi:id/{id}")) {
                    return Ok(format!(
                        r#"{{"number":{num},"title":"{id}","body":"<!-- shelbi:begin -->\n```yaml\npriority: {prio}\n```\n<!-- shelbi:end -->","state":"open","labels":[{{"name":"shelbi:id/{id}"}},{{"name":"shelbi:status/todo"}}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}}"#
                    ));
                }
            }
            Ok(list.to_string())
        });

        store.set_priority("c", PrioMove::Top).unwrap();

        let calls = calls.lock().unwrap();
        // c (issue 3) → priority 0, a (issue 1) → 1, b (issue 2) → 2.
        assert!(call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/3", "priority: 0"]).is_some());
        assert!(call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/1", "priority: 1"]).is_some());
        assert!(call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/2", "priority: 2"]).is_some());
    }

    #[test]
    fn add_comment_posts_and_returns_the_created_comment() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let comment = r#"{"id":555,"body":"hello there","created_at":"2026-08-03T00:00:00Z","user":{"login":"alice"}}"#;
        let (store, calls) = recording_store(issue, issue, "", comment);
        let c = store.add_comment("t", "hello there").unwrap();
        assert_eq!(c.id, "555");
        assert_eq!(c.body, "hello there");
        assert_eq!(c.author.as_deref(), Some("alice"));
        assert!(call_containing(
            &calls.lock().unwrap(),
            &["-X POST", "repos/owner/repo/issues/7/comments", "body=hello there"]
        )
        .is_some());
    }

    #[test]
    fn add_comment_on_a_missing_issue_errors() {
        let (store, _calls) = recording_store("", "", "", "{}");
        assert!(store.add_comment("nope", "hi").is_err());
    }

    #[test]
    fn existing_labels_are_not_recreated() {
        // Every label the create needs already exists → no label POSTs.
        let labels = r#"{"name":"shelbi:id/do-thing"}
{"name":"shelbi:status/backlog"}
{"name":"shelbi:status/todo"}
{"name":"shelbi:status/in-progress"}
{"name":"shelbi:status/review"}
{"name":"shelbi:status/done"}
{"name":"shelbi:status/canceled"}"#;
        let created = r#"{"number":10,"title":"Do the thing","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let (store, calls) = recording_store("", "", labels, created);
        store
            .add(NewIssue::new("do-thing", "Do the thing", Column::todo(), "b"))
            .unwrap();
        assert!(call_containing(&calls.lock().unwrap(), &["-X POST", "repos/owner/repo/labels"]).is_none());
    }

    #[test]
    fn create_label_tolerates_a_concurrent_already_exists() {
        // The label list is stale (empty) so the store tries to create, but the
        // POST races and GitHub answers 422 already_exists — which must be
        // swallowed so `add` still succeeds.
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if method == "POST" && path.ends_with("/labels") {
                return Err(Error::Command {
                    cmd: "gh api ...".into(),
                    status: "exit status: 1".into(),
                    stderr: "HTTP 422: Validation Failed (already_exists)".into(),
                });
            }
            if method != "GET" {
                return Ok(r#"{"number":10,"title":"X","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#.to_string());
            }
            if path.ends_with("/labels") {
                return Ok(String::new());
            }
            Ok(String::new())
        });
        assert!(store
            .add(NewIssue::new("x", "X", Column::todo(), "b"))
            .is_ok());
    }

    #[test]
    fn build_body_round_trips_through_split_shelbi_meta() {
        let meta = ShelbiMeta {
            workflow: Some("app".into()),
            branch: Some("jlong/x".into()),
            priority: Some(4),
            ..Default::default()
        };
        let body = build_body("Human prose.", &meta);
        let (prose, parsed) = split_shelbi_meta(&body);
        assert_eq!(prose, "Human prose.");
        assert_eq!(parsed.workflow.as_deref(), Some("app"));
        assert_eq!(parsed.branch.as_deref(), Some("jlong/x"));
        assert_eq!(parsed.priority, Some(4));

        // An all-empty meta emits no block at all.
        let plain = build_body("Just prose.", &ShelbiMeta::default());
        assert!(!plain.contains(META_BEGIN));
        assert_eq!(plain.trim(), "Just prose.");
    }
}
