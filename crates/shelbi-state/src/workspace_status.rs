//! Workspace state observed from each workspace's tmux pane title.
//!
//! Workspace `.claude/settings.json` hooks emit `shelbi:working|idle|blocked`
//! markers via OSC pane-title escapes (see
//! `default_workspace_settings.json.template`); the hub-side sidebar poll loop reads
//! the current pane title with `tmux display-message`, parses the trailing
//! marker, and writes any state transition here.
//!
//! Layout (all hub-side, under `$SHELBI_HOME` / `~/.shelbi`):
//!
//! - `workspaces/<name>/status.yaml` — last observed state per workspace.
//! - `events.log` — append-only transition log across all workspaces.
//!
//! Authoritative state stays on the hub: workspaces themselves only emit
//! markers; they don't own these files.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::{Column, Result, DEFAULT_WORKFLOW_NAME};

use crate::{atomic_write, ensure_dir, shelbi_home};

/// The marker emitted by the workspace's claude hooks. `idle` from the hook
/// wire-format maps to [`WorkspaceState::AwaitingInput`] — Stop fires when
/// claude finishes a turn and is waiting for the next prompt, which is
/// what we want to surface in the UI ("awaiting input"), not "no work to
/// do".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    Working,
    AwaitingInput,
    Blocked,
}

impl WorkspaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceState::Working => "working",
            WorkspaceState::AwaitingInput => "awaiting_input",
            WorkspaceState::Blocked => "blocked",
        }
    }
}

impl std::fmt::Display for WorkspaceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// All recognized `shelbi:<…>` pane-title markers. Distinct from
/// [`WorkspaceState`] because two markers — `idle` (mid-task pause, fires on
/// every claude turn end) and `review` (explicit completion handoff from
/// the workspace prompt) — both map to the same persisted state
/// ([`WorkspaceState::AwaitingInput`]) but have very different downstream
/// semantics: `review` triggers a one-shot kanban move into the review
/// column, `idle` does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMarker {
    Working,
    Idle,
    Review,
    Blocked,
}

impl PaneMarker {
    /// Persisted [`WorkspaceState`] for this marker. `Idle` and `Review`
    /// collapse to `AwaitingInput` — the status file just records that
    /// claude is sitting at a prompt; the review-handoff side effect
    /// happens elsewhere.
    pub fn workspace_state(self) -> WorkspaceState {
        match self {
            PaneMarker::Working => WorkspaceState::Working,
            PaneMarker::Idle | PaneMarker::Review => WorkspaceState::AwaitingInput,
            PaneMarker::Blocked => WorkspaceState::Blocked,
        }
    }
}

/// Extract the trailing `shelbi:<marker>` from a pane title. Returns
/// `None` if the marker is missing or unrecognized — the pane is either
/// pre-hook-emit or running something other than a shelbi-deployed workspace.
pub fn parse_pane_title_marker(title: &str) -> Option<PaneMarker> {
    // Anchor to the *last whitespace-delimited token* and require it to
    // *start* with `shelbi:`. A substring match (the old `rfind`) let
    // `myshelbi:working`, or a task name like `fix shelbi:review parser`
    // sitting mid-title, parse as a live marker. Our hooks always emit the
    // marker as the trailing token of the OSC pane title, so the token
    // boundary is the right anchor. This is a *state hint* only — board
    // moves are driven solely by the independent file-based review marker
    // (`maybe_promote_to_review`), never by this title, because any program
    // the agent runs can print an OSC title sequence into the pane.
    let last = title.split_whitespace().next_back()?;
    let marker = last.strip_prefix("shelbi:")?;
    // Trim trailing control chars (BEL, ST) some terminals leave behind.
    let marker = marker.trim_end_matches(|c: char| c.is_control() || c == '\u{0007}');
    match marker {
        "working" => Some(PaneMarker::Working),
        "idle" => Some(PaneMarker::Idle),
        "review" => Some(PaneMarker::Review),
        "blocked" => Some(PaneMarker::Blocked),
        _ => None,
    }
}

/// Convenience: just the persisted state, dropping the marker
/// distinction. Callers that need to know `review` vs `idle` should use
/// [`parse_pane_title_marker`] instead.
pub fn parse_pane_title_state(title: &str) -> Option<WorkspaceState> {
    parse_pane_title_marker(title).map(PaneMarker::workspace_state)
}

/// `~/.shelbi/workspaces` — root for per-workspace status dirs.
///
/// As a one-shot migration, if the legacy `~/.shelbi/workers/` directory
/// exists and the new `workspaces/` doesn't, the legacy dir is renamed in
/// place. Idempotent and best-effort — any IO error is swallowed; the
/// poller will recreate either directory on its next write.
pub fn workspaces_dir() -> Result<PathBuf> {
    let home = shelbi_home()?;
    let new = home.join("workspaces");
    if !new.exists() {
        let legacy = home.join("workers");
        if legacy.exists() {
            let _ = fs::rename(&legacy, &new);
        }
    }
    Ok(new)
}

/// `~/.shelbi/workspaces/<name>/status.yaml`.
pub fn workspace_status_path(workspace: &str) -> Result<PathBuf> {
    Ok(workspaces_dir()?.join(workspace).join("status.yaml"))
}

/// `~/.shelbi/workspaces/<name>/.expected-teardown` — presence signals that
/// a shelbi-initiated caller (`shelbi task start`, `shelbi workspace stop`,
/// `shelbi quit`, project quit) is about to kill the workspace's pane, so
/// the pane's lifecycle wrapper should suppress its `pane_alive=false`
/// event on exit. Otherwise every dispatch would fire a spurious
/// `pane_alive=false reason=signal:SIGHUP` right before the new pane comes
/// up (bug-workspace-pane-alive-false-sighup-fires-spuriously-right-after-dispatch).
pub fn expected_teardown_marker_path(workspace: &str) -> Result<PathBuf> {
    Ok(workspaces_dir()?.join(workspace).join(".expected-teardown"))
}

/// Freshness window on the expected-teardown marker. The wrapper's exit
/// path runs between the mark and the check: mark → tmux kill-window →
/// SIGHUP → forward to child → child.wait() → cleanup → consume. That
/// chain is usually under a second, but claude has its own shutdown flow
/// and could dawdle. 30 s is more than any observed teardown and still
/// short enough that a stale marker (e.g. mark→SIGKILL race that never
/// ran the consume) can't leak past the very next pane's real exit.
pub const EXPECTED_TEARDOWN_MAX_AGE: Duration = Duration::from_secs(30);

/// Write the expected-teardown marker for `workspace`. Best-effort:
/// callers use this before `tmux kill-window` (or equivalent), so a
/// failure to write just means the pane_alive event fires with its
/// historical `signal:SIGHUP` reason — degraded but not broken.
pub fn mark_expected_teardown(workspace: &str) -> Result<()> {
    let path = expected_teardown_marker_path(workspace)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    // Empty body — the marker's presence + kernel-recorded mtime are the
    // signal. No timestamp in the body means we don't have to parse
    // anything on the consume side.
    atomic_write(&path, b"")
}

/// If a fresh (< [`EXPECTED_TEARDOWN_MAX_AGE`]) marker exists, remove it
/// and return `true` — the current pane teardown was intentional and the
/// caller should suppress the `pane_alive=false` event. If the marker is
/// older than the window, remove it and return `false` (the recorded
/// intent is stale — an SIGKILL race or a caller that never got as far
/// as the actual kill). If no marker exists → `false`.
///
/// Always deleting on read keeps a stale marker from leaking into a
/// later, unrelated exit event.
pub fn consume_expected_teardown(workspace: &str) -> Result<bool> {
    let path = expected_teardown_marker_path(workspace)?;
    let mtime = match fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let fresh = SystemTime::now()
        .duration_since(mtime)
        .map(|elapsed| elapsed < EXPECTED_TEARDOWN_MAX_AGE)
        // Clock skew (mtime in the future): treat as fresh. That's the
        // conservative call — we'd rather miss one true pane-death event
        // than spam the log with a spurious one every dispatch.
        .unwrap_or(true);
    let _ = fs::remove_file(&path);
    Ok(fresh)
}

/// Unconditionally remove the expected-teardown marker for `workspace`.
/// Called by the pane wrapper at startup so a marker left behind by a
/// crashed prior lifecycle (mark → SIGKILL → no consume) can't survive
/// long enough to accidentally suppress a real exit event later.
pub fn clear_expected_teardown(workspace: &str) -> Result<()> {
    let path = expected_teardown_marker_path(workspace)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

/// `~/.shelbi/events.log`.
pub fn events_log_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("events.log"))
}

/// Local Unix-domain socket the hub daemon (`shelbi daemon`) listens on.
/// `$SHELBI_HUB_SOCK` wins when set so tests, alternate users, or
/// XDG_RUNTIME_DIR layouts can re-home it without touching `SHELBI_HOME`.
/// Default is `~/.shelbi/hub.sock`.
pub fn hub_socket_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SHELBI_HUB_SOCK") {
        return Ok(PathBuf::from(p));
    }
    Ok(shelbi_home()?.join("hub.sock"))
}

/// Last observed state for a workspace — persisted to disk so a fresh hub
/// process can see the prior state without re-deriving it from the pane
/// title (which may have rolled past the marker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStatus {
    /// The workspace's stable name. Accepts the legacy `worker:` YAML key
    /// as an alias for one release so existing on-disk `status.yaml` files
    /// keep loading without manual migration.
    #[serde(alias = "worker")]
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    pub state: WorkspaceState,
    /// When the state most recently *changed*. Stays put across polls
    /// that observe the same state.
    pub last_transition: DateTime<Utc>,
    /// When the marker was most recently observed (any state). Bumped on
    /// every successful poll regardless of transition.
    pub last_seen: DateTime<Utc>,
}

pub fn save_workspace_status(status: &WorkspaceStatus) -> Result<()> {
    let path = workspace_status_path(&status.workspace)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let yaml = serde_yaml::to_string(status)?;
    atomic_write(&path, yaml.as_bytes())
}

pub fn load_workspace_status(workspace: &str) -> Result<Option<WorkspaceStatus>> {
    let path = workspace_status_path(workspace)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    Ok(Some(serde_yaml::from_str(&text)?))
}

/// Append `<rfc3339> workspace=<name> <prev> -> <new>` to
/// `~/.shelbi/events.log`. `prev` is `None` on the first observation.
pub fn append_workspace_event(
    workspace: &str,
    prev: Option<WorkspaceState>,
    new: WorkspaceState,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let workspace = sanitize_field(workspace);
    let prev_str = prev.map(|s| s.as_str()).unwrap_or("none");
    append_event_line(&format!("{ts} workspace={workspace} {prev_str} -> {new}"))
}

/// Append `<rfc3339> message=<msg_id> task=<task-id> push=ok` to
/// `~/.shelbi/events.log`. Records a hub → workspace push on the file-based
/// message channel (see `shelbi message`) so the channel is auditable from
/// the same stream as workspace/task transitions. The leading timestamp
/// keeps every events.log line uniform and parseable by the activity feed
/// and `shelbi events tail`.
pub fn append_message_event(msg_id: &str, task_id: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let msg_id = sanitize_field(msg_id);
    let task_id = sanitize_field(task_id);
    append_event_line(&format!("{ts} message={msg_id} task={task_id} push=ok"))
}

/// Append `<rfc3339> message=<msg_id> task=<task-id> ack=<kind>` to
/// `~/.shelbi/events.log`. `kind` is the literal worker/timeout token that
/// the worker emitted (`worker`) or the daemon synthesized
/// (`timeout`) — see Phase 9 of `Plans/worker-orchestrator-communication.md`
/// §9 / §13. The shared `message=` prefix lets the activity feed correlate
/// pushes (`push=ok`) with their delivery state without re-reading the
/// pending map.
pub fn append_message_ack_event(msg_id: &str, task_id: &str, kind: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let msg_id = sanitize_field(msg_id);
    let task_id = sanitize_field(task_id);
    let kind = sanitize_reason(kind);
    append_event_line(&format!("{ts} message={msg_id} task={task_id} ack={kind}"))
}

/// Append `<rfc3339> question=<question-id> task=<task-id> kind=clarification text=<truncated>`
/// to `~/.shelbi/events.log`. Emitted by the hub daemon when a worker sends
/// `{"verb":"request-clarification", ...}` over the hub socket — surfaces
/// the question to the orchestrator via the same events stream every other
/// transition rides on. The original question text is folded (whitespace →
/// underscores) and truncated so the line stays single-record and bounded,
/// then the trailing `…` marker tells operators reading the log that the
/// full body lives in the worker's `.shelbi/messages/<task-id>.log` thread
/// (the daemon never persists clarification text — it's hands-off
/// orchestration metadata).
pub fn append_clarification_event(question_id: &str, task_id: &str, text: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let question_id = sanitize_field(question_id);
    let task_id = sanitize_field(task_id);
    let truncated = sanitize_reason(&truncate_with_ellipsis(text, CLARIFICATION_TEXT_BUDGET));
    append_event_line(&format!(
        "{ts} question={question_id} task={task_id} kind=clarification text={truncated}"
    ))
}

/// Maximum chars of the clarification question we copy into the event line.
/// Bounded so a verbose question doesn't blow the line past readable
/// terminals; the full text lives in the orchestrator's message log thread
/// anyway. Picked at 120 — long enough to be informative in `events tail`,
/// short enough to stay on one screen-line in a typical 200-col terminal
/// after the timestamp + key/value prefixes.
const CLARIFICATION_TEXT_BUDGET: usize = 120;

/// Truncate `s` to at most `max` *chars* (not bytes — we never want to
/// split a UTF-8 codepoint), appending a single `…` marker when content
/// was elided so downstream readers see the truncation explicitly.
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4) + 1);
    for (count, ch) in s.chars().enumerate() {
        if count >= max {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

/// Append a task transition line to `~/.shelbi/events.log` using the
/// workflow-aware line shape from `Plans/workflows.md` §10:
///
/// ```text
/// <rfc3339> task=<id> workflow=<name> <from> -> <to> reason=<short> from_category=<cat> to_category=<cat>
/// ```
///
/// Shares the file with workspace events; the orchestrator distinguishes the
/// two by the `task=` vs `workspace=` prefix.
///
/// `workflow` is the name from the task's frontmatter (typically
/// `task.workflow_or_default()`); passing `""` is treated as the default
/// workflow so callers that haven't yet plumbed through the lookup don't
/// emit a malformed line. `<from>` / `<to>` are the column-status ids
/// (lowercase) and the `from_category` / `to_category` annotations are
/// derived from [`Column::category`] so reaction rules can match
/// semantically without re-reading the workflow YAML.
///
/// `reason` should be a single short token (whitespace/newlines are folded
/// to underscores so the line stays parseable).
pub fn append_task_event(
    task_id: &str,
    workflow: &str,
    from: Column,
    to: Column,
    reason: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let task_id = sanitize_field(task_id);
    let reason = sanitize_reason(reason);
    let workflow_name = if workflow.trim().is_empty() {
        DEFAULT_WORKFLOW_NAME.to_string()
    } else {
        sanitize_field(workflow)
    };
    let from_category = from.category();
    let to_category = to.category();
    append_event_line(&format!(
        "{ts} task={task_id} workflow={workflow_name} {from} -> {to} \
         reason={reason} from_category={from_category} to_category={to_category}"
    ))
}

/// Append `<rfc3339> project=<name> <action> reason=<reason>` to
/// `~/.shelbi/events.log`. Use for project-scoped lifecycle events
/// (currently just `closed` from the palette's quit-project action) that
/// aren't task or workspace transitions but should still surface in the
/// activity feed.
pub fn append_project_event(project: &str, action: &str, reason: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let action = sanitize_reason(action);
    let reason = sanitize_reason(reason);
    append_event_line(&format!("{ts} project={project} {action} reason={reason}"))
}

/// Append `<rfc3339> contextstore space=<space> machine=<machine> status=<status> detail=<detail>`
/// to `~/.shelbi/events.log`. Use this to record cross-machine ContextStore
/// sync attempts run after a remote workspace hands off for review, so the user
/// (and the orchestrator) can see when a workspace's `cstore` writes did — or
/// did not — make it back to the hub copy.
///
/// `detail` is folded to a single token (whitespace → underscores) so the
/// line stays parseable; pass the short rsync stderr excerpt or a status label.
pub fn append_contextstore_event(
    space: &str,
    machine: &str,
    status: &str,
    detail: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let space = sanitize_field(space);
    let machine = sanitize_field(machine);
    let status = sanitize_reason(status);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} contextstore space={space} machine={machine} status={status} detail={detail}"
    ))
}

/// Append `<rfc3339> mode=zen <prev> -> <new> reason=<source>` to
/// `~/.shelbi/events.log`. The orchestrator's tail watches this line shape
/// to react to Zen Mode toggles without re-reading `state.json`. Sources
/// are short tokens identifying the toggle path (`user:cli`, `user:hotkey`,
/// `system:crash-recovery`); whitespace folds to underscores so the line
/// stays parseable.
pub fn append_zen_mode_event(prev: &str, new: &str, source: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let prev = sanitize_reason(prev);
    let new = sanitize_reason(new);
    let source = sanitize_reason(source);
    append_event_line(&format!("{ts} mode=zen {prev} -> {new} reason={source}"))
}

/// Append `<rfc3339> zen-dryrun task=<id> action=<action> detail=<detail>`
/// to `~/.shelbi/events.log`. Emitted by `shelbi zen dry-run` for every
/// would-have decision it makes — distinct prefix so the activity feed
/// can render dry-run rows with their own visual tag and so `grep
/// zen-dryrun` over the log isolates a preview run from real activity.
///
/// `action` and `detail` collapse whitespace to underscores so the line
/// stays a single parseable record (same convention as `append_task_event`).
pub fn append_zen_dryrun_event(task_id: &str, action: &str, detail: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let task_id = sanitize_field(task_id);
    let action = sanitize_reason(action);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} zen-dryrun task={task_id} action={action} detail={detail}"
    ))
}

/// Append `<rfc3339> project=<name> heartbeat zen_eligible=<N>
/// idle_workspaces=<M>` to `~/.shelbi/events.log`. The hub-side poller emits
/// this on its configured `heartbeat` cadence (see
/// `shelbi_core::HeartbeatConfig`) so the orchestrator's `events tail --follow`
/// watch has a guaranteed recurring trigger even when the board is otherwise
/// silent. Heartbeats are filtered out of the human-facing activity feed by
/// default — they're a wake-up signal, not user-facing news.
///
/// The two trailing counts are computed fresh at emit time:
/// - `zen_eligible` — how many `backlog`-category tasks `shelbi zen scan`
///   would return right now (mechanical eligibility only).
/// - `idle_workspaces` — workspaces with no active-category task assigned.
///
/// Together they're the safety net for a skipped post-merge scan: a heartbeat
/// with `zen_eligible > 0` and `idle_workspaces > 0` forces the orchestrator
/// back into the scan loop. The tokens are appended after the `heartbeat`
/// keyword so existing parsers that key off the leading `project=<name>
/// heartbeat` prefix keep working.
pub fn append_heartbeat_event(
    project: &str,
    zen_eligible: usize,
    idle_workspaces: usize,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    append_event_line(&format!(
        "{ts} project={project} heartbeat zen_eligible={zen_eligible} idle_workspaces={idle_workspaces}"
    ))
}

/// Append `<rfc3339> dispatch task=<id> workspace=<name> status=<status> detail=<detail>`
/// to `~/.shelbi/events.log`. Use this to surface dispatch-time anomalies
/// (e.g. the initial prompt was pasted but Enter never landed) that aren't
/// state transitions but still need to show up in `shelbi events tail` so the
/// orchestrator (and the user) sees them at the moment they happen.
///
/// Detail is a single short token; whitespace folds to underscores so the
/// line stays parseable.
pub fn append_dispatch_event(
    task_id: &str,
    workspace: &str,
    status: &str,
    detail: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let task_id = sanitize_field(task_id);
    let workspace = sanitize_field(workspace);
    let status = sanitize_reason(status);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} dispatch task={task_id} workspace={workspace} status={status} detail={detail}"
    ))
}

/// Append `<rfc3339> workspace=<name> pane_alive=<bool> reason=<short>` to
/// `~/.shelbi/events.log`. Emitted by the `shelbi open --as-pane`
/// wrapper when its agent subprocess exits (any reason — clean exit,
/// signal, tmux teardown) so the orchestrator's reaction rules can fire
/// on a pane death.
///
/// `reason` is folded to a single short token (whitespace → underscores) so
/// the line stays parseable.
///
/// Phase 3 of the Worker → Orchestrator Communication feature: hub workers
/// emit pane_alive lines through the daemon socket so a single writer
/// (the daemon) owns the events.log file. The socket write is tried first
/// with one 500ms retry; on persistent failure (daemon down, socket gone)
/// the call falls back to a direct `O_APPEND` write to events.log —
/// POSIX guarantees atomicity for writes ≤ PIPE_BUF and the line is well
/// under that, so the degraded path is correct even with concurrent
/// appenders. The loss of single-writer property is the documented
/// degraded-mode tradeoff (see `Plans/worker-orchestrator-communication.md`
/// §3).
pub fn append_workspace_pane_event(
    workspace: &str,
    alive: bool,
    reason: &str,
) -> Result<()> {
    let workspace = sanitize_field(workspace);
    let reason = sanitize_reason(reason);
    let body = format!("workspace={workspace} pane_alive={alive} reason={reason}");
    emit_event_body(&body)
}

/// Append `<rfc3339> rebase task=<id> workspace=<name> branch=<branch> status=<status> detail=<detail>`
/// to `~/.shelbi/events.log`. Emitted by the poller's review-marker handler
/// when it auto-rebases a workspace's branch onto the project's default branch
/// — so the user (and `shelbi events tail`) can see whether the rebase
/// succeeded, was a no-op (`up-to-date`), conflicted (`conflict`, worktree
/// returned to a clean pre-rebase state), or was skipped (`skipped`, e.g.
/// missing default branch ref or dirty worktree).
///
/// Detail is a single short token (short shas, conflict excerpt, or reason
/// snippet); whitespace folds to underscores so the line stays parseable.
pub fn append_rebase_event(
    task_id: &str,
    workspace: &str,
    branch: &str,
    status: &str,
    detail: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let task_id = sanitize_field(task_id);
    let workspace = sanitize_field(workspace);
    let branch = sanitize_reason(branch);
    let status = sanitize_reason(status);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} rebase task={task_id} workspace={workspace} branch={branch} status={status} detail={detail}"
    ))
}

/// Append `<rfc3339> <body>` to `~/.shelbi/events.log`. Used by the hub
/// daemon (`shelbi daemon`) for `event`-verb messages received over the
/// Unix socket — the worker hands us a pre-formatted body line (e.g.
/// `workspace=delta pane_alive=false reason=signal:SIGHUP`) and the daemon
/// is the single authority on the timestamp prefix so ordering matches the
/// arrival order, not the worker's clock.
///
/// `body` must not contain a newline — the line shape is one event per
/// line and an embedded `\n` would inject a second (likely malformed)
/// record. Callers should reject newlines before calling this.
pub fn append_external_event(body: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    append_event_line(&format!("{ts} {body}"))
}

/// How long to wait for the daemon to read our line before giving up on
/// a single send attempt. The daemon's `handle_client` is one-line-per-
/// connection in steady state and the writes are tiny — this is generous
/// for a healthy daemon and short enough that a wedged one doesn't stall
/// pane teardown.
const SOCKET_EMIT_WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// Single retry delay between the first and second socket attempt. Matches
/// the agent-instructions guidance (one retry after 500ms) so the hub-side
/// emit path behaves the same as the agent's shell-out — keeps debugging
/// across emit sites uniform.
const SOCKET_EMIT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// The ack line the daemon writes back on the same connection after it
/// has successfully processed one inbound frame. Shared between the
/// daemon (writer) and every socket client (reader): a client that reads
/// this before reporting success gets a real delivery guarantee — a
/// daemon killed between `accept()` and dispatch never acks, so the
/// client-side file fallback fires instead of the event silently
/// vanishing. Shell clients see a literal `ok` echoed by `nc`.
pub const DAEMON_ACK: &[u8] = b"ok\n";

/// How long to wait for the daemon's [`DAEMON_ACK`] after our write.
/// Longer than the write timeout because the daemon does real IO
/// (events.log append) before acking; still short enough that a wedged
/// daemon doesn't stall pane teardown beyond the retry budget.
const SOCKET_EMIT_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// Emit a pre-formatted event body, preferring the hub daemon socket and
/// falling back to a direct `O_APPEND` write of the timestamped line to
/// `events.log` if the socket is unreachable.
///
/// **Preferred path:** open `hub_socket_path()`, send one
/// `{"verb":"event","line":"<body>"}` line, then wait for the daemon's
/// [`DAEMON_ACK`] confirming the append happened. The daemon prepends
/// the timestamp and appends the line — same shape we'd produce locally.
/// On `ConnectionRefused` / `NotFound` (or any other write error) we wait
/// 500ms and retry once. If both attempts fail the **degraded-mode
/// fallback** writes the timestamped line directly to events.log; we
/// lose the single-writer property but the line still lands and any
/// downstream tail keeps working. This is the hub-side mirror of the
/// fallback paragraph in the developer agent instructions.
///
/// `body` is the trailing portion that lands after `<rfc3339> `; callers
/// have already sanitized whitespace. Embedded newlines would tear the
/// record, so we reject them up front — the daemon rejects them too.
pub fn emit_event_body(body: &str) -> Result<()> {
    if body.contains('\n') || body.contains('\r') {
        return Err(shelbi_core::Error::Other(
            "emit_event_body: body may not contain newlines".into(),
        ));
    }
    let sock = hub_socket_path()?;
    match try_emit_via_socket(&sock, body) {
        Ok(()) => return Ok(()),
        Err(e) if !should_retry(&e) => {
            // No socket file at all (NotFound) — daemon isn't installed
            // or hasn't been started yet. Skip the retry sleep entirely;
            // 500ms isn't going to summon a daemon and the pane teardown
            // path doesn't want to wait on it.
        }
        Err(_) => {
            std::thread::sleep(SOCKET_EMIT_RETRY_DELAY);
            if try_emit_via_socket(&sock, body).is_ok() {
                return Ok(());
            }
        }
    }
    // Degraded-mode fallback: the daemon is down or the socket is gone.
    // POSIX guarantees writes ≤ PIPE_BUF are atomic under O_APPEND, so
    // concurrent appenders still interleave whole lines.
    append_external_event(body)
}

/// True for socket errors that might be transient (daemon momentarily not
/// reading, accept-queue full, etc.) — worth one 500ms retry. `NotFound`
/// is excluded: if the socket file doesn't exist, no daemon will be
/// listening to it in 500ms either.
fn should_retry(err: &std::io::Error) -> bool {
    !matches!(err.kind(), std::io::ErrorKind::NotFound)
}

/// One socket attempt: connect → send one JSON line → half-close → read
/// the daemon's [`DAEMON_ACK`]. Returns the underlying IO error on any
/// step so the caller can decide whether to retry, fall back, or
/// surface. Write timeout is short on purpose — the daemon's hot path
/// is tiny.
///
/// The ack read is what makes this a delivery guarantee rather than a
/// hope: a kernel-buffered `write_all` succeeds even against a daemon
/// that's already exiting, so before the ack existed a SIGTERM'd daemon
/// could eat the event *and* convince us not to fall back. No ack (EOF,
/// timeout, wrong bytes) → error → the caller's file fallback fires.
/// Worst case is a duplicated event (daemon appended, then died before
/// acking) — strictly better than a lost one.
fn try_emit_via_socket(sock: &std::path::Path, body: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(sock)?;
    let _ = stream.set_write_timeout(Some(SOCKET_EMIT_WRITE_TIMEOUT));
    let msg = serde_json::json!({
        "verb": "event",
        "line": body,
    });
    let mut payload = serde_json::to_vec(&msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    payload.push(b'\n');
    stream.write_all(&payload)?;
    // Half-close on our side so the daemon sees EOF on the read loop —
    // it's done with this connection after one line. The read side stays
    // open for the ack.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(SOCKET_EMIT_ACK_TIMEOUT));
    let mut ack = [0u8; DAEMON_ACK.len()];
    stream.read_exact(&mut ack)?;
    if ack != *DAEMON_ACK {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unexpected daemon ack",
        ));
    }
    Ok(())
}

/// Open `events.log` with O_APPEND and write one terminated line in a
/// single `write_all` call. POSIX guarantees that writes <= PIPE_BUF
/// (4096B) under O_APPEND are atomic relative to other appenders, so
/// concurrent writes from the CLI and the poller interleave whole lines
/// rather than tearing. We must hand the kernel one finished buffer —
/// `writeln!(f, …)` would split the line into separate `write` syscalls
/// per format fragment, which the OS is free to interleave.
fn append_event_line(line: &str) -> Result<()> {
    // Last-line defense against record injection: an embedded newline (from
    // an unsanitized interpolated field) would split one logical event into
    // two physical lines, the second of which a downstream parser reads as a
    // forged record. Every `append_*` helper sanitizes its fields, but this
    // shared sink rejects the tear outright so a future caller that forgets
    // can't punch a hole. Mirrors the guard `emit_event_body` already applies.
    if line.contains('\n') || line.contains('\r') {
        return Err(shelbi_core::Error::Other(
            "append_event_line: line may not contain newlines".into(),
        ));
    }
    let path = events_log_path()?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    maybe_rotate_events_log(&path, EVENTS_LOG_MAX_BYTES);
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(buf.as_bytes())?;
    Ok(())
}

/// Size ceiling for `events.log` before an append rotates it. The log is
/// append-only and otherwise grows without bound; rotating at ~8 MiB
/// bounds disk use and keeps the tail-scan readers cheap (the CLI's
/// crash-recovery check reads only the last 64 KiB — see
/// `commands::status`). One `.1` generation is kept; older history is
/// dropped on the next rotation.
const EVENTS_LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Best-effort size-based rotation: if `path` (the current `events.log`)
/// is at least `max_bytes`, rename it to `events.log.1` — replacing any
/// prior generation — so the next append starts a fresh file. Racy by
/// design under concurrent appenders: whichever writer wins the rename
/// rotates, and a loser that then stats the fresh small file simply skips
/// (which is correct). Every error is swallowed — a rotation hiccup must
/// never block an event write, which is the caller's actual job.
fn maybe_rotate_events_log(path: &std::path::Path, max_bytes: u64) {
    match fs::metadata(path) {
        Ok(m) if m.len() >= max_bytes => {}
        _ => return,
    }
    let rotated = path.with_extension("log.1");
    let _ = fs::rename(path, &rotated);
}

fn sanitize_reason(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// Sanitize an *identifier* field before interpolating it into an events.log
/// line. Unlike [`sanitize_reason`] (which only folds whitespace, keeping
/// free-text readable), identifiers — task ids from filenames, workflow
/// names from user-editable YAML frontmatter, workspace/project/space/machine
/// names from config that may be synced from a checked-out repo — are pinned
/// to a strict `[A-Za-z0-9._:-]` allowlist. Every other byte, including
/// whitespace, `\n`/`\r`, and `=`, folds to `_`.
///
/// This closes the record-injection gap: a raw newline in an id would
/// otherwise write a second, attacker-shaped line the orchestrator's reaction
/// rules act on, and stray whitespace would shift the space-delimited token
/// positions every prefix/position-keyed parser depends on.
fn sanitize_field(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-workspace-status-test-{}-{}",
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
    fn events_log_rotates_when_over_size() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rotated = path.with_extension("log.1");

        // Under the (test) threshold: no rotation, file untouched.
        std::fs::write(&path, "small\n").unwrap();
        maybe_rotate_events_log(&path, 1024);
        assert!(path.exists());
        assert!(!rotated.exists());

        // At/over the threshold: current log renames to `.1`, leaving room
        // for a fresh file on the next append.
        std::fs::write(&path, "x".repeat(2048)).unwrap();
        maybe_rotate_events_log(&path, 1024);
        assert!(!path.exists(), "over-size log should be rotated away");
        assert!(rotated.exists(), "rotated generation should exist");

        // A second rotation replaces the prior `.1` rather than erroring.
        std::fs::write(&path, "y".repeat(2048)).unwrap();
        maybe_rotate_events_log(&path, 1024);
        assert!(rotated.exists());
        assert_eq!(std::fs::read(&rotated).unwrap(), b"y".repeat(2048));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn parses_each_marker() {
        assert_eq!(
            parse_pane_title_state("foo shelbi:working"),
            Some(WorkspaceState::Working)
        );
        // `idle` from the wire format surfaces as awaiting_input — that's
        // what the user actually wants to see in the UI when claude is
        // sitting at a prompt.
        assert_eq!(
            parse_pane_title_state("shelbi:idle"),
            Some(WorkspaceState::AwaitingInput)
        );
        assert_eq!(
            parse_pane_title_state("claude · shelbi:blocked"),
            Some(WorkspaceState::Blocked)
        );
        // `review` is the explicit completion handoff. For status-file
        // purposes it collapses to AwaitingInput (claude is sitting at a
        // prompt); the kanban move side-effect is handled by the poller.
        assert_eq!(
            parse_pane_title_state("shelbi:review"),
            Some(WorkspaceState::AwaitingInput)
        );
    }

    #[test]
    fn marker_parser_distinguishes_idle_from_review() {
        assert_eq!(
            parse_pane_title_marker("shelbi:idle"),
            Some(PaneMarker::Idle)
        );
        assert_eq!(
            parse_pane_title_marker("shelbi:review"),
            Some(PaneMarker::Review)
        );
        assert_eq!(
            parse_pane_title_marker("shelbi:working"),
            Some(PaneMarker::Working)
        );
        assert_eq!(
            parse_pane_title_marker("shelbi:blocked"),
            Some(PaneMarker::Blocked)
        );
        assert!(parse_pane_title_marker("shelbi:bogus").is_none());
    }

    #[test]
    fn ignores_unknown_or_missing_markers() {
        assert!(parse_pane_title_state("zsh").is_none());
        assert!(parse_pane_title_state("shelbi:bogus").is_none());
        assert!(parse_pane_title_state("").is_none());
    }

    #[test]
    fn marker_match_is_anchored_to_a_token_boundary() {
        // `rfind("shelbi:")` used to match a substring — `myshelbi:working`
        // or a `shelbi:review` embedded mid-title (e.g. inside a task name
        // the agent prints) parsed as a live marker. The parser now anchors
        // on the trailing whitespace-delimited token starting with
        // `shelbi:`, so a non-boundary occurrence is ignored.
        assert!(
            parse_pane_title_marker("myshelbi:working").is_none(),
            "longer word ending in shelbi:… must not match"
        );
        assert!(
            parse_pane_title_marker("fix shelbi:review parser").is_none(),
            "a shelbi:… token that isn't the trailing token must not match"
        );
        // The legitimate trailing-token forms still parse.
        assert_eq!(
            parse_pane_title_marker("claude · shelbi:working"),
            Some(PaneMarker::Working)
        );
        assert_eq!(
            parse_pane_title_marker("shelbi:review"),
            Some(PaneMarker::Review)
        );
    }

    #[test]
    fn parses_last_marker_when_multiple_present() {
        // OSC re-writes append a fresh title segment; take the right-most
        // marker so a stale `shelbi:idle` earlier in the buffer doesn't
        // mask a current `shelbi:working`.
        assert_eq!(
            parse_pane_title_state("shelbi:idle  shelbi:working"),
            Some(WorkspaceState::Working)
        );
    }

    #[test]
    fn parses_marker_followed_by_terminator_bytes() {
        // Some terminal stacks (or our own OSC capture path) can leave a
        // BEL or stray newline trailing the marker. The parser should
        // ignore those rather than failing the marker match.
        assert_eq!(
            parse_pane_title_state("shelbi:working\u{0007}"),
            Some(WorkspaceState::Working)
        );
    }

    #[test]
    fn workspace_state_serializes_snake_case() {
        let s = serde_yaml::to_string(&WorkspaceState::AwaitingInput).unwrap();
        assert!(s.trim().ends_with("awaiting_input"), "got {s:?}");
    }

    #[test]
    fn save_and_load_workspace_status_roundtrip() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let now = Utc::now();
        let status = WorkspaceStatus {
            workspace: "alpha".into(),
            current_task: Some("fix-thing".into()),
            state: WorkspaceState::Working,
            last_transition: now,
            last_seen: now,
        };
        save_workspace_status(&status).unwrap();
        let path = workspace_status_path("alpha").unwrap();
        assert!(path.exists());
        let back = load_workspace_status("alpha").unwrap().unwrap();
        assert_eq!(back.workspace, "alpha");
        assert_eq!(back.state, WorkspaceState::Working);
        assert_eq!(back.current_task.as_deref(), Some("fix-thing"));

        // Missing workspace returns None, not an error — the sidebar uses
        // this to bootstrap fresh on first observation.
        assert!(load_workspace_status("ghost").unwrap().is_none());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_external_event_prepends_timestamp() {
        // Round-trip a daemon-supplied body and confirm the file picks up
        // the RFC3339 timestamp prefix the daemon contract guarantees,
        // followed by the verbatim body. The body is preserved exactly —
        // sanitization is the caller's job, not the storage layer's.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_external_event("workspace=delta pane_alive=false reason=signal:SIGHUP").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();

        // Leading token is a parseable RFC3339 timestamp.
        let ts_str = line.split_whitespace().next().unwrap();
        DateTime::parse_from_rfc3339(ts_str)
            .unwrap_or_else(|e| panic!("expected RFC3339 prefix in `{line}`: {e}"));
        // Body is preserved verbatim after the timestamp + single space.
        assert!(
            line.ends_with(" workspace=delta pane_alive=false reason=signal:SIGHUP"),
            "line: {line}",
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn hub_socket_path_defaults_under_home() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");
        assert_eq!(hub_socket_path().unwrap(), home.join("hub.sock"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn hub_socket_path_env_override_wins() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let override_path = std::env::temp_dir().join("shelbi-hub-override.sock");
        std::env::set_var("SHELBI_HUB_SOCK", &override_path);
        assert_eq!(hub_socket_path().unwrap(), override_path);
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_workspace_event_writes_transition_line() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_workspace_event("alpha", None, WorkspaceState::Working).unwrap();
        append_workspace_event("alpha", Some(WorkspaceState::Working), WorkspaceState::AwaitingInput)
            .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("workspace=alpha"));
        assert!(lines[0].contains("none -> working"));
        assert!(lines[1].contains("working -> awaiting_input"));

        std::env::remove_var("SHELBI_HOME");
    }

    /// Phase 3 of Worker → Orchestrator Communication: emit_event_body
    /// is the hub-side mirror of the agent's `nc -U $SHELBI_HUB_SOCK`
    /// path. When the daemon is up, it must hit the socket and the
    /// daemon's append (timestamp + body) lands the line — same shape we
    /// produce when we write directly. Stand up a minimal in-test
    /// listener that mimics `shelbi daemon`'s `event`-verb handler and
    /// confirm the line lands via that path, not via the fallback.
    #[test]
    fn emit_event_body_prefers_socket_when_daemon_is_up() {
        use std::io::BufRead;
        use std::os::unix::net::UnixListener;
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // macOS limits Unix-socket paths to ~104 bytes (SUN_LEN). The
        // `/var/folders/..` temp dir under fresh_home() can blow past
        // that with the test-name suffix, so we pin a short
        // `/tmp/shelbi-tN.sock`-style path via the env override. The
        // production path (`~/.shelbi/hub.sock`) is well under the
        // limit; this is purely a test-environment concession.
        let sock = short_test_socket("dn-up");
        std::env::set_var("SHELBI_HUB_SOCK", &sock);

        // The minimal daemon: accept one connection, read one JSON line,
        // append `<rfc3339> <line>` to events.log, ack, exit. Mirrors
        // `shelbi-cli::commands::daemon::handle_client`/`handle_event`
        // without pulling the whole binary into this crate.
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let log_path = events_log_path().unwrap();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let msg: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            let body = msg["line"].as_str().unwrap().to_string();
            append_external_event(&body).unwrap();
            stream.write_all(DAEMON_ACK).unwrap();
        });

        emit_event_body("workspace=alpha pane_alive=false reason=signal:SIGTERM").unwrap();
        daemon.join().unwrap();

        let log = std::fs::read_to_string(&log_path).unwrap();
        let line = log.lines().next().expect("daemon should have appended");
        assert!(
            line.ends_with(" workspace=alpha pane_alive=false reason=signal:SIGTERM"),
            "line: {line}"
        );
        // Exactly one line — the fallback must NOT have fired alongside
        // the daemon append (that would duplicate the event).
        assert_eq!(log.lines().count(), 1, "log: {log}");

        let _ = std::fs::remove_file(&sock);
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_HOME");
    }

    /// A daemon that accepts and reads the frame but dies before acking
    /// must NOT count as delivery — that's the restart window where
    /// `write_all` succeeds against a kernel buffer nobody will ever
    /// dispatch. The client has to notice the missing ack and fire the
    /// file fallback so the event still lands (exactly once).
    #[test]
    fn emit_event_body_falls_back_when_daemon_never_acks() {
        use std::io::BufRead;
        use std::os::unix::net::UnixListener;
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let sock = short_test_socket("dn-noack");
        std::env::set_var("SHELBI_HUB_SOCK", &sock);

        // Malicious-restart stand-in: read the client's line, then close
        // without acking (and without appending). Serve both the first
        // attempt and the 500ms retry so each fails fast on EOF.
        let listener = UnixListener::bind(&sock).unwrap();
        let daemon = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                // Drop: connection closes with no ack written.
            }
        });

        emit_event_body("workspace=echo pane_alive=false reason=exit:1").unwrap();
        daemon.join().unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().expect("fallback should have appended");
        assert!(
            line.ends_with(" workspace=echo pane_alive=false reason=exit:1"),
            "line: {line}"
        );
        assert_eq!(log.lines().count(), 1, "log: {log}");

        let _ = std::fs::remove_file(&sock);
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_HOME");
    }

    /// Build a short Unix-socket path under `/tmp`. macOS' SUN_LEN is
    /// ~104 bytes; the deep `/var/folders/.../shelbi-workspace-status-
    /// test-…` paths fresh_home() returns can overflow that. `/tmp/`
    /// keeps us well under, and the PID + tag still gives parallel
    /// isolation across test binaries.
    fn short_test_socket(tag: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/shb-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Phase 3 fallback: when no daemon is listening on the configured
    /// socket, `emit_event_body` falls through to a direct timestamped
    /// append to events.log. Same line shape as the socket path so a
    /// downstream tail can't tell which path produced the line — that's
    /// the whole point of the degraded mode.
    #[test]
    fn emit_event_body_falls_back_to_file_when_daemon_is_down() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Pin a short, definitely-absent socket path so we hit
        // NotFound deterministically (and dodge the SUN_LEN trap that
        // would otherwise hide the fast-path assertion below).
        let sock = short_test_socket("dn-down");
        std::env::set_var("SHELBI_HUB_SOCK", &sock);
        // No listener, no file at hub_socket_path — the connect should
        // fail with NotFound and we skip the retry sleep entirely.
        let started = std::time::Instant::now();
        emit_event_body("workspace=delta pane_alive=false reason=exit:0").unwrap();
        // The NotFound fast-path must skip the 500ms retry; if it
        // didn't, this assertion would catch the regression. Tight bound
        // (< 400ms) leaves headroom for CI scheduling jitter without
        // letting an accidental sleep slip past.
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "fallback took too long; retry sleep probably wasn't skipped: {:?}",
            started.elapsed(),
        );

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        assert!(
            line.ends_with(" workspace=delta pane_alive=false reason=exit:0"),
            "line: {line}"
        );

        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_HOME");
    }

    /// Embedded newlines in the body would tear the event line across
    /// two records (and the daemon would reject it server-side too).
    /// Reject up front so callers see the error at the emit site, not
    /// downstream in some unrelated parser.
    #[test]
    fn emit_event_body_rejects_embedded_newlines() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");

        let err = emit_event_body("workspace=alpha\nfoo=bar").unwrap_err();
        assert!(err.to_string().contains("newlines"), "{err}");
        // Nothing landed in the log — the rejection happens before any
        // socket/file IO.
        assert!(!events_log_path().unwrap().exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_workspace_pane_event_writes_lifecycle_line() {
        // The wrapper writes this line whenever its agent subprocess
        // exits (any reason — clean exit, signal, kill-window, child
        // crash). The orchestrator's reaction rules key on the
        // `workspace=` prefix + `pane_alive=` field, so pin both.
        // `reason` whitespace folds to underscores so the line stays a
        // single parseable record alongside the other event shapes.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_workspace_pane_event("alpha", false, "signal:SIGTERM").unwrap();
        append_workspace_pane_event("bravo", false, "exit:0").unwrap();
        append_workspace_pane_event("charlie", false, "claude exited normally").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "log: {log}");

        assert!(lines[0].contains(" workspace=alpha "), "line: {}", lines[0]);
        assert!(lines[0].contains(" pane_alive=false "), "line: {}", lines[0]);
        assert!(lines[0].ends_with(" reason=signal:SIGTERM"), "line: {}", lines[0]);

        assert!(lines[1].ends_with(" reason=exit:0"), "line: {}", lines[1]);

        // Whitespace in reason folds to underscores so the field stays a
        // single token (matches the sanitize_reason contract used by the
        // other append_… helpers).
        assert!(
            lines[2].ends_with(" reason=claude_exited_normally"),
            "line: {}",
            lines[2],
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_message_event_writes_push_line() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_message_event("m-123", "fix-login").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        // Leading RFC3339 timestamp keeps the line uniform with the rest of
        // events.log (and parseable by the activity feed / `events tail`).
        assert!(DateTime::parse_from_rfc3339(line.split_whitespace().next().unwrap()).is_ok());
        assert!(line.contains("message=m-123"));
        assert!(line.contains("task=fix-login"));
        assert!(line.ends_with("push=ok"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_message_ack_event_writes_worker_and_timeout_lines() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_message_ack_event("m-1", "fix-login", "worker").unwrap();
        append_message_ack_event("m-1", "fix-login", "timeout").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        // Each line is `<ts> message=<id> task=<id> ack=<kind>` and the
        // ack kind must round-trip verbatim so the activity feed can
        // distinguish worker-confirmed from synthesized timeouts.
        assert!(
            lines[0].contains(" message=m-1 task=fix-login ack=worker"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains(" message=m-1 task=fix-login ack=timeout"),
            "{}",
            lines[1]
        );
        for line in &lines {
            let ts = line.split_whitespace().next().unwrap();
            DateTime::parse_from_rfc3339(ts).unwrap();
        }

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_clarification_event_truncates_long_text_and_folds_whitespace() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // 240 chars — well past the 120-char budget, so the line should
        // get a trailing `…` and stop expanding events.log into a
        // multi-screen blob. Whitespace folds to `_` so the line stays
        // parseable as a single space-separated record.
        let q = "lorem ipsum dolor sit amet ".repeat(10);
        append_clarification_event("q-001", "feat-X", &q).unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        // Shape: `<ts> question=q-001 task=feat-X kind=clarification text=…`
        assert!(line.contains(" question=q-001 "), "{line}");
        assert!(line.contains(" task=feat-X "), "{line}");
        assert!(line.contains(" kind=clarification "), "{line}");
        assert!(line.contains(" text="), "{line}");
        assert!(line.ends_with('…'), "expected ellipsis tail: {line}");
        // No internal whitespace inside the text= token — sanitize_reason
        // collapsed it. The 120-char body + one ellipsis lands well under
        // a typical terminal width.
        let text = line.split(" text=").nth(1).unwrap();
        assert!(
            !text.contains(' '),
            "internal whitespace not folded: {text}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_clarification_event_keeps_short_text_intact_and_no_ellipsis() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_clarification_event("q-002", "fix-bug", "use http or https?").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        // Short question fits entirely; sanitize folds the two spaces in
        // the question to underscores so the record stays single-line.
        assert!(line.ends_with(" text=use_http_or_https?"), "{line}");
    }

    #[test]
    fn truncate_with_ellipsis_respects_char_boundary() {
        // Non-ASCII content must split cleanly on character boundaries —
        // a byte-level slice could panic mid-codepoint. We construct a
        // 130-char string of 3-byte chars and assert the output is exactly
        // 120 chars + the ellipsis.
        let s: String = "é".repeat(130);
        let out = truncate_with_ellipsis(&s, 120);
        assert_eq!(out.chars().count(), 121, "chars: {}", out.chars().count());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn append_task_event_round_trips() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event(
            "fix-login",
            "default",
            Column::Todo,
            Column::InProgress,
            "assigned",
        )
        .unwrap();
        append_task_event(
            "fix-login",
            "default",
            Column::InProgress,
            Column::Review,
            "workspace_review",
        )
        .unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);

        // Each line must split cleanly back into its fields:
        // `<ts> task=<id> workflow=<name> <from> -> <to> reason=<r>
        //  from_category=<c> to_category=<c>` — see `Plans/workflows.md` §10.
        let parsed: Vec<Vec<&str>> = lines
            .iter()
            .map(|line| line.split(' ').collect::<Vec<&str>>())
            .collect();

        // Timestamp parses as RFC3339.
        for tokens in &parsed {
            let ts = tokens[0];
            chrono::DateTime::parse_from_rfc3339(ts)
                .unwrap_or_else(|_| panic!("not rfc3339: {ts}"));
        }
        assert_eq!(parsed[0][1], "task=fix-login");
        assert_eq!(parsed[0][2], "workflow=default");
        assert_eq!(parsed[0][3], "todo");
        assert_eq!(parsed[0][4], "->");
        assert_eq!(parsed[0][5], "in_progress");
        assert_eq!(parsed[0][6], "reason=assigned");
        assert_eq!(parsed[0][7], "from_category=ready");
        assert_eq!(parsed[0][8], "to_category=active");
        assert_eq!(parsed[1][5], "review");
        assert_eq!(parsed[1][6], "reason=workspace_review");
        assert_eq!(parsed[1][7], "from_category=active");
        assert_eq!(parsed[1][8], "to_category=handoff");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_task_event_uses_default_workflow_when_blank() {
        // Callers that haven't yet plumbed through `task.workflow_or_default()`
        // can pass an empty string and still get a well-formed line — the
        // line never carries `workflow=` with no value.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event("a", "", Column::Todo, Column::Done, "assigned").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        assert!(line.contains(" workflow=default "), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_task_event_emits_workflow_name_verbatim() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event(
            "ship-it",
            "feature-task",
            Column::InProgress,
            Column::Review,
            "workspace:review-marker",
        )
        .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        assert!(line.contains(" workflow=feature-task "), "line: {line}");
        assert!(line.ends_with(" to_category=handoff"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn heartbeat_event_writes_project_scoped_line_with_counts() {
        // Shape: `<ts> project=<name> heartbeat zen_eligible=<N>
        // idle_workspaces=<M>`. No `task=`/`workspace=` prefix on purpose —
        // the orchestrator's tail uses the leading token after the timestamp
        // to dispatch handlers, and the `project=…` form lets a heartbeat
        // live alongside other project-scoped events without colliding. The
        // two counts trail the `heartbeat` keyword so prefix-keyed parsers
        // are unaffected.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_heartbeat_event("myapp", 5, 4).unwrap();
        append_heartbeat_event("myapp", 0, 0).unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].ends_with(" project=myapp heartbeat zen_eligible=5 idle_workspaces=4"),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[1].ends_with(" project=myapp heartbeat zen_eligible=0 idle_workspaces=0"),
            "line: {}",
            lines[1]
        );
        for line in &lines {
            // Timestamp parses as RFC3339 so `--since` filtering works
            // the same way it does for every other event shape.
            let ts = line.split_whitespace().next().unwrap();
            chrono::DateTime::parse_from_rfc3339(ts)
                .unwrap_or_else(|_| panic!("not rfc3339: {ts}"));
        }

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn dispatch_event_writes_distinct_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // `prompt-lost` and `readiness-timeout` are the two dispatch-failure
        // statuses the workspace dispatch path emits (see
        // `shelbi_orchestrator::workspace`); the orchestrator greps for them to
        // recover a dispatch that never reached its workspace.
        append_dispatch_event(
            "fix-login",
            "alpha",
            "prompt-lost",
            "no submit signal after retry",
        )
        .unwrap();
        append_dispatch_event("build-thing", "charlie", "readiness-timeout", "input box not ready")
            .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        // Shape: `<ts> dispatch task=<id> workspace=<name> status=<s> detail=<d>`.
        // The `dispatch` prefix lets `shelbi events tail` show it without
        // colliding with task=... or workspace=... lines.
        let line = lines[0];
        assert!(line.contains(" dispatch task=fix-login "), "line: {line}");
        assert!(line.contains(" workspace=alpha "), "line: {line}");
        assert!(line.contains(" status=prompt-lost "), "line: {line}");
        // Whitespace in detail folds to underscores so the line stays parseable.
        assert!(line.ends_with(" detail=no_submit_signal_after_retry"), "line: {line}");
        assert!(lines[1].contains(" status=readiness-timeout "), "line: {}", lines[1]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_dryrun_event_writes_canonical_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_zen_dryrun_event(
            "fix-typo",
            "consider-auto-promote",
            "mechanically eligible",
        )
        .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        let line = lines[0];
        // Prefix lets the activity feed match and render dry-run rows
        // with a distinct visual tag. Detail whitespace folds to
        // underscores so the line stays parseable.
        assert!(line.contains(" zen-dryrun task=fix-typo "), "line: {line}");
        assert!(line.contains(" action=consider-auto-promote "), "line: {line}");
        assert!(line.ends_with(" detail=mechanically_eligible"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_mode_event_writes_canonical_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_zen_mode_event("off", "on", "user:cli").unwrap();
        append_zen_mode_event("on", "paused", "user:hotkey").unwrap();
        append_zen_mode_event("paused", "off", "system:crash-recovery").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        // No project tag — the orchestrator's tail is hub-global and the
        // line is project-implicit. Shape: `<ts> mode=zen <prev> -> <new>
        // reason=<source>`.
        assert!(lines[0].contains(" mode=zen off -> on reason=user:cli"));
        assert!(lines[1].contains(" mode=zen on -> paused reason=user:hotkey"));
        assert!(lines[2].contains(" mode=zen paused -> off reason=system:crash-recovery"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn task_event_sanitizes_whitespace_in_reason() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event("a", "default", Column::Todo, Column::Done, "user moved\nit").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        // The reason newline must not produce a torn line — the line keeps
        // going past `reason=` into the trailing category annotations.
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains(" reason=user_moved_it "),
            "line: {}",
            lines[0]
        );
        assert!(lines[0].ends_with(" to_category=done"), "line: {}", lines[0]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn identifier_fields_cannot_inject_a_second_record() {
        // A task id or workflow name carrying a newline used to write a
        // second, attacker-shaped events.log line (task ids come from
        // filenames, workflow names from user-editable YAML frontmatter).
        // Every identifier field is now allowlist-sanitized, so the whole
        // event stays a single physical line and the injected token folds
        // to underscores.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // The injection payload: a newline followed by a forged
        // `workspace=... pane_alive=false` record the orchestrator would act
        // on. The trailing spaces would also shift token positions.
        let hostile_id = "evil\nworkspace=x pane_alive=false reason=pwned";
        let hostile_workflow = "wf\r\nmode=zen off -> on reason=user:hotkey";
        append_task_event(
            hostile_id,
            hostile_workflow,
            Column::Todo,
            Column::InProgress,
            "assigned",
        )
        .unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        // Exactly one record — the newline never tore the line in two.
        assert_eq!(lines.len(), 1, "log: {log:?}");
        let line = lines[0];
        // No raw injected key survives as its own token.
        assert!(
            !line.contains("pane_alive="),
            "forged record leaked into the line: {line}"
        );
        // The id and workflow appear folded to the allowlist.
        assert!(
            line.contains(" task=evil_workspace_x_pane_alive_false_reason_pwned "),
            "task id not sanitized: {line}"
        );
        assert!(
            line.contains(" workflow=wf__mode_zen_off_-__on_reason_user:hotkey "),
            "workflow not sanitized: {line}"
        );
        // The record still ends with the well-formed category annotations —
        // token positions weren't shifted.
        assert!(line.ends_with(" to_category=active"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_event_line_rejects_embedded_newlines() {
        // Last-line defense: even if a future caller forgets to sanitize a
        // field, the shared sink refuses a line carrying a newline rather
        // than writing a torn/forged second record.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let err = append_event_line("task=a\nworkspace=x pane_alive=false").unwrap_err();
        assert!(err.to_string().contains("newlines"), "{err}");
        assert!(!events_log_path().unwrap().exists());

        let err = append_event_line("task=a\rworkspace=x").unwrap_err();
        assert!(err.to_string().contains("newlines"), "{err}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn concurrent_task_and_workspace_appends_dont_tear() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        const N: usize = 200;
        let task_thread = std::thread::spawn(|| {
            for i in 0..N {
                append_task_event(
                    &format!("t{i:04}"),
                    "default",
                    Column::Todo,
                    Column::InProgress,
                    "assigned",
                )
                .unwrap();
            }
        });
        let workspace_thread = std::thread::spawn(|| {
            for i in 0..N {
                let prev = if i == 0 {
                    None
                } else {
                    Some(WorkspaceState::Working)
                };
                append_workspace_event("alpha", prev, WorkspaceState::AwaitingInput).unwrap();
            }
        });
        task_thread.join().unwrap();
        workspace_thread.join().unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2 * N, "expected {} lines, got {}", 2 * N, lines.len());

        let mut task_lines = 0usize;
        let mut workspace_lines = 0usize;
        for line in &lines {
            // No line should mix prefixes — that would mean an interleaved
            // write tore one record across another.
            assert!(line.contains(" -> "), "malformed: {line:?}");
            let has_task = line.contains(" task=");
            let has_workspace = line.contains(" workspace=");
            assert!(
                has_task ^ has_workspace,
                "torn or unrecognized line: {line:?}"
            );
            if has_task {
                task_lines += 1;
                assert!(line.contains("reason="), "task line missing reason: {line:?}");
            } else {
                workspace_lines += 1;
            }
        }
        assert_eq!(task_lines, N);
        assert_eq!(workspace_lines, N);

        std::env::remove_var("SHELBI_HOME");
    }

    /// Round-trip: `mark_expected_teardown` writes the marker,
    /// `consume_expected_teardown` finds it fresh, returns true, and
    /// removes the file. Second consume finds nothing → false.
    #[test]
    fn expected_teardown_marker_round_trips() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        mark_expected_teardown("alpha").unwrap();
        let marker = expected_teardown_marker_path("alpha").unwrap();
        assert!(marker.exists(), "mark must create the marker file");

        assert!(
            consume_expected_teardown("alpha").unwrap(),
            "fresh marker must consume as true"
        );
        assert!(
            !marker.exists(),
            "consume must remove the marker (one-shot signal)"
        );

        assert!(
            !consume_expected_teardown("alpha").unwrap(),
            "second consume with no marker returns false"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// A stale marker (mtime older than [`EXPECTED_TEARDOWN_MAX_AGE`])
    /// must not suppress: it means an intent was recorded but never
    /// consumed (a mark→SIGKILL race), and this exit is not the one that
    /// intent was talking about. Consume still deletes the stale marker
    /// so it can't leak further forward.
    #[test]
    fn expected_teardown_marker_expires() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        mark_expected_teardown("alpha").unwrap();
        let marker = expected_teardown_marker_path("alpha").unwrap();
        // Rewind mtime past the freshness window via `libc::utimes` —
        // avoids pulling in the `filetime` crate just for one test.
        set_mtime_to(&marker, SystemTime::now() - Duration::from_secs(3600));

        assert!(
            !consume_expected_teardown("alpha").unwrap(),
            "stale marker must not suppress"
        );
        assert!(
            !marker.exists(),
            "stale marker must still be removed on consume so it can't linger"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// Set `path`'s access and modification times to `when`. Test-only —
    /// stdlib doesn't expose a stable mtime setter without opening the
    /// file (`File::set_modified`), which changes size behavior on some
    /// FSes; `utimes(2)` is the historical POSIX path and does exactly
    /// what we need.
    fn set_mtime_to(path: &std::path::Path, when: SystemTime) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let secs = when
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("test uses recent-enough times")
            .as_secs() as libc::time_t;
        let times = [
            libc::timeval {
                tv_sec: secs,
                tv_usec: 0,
            },
            libc::timeval {
                tv_sec: secs,
                tv_usec: 0,
            },
        ];
        let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: cpath owns the null-terminated bytes for the duration
        // of the call; `times` is a valid array of two timevals.
        let rc = unsafe { libc::utimes(cpath.as_ptr(), times.as_ptr()) };
        assert_eq!(rc, 0, "utimes failed: errno={}", std::io::Error::last_os_error());
    }

    /// `clear_expected_teardown` is idempotent: no marker on disk → OK.
    /// With a marker on disk → file is removed → returns OK. Second call
    /// after remove is also OK. Used by the pane wrapper's startup so a
    /// crashed prior lifecycle can't leak its marker into the new run.
    #[test]
    fn clear_expected_teardown_is_idempotent() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // No marker yet → noop OK.
        clear_expected_teardown("alpha").unwrap();

        // Plant a marker, clear it, verify removal.
        mark_expected_teardown("alpha").unwrap();
        let marker = expected_teardown_marker_path("alpha").unwrap();
        assert!(marker.exists());
        clear_expected_teardown("alpha").unwrap();
        assert!(!marker.exists());

        // Second clear on the now-absent marker also OK.
        clear_expected_teardown("alpha").unwrap();

        std::env::remove_var("SHELBI_HOME");
    }
}
