//! The GitHub issues backend for the [`IssueStore`] seam — read path.
//!
//! A project whose `issue_tracker.backend` is `github` keeps its board *as*
//! GitHub issues in a single repo (`owner/repo`). This module implements the
//! read half of the contract (`list` / `list_in_status` / `get` /
//! `list_comments`, plus the `poll_changes` watermark) by querying the GitHub
//! REST API live through the `gh` CLI. The write half (`add` / `move_status` /
//! priority / fields / `cancel` / `add_comment`) is a later plan phase and
//! returns a typed "not yet implemented" error here.
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
//! * **assignment** (`assigned_to`) — never read from GitHub. Workspace routing
//!   is ephemeral local state (plan §3), so it is always `None` here.
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
use serde::Deserialize;
use shelbi_core::{
    Column, Error, IssueLaunchConfig, IssueZenConfig, Project, Result,
};

use crate::issue_store::{Cursor, IssueChange, IssueComment, IssueFields, IssueStore, NewIssue, PrioMove, StatusMove};
use crate::{resolve_github_token, IssueFile};
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
    /// A store bound to `repo` (`owner/repo`) for `project`, using the real
    /// `gh` CLI. Auth is resolved lazily per call via
    /// [`crate::resolve_github_token`] and handed to the `gh` subprocess as
    /// `GH_TOKEN`, so all three token sources (env / keychain / out-of-repo
    /// `tokens.yml`) funnel through one place. Nothing secret is written down.
    pub fn new(project: Project, repo: impl Into<String>) -> Self {
        let repo = repo.into();
        let gh: GhRunner = Arc::new(move |args: &[&str]| run_gh(&project, args));
        Self { repo, gh }
    }

    /// Construct a store over an arbitrary `gh` runner. The production seam for
    /// tests: inject a closure returning canned JSON instead of shelling out.
    #[cfg(test)]
    fn with_runner(
        repo: impl Into<String>,
        runner: impl Fn(&[&str]) -> Result<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
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
        let mut out: Vec<IssueFile> = issues
            .into_iter()
            .filter(|gh| !gh.is_pull_request())
            .map(|gh| gh.into_issue_file())
            .collect();
        sort_board(&mut out);
        Ok(out)
    }

    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
        // Client-side filter of the full board: the status lives in a label we
        // already parse, and this keeps a single mapping path (no second query
        // shape to keep in sync).
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
        Ok(issues
            .into_iter()
            .find(|gh| !gh.is_pull_request())
            .map(GhIssue::into_issue_file))
    }

    fn add(&self, _spec: NewIssue) -> Result<Issue> {
        Err(write_unimplemented())
    }

    fn move_status(&self, _id: &str, _to: &Column, _reason: &str) -> Result<Option<StatusMove>> {
        Err(write_unimplemented())
    }

    fn set_priority(&self, _id: &str, _pos: PrioMove) -> Result<()> {
        Err(write_unimplemented())
    }

    fn set_fields(&self, _id: &str, _fields: IssueFields) -> Result<()> {
        Err(write_unimplemented())
    }

    fn cancel(&self, _id: &str, _reason: &str) -> Result<Option<StatusMove>> {
        Err(write_unimplemented())
    }

    fn poll_changes(&self, since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
        // Live read + watermark, no content cache (plan D3). The first poll
        // from `Cursor::start()` just establishes the high-water `updated_at`
        // and reports nothing; later edits (issue `updated_at` moved past the
        // cursor) surface as upserts, and comments on those same freshly-touched
        // issues (created past the cursor) surface as CommentAdded.
        let path = format!("repos/{}/issues", self.repo);
        let issues = self.api_issues(&path, &["-f", "state=all", "-f", "per_page=100"])?;

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
                for comment in self.comments_for_number(number)? {
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

    fn add_comment(&self, _id: &str, _body: &str) -> Result<IssueComment> {
        Err(write_unimplemented())
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
        let path = format!("repos/{}/issues/{number}/comments", self.repo);
        let mut args: Vec<&str> = vec!["api", "-X", "GET", &path, "--paginate"];
        args.extend_from_slice(&["-f", "per_page=100", "--jq", ".[]"]);
        let out = (self.gh)(&args)?;
        let raw: Vec<GhComment> = parse_jsonl(&out)?;
        let mut comments: Vec<IssueComment> = raw.into_iter().map(GhComment::into_comment).collect();
        // The API returns comments in creation order already; sort defensively
        // on the id so the ordering contract holds regardless.
        comments.sort_by_key(|c| c.created_at);
        Ok(comments)
    }
}

/// The typed error every not-yet-built write method returns. The GitHub write
/// path is a later plan phase (§5); until then a caller gets a clear message
/// rather than a panic or a silent no-op.
fn write_unimplemented() -> Error {
    Error::Other(
        "the GitHub issue-tracker write path is not yet implemented \
         (Plans/pluggable-task-stores.md phase 5); only reads are available today"
            .to_string(),
    )
}

/// Run the real `gh` CLI with resolved auth. The token is resolved through the
/// full chain (env → `gh` keychain → out-of-repo `tokens.yml`) and handed to
/// the child as `GH_TOKEN`; a failure to resolve surfaces the actionable
/// [`Error::MissingIssueTrackerAuth`]. A non-zero exit (network down, repo not
/// found, not authed) becomes an [`Error::Command`] — never a stale render.
fn run_gh(project: &Project, args: &[&str]) -> Result<String> {
    let token = resolve_github_token(project)?;
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

/// The shelbi-only fields carried in the fenced `<!-- shelbi:begin -->` block.
/// Every field is optional; unknown keys flatten into `params`, mirroring
/// [`Issue::params`] so a newer binary's fields survive an older read.
#[derive(Debug, Default, Deserialize)]
struct ShelbiMeta {
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    prefers_machine: Option<String>,
    #[serde(default)]
    priority: Option<u32>,
    #[serde(default)]
    zen: Option<IssueZenConfig>,
    #[serde(default)]
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
    fn write_methods_are_typed_unimplemented() {
        let store = GitHubStore::with_runner("owner/repo", |_| Ok(String::new()));
        assert!(store.add(NewIssue::new("x", "X", Column::todo(), "b")).is_err());
        assert!(store.move_status("x", &Column::done(), "r").is_err());
        assert!(store.set_priority("x", PrioMove::Top).is_err());
        assert!(store.set_fields("x", IssueFields::default()).is_err());
        assert!(store.cancel("x", "r").is_err());
        assert!(store.add_comment("x", "hi").is_err());
    }
}
