//! Workspace state observed from each workspace's tmux pane title.
//!
//! Workspace `.claude/settings.json` hooks emit `shelbi:working|idle|blocked`
//! markers via OSC pane-title escapes (see
//! `default_workspace_settings.json.template`); the hub-side sidebar poll loop
//! reads the current pane title with `tmux display-message`, parses the
//! trailing marker, and persists any transition to
//! `~/.shelbi/workspaces/<name>/status.yaml`.
//!
//! This module is limited to pane/status *observation*: the pane-title marker
//! vocabulary ([`PaneMarker`] / [`WorkspaceState`]), the persisted
//! [`WorkspaceStatus`], the expected-teardown markers that suppress spurious
//! pane-death events, and the hub daemon socket path. The durable event stream
//! those transitions are appended to — `events.log`, its rotation, the logical
//! cursors, and the `append_*_event` producers — lives in [`crate::event_log`].
//!
//! Authoritative state stays on the hub: workspaces themselves only emit
//! markers; they don't own these files.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::Result;

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
    /// The runner stalled on a usage/session limit (e.g. Claude Code's "usage
    /// limit reached … resets at <time>"). Unlike the other states this one is
    /// derived from the poller's pane *sample*, not the `shelbi:<state>` title
    /// marker — a usage-limited pane keeps a stale `shelbi:working` title — and
    /// it reverts on the first poll after the limit lifts. Surfaced as the ⏸
    /// pause badge so a paused slot is visible at a glance.
    Paused,
}

impl WorkspaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceState::Working => "working",
            WorkspaceState::AwaitingInput => "awaiting_input",
            WorkspaceState::Blocked => "blocked",
            WorkspaceState::Paused => "paused",
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
    crate::ensure_flat_path_component("workspace", workspace)?;
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
    let text = crate::read_to_string_at(&path)?;
    Ok(Some(serde_yaml::from_str(&text)?))
}

/// The no-restart key the supervisor consumes to tell a crash from a
/// deliberate shutdown.
///
/// The plain [`expected_teardown_marker_path`] marker is consumed by the
/// pane's lifecycle wrapper on exit (to suppress its `pane_alive=false`
/// event), so by the time the sidebar supervisor observes the dead pane
/// that marker is already gone — the two processes would race over one
/// file. We derive an independent key by suffixing `.supervision` so it
/// lands in its own `workspaces/<name>.supervision/.expected-teardown`
/// file and reuses the exact [`mark_expected_teardown`] /
/// [`consume_expected_teardown`] machinery. The lifecycle wrapper marks it
/// whenever a death is *not* a crash to restart (a fresh expected-teardown
/// was present, or the agent exited cleanly with `exit:0`); the supervisor
/// consumes it on the death edge and treats a fresh hit as "stay down."
pub fn supervision_shutdown_key(workspace: &str) -> String {
    format!("{workspace}.supervision")
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
    fn supervision_marker_uses_an_independent_key_from_the_wrapper_teardown() {
        // The supervisor's no-restart marker must land in its own file so it
        // never races the pane wrapper's expected-teardown consume.
        assert_eq!(supervision_shutdown_key("alpha"), "alpha.supervision");
        assert_ne!(
            expected_teardown_marker_path("alpha").unwrap(),
            expected_teardown_marker_path(&supervision_shutdown_key("alpha")).unwrap(),
        );
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
    fn workspace_status_path_rejects_traversal_names() {
        // Residual chokepoint hardening (Shelbi ContextStore
        // docs/planning:reviews/adversarial-2026-07/state-runtime.md F14): a `..`/absolute/
        // separator workspace name must not escape `~/.shelbi/workspaces/`.
        for bad in ["..", "../evil", "a/b", "/abs", "nested/../escape", ""] {
            assert!(
                workspace_status_path(bad).is_err(),
                "workspace_status_path should reject `{bad}`"
            );
        }
        // A normal single-component name still resolves.
        assert!(workspace_status_path("review-1").is_ok());
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
        assert_eq!(
            rc,
            0,
            "utimes failed: errno={}",
            std::io::Error::last_os_error()
        );
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
