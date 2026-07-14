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
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::{Column, Result, DEFAULT_WORKFLOW_NAME};

use crate::{acquire_file_lock, atomic_write, ensure_dir, project_dir, projects_dir, shelbi_home};

/// Harness callback socket for push-capable orchestrator runners.
///
/// When set, every successfully appended `events.log` line is also delivered
/// as a newline-delimited [`EventEnvelope`] JSON object to this Unix socket.
/// Delivery is best-effort by design: Shelbi is the event transport, not the
/// scheduler. A missing or failing callback never replaces the durable log;
/// runners without push support keep draining or tailing `events.log`.
pub const ORCH_EVENT_CALLBACK_SOCK_ENV: &str = "SHELBI_ORCH_EVENT_CALLBACK_SOCK";

/// Normalized, harness-neutral event envelope.
///
/// The `line` field is exactly the event-stream record that polling runners
/// consume from `events.log`; push-capable runners receive this same envelope
/// over their callback transport so both paths share semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub version: u8,
    pub transport: String,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub line: String,
}

impl EventEnvelope {
    pub fn from_log_line(line: &str) -> Self {
        let mut parts = line.split_whitespace();
        let timestamp = parts.next().map(str::to_string);
        let body = match timestamp.as_deref() {
            Some(ts) => line.strip_prefix(ts).map(str::trim_start).unwrap_or(line),
            None => line,
        };
        let project = event_field(body, "project").map(str::to_string);
        Self {
            version: 1,
            transport: "shelbi.events".to_string(),
            kind: EventKind::from_body(body),
            project,
            timestamp,
            line: line.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Heartbeat,
    Task,
    Workspace,
    WorkspacePane,
    Message,
    Clarification,
    Dispatch,
    Send,
    Rebase,
    Zen,
    ZenDryrun,
    Supervision,
    Project,
    External,
}

impl EventKind {
    fn from_body(body: &str) -> Self {
        // Prefix events may also carry `task=` / `workspace=` metadata. Match
        // their discriminator before those broad field-based branches or a
        // send verdict is mislabeled as an ordinary workspace event.
        if body.starts_with("send ") || body == "send" {
            EventKind::Send
        } else if body.contains(" heartbeat") || body.ends_with(" heartbeat") {
            EventKind::Heartbeat
        } else if body.contains(" task=") || body.starts_with("task=") {
            EventKind::Task
        } else if body.contains(" workspace=") || body.starts_with("workspace=") {
            if body.contains(" pane_alive=") {
                EventKind::WorkspacePane
            } else if body.contains(" supervision=") {
                EventKind::Supervision
            } else {
                EventKind::Workspace
            }
        } else if body.contains(" message=") || body.starts_with("message=") {
            EventKind::Message
        } else if body.contains(" question=") || body.starts_with("question=") {
            EventKind::Clarification
        } else if body.contains(" dispatch ") || body.starts_with("dispatch ") {
            EventKind::Dispatch
        } else if body.contains(" rebase ") || body.starts_with("rebase ") {
            EventKind::Rebase
        } else if body.contains(" mode=zen ") {
            EventKind::Zen
        } else if body.contains(" zen-dryrun ") || body.starts_with("zen-dryrun ") {
            EventKind::ZenDryrun
        } else if body.contains(" supervision=") {
            EventKind::Supervision
        } else if body.contains(" project=") || body.starts_with("project=") {
            EventKind::Project
        } else {
            EventKind::External
        }
    }
}

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

/// `~/.shelbi/events.log`.
pub fn events_log_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("events.log"))
}

const EVENT_LOG_INDEX_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct EventLogIndex {
    version: u8,
    previous_base: Option<u64>,
    current_base: u64,
}

impl Default for EventLogIndex {
    fn default() -> Self {
        Self {
            version: EVENT_LOG_INDEX_VERSION,
            previous_base: None,
            current_base: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct EventLogRotation {
    version: u8,
    old_base: u64,
    old_len: u64,
    old_dev: u64,
    old_ino: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct EventLogMigration {
    version: u8,
    current_len: u64,
}

/// Raw bytes read from the retained event-log generations using one monotonic
/// logical byte cursor. `start + bytes.len()` is the logical head captured by
/// this read; callers may hold back a final unterminated line and advance less.
#[derive(Debug)]
pub struct EventLogRead {
    pub start: u64,
    pub head: u64,
    pub bytes: Vec<u8>,
}

fn event_log_index_path(path: &Path) -> PathBuf {
    path.with_extension("log.index.json")
}

fn event_log_rotation_path(path: &Path) -> PathBuf {
    path.with_extension("log.rotation.json")
}

fn event_log_migration_path(path: &Path) -> PathBuf {
    path.with_extension("log.migration.json")
}

fn event_log_lock_path(path: &Path) -> PathBuf {
    path.with_extension("log.lock")
}

fn save_event_log_index(path: &Path, index: &EventLogIndex) -> Result<()> {
    let bytes = serde_json::to_vec(index)
        .map_err(|e| shelbi_core::Error::Other(format!("serialize event-log index: {e}")))?;
    atomic_write(&event_log_index_path(path), &bytes)
}

fn event_cursor_paths() -> Result<Vec<PathBuf>> {
    let dir = projects_dir()?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(shelbi_core::Error::Io)?;
        if entry.file_type().map_err(shelbi_core::Error::Io)?.is_dir() {
            paths.push(entry.path().join("event-cursor"));
        }
    }
    paths.sort();
    Ok(paths)
}

fn load_or_create_event_log_migration(path: &Path, current_len: u64) -> Result<EventLogMigration> {
    let migration_path = event_log_migration_path(path);
    match fs::read_to_string(&migration_path) {
        Ok(text) => {
            let migration: EventLogMigration = serde_json::from_str(&text).map_err(|e| {
                shelbi_core::Error::Other(format!("invalid event-log migration marker: {e}"))
            })?;
            if migration.version != EVENT_LOG_INDEX_VERSION {
                return Err(shelbi_core::Error::Other(format!(
                    "unsupported event-log migration version {}",
                    migration.version
                )));
            }
            Ok(migration)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let migration = EventLogMigration {
                version: EVENT_LOG_INDEX_VERSION,
                current_len,
            };
            let bytes = serde_json::to_vec(&migration).map_err(|e| {
                shelbi_core::Error::Other(format!("serialize event-log migration marker: {e}"))
            })?;
            atomic_write(&migration_path, &bytes)?;
            Ok(migration)
        }
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

fn cleanup_event_log_migration(path: &Path) {
    let migration_path = event_log_migration_path(path);
    if let Err(error) = fs::remove_file(&migration_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                %error,
                path = %migration_path.display(),
                "event-log migration committed but marker cleanup failed"
            );
        }
    }
}

/// Before the logical index existed, project cursors were physical offsets
/// into the current `events.log`. The legacy reader treated an offset beyond
/// the current file length as a rotation and restarted at zero. Preserve that
/// behavior exactly once, while establishing the first logical index; after
/// the index exists, a future cursor is corruption and must fail closed.
fn normalize_legacy_event_cursors(current_len: u64) -> Result<()> {
    for cursor_path in event_cursor_paths()? {
        let text = match fs::read_to_string(&cursor_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(shelbi_core::Error::Io(e)),
        };
        if text
            .trim()
            .parse::<u64>()
            .map_or(true, |cursor| cursor > current_len)
        {
            atomic_write(&cursor_path, b"0")?;
        }
    }
    Ok(())
}

/// Finish or roll back the narrow rename/index crash window. Writers create
/// the journal before renaming current to `.1`, and do not recreate current
/// until the new index is durable. The journal's source identity and length
/// distinguish a genuine pre-rename current file from one recreated by a
/// degraded writer after rename.
fn optional_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

fn metadata_matches_rotation_source(
    metadata: &fs::Metadata,
    rotation: &EventLogRotation,
) -> bool {
    metadata.len() == rotation.old_len
        && metadata_has_rotation_source_identity(metadata, rotation)
}

fn metadata_is_pre_rename_source(
    metadata: &fs::Metadata,
    rotation: &EventLogRotation,
) -> bool {
    metadata.len() >= rotation.old_len
        && metadata_has_rotation_source_identity(metadata, rotation)
}

fn metadata_has_rotation_source_identity(
    metadata: &fs::Metadata,
    rotation: &EventLogRotation,
) -> bool {
    metadata.dev() == rotation.old_dev && metadata.ino() == rotation.old_ino
}

fn recover_event_log_rotation(path: &Path, index: &mut EventLogIndex) -> Result<()> {
    let journal_path = event_log_rotation_path(path);
    let text = match fs::read_to_string(&journal_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let rotation: EventLogRotation = serde_json::from_str(&text).map_err(|e| {
        shelbi_core::Error::Other(format!("invalid event-log rotation journal: {e}"))
    })?;
    if rotation.version != EVENT_LOG_INDEX_VERSION {
        return Err(shelbi_core::Error::Other(format!(
            "unsupported event-log rotation journal version {}",
            rotation.version
        )));
    }

    let new_base = rotation.old_base.saturating_add(rotation.old_len);
    let rotated = path.with_extension("log.1");
    let current_metadata = optional_metadata(path)?;
    let rotated_metadata = optional_metadata(&rotated)?;
    let current_is_source = current_metadata
        .as_ref()
        .is_some_and(|metadata| metadata_is_pre_rename_source(metadata, &rotation));
    let rotated_is_source = rotated_metadata
        .as_ref()
        .is_some_and(|metadata| metadata_matches_rotation_source(metadata, &rotation));

    if index.current_base == new_base && index.previous_base == Some(rotation.old_base) {
        if !rotated_is_source {
            return Err(shelbi_core::Error::Other(
                "committed event-log rotation does not match retained generation".into(),
            ));
        }
        if let Err(error) = fs::remove_file(&journal_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    %error,
                    path = %journal_path.display(),
                    "committed event-log rotation journal cleanup failed"
                );
            }
        }
        return Ok(());
    }

    if index.current_base != rotation.old_base {
        return Err(shelbi_core::Error::Other(
            "event-log rotation journal does not match the persisted index".into(),
        ));
    }

    if current_is_source {
        // The process stopped after journaling but before rename. Leave
        // current untouched; the next append can retry rotation.
        if let Err(error) = fs::remove_file(&journal_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    %error,
                    path = %journal_path.display(),
                    "pre-rename event-log rotation journal cleanup failed"
                );
            }
        }
        return Ok(());
    }

    if current_metadata.is_some() {
        return Err(shelbi_core::Error::Other(
            "event-log current file changed after rotation was journaled; refusing ambiguous recovery"
                .into(),
        ));
    }
    if !rotated_is_source {
        return Err(shelbi_core::Error::Other(
            "event-log rotated generation does not match its recovery journal".into(),
        ));
    }
    *index = EventLogIndex {
        version: EVENT_LOG_INDEX_VERSION,
        previous_base: Some(rotation.old_base),
        current_base: new_base,
    };
    save_event_log_index(path, index)?;
    if let Err(error) = fs::remove_file(&journal_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                %error,
                path = %journal_path.display(),
                "recovered event-log rotation journal cleanup failed"
            );
        }
    }
    Ok(())
}

fn load_event_log_index(path: &Path) -> Result<EventLogIndex> {
    let index_path = event_log_index_path(path);
    let (mut index, index_was_absent) = match fs::read_to_string(&index_path) {
        Ok(text) => (
            serde_json::from_str::<EventLogIndex>(&text)
                .map_err(|e| shelbi_core::Error::Other(format!("invalid event-log index: {e}")))?,
            false,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (EventLogIndex::default(), true),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    if index.version != EVENT_LOG_INDEX_VERSION {
        return Err(shelbi_core::Error::Other(format!(
            "unsupported event-log index version {}",
            index.version
        )));
    }
    recover_event_log_rotation(path, &mut index)?;
    let index_is_established = match fs::metadata(&index_path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    if index_was_absent && !index_is_established {
        let current_len = match fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(shelbi_core::Error::Io(e)),
        };
        let migration = load_or_create_event_log_migration(path, current_len)?;
        normalize_legacy_event_cursors(migration.current_len)?;
        save_event_log_index(path, &index)?;
        cleanup_event_log_migration(path);
    } else if index_is_established {
        cleanup_event_log_migration(path);
    }
    Ok(index)
}

/// Logical start of the current `events.log` generation. Missing project
/// cursors initialize here, preserving the historical behavior of starting at
/// the current log rather than replaying an already-rotated legacy `.1`.
pub fn event_log_current_base() -> Result<u64> {
    let path = events_log_path()?;
    let _lock = acquire_file_lock(&event_log_lock_path(&path))?;
    Ok(load_event_log_index(&path)?.current_base)
}

pub fn event_log_head() -> Result<u64> {
    let path = events_log_path()?;
    let _lock = acquire_file_lock(&event_log_lock_path(&path))?;
    let index = load_event_log_index(&path)?;
    let len = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    Ok(index.current_base.saturating_add(len))
}

fn append_exact_log_range(
    path: &Path,
    offset: u64,
    len: u64,
    bytes: &mut Vec<u8>,
) -> Result<()> {
    if len == 0 {
        return Ok(());
    }
    let mut file = fs::File::open(path)
        .map_err(|e| shelbi_core::Error::Io(crate::annotate_io_error(path, e)))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(shelbi_core::Error::Io)?;
    let before = bytes.len();
    file.take(len)
        .read_to_end(bytes)
        .map_err(shelbi_core::Error::Io)?;
    let read = (bytes.len() - before) as u64;
    if read != len {
        return Err(shelbi_core::Error::Other(format!(
            "event-log range in {} changed while reading: expected {len} bytes, read {read}",
            path.display()
        )));
    }
    Ok(())
}

/// Read retained event bytes from a monotonic logical cursor. A cursor in the
/// prior generation drains the remainder of `.1` and then current in one
/// contiguous result. Expired or future cursors fail closed instead of being
/// silently reset to a different generation.
pub fn read_event_log_from(cursor: u64) -> Result<EventLogRead> {
    let path = events_log_path()?;
    let _lock = acquire_file_lock(&event_log_lock_path(&path))?;
    let index = load_event_log_index(&path)?;
    let current_len = match fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let head = index.current_base.saturating_add(current_len);
    let earliest = index.previous_base.unwrap_or(index.current_base);
    if cursor < earliest {
        return Err(shelbi_core::Error::Other(format!(
            "event cursor {cursor} predates earliest retained cursor {earliest}"
        )));
    }
    if cursor > head {
        return Err(shelbi_core::Error::Other(format!(
            "event cursor {cursor} is ahead of event-log head {head}"
        )));
    }

    let mut bytes = Vec::with_capacity((head - cursor) as usize);
    if cursor < index.current_base {
        let previous_base = index.previous_base.ok_or_else(|| {
            shelbi_core::Error::Other("event cursor requires an unavailable generation".into())
        })?;
        let rotated = path.with_extension("log.1");
        let expected_previous_len = index
            .current_base
            .checked_sub(previous_base)
            .ok_or_else(|| shelbi_core::Error::Other("invalid event-log generation bases".into()))?;
        let retained_len = fs::metadata(&rotated)
            .map_err(|e| shelbi_core::Error::Io(crate::annotate_io_error(&rotated, e)))?
            .len();
        if retained_len != expected_previous_len {
            return Err(shelbi_core::Error::Other(format!(
                "retained event-log generation length {retained_len} does not match indexed length {expected_previous_len}"
            )));
        }
        append_exact_log_range(
            &rotated,
            cursor - previous_base,
            index.current_base - cursor,
            &mut bytes,
        )?;
        let retained_len_after = fs::metadata(&rotated)
            .map_err(|e| shelbi_core::Error::Io(crate::annotate_io_error(&rotated, e)))?
            .len();
        if retained_len_after != expected_previous_len {
            return Err(shelbi_core::Error::Other(format!(
                "retained event-log generation changed while reading: expected {expected_previous_len} bytes, found {retained_len_after}"
            )));
        }
    }
    let current_offset = cursor.saturating_sub(index.current_base);
    let current_read_len = current_len.checked_sub(current_offset).ok_or_else(|| {
        shelbi_core::Error::Other("event cursor is outside the current generation".into())
    })?;
    append_exact_log_range(&path, current_offset, current_read_len, &mut bytes)?;
    Ok(EventLogRead {
        start: cursor,
        head,
        bytes,
    })
}

pub fn event_cursor_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("event-cursor"))
}

/// Read a project's applied cursor, registering a missing cursor at the start
/// of the current generation before any later rotation can discard it.
pub fn read_or_initialize_event_cursor(project: &str) -> Result<u64> {
    let path = event_cursor_path(project)?;
    let log_path = events_log_path()?;
    let _lock = acquire_file_lock(&event_log_lock_path(&log_path))?;
    let index = load_event_log_index(&log_path)?;
    match fs::read_to_string(&path) {
        Ok(text) => text.trim().parse().map_err(|_| {
            shelbi_core::Error::Other(format!("invalid event cursor in {}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cursor = index.current_base;
            atomic_write(&path, cursor.to_string().as_bytes())?;
            Ok(cursor)
        }
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

pub fn write_event_cursor(project: &str, cursor: u64) -> Result<()> {
    let path = event_cursor_path(project)?;
    let log_path = events_log_path()?;
    let _lock = acquire_file_lock(&event_log_lock_path(&log_path))?;
    // Deliberately do not load or create the global index here. Upgrade
    // migration must be able to observe a legacy cursor while the index is
    // still absent; the next read initializes both under this same lock.
    atomic_write(&path, cursor.to_string().as_bytes())
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

/// Append `<rfc3339> project=<project> workspace=<name> <prev> -> <new>` to
/// `~/.shelbi/events.log`. `prev` is `None` on the first observation.
///
/// The leading `project=<name>` scope is load-bearing: `events.log` is
/// hub-global (every project's orchestrator tails the same file), and
/// workspace names are only unique *within* a project — two projects can
/// each own an `alpha`. Without the scope a transition (or pane death) in one
/// project's `alpha` is indistinguishable from the other's, so every
/// orchestrator would react to it. With the scope each orchestrator filters
/// to `project=<its-own-name>` — matching the heartbeat / zen / crash-recovery
/// convention already on the wire.
pub fn append_workspace_event(
    project: &str,
    workspace: &str,
    prev: Option<WorkspaceState>,
    new: WorkspaceState,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let workspace = sanitize_field(workspace);
    let prev_str = prev.map(|s| s.as_str()).unwrap_or("none");
    append_event_line(&format!(
        "{ts} project={project} workspace={workspace} {prev_str} -> {new}"
    ))
}

/// Append a blocking-dialog transition line to `~/.shelbi/events.log`:
///
/// - blocked: `<rfc3339> project=<project> workspace=<name> working -> blocked reason=dialog:<kind>`
/// - cleared: `<rfc3339> project=<project> workspace=<name> blocked -> working reason=dialog:<kind>:cleared`
///
/// Emitted by the hub poller when its `tmux capture-pane` sample starts (or
/// stops) matching a configured blocking-dialog signature — a workspace
/// frozen on a usage-limit/trust/permission modal whose pane title still
/// reads `shelbi:working`. Distinct from [`append_workspace_event`] (which
/// records pane-title state transitions and takes no reason) so the two
/// don't fight over `status.yaml`: a dialog line is an advisory heads-up,
/// deduped in the poller so it fires once per incident with a matching
/// recovery line when the modal clears.
///
/// Carries the same leading `project=<name>` scope as [`append_workspace_event`]
/// so a hub-global tail can tell two projects' same-named workspaces apart.
///
/// `kind` is the signature's short token (`usage-limit`, `trust`, …);
/// whitespace folds to underscores so the line stays a single parseable
/// record.
pub fn append_workspace_dialog_event(
    project: &str,
    workspace: &str,
    kind: &str,
    blocked: bool,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let workspace = sanitize_field(workspace);
    let kind = sanitize_reason(kind);
    let line = if blocked {
        format!(
            "{ts} project={project} workspace={workspace} working -> blocked reason=dialog:{kind}"
        )
    } else {
        format!(
            "{ts} project={project} workspace={workspace} blocked -> working reason=dialog:{kind}:cleared"
        )
    };
    append_event_line(&line)
}

/// Append a usage-limit *pause* transition line to `~/.shelbi/events.log`:
///
/// ```text
/// <rfc3339> project=<project> workspace=<name> <prev> -> paused reason=usage-limit[ reset=<hint>]
/// ```
///
/// Emitted by the hub poller when its `tmux capture-pane` sample first matches
/// the runner's usage-limit signature — a worker whose runner stopped
/// mid-task on a usage/session limit while its pane title still reads
/// `shelbi:working`. Distinct from [`append_workspace_dialog_event`]: a
/// usage-limited slot isn't waiting on a human, so it becomes a first-class
/// [`WorkspaceState::Paused`] (⏸ badge) rather than a `blocked` advisory. The
/// *resume* edge rides the ordinary [`append_workspace_event`] transition
/// (`paused -> working`) once the pane's live marker reappears, so this helper
/// only carries the into-the-stall edge and its reason/reset detail.
///
/// `reset` is the optional reset-time hint scraped from the pane (folded to a
/// single token; omitted entirely when claude didn't show one). Carries the
/// same leading `project=<name>` scope as every other workspace event so a
/// hub-global tail can tell two projects' same-named workspaces apart.
pub fn append_workspace_pause_event(
    project: &str,
    workspace: &str,
    prev: Option<WorkspaceState>,
    reset: Option<&str>,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let workspace = sanitize_field(workspace);
    // A usage-limited pane was, by definition, working — use that as the
    // sensible default when we've no prior observation on record yet.
    let prev_str = prev.map(|s| s.as_str()).unwrap_or("working");
    let mut line = format!(
        "{ts} project={project} workspace={workspace} {prev_str} -> paused reason=usage-limit"
    );
    if let Some(reset) = reset {
        let reset = sanitize_reason(reset);
        if !reset.is_empty() {
            line.push_str(&format!(" reset={reset}"));
        }
    }
    append_event_line(&line)
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
/// <rfc3339> project=<project> task=<id> workflow=<name> <from> -> <to> reason=<short> from_category=<cat> to_category=<cat>
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
    project: &str,
    task_id: &str,
    workflow: &str,
    from: Column,
    to: Column,
    reason: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let task_id = sanitize_field(task_id);
    let reason = sanitize_reason(reason);
    let workflow_name = if workflow.trim().is_empty() {
        DEFAULT_WORKFLOW_NAME.to_string()
    } else {
        sanitize_field(workflow)
    };
    let from_category = from.category();
    let to_category = to.category();
    let body = format!(
        "project={project} task={task_id} workflow={workflow_name} {from} -> {to} \
         reason={reason} from_category={from_category} to_category={to_category}"
    );
    let line = format!("{ts} {body}");
    match append_event_line(&line) {
        Ok(()) => Ok(()),
        Err(e) if is_permission_denied(&e) => emit_event_body(&body),
        Err(e) => Err(e),
    }
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

/// Append a supervision line recording an automatic pane relaunch or a
/// crash-loop give-up to `~/.shelbi/events.log`.
///
/// - workspace pane (`Some(name)`):
///   `<rfc3339> project=<p> workspace=<name> supervision=<action> reason=<reason>`
/// - orchestrator pane (`None`):
///   `<rfc3339> project=<p> supervision=<action> target=orchestrator reason=<reason>`
///
/// Emitted by the sidebar supervisor when it relaunches a shelbi-managed
/// pane that died unexpectedly (`action=restart`), when a relaunch attempt
/// itself failed (`action=restart-failed`), or when it stops trying after
/// the crash-loop cap (`action=gave-up reason=crash-loop`). Every line
/// carries the leading `project=<name>` scope — same rationale as
/// [`append_workspace_pane_event`] — so a hub-global tail can tell two
/// projects' same-named workspaces apart. `action`/`reason` fold whitespace
/// to underscores so the line stays a single parseable record.
pub fn append_supervision_event(
    project: &str,
    workspace: Option<&str>,
    action: &str,
    reason: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let action = sanitize_reason(action);
    let reason = sanitize_reason(reason);
    let line = match workspace {
        Some(w) => {
            let w = sanitize_field(w);
            format!("{ts} project={project} workspace={w} supervision={action} reason={reason}")
        }
        None => {
            format!(
                "{ts} project={project} supervision={action} target=orchestrator reason={reason}"
            )
        }
    };
    append_event_line(&line)
}

/// Append a usage-limit auto-resume line to `~/.shelbi/events.log`:
///
/// ```text
/// <rfc3339> project=<p> workspace=<name> supervision=limit-resume status=<status>[ k=v]…
/// ```
///
/// Emitted by the hub poller's limit-resume scheduler — the action half of
/// the usage-limit pause detection (see [`append_workspace_pause_event`]).
/// `status` is a short token for where the cycle is (`scheduled`, `sent`,
/// `skipped`, `failed`, `needs-human`) and `details` carries the
/// status-specific context (`scheduled_for=<ts>`, `reason=<short>`,
/// `reset=<hint>`). Keys and values fold whitespace to underscores so the
/// line stays a single parseable record; the leading `project=` scope
/// matches every other supervision line.
pub fn append_limit_resume_event(
    project: &str,
    workspace: &str,
    status: &str,
    details: &[(&str, &str)],
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let workspace = sanitize_field(workspace);
    let status = sanitize_reason(status);
    let mut line = format!(
        "{ts} project={project} workspace={workspace} supervision=limit-resume status={status}"
    );
    for (key, value) in details {
        line.push_str(&format!(
            " {}={}",
            sanitize_reason(key),
            sanitize_reason(value)
        ));
    }
    append_event_line(&line)
}

/// Append `<rfc3339> project=<name> mode=zen <prev> -> <new> reason=<source>`
/// to `~/.shelbi/events.log`. The orchestrator's tail watches this line shape
/// to react to Zen Mode toggles without re-reading `state.json`. Sources
/// are short tokens identifying the toggle path (`user:cli`, `user:hotkey`,
/// `user:palette`, `system:crash-recovery`); whitespace folds to underscores
/// so the line stays parseable.
///
/// The leading `project=<name>` scope is load-bearing: `events.log` is
/// hub-global (every project's orchestrator tails the same file), so without
/// the scope a toggle in one project would be indistinguishable from a toggle
/// in another and every orchestrator would react to it. With the scope each
/// orchestrator filters to `project=<its-own-name>` — matching the same
/// convention the heartbeat (`append_heartbeat_event`) and crash-recovery
/// lines already use.
pub fn append_zen_mode_event(project: &str, prev: &str, new: &str, source: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let prev = sanitize_reason(prev);
    let new = sanitize_reason(new);
    let source = sanitize_reason(source);
    append_event_line(&format!(
        "{ts} project={project} mode=zen {prev} -> {new} reason={source}"
    ))
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
///
/// `zen` is `Some(..)` only while Zen Mode is On, in which case a `zen=on`
/// token is inserted right after `heartbeat` and a trailing reminder may be
/// appended per the [`ZenHeartbeatCue`] variant (the poller drives the two
/// re-injection cadences). When Zen is Off the caller passes `None` and the
/// line is byte-identical to the pre-Zen-pairing shape.
pub fn append_heartbeat_event(
    project: &str,
    zen_eligible: usize,
    idle_workspaces: usize,
    zen: Option<ZenHeartbeatCue>,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    // `zen=on` is emitted only when Zen Mode is On (the poller passes
    // `Some(..)` in that case). A Zen-off heartbeat carries no zen token at
    // all, so its line shape is byte-identical to the pre-Zen-pairing form.
    let zen_marker = if zen.is_some() { " zen=on" } else { "" };
    let mut line = format!(
        "{ts} project={project} heartbeat{zen_marker} zen_eligible={zen_eligible} idle_workspaces={idle_workspaces}"
    );
    // The reminder is trailing free text (introduced by ` — `) so it stays
    // human-readable for the orchestrator rather than underscore-folded; the
    // structured `key=value` tokens above are unaffected. `Plain` adds
    // nothing beyond `zen=on`.
    match zen {
        Some(ZenHeartbeatCue::Summary(summary)) => {
            // The summary is the first line of `zenmode.md` (already a single
            // line); collapse any stray whitespace so the record can't be
            // torn across lines.
            let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
            if !summary.is_empty() {
                line.push_str(" — ");
                line.push_str(&summary);
            }
        }
        Some(ZenHeartbeatCue::Reread) => {
            line.push_str(" — re-read zenmode.md now to refresh Zen policy");
        }
        Some(ZenHeartbeatCue::Plain) | None => {}
    }
    append_event_line(&line)
}

/// Reminder appended to a heartbeat line while Zen Mode is On. Off-mode
/// heartbeats pass `None` to [`append_heartbeat_event`] and carry no zen
/// tokens at all. The poller decides which variant to emit on two cadences
/// (see its `maybe_emit_heartbeat`): the one-line [`Summary`] roughly every
/// few heartbeats, the full [`Reread`] instruction roughly hourly, and
/// [`Plain`] (bare `zen=on`) on the ticks in between.
///
/// [`Summary`]: ZenHeartbeatCue::Summary
/// [`Reread`]: ZenHeartbeatCue::Reread
/// [`Plain`]: ZenHeartbeatCue::Plain
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZenHeartbeatCue {
    /// Zen on, no extra reminder this tick — the line carries only `zen=on`.
    Plain,
    /// Append the one-line Zen summary (the first line of `zenmode.md`, read
    /// fresh) as a live reminder of what Zen means for this project.
    Summary(String),
    /// Append the instruction to re-read `zenmode.md` in full now — the
    /// deeper periodic refresh of Zen policy.
    Reread,
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

/// Append `<rfc3339> send project=<project> workspace=<name> status=<status> detail=<detail>`
/// to `~/.shelbi/events.log`. Records the delivery verdict of a
/// `shelbi send` (verified pane-injection): `status=submitted` when the
/// worker's input consumed the text, `status=queued` when the pane was
/// mid-turn and the text is parked as claude's queued input, and
/// `status=unverified` when a custom runner received the split text/Enter
/// delivery but exposes no supported pane-verification capability. Finally,
/// `status=stuck` when no submission signal appeared even after the retry
/// Enter — the failure that used to be silent, leaving text sitting in the
/// input box until a human pressed Enter. The orchestrator reads this off
/// the events tail to react instead of assuming a send landed.
pub fn append_send_event(project: &str, workspace: &str, status: &str, detail: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let project = sanitize_field(project);
    let workspace = sanitize_field(workspace);
    let status = sanitize_reason(status);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} send project={project} workspace={workspace} status={status} detail={detail}"
    ))
}

/// Append `<rfc3339> project=<project> workspace=<name> pane_alive=<bool> reason=<short>`
/// to `~/.shelbi/events.log`. Emitted by the `shelbi open --as-pane`
/// wrapper when its agent subprocess exits (any reason — clean exit,
/// signal, tmux teardown) so the orchestrator's reaction rules can fire
/// on a pane death.
///
/// The leading `project=<name>` scope is load-bearing: workspace names are
/// only unique within a project, and `events.log` is hub-global. Without the
/// scope, project B's `alpha` pane dying would emit `workspace=alpha
/// pane_alive=false` — which project A (whose own `alpha` is alive and
/// mid-task) would read off the shared tail as *its* alpha dying, spuriously
/// tripping the "pane died, surface to user" reaction rule. With the scope,
/// each orchestrator filters to `project=<its-own-name>` and ignores the
/// other project's death. This is the cross-project false-death bug.
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
    project: &str,
    workspace: &str,
    alive: bool,
    reason: &str,
) -> Result<()> {
    let project = sanitize_field(project);
    let workspace = sanitize_field(workspace);
    let reason = sanitize_reason(reason);
    let body =
        format!("project={project} workspace={workspace} pane_alive={alive} reason={reason}");
    emit_event_body(&body)
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

/// Append a worktree-detach line to `~/.shelbi/events.log`. Emitted by the
/// poller's ready-marker handoff after it promotes a task: the finishing
/// worker's worktree is detached from the task branch so nothing holds it,
/// freeing the branch for the review checkout and the later merge /
/// `delete_branch`.
///
/// - success: `<rfc3339> worktree-detach task=<id> workspace=<name> detached-from=<branch> status=ok`
/// - failure: `<rfc3339> worktree-detach task=<id> workspace=<name> branch=<branch> status=failed reason=worktree-detach-failed detail=<detail>`
///
/// The failure line carries an explicit `reason=worktree-detach-failed` token so
/// a subsequent `already checked out` merge failure is traceable back to a
/// still-held branch rather than looking silent. Carries the same leading
/// `project=`-less shape as [`append_rebase_event`] (both are task-scoped, not
/// workspace-state lines); whitespace in every field folds to underscores so
/// the record stays a single parseable line.
pub fn append_worktree_detach_event(
    task_id: &str,
    workspace: &str,
    branch: &str,
    detached: bool,
    detail: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let task_id = sanitize_field(task_id);
    let workspace = sanitize_field(workspace);
    let branch = sanitize_reason(branch);
    let detail = sanitize_reason(detail);
    let line = if detached {
        format!(
            "{ts} worktree-detach task={task_id} workspace={workspace} detached-from={branch} status=ok"
        )
    } else {
        format!(
            "{ts} worktree-detach task={task_id} workspace={workspace} branch={branch} status=failed reason=worktree-detach-failed detail={detail}"
        )
    };
    append_event_line(&line)
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
    // Stable daemons answer event frames with a bare ack. The shared reader
    // also tolerates the briefly-shipped server-first hello so upgrades can
    // still communicate with and restart that interim daemon.
    crate::hub_version::read_daemon_ack(&stream)
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
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    {
        let _lock = acquire_file_lock(&event_log_lock_path(&path))?;
        match load_event_log_index(&path) {
            Ok(mut index) => {
                match maybe_rotate_events_log(&path, EVENTS_LOG_MAX_BYTES, &mut index) {
                    Ok(_) => {}
                    Err(EventLogRotationFailure::BeforeRename(error)) => {
                        tracing::warn!(
                            %error,
                            path = %path.display(),
                            "event-log rotation preparation failed; appending without rotation"
                        );
                    }
                    Err(EventLogRotationFailure::AfterRename(error)) => {
                        return Err(shelbi_core::Error::Other(format!(
                            "event-log rotation failed after renaming current data; recovery is required before append: {error}"
                        )));
                    }
                }
            }
            Err(error) if pending_rotation_is_post_rename_or_ambiguous(&path) => {
                return Err(shelbi_core::Error::Other(format!(
                    "event-log bookkeeping failed with a pending post-rename rotation; refusing an ambiguous append: {error}"
                )));
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "event-log bookkeeping unavailable; appending without rotation"
                );
            }
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| shelbi_core::Error::Io(crate::annotate_io_error(&path, e)))?;
        f.write_all(buf.as_bytes())
            .map_err(|e| shelbi_core::Error::Io(crate::annotate_io_error(&path, e)))?;
    }
    deliver_event_envelope(&EventEnvelope::from_log_line(line));
    Ok(())
}

fn is_permission_denied(err: &shelbi_core::Error) -> bool {
    matches!(err, shelbi_core::Error::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
}

fn deliver_event_envelope(envelope: &EventEnvelope) {
    let Some(sock) = std::env::var_os(ORCH_EVENT_CALLBACK_SOCK_ENV) else {
        return;
    };
    let payload = match serde_json::to_vec(envelope) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            bytes
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize event envelope");
            return;
        }
    };
    match UnixStream::connect(PathBuf::from(sock)).and_then(|mut stream| {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
        stream.write_all(&payload)
    }) {
        Ok(()) => {}
        Err(e) => {
            tracing::debug!(
                error = %e,
                "orchestrator event callback unavailable; durable events.log remains the fallback",
            );
        }
    }
}

/// Size ceiling for `events.log` before an append rotates it. The log is
/// append-only and otherwise grows without bound; rotating at ~8 MiB
/// bounds disk use and keeps the tail-scan readers cheap (the CLI's
/// crash-recovery check reads only the last 64 KiB — see
/// `commands::status`). One `.1` generation is kept; older history is
/// dropped on the next rotation.
const EVENTS_LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
enum EventLogRotationFailure {
    /// No rename completed, so the current file is still safe to append.
    BeforeRename(shelbi_core::Error),
    /// The current file moved to `.1`; callers must recover before appending
    /// instead of guessing which generation owns the next logical offset.
    AfterRename(shelbi_core::Error),
}

/// A failed bookkeeping load is normally safe to bypass because no rename
/// was attempted by the current append. With a pending journal, bypass is
/// safe only when the current file still has the journaled source identity;
/// a missing, replaced, or uninspectable source may be post-rename.
fn pending_rotation_is_post_rename_or_ambiguous(path: &Path) -> bool {
    let journal_path = event_log_rotation_path(path);
    let text = match fs::read_to_string(&journal_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    let rotation: EventLogRotation = match serde_json::from_str(&text) {
        Ok(rotation) => rotation,
        Err(_) if malformed_journal_is_definitely_pre_rename(path) => {
            if let Err(error) = fs::remove_file(&journal_path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        %error,
                        path = %journal_path.display(),
                        "malformed pre-rename event-log journal cleanup failed"
                    );
                }
            }
            return false;
        }
        Err(_) => return true,
    };
    match fs::metadata(path) {
        Ok(metadata) => !metadata_is_pre_rename_source(&metadata, &rotation),
        Err(_) => true,
    }
}

/// A malformed journal has lost the source inode needed for exact recovery.
/// It is still positively pre-rename when current remains over the rotation
/// threshold and the retained generation still matches the persisted index:
/// the rename would have replaced that retained file (or created it for the
/// first generation). Any other shape remains ambiguous and fails closed.
fn malformed_journal_is_definitely_pre_rename(path: &Path) -> bool {
    let current = match fs::metadata(path) {
        Ok(metadata) if metadata.len() >= EVENTS_LOG_MAX_BYTES => metadata,
        _ => return false,
    };
    let index = match fs::read_to_string(event_log_index_path(path))
        .ok()
        .and_then(|text| serde_json::from_str::<EventLogIndex>(&text).ok())
    {
        Some(index) if index.version == EVENT_LOG_INDEX_VERSION => index,
        _ => return false,
    };
    let rotated = path.with_extension("log.1");
    (match index.previous_base {
        None => rotated.try_exists().ok() == Some(false),
        Some(previous_base) => index
            .current_base
            .checked_sub(previous_base)
            .and_then(|expected_len| {
                fs::metadata(rotated)
                    .ok()
                    .map(|metadata| metadata.len() == expected_len)
            })
            .unwrap_or(false),
    }) && current.is_file()
}

/// Return true when an indexed `.1` still contains bytes needed by any
/// registered project cursor. Malformed cursors block destructive rotation;
/// preserving the log is safer than guessing that corrupt state was applied.
fn prior_generation_is_needed(current_base: u64) -> Result<bool> {
    let dir = projects_dir()?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(shelbi_core::Error::Io)?;
        if !entry.file_type().map_err(shelbi_core::Error::Io)?.is_dir() {
            continue;
        }
        let cursor_path = entry.path().join("event-cursor");
        let text = match fs::read_to_string(&cursor_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(shelbi_core::Error::Io(e)),
        };
        let Ok(cursor) = text.trim().parse::<u64>() else {
            tracing::warn!(
                path = %cursor_path.display(),
                "malformed event cursor blocks destructive log rotation"
            );
            return Ok(true);
        };
        if cursor < current_base {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Size-based rotation with monotonic logical offsets. The caller holds the
/// event-log lock. A second rotation is deferred while a registered cursor
/// still needs `.1`; the current file may temporarily exceed the soft size
/// bound, but durable facts are never discarded to enforce that bound.
fn maybe_rotate_events_log(
    path: &Path,
    max_bytes: u64,
    index: &mut EventLogIndex,
) -> std::result::Result<bool, EventLogRotationFailure> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.len() >= max_bytes => metadata,
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(EventLogRotationFailure::BeforeRename(
                shelbi_core::Error::Io(e),
            ))
        }
    };
    let len = metadata.len();
    if index.previous_base.is_some() {
        match prior_generation_is_needed(index.current_base) {
            Ok(true) => return Ok(false),
            Ok(false) => {}
            Err(error) => return Err(EventLogRotationFailure::BeforeRename(error)),
        }
    }

    let journal = EventLogRotation {
        version: EVENT_LOG_INDEX_VERSION,
        old_base: index.current_base,
        old_len: len,
        old_dev: metadata.dev(),
        old_ino: metadata.ino(),
    };
    let journal_bytes = serde_json::to_vec(&journal).map_err(|e| {
        EventLogRotationFailure::BeforeRename(shelbi_core::Error::Other(format!(
            "serialize event-log rotation: {e}"
        )))
    })?;
    atomic_write(&event_log_rotation_path(path), &journal_bytes)
        .map_err(EventLogRotationFailure::BeforeRename)?;
    let rotated = path.with_extension("log.1");
    if let Err(error) = fs::rename(path, &rotated) {
        let error = shelbi_core::Error::Io(crate::annotate_io_error(path, error));
        return match fs::metadata(path) {
            Ok(_) => {
                let _ = fs::remove_file(event_log_rotation_path(path));
                Err(EventLogRotationFailure::BeforeRename(error))
            }
            Err(_) => Err(EventLogRotationFailure::AfterRename(error)),
        };
    }
    let rotated_metadata = fs::metadata(&rotated).map_err(|error| {
        EventLogRotationFailure::AfterRename(shelbi_core::Error::Io(
            crate::annotate_io_error(&rotated, error),
        ))
    })?;
    if !metadata_matches_rotation_source(&rotated_metadata, &journal) {
        return Err(EventLogRotationFailure::AfterRename(
            shelbi_core::Error::Other(
                "event-log source changed across rename; refusing to commit its index".into(),
            ),
        ));
    }
    *index = EventLogIndex {
        version: EVENT_LOG_INDEX_VERSION,
        previous_base: Some(journal.old_base),
        current_base: journal.old_base.saturating_add(journal.old_len),
    };
    save_event_log_index(path, index).map_err(EventLogRotationFailure::AfterRename)?;
    if let Err(error) = fs::remove_file(event_log_rotation_path(path)) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                %error,
                path = %event_log_rotation_path(path).display(),
                "event-log index committed but rotation journal cleanup failed"
            );
        }
    }
    Ok(true)
}

fn sanitize_reason(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

fn event_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    body.split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
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
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;
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

    fn journal_for(path: &Path, old_base: u64) -> EventLogRotation {
        let metadata = std::fs::metadata(path).unwrap();
        EventLogRotation {
            version: EVENT_LOG_INDEX_VERSION,
            old_base,
            old_len: metadata.len(),
            old_dev: metadata.dev(),
            old_ino: metadata.ino(),
        }
    }

    #[test]
    fn event_envelope_normalizes_project_heartbeat_and_workspace_lines() {
        let heartbeat = EventEnvelope::from_log_line(
            "2026-07-06T12:00:00+00:00 project=demo heartbeat zen_eligible=1 idle_workspaces=2",
        );
        assert_eq!(heartbeat.version, 1);
        assert_eq!(heartbeat.transport, "shelbi.events");
        assert_eq!(heartbeat.kind, EventKind::Heartbeat);
        assert_eq!(heartbeat.project.as_deref(), Some("demo"));
        assert_eq!(
            heartbeat.timestamp.as_deref(),
            Some("2026-07-06T12:00:00+00:00")
        );

        let workspace = EventEnvelope::from_log_line(
            "2026-07-06T12:01:00+00:00 project=demo workspace=alpha working -> awaiting_input",
        );
        assert_eq!(workspace.kind, EventKind::Workspace);
        assert_eq!(workspace.project.as_deref(), Some("demo"));

        let send = EventEnvelope::from_log_line(
            "2026-07-06T12:02:00+00:00 send project=demo workspace=alpha status=stuck detail=unconfirmed_after_retry",
        );
        assert_eq!(send.kind, EventKind::Send);
        assert_eq!(send.project.as_deref(), Some("demo"));
        assert_eq!(
            serde_json::to_value(&send).unwrap()["kind"],
            serde_json::json!("send")
        );
    }

    #[test]
    fn callback_socket_receives_same_envelope_after_log_append() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let sock = PathBuf::from(format!(
            "/tmp/sb-cb-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = match UnixListener::bind(&sock) {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::env::remove_var("SHELBI_HOME");
                let _ = std::fs::remove_file(&sock);
                let _ = std::fs::remove_dir_all(&home);
                eprintln!("skipping callback socket test: Unix sockets unavailable in sandbox");
                return;
            }
            Err(e) => panic!("bind callback socket: {e}"),
        };
        std::env::set_var(ORCH_EVENT_CALLBACK_SOCK_ENV, &sock);

        append_heartbeat_event("demo", 3, 4, None).unwrap();

        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        let envelope: EventEnvelope = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(envelope.kind, EventKind::Heartbeat);
        assert_eq!(envelope.project.as_deref(), Some("demo"));
        assert!(envelope.line.contains("project=demo heartbeat"));

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        assert_eq!(log.trim_end(), envelope.line);

        std::env::remove_var(ORCH_EVENT_CALLBACK_SOCK_ENV);
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn callback_socket_receives_ready_task_without_polling_turn() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let sock = PathBuf::from(format!(
            "/tmp/sb-cbr-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = match UnixListener::bind(&sock) {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::env::remove_var("SHELBI_HOME");
                let _ = std::fs::remove_file(&sock);
                let _ = std::fs::remove_dir_all(&home);
                eprintln!("skipping callback socket test: Unix sockets unavailable in sandbox");
                return;
            }
            Err(e) => panic!("bind callback socket: {e}"),
        };
        listener
            .set_nonblocking(false)
            .expect("callback listener should accept");
        std::env::set_var(ORCH_EVENT_CALLBACK_SOCK_ENV, &sock);

        append_task_event(
            "demo",
            "ready-task",
            "default",
            Column::backlog(),
            Column::todo(),
            "user:move",
        )
        .unwrap();

        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        let envelope: EventEnvelope = serde_json::from_str(line.trim_end()).unwrap();

        assert_eq!(envelope.kind, EventKind::Task);
        assert_eq!(envelope.project.as_deref(), Some("demo"));
        assert!(envelope.line.contains(" task=ready-task "));
        assert!(envelope.line.contains("backlog -> todo"));
        assert!(envelope.line.contains(" to_category=ready"));

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        assert_eq!(log.trim_end(), envelope.line);

        std::env::remove_var(ORCH_EVENT_CALLBACK_SOCK_ENV);
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn events_log_rotates_when_over_size() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rotated = path.with_extension("log.1");
        let mut index = load_event_log_index(&path).unwrap();

        // Under the (test) threshold: no rotation, file untouched.
        std::fs::write(&path, "small\n").unwrap();
        assert!(!maybe_rotate_events_log(&path, 1024, &mut index).unwrap());
        assert!(path.exists());
        assert!(!rotated.exists());

        // At/over the threshold: current log renames to `.1`, leaving room
        // for a fresh file on the next append.
        std::fs::write(&path, "x".repeat(2048)).unwrap();
        assert!(maybe_rotate_events_log(&path, 1024, &mut index).unwrap());
        assert!(!path.exists(), "over-size log should be rotated away");
        assert!(rotated.exists(), "rotated generation should exist");
        assert_eq!(index.previous_base, Some(0));
        assert_eq!(index.current_base, 2048);

        // A second rotation replaces the prior `.1` rather than erroring.
        std::fs::write(&path, "y".repeat(2048)).unwrap();
        assert!(maybe_rotate_events_log(&path, 1024, &mut index).unwrap());
        assert!(rotated.exists());
        assert_eq!(std::fs::read(&rotated).unwrap(), b"y".repeat(2048));
        assert_eq!(index.previous_base, Some(2048));
        assert_eq!(index.current_base, 4096);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn malformed_pre_rename_bookkeeping_does_not_suppress_event_append() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let original = "x".repeat(EVENTS_LOG_MAX_BYTES as usize);
        std::fs::write(&path, &original).unwrap();
        std::fs::write(event_log_index_path(&path), b"{not-json").unwrap();

        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();

        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.len() > original.len());
        assert!(current.contains("project=demo workspace=alpha"));
        assert!(!path.with_extension("log.1").exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn malformed_pre_rename_journal_does_not_suppress_event_append() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        load_event_log_index(&path).unwrap();
        let original = "x".repeat(EVENTS_LOG_MAX_BYTES as usize);
        std::fs::write(&path, &original).unwrap();
        std::fs::write(event_log_rotation_path(&path), b"{not-json").unwrap();

        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();

        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.len() > original.len());
        assert!(current.contains("project=demo workspace=alpha"));
        assert!(!event_log_rotation_path(&path).exists());
        assert!(!path.with_extension("log.1").exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn pre_rename_journal_remains_recoverable_after_unindexed_appends() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        std::fs::write(&path, b"current-generation\n").unwrap();
        let index = load_event_log_index(&path).unwrap();
        let journal = journal_for(&path, index.current_base);
        atomic_write(
            &event_log_rotation_path(&path),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        std::fs::write(event_log_index_path(&path), b"{not-json").unwrap();

        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        append_workspace_event("demo", "bravo", None, WorkspaceState::Working).unwrap();
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains("workspace=alpha"));
        assert!(current.contains("workspace=bravo"));

        save_event_log_index(&path, &index).unwrap();
        assert_eq!(load_event_log_index(&path).unwrap().current_base, 0);
        assert!(!event_log_rotation_path(&path).exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_migration_retry_keeps_original_length_cutoff_after_append() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        std::fs::write(&path, b"x\n").unwrap();
        let broken_cursor = project_dir("a-broken").unwrap().join("event-cursor");
        std::fs::create_dir_all(&broken_cursor).unwrap();
        write_event_cursor("z-demo", 50).unwrap();

        // The sorted migration sweep fails on a-broken after persisting its
        // original two-byte cutoff. The event itself must still append.
        append_workspace_event("z-demo", "alpha", None, WorkspaceState::Working).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 50);
        assert!(!event_log_index_path(&path).exists());
        assert!(event_log_migration_path(&path).exists());

        std::fs::remove_dir(&broken_cursor).unwrap();
        assert_eq!(read_or_initialize_event_cursor("z-demo").unwrap(), 0);
        assert!(event_log_index_path(&path).exists());
        assert!(!event_log_migration_path(&path).exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn malformed_legacy_cursor_normalizes_once_then_fails_closed() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = event_cursor_path("demo").unwrap();
        atomic_write(&path, b"not-a-cursor").unwrap();
        assert_eq!(read_or_initialize_event_cursor("demo").unwrap(), 0);

        atomic_write(&path, b"still-not-a-cursor").unwrap();
        let error = read_or_initialize_event_cursor("demo").unwrap_err();
        assert!(error.to_string().contains("invalid event cursor"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ambiguous_post_rename_bookkeeping_refuses_to_create_current_log() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let rotated = path.with_extension("log.1");
        std::fs::write(&rotated, b"renamed-generation\n").unwrap();
        std::fs::write(event_log_index_path(&path), b"{not-json").unwrap();
        let journal = journal_for(&rotated, 0);
        atomic_write(
            &event_log_rotation_path(&path),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        let error = append_workspace_event("demo", "alpha", None, WorkspaceState::Working)
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous append"));
        assert!(!path.exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn malformed_journal_with_missing_current_refuses_ambiguous_append() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        load_event_log_index(&path).unwrap();
        std::fs::write(path.with_extension("log.1"), b"renamed-generation\n").unwrap();
        std::fs::write(event_log_rotation_path(&path), b"{not-json").unwrap();

        let error = append_workspace_event("demo", "alpha", None, WorkspaceState::Working)
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous append"));
        assert!(!path.exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn recovery_rejects_current_recreated_after_rename() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let index = load_event_log_index(&path).unwrap();
        std::fs::write(&path, b"renamed-generation\n").unwrap();
        let journal = journal_for(&path, index.current_base);
        atomic_write(
            &event_log_rotation_path(&path),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        std::fs::rename(&path, path.with_extension("log.1")).unwrap();
        std::fs::write(&path, b"degraded-writer-current\n").unwrap();

        let error = load_event_log_index(&path).unwrap_err();
        assert!(error.to_string().contains("refusing ambiguous recovery"));
        assert!(event_log_rotation_path(&path).exists());
        let append_error = append_workspace_event(
            "demo",
            "alpha",
            None,
            WorkspaceState::Working,
        )
        .unwrap_err();
        assert!(append_error.to_string().contains("ambiguous append"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"degraded-writer-current\n"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn recovery_rejects_rotated_generation_with_wrong_length() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let index = load_event_log_index(&path).unwrap();
        std::fs::write(&path, b"renamed-generation\n").unwrap();
        let journal = journal_for(&path, index.current_base);
        atomic_write(
            &event_log_rotation_path(&path),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let rotated = path.with_extension("log.1");
        std::fs::rename(&path, &rotated).unwrap();
        std::fs::write(&rotated, b"truncated\n").unwrap();

        let error = load_event_log_index(&path).unwrap_err();
        assert!(error
            .to_string()
            .contains("rotated generation does not match"));
        assert!(event_log_rotation_path(&path).exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rotation_journal_before_rename_rolls_back_without_moving_current() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let current = b"current-generation\n";
        std::fs::write(&path, current).unwrap();
        let index = load_event_log_index(&path).unwrap();
        let journal = journal_for(&path, index.current_base);
        atomic_write(
            &event_log_rotation_path(&path),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        let recovered = load_event_log_index(&path).unwrap();
        assert_eq!(recovered.previous_base, index.previous_base);
        assert_eq!(recovered.current_base, index.current_base);
        assert_eq!(std::fs::read(&path).unwrap(), current);
        assert!(!event_log_rotation_path(&path).exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rotation_journal_after_rename_reconstructs_logical_index() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let index = load_event_log_index(&path).unwrap();
        let previous = b"previous-generation\n";
        std::fs::write(&path, previous).unwrap();
        let journal = journal_for(&path, index.current_base);
        atomic_write(
            &event_log_rotation_path(&path),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        std::fs::rename(&path, path.with_extension("log.1")).unwrap();

        let recovered = load_event_log_index(&path).unwrap();
        assert_eq!(recovered.previous_base, Some(index.current_base));
        assert_eq!(
            recovered.current_base,
            index.current_base + previous.len() as u64
        );
        assert!(!event_log_rotation_path(&path).exists());
        let persisted: EventLogIndex = serde_json::from_str(
            &std::fs::read_to_string(event_log_index_path(&path)).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted.previous_base, recovered.previous_base);
        assert_eq!(persisted.current_base, recovered.current_base);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn committed_rotation_index_cleans_up_leftover_journal() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let index = load_event_log_index(&path).unwrap();
        let previous = b"previous-generation\n";
        std::fs::write(&path, previous).unwrap();
        let journal = journal_for(&path, index.current_base);
        atomic_write(
            &event_log_rotation_path(&path),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        std::fs::rename(&path, path.with_extension("log.1")).unwrap();
        let committed = EventLogIndex {
            version: EVENT_LOG_INDEX_VERSION,
            previous_base: Some(journal.old_base),
            current_base: journal.old_base + journal.old_len,
        };
        save_event_log_index(&path, &committed).unwrap();
        std::fs::write(&path, b"new-current\n").unwrap();

        let recovered = load_event_log_index(&path).unwrap();
        assert_eq!(recovered.previous_base, committed.previous_base);
        assert_eq!(recovered.current_base, committed.current_base);
        assert!(!event_log_rotation_path(&path).exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"new-current\n");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn logical_cursor_reads_rotated_tail_before_regrown_current_prefix() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        let old_first = b"old-first\n";
        let old_second = b"old-second\n";
        let mut old = old_first.to_vec();
        old.extend_from_slice(old_second);
        std::fs::write(&path, &old).unwrap();
        let cursor = old_first.len() as u64;
        let mut index = load_event_log_index(&path).unwrap();
        assert!(maybe_rotate_events_log(&path, 1, &mut index).unwrap());

        // Grow current beyond the old physical cursor. A length-only reader
        // would now seek `cursor` into this generation and skip its prefix.
        let current = b"new-prefix-longer-than-old-cursor\nnew-second\n";
        assert!(current.len() as u64 > cursor);
        std::fs::write(&path, current).unwrap();

        let read = read_event_log_from(cursor).unwrap();
        let mut expected = old_second.to_vec();
        expected.extend_from_slice(current);
        assert_eq!(read.bytes, expected);
        assert_eq!(read.start, cursor);
        assert_eq!(read.head, old.len() as u64 + current.len() as u64);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn logical_reader_rejects_mismatched_retained_generation_length() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        std::fs::write(&path, b"complete-generation\n").unwrap();
        let mut index = load_event_log_index(&path).unwrap();
        assert!(maybe_rotate_events_log(&path, 1, &mut index).unwrap());
        std::fs::write(path.with_extension("log.1"), b"short\n").unwrap();
        std::fs::write(&path, b"current\n").unwrap();

        let error = read_event_log_from(0).unwrap_err();
        assert!(error
            .to_string()
            .contains("does not match indexed length"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rotation_waits_for_slowest_of_two_project_cursors() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = events_log_path().unwrap();
        std::fs::write(&path, b"first-generation\n").unwrap();
        let mut index = load_event_log_index(&path).unwrap();
        assert!(maybe_rotate_events_log(&path, 1, &mut index).unwrap());
        let current_base = index.current_base;

        std::fs::create_dir_all(project_dir("fast").unwrap()).unwrap();
        std::fs::create_dir_all(project_dir("slow").unwrap()).unwrap();
        write_event_cursor("fast", current_base).unwrap();
        write_event_cursor("slow", 0).unwrap();
        std::fs::write(&path, b"second-generation\n").unwrap();
        assert!(!maybe_rotate_events_log(&path, 1, &mut index).unwrap());
        assert_eq!(index.current_base, current_base);

        write_event_cursor("slow", current_base).unwrap();
        assert!(maybe_rotate_events_log(&path, 1, &mut index).unwrap());
        assert!(index.current_base > current_base);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn supervision_events_are_project_scoped_for_both_pane_kinds() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A workspace pane relaunch carries the workspace scope.
        append_supervision_event("demo", Some("alpha"), "restart", "crash").unwrap();
        // A crash-loop give-up on the same workspace.
        append_supervision_event("demo", Some("alpha"), "gave-up", "crash-loop").unwrap();
        // The orchestrator pane has no workspace — it gets `target=orchestrator`.
        append_supervision_event("demo", None, "restart", "crash").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert!(
            lines[0].contains(" project=demo workspace=alpha supervision=restart reason=crash"),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[1]
                .contains(" project=demo workspace=alpha supervision=gave-up reason=crash-loop"),
            "line: {}",
            lines[1]
        );
        assert!(
            lines[2].contains(" project=demo supervision=restart target=orchestrator reason=crash"),
            "line: {}",
            lines[2]
        );
        // Every supervision line is project-scoped so a hub-global tail can
        // tell two projects' same-named workspaces apart (acceptance §5).
        assert!(
            lines.iter().all(|l| l.contains("project=demo")),
            "log: {log}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn limit_resume_events_carry_status_and_folded_details() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // The scheduled half of the cycle: when the resume will fire.
        append_limit_resume_event(
            "demo",
            "alpha",
            "scheduled",
            &[("scheduled_for", "2026-07-11T11:21:30+00:00")],
        )
        .unwrap();
        // The delivery half: the prompt provably submitted.
        append_limit_resume_event("demo", "alpha", "sent", &[]).unwrap();
        // The degrade path: an unparseable banner surfaces the raw hint
        // (whitespace folded so the line stays one record).
        append_limit_resume_event(
            "demo",
            "alpha",
            "needs-human",
            &[("reason", "unparseable-reset"), ("reset", "soon ish (ET)")],
        )
        .unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert!(
            lines[0].contains(
                " project=demo workspace=alpha supervision=limit-resume status=scheduled \
                 scheduled_for=2026-07-11T11:21:30+00:00"
            ),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[1].contains(" project=demo workspace=alpha supervision=limit-resume status=sent")
                && !lines[1].contains("status=sent "),
            "line: {}",
            lines[1]
        );
        assert!(
            lines[2].contains(
                " supervision=limit-resume status=needs-human reason=unparseable-reset \
                 reset=soon_ish_(ET)"
            ),
            "line: {}",
            lines[2]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn pause_event_carries_usage_limit_reason_and_optional_reset() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(events_log_path().unwrap().parent().unwrap()).unwrap();

        // With a reset hint scraped from the pane.
        append_workspace_pause_event(
            "demo",
            "alpha",
            Some(WorkspaceState::Working),
            Some("3pm (America/New_York)"),
        )
        .unwrap();
        // Without a reset hint (claude didn't show one) — the `reset=` token is
        // omitted entirely rather than emitted empty.
        append_workspace_pause_event("demo", "alpha", None, None).unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert!(
            lines[0].contains(
                " project=demo workspace=alpha working -> paused reason=usage-limit \
                 reset=3pm_(America/New_York)"
            ),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[1].contains(" project=demo workspace=alpha working -> paused reason=usage-limit")
                && !lines[1].contains("reset="),
            "line: {}",
            lines[1]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
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

        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        append_workspace_event(
            "demo",
            "alpha",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        // The `project=` scope leads the workspace token so a hub-global tail
        // can be filtered per-project.
        assert!(lines[0].contains("project=demo workspace=alpha"));
        assert!(lines[0].contains("none -> working"));
        assert!(lines[1].contains("project=demo workspace=alpha"));
        assert!(lines[1].contains("working -> awaiting_input"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_workspace_dialog_event_writes_block_and_recovery_lines() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_workspace_dialog_event("demo", "alpha", "usage-limit", true).unwrap();
        append_workspace_dialog_event("demo", "alpha", "usage-limit", false).unwrap();
        // A kind with whitespace folds to underscores so the line stays a
        // single parseable record.
        append_workspace_dialog_event("demo", "bravo", "trust prompt", true).unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "log: {log}");

        // Each line carries a parseable RFC3339 timestamp prefix.
        for line in &lines {
            let ts = line.split_whitespace().next().unwrap();
            DateTime::parse_from_rfc3339(ts).unwrap();
        }
        // The `project=` scope leads the workspace token on every dialog line.
        assert!(
            lines[0].ends_with(
                " project=demo workspace=alpha working -> blocked reason=dialog:usage-limit"
            ),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[1].ends_with(
                " project=demo workspace=alpha blocked -> working reason=dialog:usage-limit:cleared"
            ),
            "line: {}",
            lines[1]
        );
        assert!(
            lines[2].ends_with(
                " project=demo workspace=bravo working -> blocked reason=dialog:trust_prompt"
            ),
            "line: {}",
            lines[2]
        );

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

    #[test]
    fn append_task_event_uses_socket_when_direct_append_is_permission_denied() {
        use std::io::BufRead;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let sock = short_test_socket("task-perm");
        std::env::set_var("SHELBI_HUB_SOCK", &sock);

        let log_path = events_log_path().unwrap();
        std::fs::write(&log_path, "").unwrap();
        std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o444)).unwrap();

        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let daemon_log_path = log_path.clone();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let msg: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            let body = msg["line"].as_str().unwrap().to_string();
            std::fs::set_permissions(&daemon_log_path, std::fs::Permissions::from_mode(0o644))
                .unwrap();
            append_external_event(&body).unwrap();
            stream.write_all(DAEMON_ACK).unwrap();
        });

        append_task_event(
            "demo",
            "promote-me",
            "default",
            Column::backlog(),
            Column::todo(),
            "orchestrator:zen-promote category=2",
        )
        .unwrap();
        daemon.join().unwrap();

        let log = std::fs::read_to_string(&log_path).unwrap();
        let line = log
            .lines()
            .next()
            .expect("daemon should have appended task event");
        assert!(line.contains(" task=promote-me "), "line: {line}");
        assert!(line.contains(" backlog -> todo "), "line: {line}");
        assert!(
            line.contains(" reason=orchestrator:zen-promote_category=2 "),
            "line: {line}"
        );
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

        append_workspace_pane_event("demo", "alpha", false, "signal:SIGTERM").unwrap();
        append_workspace_pane_event("demo", "bravo", false, "exit:0").unwrap();
        append_workspace_pane_event("demo", "charlie", false, "claude exited normally").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "log: {log}");

        // The `project=` scope leads the `workspace=` token so a hub-global
        // tail can tell two projects' same-named panes apart.
        assert!(
            lines[0].contains(" project=demo workspace=alpha "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].contains(" pane_alive=false "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].ends_with(" reason=signal:SIGTERM"),
            "line: {}",
            lines[0]
        );

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
    fn same_named_workspace_in_two_projects_does_not_cross_talk() {
        // Regression for the cross-project false-death bug: two projects each
        // own a workspace named `alpha`. Project `beta`'s alpha pane exits
        // (real death) while project `demo`'s alpha stays healthy. Because
        // every line carries a `project=` scope, an orchestrator filtering the
        // hub-global log to its own project can tell the two apart — `demo`
        // sees no death for *its* alpha, so the "pane died, surface to user"
        // rule never trips spuriously.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");

        // Only beta's alpha dies. demo's alpha is alive and emits nothing.
        append_workspace_pane_event("beta", "alpha", false, "exit:0").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();

        // The death is scoped to beta on the wire.
        assert!(
            log.contains(" project=beta workspace=alpha pane_alive=false "),
            "beta's alpha death must be project-scoped; got: {log}"
        );
        // A demo-scoped orchestrator (filtering `project=demo`) sees no death
        // for its own alpha — no cross-talk, no false burst.
        assert!(
            !log.contains("project=demo workspace=alpha pane_alive=false"),
            "demo's healthy alpha must not appear dead; got: {log}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn healthy_pane_emits_no_pane_death_on_the_wire() {
        // The false-death burst scenario: a workspace whose pane is alive and
        // mid-task must produce no `pane_alive=false` line for its own project.
        // The pane-death line is only ever written by the pane wrapper on a
        // real subprocess exit (see `append_workspace_pane_event`) — the poller
        // never fabricates one, even when its status.yaml was reset. So for a
        // healthy pane, the only workspace lines that can appear are state
        // transitions, all scoped to the project. Assert the log carries no
        // death line for the healthy project.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");

        // A healthy pane's poller only ever emits scoped state transitions.
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        append_workspace_event(
            "demo",
            "alpha",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        assert!(
            !log.contains("pane_alive=false"),
            "a healthy pane must never produce a pane-death line; got: {log}"
        );
        // Every workspace line it *does* produce is project-scoped.
        for line in log.lines().filter(|l| l.contains("workspace=alpha")) {
            assert!(
                line.contains("project=demo workspace=alpha"),
                "workspace line must be project-scoped: {line}"
            );
        }

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
    fn append_send_event_writes_delivery_verdict_line() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_send_event("demo", "alpha", "submitted", "busy_observed").unwrap();
        append_send_event("demo", "alpha", "stuck", "no submit signal").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        // Leading RFC3339 timestamp keeps the line uniform with the rest of
        // events.log (and parseable by the activity feed / `events tail`).
        assert!(DateTime::parse_from_rfc3339(lines[0].split_whitespace().next().unwrap()).is_ok());
        assert!(
            lines[0].contains(
                " send project=demo workspace=alpha status=submitted detail=busy_observed"
            ),
            "{}",
            lines[0]
        );
        // Detail whitespace folds to underscores so the line stays a
        // single-record `key=value` stream.
        assert!(
            lines[1].contains(
                " send project=demo workspace=alpha status=stuck detail=no_submit_signal"
            ),
            "{}",
            lines[1]
        );

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
            "demo",
            "fix-login",
            "default",
            Column::todo(),
            Column::in_progress(),
            "assigned",
        )
        .unwrap();
        append_task_event(
            "demo",
            "fix-login",
            "default",
            Column::in_progress(),
            Column::review(),
            "workspace_review",
        )
        .unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);

        // Each line must split cleanly back into its fields:
        // `<ts> project=<project> task=<id> workflow=<name> <from> -> <to> reason=<r>
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
        assert_eq!(parsed[0][1], "project=demo");
        assert_eq!(parsed[0][2], "task=fix-login");
        assert_eq!(parsed[0][3], "workflow=default");
        assert_eq!(parsed[0][4], "todo");
        assert_eq!(parsed[0][5], "->");
        assert_eq!(parsed[0][6], "in_progress");
        assert_eq!(parsed[0][7], "reason=assigned");
        assert_eq!(parsed[0][8], "from_category=ready");
        assert_eq!(parsed[0][9], "to_category=active");
        assert_eq!(parsed[1][6], "review");
        assert_eq!(parsed[1][7], "reason=workspace_review");
        assert_eq!(parsed[1][8], "from_category=active");
        assert_eq!(parsed[1][9], "to_category=handoff");

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

        append_task_event("demo", "a", "", Column::todo(), Column::done(), "assigned").unwrap();
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
            "demo",
            "ship-it",
            "feature-task",
            Column::in_progress(),
            Column::review(),
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

        append_heartbeat_event("myapp", 5, 4, None).unwrap();
        append_heartbeat_event("myapp", 0, 0, None).unwrap();

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
    fn heartbeat_zen_cues_write_expected_shapes() {
        // Zen-on heartbeats carry a `zen=on` marker right after `heartbeat`.
        // Plain adds nothing more; Summary appends the one-line reminder;
        // Reread appends the full re-read instruction. Zen-off stays bare.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_heartbeat_event("myapp", 2, 1, Some(ZenHeartbeatCue::Plain)).unwrap();
        append_heartbeat_event(
            "myapp",
            2,
            1,
            Some(ZenHeartbeatCue::Summary("Zen: do the thing.".into())),
        )
        .unwrap();
        append_heartbeat_event("myapp", 2, 1, Some(ZenHeartbeatCue::Reread)).unwrap();
        append_heartbeat_event("myapp", 2, 1, None).unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(
            lines[0].ends_with(" project=myapp heartbeat zen=on zen_eligible=2 idle_workspaces=1"),
            "plain: {}",
            lines[0]
        );
        assert!(
            lines[1].ends_with(
                " project=myapp heartbeat zen=on zen_eligible=2 idle_workspaces=1 — Zen: do the thing."
            ),
            "summary: {}",
            lines[1]
        );
        assert!(
            lines[2].ends_with(
                " project=myapp heartbeat zen=on zen_eligible=2 idle_workspaces=1 — re-read zenmode.md now to refresh Zen policy"
            ),
            "reread: {}",
            lines[2]
        );
        // Zen off: no `zen=on`, no reminder — the pre-Zen-pairing shape.
        assert!(
            lines[3].ends_with(" project=myapp heartbeat zen_eligible=2 idle_workspaces=1"),
            "off: {}",
            lines[3]
        );
        assert!(!lines[3].contains("zen=on"), "off must not carry zen=on");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn dispatch_event_writes_distinct_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // `confirmed` and `stalled` are the two dispatch-verification statuses
        // the workspace dispatch path emits (see `shelbi_orchestrator::workspace`);
        // the orchestrator greps for `stalled` to recover a dispatch that never
        // reached its workspace.
        append_dispatch_event("fix-login", "alpha", "confirmed", "busy observed").unwrap();
        append_dispatch_event("build-thing", "charlie", "stalled", "readiness timeout").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        // Shape: `<ts> dispatch task=<id> workspace=<name> status=<s> detail=<d>`.
        // The `dispatch` prefix lets `shelbi events tail` show it without
        // colliding with task=... or workspace=... lines.
        let line = lines[0];
        assert!(line.contains(" dispatch task=fix-login "), "line: {line}");
        assert!(line.contains(" workspace=alpha "), "line: {line}");
        assert!(line.contains(" status=confirmed "), "line: {line}");
        // Whitespace in detail folds to underscores so the line stays parseable.
        assert!(line.ends_with(" detail=busy_observed"), "line: {line}");
        assert!(lines[1].contains(" status=stalled "), "line: {}", lines[1]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_dryrun_event_writes_canonical_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_zen_dryrun_event("fix-typo", "consider-auto-promote", "mechanically eligible")
            .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        let line = lines[0];
        // Prefix lets the activity feed match and render dry-run rows
        // with a distinct visual tag. Detail whitespace folds to
        // underscores so the line stays parseable.
        assert!(line.contains(" zen-dryrun task=fix-typo "), "line: {line}");
        assert!(
            line.contains(" action=consider-auto-promote "),
            "line: {line}"
        );
        assert!(
            line.ends_with(" detail=mechanically_eligible"),
            "line: {line}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_mode_event_writes_canonical_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_zen_mode_event("alpha", "off", "on", "user:cli").unwrap();
        append_zen_mode_event("alpha", "on", "paused", "user:hotkey").unwrap();
        append_zen_mode_event("bravo", "paused", "off", "system:crash-recovery").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        // Each line carries a `project=<name>` scope so the hub-global tail
        // can be filtered per-orchestrator. Shape: `<ts> project=<name>
        // mode=zen <prev> -> <new> reason=<source>`.
        assert!(lines[0].contains(" project=alpha mode=zen off -> on reason=user:cli"));
        assert!(lines[1].contains(" project=alpha mode=zen on -> paused reason=user:hotkey"));
        assert!(
            lines[2].contains(" project=bravo mode=zen paused -> off reason=system:crash-recovery")
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn task_event_sanitizes_whitespace_in_reason() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event(
            "demo",
            "a",
            "default",
            Column::todo(),
            Column::done(),
            "user moved\nit",
        )
        .unwrap();
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
        assert!(
            lines[0].ends_with(" to_category=done"),
            "line: {}",
            lines[0]
        );

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
            "demo",
            hostile_id,
            hostile_workflow,
            Column::todo(),
            Column::in_progress(),
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
                    "demo",
                    &format!("t{i:04}"),
                    "default",
                    Column::todo(),
                    Column::in_progress(),
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
                append_workspace_event("demo", "alpha", prev, WorkspaceState::AwaitingInput)
                    .unwrap();
            }
        });
        task_thread.join().unwrap();
        workspace_thread.join().unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(
            lines.len(),
            2 * N,
            "expected {} lines, got {}",
            2 * N,
            lines.len()
        );

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
                assert!(
                    line.contains("reason="),
                    "task line missing reason: {line:?}"
                );
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
