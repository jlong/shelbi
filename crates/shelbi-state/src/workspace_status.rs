//! Workspace state observed from each workspace's tmux pane title.
//!
//! Workspace `.claude/settings.json` hooks emit
//! `shelbi:working|idle|background|blocked` markers via OSC pane-title escapes (see
//! `default_workspace_settings.json.template`); the hub-side sidebar poll loop
//! reads the current pane title with `tmux display-message`, parses the
//! trailing marker, and persists any transition to
//! `~/.shelbi/projects/<project>/workspaces/<name>/status.yaml`.
//!
//! Every path here takes the project as well as the slot name because slot
//! names are only unique *within* a project: two projects can each own a
//! `review`, and they must not share a status row, a teardown marker, or a
//! review-server record. [`crate::workspace_state_migration`] moves what older
//! releases left under the hub-global `~/.shelbi/workspaces/` into place.
//!
//! This module is limited to pane/status *observation*: the pane-title marker
//! vocabulary ([`PaneMarker`] / [`WorkspaceState`]), the persisted
//! [`WorkspaceStatus`], and the hub daemon socket path. The durable event stream
//! those transitions are appended to — `events.log`, its rotation, the logical
//! cursors, and the `append_*_event` producers — lives in [`crate::event_log`].
//!
//! Authoritative state stays on the hub: workspaces themselves only emit
//! markers; they don't own these files.

use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::Result;

use crate::{atomic_write, ensure_dir, project_dir, shelbi_home};

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
    /// The runner stalled on a usage/session limit (e.g. Claude Code's "usage
    /// limit reached … resets at <time>"). Unlike the other states this one is
    /// derived from the poller's pane *sample*, not the `shelbi:<state>` title
    /// marker — a usage-limited pane keeps a stale `shelbi:working` title — and
    /// it reverts on the first poll after the limit lifts. Surfaced as the ⏸
    /// pause badge so a paused slot is visible at a glance.
    Paused,
    /// A `review`-tagged slot whose loaded branch is confirmed serving — the
    /// workflow's `review:` recipe `ready:` probe passed. Like [`Paused`] this
    /// is derived from the poller's own observation (running the probe), not the
    /// pane title: a review pane running `npm run dev` shows the dev server's
    /// title, never a `shelbi:<state>` marker, so without this the slot would
    /// never be polled at all ("hasn't been polled"). It is the serving
    /// sub-state the sidebar's **Ready for Review** section keys off (via the
    /// poller-written `.shelbi/shelbi-review-loaded` marker).
    Serving,
}

impl WorkspaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceState::Working => "working",
            WorkspaceState::AwaitingInput => "awaiting_input",
            WorkspaceState::Blocked => "blocked",
            WorkspaceState::Paused => "paused",
            WorkspaceState::Serving => "serving",
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
    /// A turn ended while background work it started (a backgrounded shell, a
    /// monitor, a subagent, ...) is still in flight. Claude Code resumes the
    /// agent by itself when that work finishes, so nobody needs to answer it:
    /// the `Stop` hook writes `shelbi:background` instead of `shelbi:idle`
    /// (from the `background_tasks` array in its payload). On a worker pane it
    /// reads as [`WorkspaceState::Working`] ([`PaneMarker::workspace_state`]):
    /// it never notifies and is never a workspace-free signal. The orchestrator
    /// is different ([`PaneMarker::orchestrator_state`]): it holds a persistent
    /// event-feed monitor for its whole session, so every turn it ends carries
    /// background work, and ending a turn still means it is waiting for the
    /// user.
    Background,
    Review,
    Blocked,
}

impl PaneMarker {
    /// Persisted [`WorkspaceState`] for this marker. `Idle` and `Review`
    /// collapse to `AwaitingInput` — the status file just records that
    /// claude is sitting at a prompt; the review-handoff side effect
    /// happens elsewhere.
    ///
    /// This is the *worker* (workspace pane) reading: `Background` is
    /// `Working`. Never use it for the orchestrator's session; use
    /// [`PaneMarker::orchestrator_state`].
    pub fn workspace_state(self) -> WorkspaceState {
        match self {
            PaneMarker::Working | PaneMarker::Background => WorkspaceState::Working,
            PaneMarker::Idle | PaneMarker::Review => WorkspaceState::AwaitingInput,
            PaneMarker::Blocked => WorkspaceState::Blocked,
        }
    }

    /// The orchestrator session's reading of this marker. It differs from
    /// [`PaneMarker::workspace_state`] only for `Background`, which is
    /// `AwaitingInput` here: the orchestrator runs its `shelbi events tail
    /// --follow` Monitor for the whole session, so every turn it ends lists
    /// background work, and an orchestrator that ended its turn is waiting for
    /// the user whatever watches it holds. The background suppression applies
    /// to worker panes only.
    pub fn orchestrator_state(self) -> WorkspaceState {
        match self {
            PaneMarker::Background => WorkspaceState::AwaitingInput,
            other => other.workspace_state(),
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
    // moves are driven solely by the independent file-based ready marker
    // (the poller's ready-handoff path), never by this title, because any
    // program the agent runs can print an OSC title sequence into the pane.
    let last = title.split_whitespace().next_back()?;
    let marker = last.strip_prefix("shelbi:")?;
    // Trim trailing control chars (BEL, ST) some terminals leave behind.
    let marker = marker.trim_end_matches(|c: char| c.is_control() || c == '\u{0007}');
    match marker {
        "working" => Some(PaneMarker::Working),
        "idle" => Some(PaneMarker::Idle),
        "background" => Some(PaneMarker::Background),
        "review" => Some(PaneMarker::Review),
        "blocked" => Some(PaneMarker::Blocked),
        _ => None,
    }
}

/// Convenience: just the persisted state, dropping the marker
/// distinction. Callers that need to know `review` vs `idle` should use
/// [`parse_pane_title_marker`] instead.
///
/// Worker (workspace pane) semantics: `shelbi:background` is `Working`. The
/// orchestrator's title goes through [`parse_orchestrator_title_state`].
pub fn parse_pane_title_state(title: &str) -> Option<WorkspaceState> {
    parse_pane_title_marker(title).map(PaneMarker::workspace_state)
}

/// The orchestrator session's state from its title marker
/// ([`PaneMarker::orchestrator_state`]): `shelbi:background` is
/// `AwaitingInput`, since the orchestrator ending a turn means it is waiting
/// for the user, whatever background watches (its event-feed Monitor) it holds.
pub fn parse_orchestrator_title_state(title: &str) -> Option<WorkspaceState> {
    parse_pane_title_marker(title).map(PaneMarker::orchestrator_state)
}

/// `~/.shelbi/projects/<project>/workspaces`: root for one project's
/// per-workspace runtime state. The same path as
/// [`crate::ProjectPaths::workspaces_dir`], for callers holding only a name.
pub fn workspaces_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("workspaces"))
}

/// `~/.shelbi/projects/<project>/workspaces/<name>/status.yaml`.
pub fn workspace_status_path(project: &str, workspace: &str) -> Result<PathBuf> {
    crate::ensure_flat_path_component("workspace", workspace)?;
    Ok(workspaces_dir(project)?.join(workspace).join("status.yaml"))
}

/// Env var naming the file the `shelbi __review-serve` wrapper records the
/// launched review server's process-group id into. The orchestrator injects it
/// into a local review pane (value = [`review_serve_pgid_path`]); the wrapper
/// reads it. Defined here so the producer (orchestrator) and consumer (CLI
/// wrapper) can't drift on the spelling.
pub const REVIEW_SERVE_PGID_FILE_ENV: &str = "SHELBI_REVIEW_PGID_FILE";

/// `<project>/workspaces/<name>/review-serve.pgid` — records the process-group
/// id of the review dev server the Review agent launched through the
/// `shelbi __review-serve` wrapper. Kept hub-side (not in the tmux session env,
/// which dies with the pane, and not in memory) so any teardown path can signal
/// the whole server tree even across a hub / daemon restart between launch and
/// teardown. The wrapper writes it; the orchestrator reads it on teardown and
/// clears it. Keyed by project and workspace so `kill_workspace_pane` needs no
/// worktree plumbing to find it, and so one project's teardown can never signal
/// the server another project's same-named slot started.
pub fn review_serve_pgid_path(project: &str, workspace: &str) -> Result<PathBuf> {
    crate::ensure_flat_path_component("workspace", workspace)?;
    Ok(workspaces_dir(project)?.join(workspace).join("review-serve.pgid"))
}

/// Where the `shelbi __review-serve` wrapper should record its server's pgid,
/// given the path its pane's [`REVIEW_SERVE_PGID_FILE_ENV`] names and the pane's
/// `PROJECT`.
///
/// Sessions outlive an upgrade, so a review pane launched by an older release
/// still carries the hub-global `~/.shelbi/workspaces/<name>/review-serve.pgid`
/// in its environment. A record written there is one no teardown reads, and the
/// server would leak, so that one shape is rewritten to the project's path. Any
/// other path passes through untouched.
pub fn rehome_review_serve_pgid_path(path: &std::path::Path, project: Option<&str>) -> PathBuf {
    let rehomed = (|| {
        let project = project?;
        let slot = path.parent()?;
        let legacy_root = shelbi_home().ok()?.join("workspaces");
        if slot.parent()? != legacy_root {
            return None;
        }
        review_serve_pgid_path(project, slot.file_name()?.to_str()?).ok()
    })();
    rehomed.unwrap_or_else(|| path.to_path_buf())
}

/// Read the persisted review-server process-group id for `workspace`, if the
/// wrapper recorded one. A missing file (no server was tracked — a dev slot, a
/// remote review slot, or a load that never served) reads as `Ok(None)`. A
/// malformed or non-positive body also reads as `None` (best-effort: a corrupt
/// record must never wedge teardown, and the `> 1` floor makes it impossible
/// for a `kill(-pgid, …)` to ever target init / every process). The file is
/// left in place for [`clear_review_serve_pgid`] to remove.
pub fn read_review_serve_pgid(project: &str, workspace: &str) -> Result<Option<i32>> {
    let path = review_serve_pgid_path(project, workspace)?;
    match fs::read_to_string(&path) {
        Ok(s) => Ok(s.trim().parse::<i32>().ok().filter(|&p| p > 1)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

/// Remove a workspace's review-server pgid record, if any. Idempotent and
/// best-effort — a missing file is not an error.
pub fn clear_review_serve_pgid(project: &str, workspace: &str) -> Result<()> {
    let path = review_serve_pgid_path(project, workspace)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
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

/// Local Unix-domain socket the hub daemon listens on for the **mutation
/// control protocol** (`shelbi_proto::control`), separate from the worker/event
/// `hub.sock`. `$SHELBI_CONTROL_SOCK` wins when set, mirroring
/// [`hub_socket_path`]; default is `~/.shelbi/control.sock`. The daemon binds
/// both in one process.
pub fn control_socket_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SHELBI_CONTROL_SOCK") {
        return Ok(PathBuf::from(p));
    }
    Ok(shelbi_home()?.join("control.sock"))
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

pub fn save_workspace_status(project: &str, status: &WorkspaceStatus) -> Result<()> {
    let path = workspace_status_path(project, &status.workspace)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let yaml = serde_yaml::to_string(status)?;
    atomic_write(&path, yaml.as_bytes())
}

pub fn load_workspace_status(project: &str, workspace: &str) -> Result<Option<WorkspaceStatus>> {
    let path = workspace_status_path(project, workspace)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = crate::read_to_string_at(&path)?;
    Ok(Some(serde_yaml::from_str(&text)?))
}

/// Remove a workspace's `status.yaml`, if any. Called when a slot's agent pane
/// is torn down and returned to idle (a review slot reaped after its task was
/// accepted, a dev slot reaped after a manual board move) so the file doesn't
/// linger frozen at the agent's last observed state — a killed pane emits no
/// further markers, so the poller can never refresh it, and a stale
/// `working`/`blocked` row would misreport an empty slot as busy for hours.
/// After clearing, `load_workspace_status` reads `None` (the "not polled" state
/// a genuinely idle slot has) and the board-derived views render it as idle.
/// Idempotent and best-effort: a missing file is not an error.
pub fn clear_workspace_status(project: &str, workspace: &str) -> Result<()> {
    let path = workspace_status_path(project, workspace)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
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
    fn background_marker_reads_as_working_not_awaiting_input() {
        // A turn that ended with a background job still running is not waiting
        // on a human: Claude Code resumes the agent when the job exits. It must
        // never surface as awaiting input (no notification, no
        // workspace-free signal).
        assert_eq!(
            parse_pane_title_state("shelbi:background"),
            Some(WorkspaceState::Working)
        );
        assert_eq!(
            parse_pane_title_state("✳ Claude shelbi:background\u{0007}"),
            Some(WorkspaceState::Working)
        );
    }

    #[test]
    fn background_marker_is_awaiting_input_for_the_orchestrator() {
        // The orchestrator's persistent event-feed Monitor rides along on every
        // turn it ends, so `shelbi:background` must still read as waiting for
        // the user there, while every other marker reads as it does on a worker.
        assert_eq!(
            parse_orchestrator_title_state("✳ Claude shelbi:background"),
            Some(WorkspaceState::AwaitingInput)
        );
        for marker in [
            PaneMarker::Working,
            PaneMarker::Idle,
            PaneMarker::Review,
            PaneMarker::Blocked,
        ] {
            assert_eq!(marker.orchestrator_state(), marker.workspace_state());
        }
        assert!(parse_orchestrator_title_state("vim").is_none());
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
        assert_eq!(
            parse_pane_title_marker("shelbi:background"),
            Some(PaneMarker::Background)
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
        save_workspace_status("demo", &status).unwrap();
        let path = workspace_status_path("demo", "alpha").unwrap();
        assert!(path.exists());
        let back = load_workspace_status("demo", "alpha").unwrap().unwrap();
        assert_eq!(back.workspace, "alpha");
        assert_eq!(back.state, WorkspaceState::Working);
        assert_eq!(back.current_task.as_deref(), Some("fix-thing"));

        // Missing workspace returns None, not an error — the sidebar uses
        // this to bootstrap fresh on first observation.
        assert!(load_workspace_status("demo", "ghost").unwrap().is_none());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn clear_workspace_status_removes_the_file_and_is_idempotent() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // No file yet → clearing is a no-op.
        clear_workspace_status("demo", "alpha").unwrap();

        let now = Utc::now();
        save_workspace_status("demo", &WorkspaceStatus {
            workspace: "alpha".into(),
            current_task: None,
            state: WorkspaceState::Working,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();
        assert!(workspace_status_path("demo", "alpha").unwrap().exists());

        clear_workspace_status("demo", "alpha").unwrap();
        assert!(
            !workspace_status_path("demo", "alpha").unwrap().exists(),
            "clear must remove status.yaml so a reaped slot reads as unpolled/idle"
        );
        assert!(load_workspace_status("demo", "alpha").unwrap().is_none());

        // Second clear on the now-absent file is also OK.
        clear_workspace_status("demo", "alpha").unwrap();

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

    /// The bug this layout exists to fix: slot names are only unique within a
    /// project, so two projects' `review` slots must keep separate rows, and
    /// reaping one must leave the other's row (and marker, and server record)
    /// alone.
    #[test]
    fn same_named_slots_in_two_projects_keep_separate_state() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let status = |task: &str, state| WorkspaceStatus {
            workspace: "review".into(),
            current_task: Some(task.into()),
            state,
            last_transition: Utc::now(),
            last_seen: Utc::now(),
        };
        save_workspace_status("app", &status("app-task", WorkspaceState::Serving)).unwrap();
        save_workspace_status("site", &status("site-task", WorkspaceState::Blocked)).unwrap();

        // Neither write clobbered the other.
        let app = load_workspace_status("app", "review").unwrap().unwrap();
        let site = load_workspace_status("site", "review").unwrap().unwrap();
        assert_eq!(app.current_task.as_deref(), Some("app-task"));
        assert_eq!(app.state, WorkspaceState::Serving);
        assert_eq!(site.current_task.as_deref(), Some("site-task"));
        assert_eq!(site.state, WorkspaceState::Blocked);

        // Reaping `app`'s slot leaves `site`'s row intact.
        clear_workspace_status("app", "review").unwrap();
        assert!(load_workspace_status("app", "review").unwrap().is_none());
        let site = load_workspace_status("site", "review").unwrap().unwrap();
        assert_eq!(site.current_task.as_deref(), Some("site-task"));

        // Same for the review-server record.
        let pgid_path = review_serve_pgid_path("app", "review").unwrap();
        std::fs::create_dir_all(pgid_path.parent().unwrap()).unwrap();
        std::fs::write(&pgid_path, "4242\n").unwrap();
        assert_eq!(read_review_serve_pgid("site", "review").unwrap(), None);
        assert_eq!(read_review_serve_pgid("app", "review").unwrap(), Some(4242));

        // And it all lands under the project's own state root, the same place
        // `ProjectPaths::workspaces_dir` names.
        assert_eq!(
            workspace_status_path("site", "review").unwrap(),
            home.join("projects/site/workspaces/review/status.yaml"),
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_status_path_rejects_traversal_project_names() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert!(workspace_status_path("../evil", "review").is_err());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_status_path_rejects_traversal_names() {
        // The accepted name resolves under the hub root, so mount a throwaway
        // home rather than reading the live `~/.shelbi`.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Residual chokepoint hardening (Shelbi ContextStore
        // docs/planning:reviews/adversarial-2026-07/state-runtime.md F14): a `..`/absolute/
        // separator workspace name must not escape the project's `workspaces/`.
        for bad in ["..", "../evil", "a/b", "/abs", "nested/../escape", ""] {
            assert!(
                workspace_status_path("demo", bad).is_err(),
                "workspace_status_path should reject `{bad}`"
            );
        }
        // A normal single-component name still resolves.
        assert!(workspace_status_path("demo", "review-1").is_ok());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_pre_upgrade_pane_records_its_server_under_the_project() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let legacy = home.join("workspaces/review/review-serve.pgid");
        assert_eq!(
            rehome_review_serve_pgid_path(&legacy, Some("app")),
            review_serve_pgid_path("app", "review").unwrap(),
        );
        // Without a project there is nothing to rehome it to.
        assert_eq!(rehome_review_serve_pgid_path(&legacy, None), legacy);
        // A current path, or one somewhere else entirely, is left alone.
        let current = review_serve_pgid_path("app", "review").unwrap();
        assert_eq!(rehome_review_serve_pgid_path(&current, Some("site")), current);
        let elsewhere = std::path::Path::new("/tmp/custom.pgid");
        assert_eq!(rehome_review_serve_pgid_path(elsewhere, Some("app")), elsewhere);

        std::env::remove_var("SHELBI_HOME");
    }
}
