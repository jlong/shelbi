//! Activity feed — human-friendly view of `~/.shelbi/events.log`.
//!
//! Renders the same append-only event stream `shelbi events tail`
//! consumes, but reformatted as a date-bucketed reverse-chronological
//! feed: who started what, who finished what, who's idle, who's waiting.
//! Each row reads as a plain sentence — a colored `[name]` identity chip
//! pinned top-left, a dim verb + the work's real task title wrapping in
//! the middle, and a category-tinted state pill + relative time pinned
//! top-right — so the eye can group runs without re-reading names.
//!
//! Hosted in the project's hidden stash session (`shelbi __activity
//! <project>`) and swapped into the dashboard's right pane by
//! `show_view("activity")`. Parent shell wraps invocation in
//! `while true; do …; done` so a crash respawns the TUI.
//!
//! No quit key: switching away is the palette's job, same as
//! [`crate::kanban`] and [`crate::review`].
//!
//! The feed is built from three on-disk sources:
//!
//! - `~/.shelbi/events.log` — the source of truth (append-only,
//!   rfc3339-timestamped one-line records).
//! - `~/.shelbi/projects/<p>/tasks/<id>.md` — task title + branch for
//!   the row's title and dim second line. Cached by id, invalidated
//!   when the file's mtime changes.
//! - The events list itself — walked backwards to pair a
//!   `in_progress -> review` event with its prior `* -> in_progress`
//!   so the row can show "took 18m".

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Datelike, Local, Utc};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use shelbi_core::{Column, StatusCategory, DEFAULT_WORKFLOW_NAME};
use shelbi_state::keymap::{load_keymaps, ActivityAction, DisplayStyle, Keymaps};
use shelbi_state::{events_log_path, WorkspaceState, ZenModeState};

use crate::keymap::format_chord_or_unbound;

/// Per-agent tint color. Identity in the feed is carried by color — a
/// leading `●` dot plus the agent name, both painted in the workspace's
/// tint — not by a picture. The mapping is hard-coded to the six declared
/// phonetic-letter workspaces; every other name falls back to
/// [`Color::Gray`], so a project that names workspaces `frontend` /
/// `backend` still renders, just without a unique tint.
fn agent_color(name: &str) -> Color {
    match name {
        "alpha" => Color::Cyan,
        "bravo" => Color::Magenta,
        "charlie" => Color::Green,
        "delta" => Color::Yellow,
        "echo" => Color::Blue,
        "foxtrot" => Color::LightRed,
        _ => Color::Gray,
    }
}

/// Per-category tint for the status glyph (`▶`/`✓`/`✔`/…). Mirrors the
/// canonical 5-status palette the kanban board uses so a "started" glyph
/// reads the same active-yellow here as the In Progress column does
/// there — one color language across both views.
fn category_color(category: StatusCategory) -> Color {
    match category {
        StatusCategory::Backlog => Color::Gray,
        StatusCategory::Ready => Color::Blue,
        StatusCategory::Active => Color::Yellow,
        StatusCategory::Handoff => Color::Magenta,
        StatusCategory::Done => Color::Green,
        StatusCategory::Archived => Color::DarkGray,
    }
}

/// One parsed line out of `events.log`. The raw line is kept on every
/// variant so the renderer can fall back to the original text if a
/// later format change introduces a shape we don't recognize — nothing
/// in the file should ever be silently dropped.
#[derive(Debug, Clone)]
pub enum Event {
    /// A task transition. Carries the workflow-aware fields documented in
    /// `Plans/workflows.md` §10 (`workflow=`, `from_category=`,
    /// `to_category=`); pre-workflow lines have those filled in by the
    /// back-compat parser — `workflow` defaults to
    /// [`DEFAULT_WORKFLOW_NAME`] and the categories are derived from the
    /// canonical 5-status column-to-category map.
    Task {
        ts: DateTime<Utc>,
        id: String,
        workflow: String,
        from: Column,
        to: Column,
        reason: String,
        /// Agent name embedded in the `reason=` field as `_agent=<name>`
        /// — set by `shelbi task start` when it spawns a workspace with a
        /// specific agent loaded. `None` for events without the segment
        /// (older lines from before this field was emitted, plus
        /// transitions that don't spawn an agent).
        agent: Option<String>,
        from_category: StatusCategory,
        to_category: StatusCategory,
        raw: String,
    },
    Workspace {
        ts: DateTime<Utc>,
        name: String,
        prev: Option<WorkspaceState>,
        new: WorkspaceState,
        raw: String,
    },
    /// A `shelbi zen dry-run` preview decision — recorded but never
    /// acted on. Rendered with a distinct visual tag so the user can
    /// tell at a glance these rows reflect what Zen *would* have done,
    /// not a real state change.
    ZenDryRun {
        ts: DateTime<Utc>,
        task_id: String,
        action: String,
        detail: String,
        raw: String,
    },
    /// Hub-poller heartbeat — `<ts> project=<name> heartbeat`. The
    /// orchestrator follows the events log and uses these as a
    /// guaranteed recurring wake-up when the board is otherwise quiet,
    /// but a human reading the activity feed shouldn't see them — they
    /// produce one row every few minutes saying nothing happened. We
    /// keep the parsed variant so the raw line survives any future
    /// "show internal events" toggle, then filter it out at render time.
    Heartbeat {
        ts: DateTime<Utc>,
        project: String,
        raw: String,
    },
    /// A classified system / infra event: everything that isn't a task
    /// transition, workspace-state ping, zen dry-run, or heartbeat. Covers
    /// the `ssh`, `dispatch`, `rebase`, `worktree-detach`, `closed`,
    /// `handoff`, `send`, `message`, `mode=zen`, `supervision`, and
    /// pane-death line kinds — each parsed into structured fields so the
    /// renderer can build a human row (never raw wire syntax) and so
    /// consecutive near-duplicates can be folded into one summary row.
    System(SystemEvent),
    /// Recognized timestamp but the rest doesn't match any known shape.
    /// Genuine last resort — rendered as a cleaned-up row (the leading ISO
    /// timestamp stripped, relative time on the right) so nothing vanishes
    /// and nothing shows raw wire syntax with a full timestamp.
    Unknown {
        ts: Option<DateTime<Utc>>,
        raw: String,
    },
}

/// The classified kind of a [`SystemEvent`]. Drives both the renderer's
/// per-kind verb/glyph choice and — together with the event's target and
/// status — the fold key that collapses a flapping infra loop into one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemKind {
    /// `ssh reverse-forward host=… status=…` — a reverse-tunnel attempt.
    Ssh,
    /// `dispatch task=… workspace=… status=…` — a task handed to a worker.
    Dispatch,
    /// `rebase task=… workspace=… status=…` — a branch rebased onto base.
    Rebase,
    /// `worktree-detach task=… workspace=… status=…` — a worktree released.
    WorktreeDetach,
    /// `project=… closed reason=…` — a project torn down.
    Closed,
    /// `project=… handoff outcome=…` — an orchestrator handoff attempt.
    Handoff,
    /// `send project=… workspace=… status=…` — a `shelbi send` delivery.
    Send,
    /// `message=… task=… push=…|ack=…` — a worker message push / ack.
    Message,
    /// `project=… mode=zen <prev> -> <new>` — a Zen Mode toggle.
    Mode,
    /// `project=… supervision=…` — a pane restart / crash-loop give-up.
    Supervision,
    /// `[project=…] workspace=… pane_alive=<bool>` — a pane liveness change.
    PaneDeath,
    /// A project-scoped verb we don't model individually — still rendered
    /// as a clean human row (verb + humanized detail), never raw.
    Other,
}

/// One parsed system / infra event. Every field the renderer needs is
/// lifted out of the wire line so it never has to fall back to the raw
/// string. `target` and `status` are the stable identity of the event
/// (host / task / workspace and the status token) and form the fold key
/// together with `kind`; `detail` is the volatile tail and is excluded
/// from that key so a flapping loop still collapses when only its detail
/// text jitters.
#[derive(Debug, Clone)]
pub struct SystemEvent {
    pub ts: DateTime<Utc>,
    pub kind: SystemKind,
    /// Project scope, when the line carried a `project=<name>` prefix.
    pub project: Option<String>,
    /// The primary subject — host (ssh), task id (dispatch/rebase/detach/
    /// message), workspace (send/pane), or project (closed/handoff/mode).
    pub target: Option<String>,
    /// A short status / outcome token (`failed`, `established`, `ok`,
    /// `up-to-date`, `written`, …) when the line carries one.
    pub status: Option<String>,
    /// The dim detail tail — a humanized fragment (never raw `key=value`),
    /// or `None`. Excluded from the fold key.
    pub detail: Option<String>,
    pub raw: String,
}

impl SystemEvent {
    /// The identity used to fold consecutive near-duplicates: the triple of
    /// kind, target, and status, deliberately ignoring the timestamp and the
    /// volatile detail tail (per the task spec's dedup definition).
    fn fold_key(&self) -> (SystemKind, Option<&str>, Option<&str>) {
        (self.kind, self.target.as_deref(), self.status.as_deref())
    }
}

impl Event {
    pub fn ts(&self) -> Option<DateTime<Utc>> {
        match self {
            Event::Task { ts, .. }
            | Event::Workspace { ts, .. }
            | Event::ZenDryRun { ts, .. }
            | Event::Heartbeat { ts, .. } => Some(*ts),
            Event::System(sys) => Some(sys.ts),
            Event::Unknown { ts, .. } => *ts,
        }
    }
}

/// Cached subset of a task file frontmatter that the feed renders.
/// `mtime` lets us re-read the file lazily when it changes (e.g. the
/// orchestrator renames a task) without reparsing every other task.
#[derive(Debug, Clone)]
struct TaskMeta {
    title: String,
    branch: Option<String>,
    assigned_to: Option<String>,
    mtime: Option<SystemTime>,
}

/// Active filter pills above the feed. Both flags off means "All" — the
/// pill row's `All` chip lights up and every event is rendered. Toggling
/// `zen` or `workspaces` switches to a multi-select union: any event that
/// matches at least one active pill is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActivityFilter {
    pub zen: bool,
    pub workspaces: bool,
}

impl ActivityFilter {
    /// `All` is implied — neither specific pill is active, so we show
    /// every event regardless of kind.
    pub fn is_all(&self) -> bool {
        !self.zen && !self.workspaces
    }

    /// `true` when this event passes the active filter. With no specific
    /// pill on, everything passes; otherwise the event must match at
    /// least one active pill (union, not intersection).
    ///
    /// Heartbeats are always rejected, regardless of which pill is
    /// active. They're an orchestrator wake-up, not human-facing
    /// activity — a row saying "nothing happened" every three minutes
    /// would drown the feed.
    pub fn matches(&self, ev: &Event) -> bool {
        if matches!(ev, Event::Heartbeat { .. }) {
            return false;
        }
        if self.is_all() {
            return true;
        }
        if self.zen {
            if let Event::Task { reason, .. } = ev {
                if parse_zen_reason(reason).is_some() {
                    return true;
                }
            }
        }
        if self.workspaces && matches!(ev, Event::Workspace { .. }) {
            return true;
        }
        false
    }
}

/// One pill's hit-test record. Captured by [`render_pills`] each frame so
/// the mouse handler can route a left-click on the pill row back to the
/// correct toggle.
#[derive(Debug, Clone, Copy)]
struct PillHit {
    kind: PillKind,
    /// Cell row the pill paints into.
    y: u16,
    /// Inclusive left column of the pill's clickable label.
    x0: u16,
    /// Exclusive right column of the pill's clickable label.
    x1: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PillKind {
    All,
    Zen,
    Workspaces,
}

/// State for the activity TUI.
pub struct ActivityApp {
    pub project_name: String,
    /// Parsed events, oldest → newest (file order). Iterate `.rev()`
    /// for rendering so newest sits at the top of the feed.
    pub events: Vec<Event>,
    /// Bytes already consumed from `events.log` — lets `refresh` only
    /// read the tail on subsequent ticks.
    log_offset: u64,
    log_mtime: Option<SystemTime>,
    task_cache: HashMap<String, TaskMeta>,
    pub last_refresh: Instant,
    pub status_line: String,
    /// Vertical scroll offset, in lines from the top of the rendered
    /// feed. 0 = newest event at top.
    pub scroll: u16,
    /// True until the user scrolls back manually — once they do, new
    /// events appearing at the top no longer chase the cursor.
    pub auto_scroll: bool,
    /// Height of the rendered feed body. Written by `render_full`
    /// every frame and read by the scroll handlers so PageUp/Down step
    /// by a real screen of content.
    viewport_h: u16,
    /// Total number of lines `build_lines` produced this frame. Used
    /// to clamp scroll at the bottom.
    total_lines: u16,
    /// Which pill row toggles are active. Lives on the app — and so for
    /// the lifetime of the view — so the filter survives scrolling and
    /// refreshes but resets when the view is closed and re-launched.
    pub filter: ActivityFilter,
    /// Hit-test records for the pill row, rewritten each frame by
    /// [`render_pills`] and consumed by the mouse handler.
    pill_hits: Vec<PillHit>,
    /// Resolved per-mode chord → action maps used by the input handler.
    /// Loaded once at construction from `~/.shelbi/keys.yaml` (or the
    /// embedded built-ins when no file exists) so a single key press is
    /// one HashMap lookup per layer rather than a long `match` chain.
    keymaps: Keymaps,
    /// Host-platform chord-display convention, detected once at
    /// construction so the footer renderer never re-probes the OS.
    display_style: DisplayStyle,
    /// Set true when the handler observes a `GlobalAction::Quit`; the
    /// outer event loop polls this each tick and returns when it flips.
    /// The parent shell loop respawns us, matching the legacy Ctrl+C
    /// inline return.
    pub should_quit: bool,
}

impl ActivityApp {
    pub fn new(project_name: impl Into<String>) -> Self {
        let project_name = project_name.into();
        // Diagnostics aren't surfaced here — bad/missing keys.yaml falls
        // back to built-in defaults silently, same as every other handler
        // that consumes the loader. Surfacing them is the wizard's job.
        let (keymaps, _diags) = load_keymaps(Some(&project_name));
        Self {
            project_name,
            events: Vec::new(),
            log_offset: 0,
            log_mtime: None,
            task_cache: HashMap::new(),
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            scroll: 0,
            auto_scroll: true,
            viewport_h: 0,
            total_lines: 0,
            filter: ActivityFilter::default(),
            pill_hits: Vec::new(),
            keymaps,
            display_style: DisplayStyle::detect(),
            should_quit: false,
        }
    }

    /// Borrow the resolved keymaps for the input handler to dispatch
    /// chords against. Snapshot once outside the per-tick loop so the
    /// handler can take a `&mut self` without conflicting with the
    /// keymap borrow.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// Cached host-platform chord-display convention. Read by the footer
    /// renderer instead of probing the OS each frame.
    pub fn display_style(&self) -> DisplayStyle {
        self.display_style
    }

    /// Flip `state.json::zen_mode` between On and Off via the shared
    /// [`shelbi_state::toggle_zen_mode`] path — same read/write/log
    /// dance the sidebar and palette use, tagged `user:hotkey` so the
    /// events log can tell the chord apart from the CLI / palette
    /// sources.
    pub fn toggle_zen_mode(&mut self) {
        match shelbi_state::toggle_zen_mode(&self.project_name, "user:hotkey") {
            Ok(target) => {
                let label = match target {
                    ZenModeState::On => "on",
                    ZenModeState::Off => "off",
                    ZenModeState::Paused => "pause",
                };
                self.status_line = format!("zen {label}");
            }
            Err(e) => {
                self.status_line = format!("zen toggle failed: {e}");
            }
        }
    }

    /// Toggle the Zen pill — flips between including-only-zen and not.
    /// Snaps scroll back to the newest so the user's eye stays on the
    /// freshly filtered head instead of a now-empty viewport.
    pub fn toggle_zen_filter(&mut self) {
        self.filter.zen = !self.filter.zen;
        self.snap_to_top();
    }

    /// Toggle the Workspaces pill — same shape as [`Self::toggle_zen_filter`].
    pub fn toggle_workspaces_filter(&mut self) {
        self.filter.workspaces = !self.filter.workspaces;
        self.snap_to_top();
    }

    /// Reset to the "All" pill state — both specific pills off. Bound to
    /// `a` so the user has a single-keystroke escape back to the
    /// unfiltered view regardless of what's currently active.
    pub fn reset_filter(&mut self) {
        self.filter = ActivityFilter::default();
        self.snap_to_top();
    }

    fn snap_to_top(&mut self) {
        self.scroll = 0;
        self.auto_scroll = true;
    }

    /// Find the pill kind under the given click coordinates, if any.
    /// Uses the hit-test records captured by the last `render_pills`
    /// call — so a click outside the painted pill area returns `None`
    /// and the caller leaves the filter alone.
    fn pill_at(&self, col: u16, row: u16) -> Option<PillKind> {
        self.pill_hits
            .iter()
            .find(|p| p.y == row && col >= p.x0 && col < p.x1)
            .map(|p| p.kind)
    }

    /// Route a left-click on the pill row to the matching toggle.
    /// Public so the input layer in [`crate::lib`] can call it without
    /// reaching into the hit-test records directly.
    pub fn click_pill(&mut self, col: u16, row: u16) -> bool {
        match self.pill_at(col, row) {
            Some(PillKind::All) => {
                self.reset_filter();
                true
            }
            Some(PillKind::Zen) => {
                self.toggle_zen_filter();
                true
            }
            Some(PillKind::Workspaces) => {
                self.toggle_workspaces_filter();
                true
            }
            None => false,
        }
    }

    /// Re-read the events log if it's grown or rotated. Cheap when
    /// nothing has changed (mtime + size check then early return).
    /// Errors surface in `status_line` rather than panicking — a
    /// missing log file is a normal startup condition.
    pub fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        let path = match events_log_path() {
            Ok(p) => p,
            Err(e) => {
                self.status_line = format!("events.log path failed: {e}");
                return;
            }
        };
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                // No log file yet — empty feed, no error.
                self.events.clear();
                self.log_offset = 0;
                self.log_mtime = None;
                return;
            }
        };
        let len = meta.len();
        let mtime = meta.modified().ok();

        // File shrank (rotated / truncated) — re-read from the top.
        if len < self.log_offset {
            self.events.clear();
            self.log_offset = 0;
        }

        // Nothing new since last tick.
        if len == self.log_offset && self.log_mtime == mtime {
            return;
        }

        // Read everything we haven't seen yet. The file is small
        // (a few thousand lines is typical), so even a full re-read
        // is cheap; reading only the tail is the common path.
        let text = match read_tail(&path, self.log_offset) {
            Ok(t) => t,
            Err(e) => {
                self.status_line = format!("read events.log: {e}");
                return;
            }
        };
        self.log_offset = len;
        self.log_mtime = mtime;

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            self.events.push(parse_event_line(line));
        }
    }

    pub fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_millis(500) {
            self.refresh();
        }
    }

    pub fn scroll_up(&mut self) {
        // Newest is at the top with scroll=0; scrolling "up" means
        // moving toward older events, which is a positive scroll
        // offset since Paragraph's scroll trims from the top.
        self.scroll = self.scroll.saturating_add(1);
        self.auto_scroll = false;
    }

    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
        if self.scroll == 0 {
            self.auto_scroll = true;
        }
    }

    pub fn scroll_page_up(&mut self) {
        let step = self.viewport_h.max(1);
        self.scroll = self.scroll.saturating_add(step);
        self.auto_scroll = false;
    }

    pub fn scroll_page_down(&mut self) {
        let step = self.viewport_h.max(1);
        self.scroll = self.scroll.saturating_sub(step);
        if self.scroll == 0 {
            self.auto_scroll = true;
        }
    }

    pub fn scroll_home(&mut self) {
        self.scroll = 0;
        self.auto_scroll = true;
    }

    pub fn scroll_end(&mut self) {
        // "End" in newest-on-top reading order means the oldest entry.
        self.scroll = self.total_lines.saturating_sub(1);
        self.auto_scroll = false;
    }

    /// Look up the latest known metadata for a task. Re-reads the
    /// task file lazily when its mtime has changed; returns `None`
    /// when the file is gone (deleted task) so callers fall back to
    /// the task id as the display label.
    fn task_meta(&mut self, id: &str) -> Option<&TaskMeta> {
        let path = match shelbi_state::task_path(&self.project_name, id) {
            Ok(p) => p,
            Err(_) => return None,
        };
        let mtime = fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        let stale = self
            .task_cache
            .get(id)
            .map(|m| m.mtime != mtime)
            .unwrap_or(true);
        if stale {
            match shelbi_state::load_task(&self.project_name, id) {
                Ok(tf) => {
                    self.task_cache.insert(
                        id.to_string(),
                        TaskMeta {
                            title: tf.task.title,
                            branch: tf.task.branch,
                            assigned_to: tf.task.assigned_to,
                            mtime,
                        },
                    );
                }
                Err(_) => {
                    self.task_cache.remove(id);
                }
            }
        }
        self.task_cache.get(id)
    }

    /// Find the matching `* -> in_progress` event preceding `idx` for
    /// the same task id. Returns its timestamp so the caller can
    /// compute "took Xm" for the review handoff at `idx`.
    fn started_at(&self, idx: usize, task_id: &str) -> Option<DateTime<Utc>> {
        self.events[..idx].iter().rev().find_map(|e| match e {
            Event::Task { id, to, ts, .. } if id == task_id && *to == Column::in_progress() => {
                Some(*ts)
            }
            _ => None,
        })
    }
}

/// Read everything past `offset` in `path` as UTF-8. Best-effort:
/// non-UTF-8 bytes become replacement chars rather than failing the
/// whole read.
fn read_tail(path: &PathBuf, offset: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path)?;
    if offset > 0 {
        f.seek(SeekFrom::Start(offset))?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Split an optional leading `project=<name> ` scope off an event body.
/// Most hub-emitted lines are prefixed with the project the event belongs
/// to (`project=shelbi task=…`, `project=shelbi workspace=…`,
/// `project=shelbi closed …`); a handful of infra kinds (`ssh`, `dispatch`,
/// `rebase`, `worktree-detach`, `message=…`) are not, and `send` carries
/// its `project=` as a later field. Returns the parsed project name (if the
/// prefix was present) and the remainder to classify.
fn strip_project_prefix(rest: &str) -> (Option<String>, &str) {
    match rest.strip_prefix("project=") {
        Some(after) => match after.split_once(' ') {
            Some((name, tail)) => (Some(name.to_string()), tail),
            None => (Some(after.to_string()), ""),
        },
        None => (None, rest),
    }
}

/// Parse one `events.log` line into an [`Event`]. Best-effort: every
/// emitted kind is classified into a first-class, human-renderable shape.
/// Only a genuinely unrecognized line lands in [`Event::Unknown`], and even
/// that renders cleaned up (no raw leading ISO timestamp) rather than being
/// dropped.
pub fn parse_event_line(line: &str) -> Event {
    let raw = line.to_string();
    let mut parts = line.splitn(2, ' ');
    let ts_str = parts.next().unwrap_or("");
    let after_ts = parts.next().unwrap_or("");
    let ts = DateTime::parse_from_rfc3339(ts_str)
        .ok()
        .map(|t| t.with_timezone(&Utc));

    let Some(ts) = ts else {
        return Event::Unknown { ts: None, raw };
    };

    // Peel the `project=<name>` scope so both prefixed and legacy-bare
    // task/workspace lines route through the same classifier below.
    let (project, rest) = strip_project_prefix(after_ts);

    if let Some(rest) = rest.strip_prefix("task=") {
        // Two on-the-wire shapes coexist (`Plans/workflows.md` §10):
        //
        //   Old: `<id> <from> -> <to> reason=<reason>`
        //   New: `<id> workflow=<name> <from> -> <to> reason=<reason>
        //         from_category=<cat> to_category=<cat>`
        //
        // The second token after `task=<id>` disambiguates — if it starts
        // with `workflow=` we read the new fields, otherwise we default
        // `workflow` to "default" and derive categories from the canonical
        // 5-status column-to-category map (`Column::category`).
        let mut tokens = rest.split(' ');
        let id = tokens.next().unwrap_or("").to_string();
        let mut next = tokens.next().unwrap_or("");
        let workflow = if let Some(name) = next.strip_prefix("workflow=") {
            let name = name.to_string();
            next = tokens.next().unwrap_or("");
            name
        } else {
            DEFAULT_WORKFLOW_NAME.to_string()
        };
        let from_s = next;
        let arrow = tokens.next().unwrap_or("");
        let to_s = tokens.next().unwrap_or("");
        // Everything after `<to>` is k=v tokens (reason, from_category,
        // to_category) in any order — defensive against future field
        // shuffles, and tolerant of missing trailing fields on old lines.
        let kv = parse_kv(tokens.collect::<Vec<&str>>().join(" ").as_str());
        if arrow == "->" {
            if let (Ok(from), Ok(to)) = (from_s.parse::<Column>(), to_s.parse::<Column>()) {
                let reason = kv.get("reason").cloned().unwrap_or_default();
                let agent = agent_from_reason(&reason);
                let from_category = kv
                    .get("from_category")
                    .and_then(|s| s.parse::<StatusCategory>().ok())
                    .unwrap_or_else(|| from.category());
                let to_category = kv
                    .get("to_category")
                    .and_then(|s| s.parse::<StatusCategory>().ok())
                    .unwrap_or_else(|| to.category());
                return Event::Task {
                    ts,
                    id,
                    workflow,
                    from,
                    to,
                    reason,
                    agent,
                    from_category,
                    to_category,
                    raw,
                };
            }
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    if let Some(rest) = rest.strip_prefix("zen-dryrun ") {
        // Format: `task=<id> action=<verb> detail=<short>`
        let mut task_id = String::new();
        let mut action = String::new();
        let mut detail = String::new();
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("task=") {
                task_id = v.to_string();
            } else if let Some(v) = tok.strip_prefix("action=") {
                action = v.to_string();
            } else if let Some(v) = tok.strip_prefix("detail=") {
                detail = v.to_string();
            }
        }
        if !task_id.is_empty() && !action.is_empty() {
            return Event::ZenDryRun {
                ts,
                task_id,
                action,
                detail,
                raw,
            };
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    // Heartbeat shape (project-scoped): `heartbeat zen_eligible=<N>
    // idle_workspaces=<M>`. We match on the keyword token alone — the
    // trailing counts are for the orchestrator's react rule, not the feed.
    if rest == "heartbeat" || rest.starts_with("heartbeat ") {
        if let Some(name) = &project {
            return Event::Heartbeat {
                ts,
                project: name.clone(),
                raw,
            };
        }
    }

    // Workspace lines. New emissions use `workspace=<name>`; legacy lines
    // (and one-release tooling that lags the rename) use `worker=<name>` —
    // both accepted. A `workspace=<name>` line is one of three shapes: a
    // state transition (`<prev> -> <new>`), a pane-liveness change
    // (`pane_alive=<bool>`), or a supervision line (handled below); we
    // disambiguate on the fields present.
    let ws_rest = rest
        .strip_prefix("workspace=")
        .or_else(|| rest.strip_prefix("worker="));
    if let Some(ws_rest) = ws_rest {
        let mut tokens = ws_rest.split(' ');
        let name = tokens.next().unwrap_or("");
        let second = tokens.next().unwrap_or("");
        // `workspace=<name> pane_alive=<bool> reason=<r>` — a pane death /
        // recovery, not a state transition.
        if let Some(alive) = second.strip_prefix("pane_alive=") {
            let kv = parse_kv(ws_rest);
            return Event::System(SystemEvent {
                ts,
                kind: SystemKind::PaneDeath,
                project,
                target: Some(name.to_string()),
                status: Some(alive.to_string()),
                detail: kv.get("reason").map(|r| humanize_token(r)),
                raw,
            });
        }
        // `workspace=<name> supervision=<action> …` — a supervisor action
        // scoped to a workspace pane.
        if second.starts_with("supervision=") {
            return parse_supervision(ts, project, Some(name.to_string()), ws_rest, raw);
        }
        // Otherwise a state transition: `<prev> -> <new> [reason=…]`.
        let prev_s = second;
        let arrow = tokens.next().unwrap_or("");
        let new_s = tokens.next().unwrap_or("");
        if arrow == "->" {
            if let Some(new) = parse_workspace_state(new_s) {
                let prev = if prev_s == "none" {
                    None
                } else {
                    parse_workspace_state(prev_s)
                };
                return Event::Workspace {
                    ts,
                    name: name.to_string(),
                    prev,
                    new,
                    raw,
                };
            }
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    // -- Infra kinds carrying no `project=` prefix ------------------------

    if let Some(body) = rest.strip_prefix("ssh reverse-forward ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Ssh,
            project,
            target: kv.get("host").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("detail").map(|d| humanize_token(d)),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("dispatch ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Dispatch,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("workspace").cloned(),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("rebase ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Rebase,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("workspace").cloned(),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("worktree-detach ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::WorktreeDetach,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("workspace").cloned(),
            raw,
        });
    }

    // `send project=<p> workspace=<name> status=<s> detail=<d>` — the
    // `project=` here is a field, not the leading scope, so it survived the
    // prefix peel and we read it back out of the k=v tail.
    if let Some(body) = rest.strip_prefix("send ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Send,
            project: kv.get("project").cloned(),
            target: kv.get("workspace").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("detail").map(|d| humanize_token(d)),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("message=") {
        // `message=<id> task=<id> push=ok` or `… ack=<kind>`.
        let kv = parse_kv(body);
        let (status, verb) = if let Some(p) = kv.get("push") {
            (Some(p.clone()), "push")
        } else if let Some(a) = kv.get("ack") {
            (Some(a.clone()), "ack")
        } else {
            (None, "message")
        };
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Message,
            project,
            target: kv.get("task").cloned(),
            status,
            detail: Some(verb.to_string()),
            raw,
        });
    }

    // -- Project-scoped verbs (prefix already peeled) ---------------------

    if let Some(reason) = rest.strip_prefix("closed") {
        let kv = parse_kv(reason);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Closed,
            project: project.clone(),
            target: project,
            status: None,
            detail: kv.get("reason").map(|r| humanize_token(r)),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("handoff ") {
        let kv = parse_kv(body);
        let detail = kv
            .get("detail")
            .filter(|d| d.as_str() != "-")
            .map(|d| humanize_token(d));
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Handoff,
            project: project.clone(),
            target: project,
            status: kv.get("outcome").cloned(),
            detail,
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("mode=zen ") {
        // `<prev> -> <new> reason=<source>`.
        let mut tokens = body.split(' ');
        let prev = tokens.next().unwrap_or("");
        let _arrow = tokens.next();
        let new = tokens.next().unwrap_or("");
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Mode,
            project: project.clone(),
            target: project,
            status: Some(new.to_string()),
            detail: Some(format!("{prev} → {new}")),
            raw,
        });
    }

    if rest.starts_with("supervision=") {
        return parse_supervision(ts, project, None, rest, raw);
    }

    // Any remaining project-scoped line: keep it human (verb + humanized
    // tail) rather than dumping raw wire syntax. Non-project lines with no
    // recognized shape are the genuine last resort → cleaned Unknown.
    if project.is_some() {
        let verb = rest.split(' ').next().unwrap_or("").to_string();
        let detail = rest
            .split_once(' ')
            .map(|(_, tail)| humanize_token(tail))
            .filter(|d| !d.is_empty());
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Other,
            project,
            target: None,
            status: None,
            detail: detail.or(Some(verb)),
            raw,
        });
    }

    Event::Unknown { ts: Some(ts), raw }
}

/// Parse a supervision line body (`supervision=<action> [target=orchestrator]
/// reason=<r>`) into a [`SystemEvent`]. Shared by the workspace-scoped form
/// (`workspace=<name> supervision=…`) and the orchestrator-scoped form
/// (`supervision=… target=orchestrator …`).
fn parse_supervision(
    ts: DateTime<Utc>,
    project: Option<String>,
    workspace: Option<String>,
    body: &str,
    raw: String,
) -> Event {
    let kv = parse_kv(body);
    // Prefer the explicit workspace, else the `target=` field (orchestrator).
    let target = workspace.or_else(|| kv.get("target").cloned());
    Event::System(SystemEvent {
        ts,
        kind: SystemKind::Supervision,
        project,
        target,
        status: kv.get("supervision").cloned(),
        detail: kv.get("reason").map(|r| humanize_token(r)),
        raw,
    })
}

fn parse_workspace_state(s: &str) -> Option<WorkspaceState> {
    match s {
        "working" => Some(WorkspaceState::Working),
        "awaiting_input" => Some(WorkspaceState::AwaitingInput),
        "blocked" => Some(WorkspaceState::Blocked),
        "paused" => Some(WorkspaceState::Paused),
        _ => None,
    }
}

/// Turn a wire token into a human fragment: underscores and hyphens become
/// spaces so `master_open_failed` reads `master open failed` and
/// `up-to-date` reads `up to date`. Any residual `key=value` pairs are
/// dropped so a row never shows raw wire syntax; if that empties the string,
/// the caller falls back to its own label.
fn humanize_token(s: &str) -> String {
    s.split_whitespace()
        .filter(|tok| !tok.contains('='))
        .map(|tok| tok.replace(['_', '-'], " "))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Rendering
//
// The feed paints into a single scrollable `Paragraph`. We build a
// flat `Vec<Line>` newest-first, interleaving "── Today ──" headers
// when the local-time day changes, then let the Paragraph's scroll
// trim from the top — straightforward and avoids managing per-row
// hit-test areas the feed has no actions for.

/// Public entry point — paints title + scrollable feed + footer hint
/// into `area`. Mutates `app` to record viewport height and total
/// rendered-line count so the scroll handlers can clamp correctly.
pub fn render_full(f: &mut Frame, app: &mut ActivityApp, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(2)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // pills
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_title(f, app, outer[0]);
    render_pills(f, app, outer[1]);
    render_body(f, app, outer[3]);
    render_footer(f, app, outer[4]);
}

fn render_title(f: &mut Frame, app: &ActivityApp, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            "Activity",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}", app.project_name),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(title), area);
}

/// Paint the `All · Zen · Workspaces` pill row and record each pill's
/// cell coordinates in `app.pill_hits` so the mouse handler can route
/// clicks back to the right toggle. Active pills get a bold accent
/// color; inactive ones sit muted so the eye picks out the live filters
/// at a glance.
fn render_pills(f: &mut Frame, app: &mut ActivityApp, area: Rect) {
    app.pill_hits.clear();
    if area.height == 0 {
        return;
    }

    let pills = [
        (PillKind::All, "All", app.filter.is_all(), Color::Cyan),
        (PillKind::Zen, "Zen", app.filter.zen, ZEN_FG),
        (
            PillKind::Workspaces,
            "Workspaces",
            app.filter.workspaces,
            Color::Magenta,
        ),
    ];

    let inactive = Style::default().fg(Color::DarkGray);
    let sep_style = Style::default().fg(Color::DarkGray);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut col: u16 = area.x;
    for (i, (kind, label, active, accent)) in pills.iter().enumerate() {
        if i > 0 {
            let sep = "  ·  ";
            spans.push(Span::styled(sep.to_string(), sep_style));
            col = col.saturating_add(sep.chars().count() as u16);
        }
        let style = if *active {
            Style::default().fg(*accent).add_modifier(Modifier::BOLD)
        } else {
            inactive
        };
        let label_owned = (*label).to_string();
        let len = label_owned.chars().count() as u16;
        spans.push(Span::styled(label_owned, style));
        app.pill_hits.push(PillHit {
            kind: *kind,
            y: area.y,
            x0: col,
            x1: col.saturating_add(len),
        });
        col = col.saturating_add(len);
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_body(f: &mut Frame, app: &mut ActivityApp, area: Rect) {
    app.viewport_h = area.height;
    let width = area.width as usize;
    let now = Utc::now();
    let lines = build_lines(app, width, now);
    app.total_lines = lines.len() as u16;

    // Clamp scroll to the last full screen so the user can't drift
    // off the bottom of the feed.
    let max_scroll = app.total_lines.saturating_sub(area.height);
    if app.scroll > max_scroll {
        app.scroll = max_scroll;
    }

    let body = Paragraph::new(lines).scroll((app.scroll, 0));
    f.render_widget(body, area);
}

fn render_footer(f: &mut Frame, app: &ActivityApp, area: Rect) {
    // Every key glyph is sourced from the merged keymaps and rendered in
    // the host platform's convention — rebinding any of these actions in
    // `keys.yaml` updates the hint on next launch. Multi-bound actions show
    // their first chord only (the full list lives in `config list-actions`).
    let km = app.keymaps();
    let style = app.display_style();
    let fc = |c| format_chord_or_unbound(c, style);
    let text = format!(
        "{}/{} scroll · {}/{} page · {} top · {} bottom · {} refresh · {}/{}/{} filter",
        fc(km.activity.first_chord_for(ActivityAction::ScrollDown)),
        fc(km.activity.first_chord_for(ActivityAction::ScrollUp)),
        fc(km.activity.first_chord_for(ActivityAction::PageUp)),
        fc(km.activity.first_chord_for(ActivityAction::PageDown)),
        fc(km.activity.first_chord_for(ActivityAction::ScrollHome)),
        fc(km.activity.first_chord_for(ActivityAction::ScrollEnd)),
        fc(km.activity.first_chord_for(ActivityAction::Refresh)),
        fc(km.activity.first_chord_for(ActivityAction::ResetFilter)),
        fc(km.activity.first_chord_for(ActivityAction::ToggleZenFilter)),
        fc(km
            .activity
            .first_chord_for(ActivityAction::ToggleWorkspacesFilter)),
    );
    let footer = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(footer, area);
}

/// Build the full feed as a flat `Vec<Line>` ready to hand to a
/// scrollable `Paragraph`. Walks events newest → oldest, inserting a
/// "── Today ──" style header whenever the local-time day changes.
fn build_lines(app: &mut ActivityApp, width: usize, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.events.is_empty() {
        lines.push(Line::from(Span::styled(
            "no activity yet — waiting for the first event",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }

    let today_local = Local::now().date_naive();
    let yesterday_local = today_local.pred_opt();
    let mut last_day: Option<chrono::NaiveDate> = None;

    // Newest indices first, filtered to whatever the pill row says.
    // Cloning the event out is cheap (small strings); it dodges a
    // borrow conflict between iterating `app.events` and calling
    // `app.task_meta` inside the loop body.
    let filter = app.filter;
    let order: Vec<usize> = (0..app.events.len())
        .rev()
        .filter(|&i| filter.matches(&app.events[i]))
        .collect();

    if order.is_empty() {
        lines.push(Line::from(Span::styled(
            "no events match this filter",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }

    // Walk the filtered order newest → oldest. Consecutive system events
    // sharing a fold key (kind + target + status) on the same local day are
    // collapsed into a single row so a flapping infra loop (e.g. an offline
    // host's ssh reverse-forward retrying every couple minutes) can't bury
    // the real activity below it. Because `order` is newest-first, the run's
    // first element is the most recent — its timestamp is the "last seen".
    let mut pos = 0;
    while pos < order.len() {
        let idx = order[pos];
        let ev = app.events[idx].clone();
        let day = ev.ts().map(|t| t.with_timezone(&Local).date_naive());

        if day != last_day {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            let label = match day {
                Some(d) if d == today_local => "Today".to_string(),
                Some(d) if yesterday_local == Some(d) => "Yesterday".to_string(),
                Some(d) => d.format("%A, %B %-d").to_string(),
                None => "—".to_string(),
            };
            lines.push(date_header(&label, width));
            lines.push(Line::raw(""));
            last_day = day;
        }

        if let Event::System(sys) = &ev {
            // How many following events fold into this one — same key, same
            // day, contiguous in the filtered stream.
            let key = sys.fold_key();
            let mut run_end = pos + 1;
            while run_end < order.len() {
                let next = &app.events[order[run_end]];
                let next_day = next.ts().map(|t| t.with_timezone(&Local).date_naive());
                match next {
                    Event::System(s2) if s2.fold_key() == key && next_day == day => {
                        run_end += 1;
                    }
                    _ => break,
                }
            }
            let count = run_end - pos;
            for l in render_system_event(app, sys, width, now, count) {
                lines.push(l);
            }
            lines.push(Line::raw(""));
            pos = run_end;
            continue;
        }

        // Review handoff (in_progress → review) is the only event
        // that joins to its prior `* -> in_progress` partner. Compute
        // it now while we still have `idx` in scope.
        let started_at = if let Event::Task { id, to, .. } = &ev {
            if *to == Column::review() {
                app.started_at(idx, id)
            } else {
                None
            }
        } else {
            None
        };

        for l in render_event(&ev, app, width, now, started_at) {
            lines.push(l);
        }
        lines.push(Line::raw(""));
        pos += 1;
    }

    lines
}

fn date_header(label: &str, width: usize) -> Line<'static> {
    let label_str = format!("── {label} ");
    let trail_w = width.saturating_sub(label_str.chars().count());
    let trail = "─".repeat(trail_w);
    Line::from(Span::styled(
        format!("{label_str}{trail}"),
        Style::default().fg(Color::DarkGray),
    ))
}

/// A colored outcome pill pinned to the right of a row — `[in progress]`,
/// `[ready for review]`, `[done]`, and so on. Tinted with the canonical
/// category color so the pill reads the same as the kanban column it
/// names. Only task-transition rows carry one; system / workspace rows
/// leave it `None`.
#[derive(Clone)]
struct Pill {
    text: String,
    color: Color,
}

/// The state pill for a task landing in `category`: the human column name
/// tinted with the canonical category color.
fn category_pill(category: StatusCategory) -> Pill {
    let text = match category {
        StatusCategory::Backlog => "backlog",
        StatusCategory::Ready => "ready",
        StatusCategory::Active => "in progress",
        StatusCategory::Handoff => "ready for review",
        StatusCategory::Done => "done",
        StatusCategory::Archived => "archived",
    };
    Pill {
        text: text.to_string(),
        color: category_color(category),
    }
}

/// A bracketed identity chip — `[name]` painted in `color`. Agent chips
/// carry the workspace tint (bold); `you` and system sources are dimmed
/// (gray, not bold) so the eye separates human/agent activity from
/// machine noise. An empty label yields no chip at all.
fn identity_chip(label: &str, color: Color, bold: bool) -> Vec<Span<'static>> {
    if label.is_empty() {
        return Vec::new();
    }
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    vec![Span::styled(format!("[{label}]"), style)]
}

/// Chip for an agent-attributed row: the workspace name in its tint, bold.
fn agent_chip(name: &str, color: Color) -> Vec<Span<'static>> {
    identity_chip(name, color, true)
}

/// Chip for a machine-driven or board-level row: a dimmed neutral label
/// (`you`, `zen`, a host, a workspace ping, …).
fn system_chip(label: &str) -> Vec<Span<'static>> {
    identity_chip(label, Color::DarkGray, false)
}

/// The dim second line under a row. `Branch` gets the `⎇ ` icon and holds
/// a git branch name; `Detail` is any other muted context fragment
/// (`backlog → todo`, a zen bail reason, …). Both truncate with `…` and
/// render in [`Color::DarkGray`].
enum SecondaryLine {
    Branch(String),
    Detail(String),
}

/// Foreground for the Zen filter pill. Kept from the old zen styling —
/// the badge/background tint is gone, but green still reads "zen".
const ZEN_FG: Color = Color::Green;

/// One parsed `reason=` string from a zen-driven task event. The
/// orchestrator emits these as `key=value` pairs trailing a
/// `zen:`/`orchestrator:zen-*` head; this enum is the renderer-facing
/// shape after stripping the head and lifting out the fields each
/// variant displays. Unknown keys are dropped — we never want a missing
/// field to crash the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ZenReason {
    Promote {
        category: Option<String>,
    },
    Merge {
        sha: Option<String>,
    },
    FailedChecks {
        cmd: Option<String>,
        exit: Option<String>,
    },
    DiffTooLarge {
        files: Option<String>,
        lines: Option<String>,
    },
    DangerPath {
        paths: Option<String>,
    },
    CiTimeout {
        duration: Option<String>,
    },
    MergeConflict {
        files: Option<String>,
    },
    /// Recognized zen-family prefix but not one of the specific kinds
    /// above. Renders with the generic zen badge + tint so future
    /// reasons still look "machine-driven" without a code change.
    Other,
}

/// Recognize `orchestrator:zen-*` and `zen:*` reason strings and pull
/// out the trailing `key=value` pairs each variant cares about. Returns
/// `None` for non-zen reasons so callers can fall through to the
/// default user-action renderer.
fn parse_zen_reason(reason: &str) -> Option<ZenReason> {
    let (head, rest) = reason.split_once(' ').unwrap_or((reason, ""));
    let extras = parse_kv(rest);
    let get = |k: &str| extras.get(k).cloned();
    Some(match head {
        "orchestrator:zen-promote" => ZenReason::Promote {
            category: get("category"),
        },
        "orchestrator:zen-merge" => ZenReason::Merge { sha: get("sha") },
        "zen:failed-checks" => ZenReason::FailedChecks {
            cmd: get("cmd"),
            exit: get("exit"),
        },
        "zen:diff-too-large" => ZenReason::DiffTooLarge {
            files: get("files"),
            lines: get("lines"),
        },
        "zen:danger-path" => ZenReason::DangerPath {
            paths: get("paths"),
        },
        "zen:ci-timeout" => ZenReason::CiTimeout {
            duration: get("duration"),
        },
        "zen:merge-conflict" => ZenReason::MergeConflict {
            files: get("files"),
        },
        other if other.starts_with("zen:") || other.starts_with("orchestrator:zen-") => {
            ZenReason::Other
        }
        _ => return None,
    })
}

/// Pull `agent=<name>` out of a sanitized reason value. The writer
/// (`shelbi task start`) appends ` agent=<name>` to the human reason and
/// `append_task_event` folds the space into an underscore — so on disk
/// the field lives as one of the underscore-joined segments inside
/// `reason=<value>` (e.g. `orchestrator:auto-dispatch_workspace=alpha_agent=developer`).
/// Returns `None` for lines emitted before this field existed.
fn agent_from_reason(reason: &str) -> Option<String> {
    reason
        .split('_')
        .find_map(|seg| seg.strip_prefix("agent=").map(str::to_string))
}

/// Parse `k=v k2=v2 …` from a reason tail. Values may be double-quoted
/// (`cmd="cargo test"`) to allow embedded spaces. Tokens missing a
/// `=` are skipped silently — the parser should never reject a real
/// event log line.
fn parse_kv(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut chars = s.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }
        if chars.peek() != Some(&'=') {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                chars.next();
            }
            continue;
        }
        chars.next();
        let mut val = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(&c) = chars.peek() {
                if c == '"' {
                    chars.next();
                    break;
                }
                val.push(c);
                chars.next();
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                val.push(c);
                chars.next();
            }
        }
        if !key.is_empty() {
            out.insert(key, val);
        }
    }
    out
}

/// One fully-specified feed row, ready for [`paint_row`] to lay out as a
/// top-aligned three-column flex: the identity chip pins top-left, the
/// state pill + relative time pin top-right, and the message (verb +
/// title) wraps to as many lines as it needs in between. No background
/// fill; the optional secondary prints as a dim indented line beneath.
struct Row {
    /// Bracketed identity chip spans (`[alpha]`, `[you]`, `[zen]`). Painted
    /// on the first line only; wrapped continuation lines leave it blank.
    chip: Vec<Span<'static>>,
    /// Dim verb word (finished / is building / merged / …). Empty means the
    /// message is a bare phrase with no verb column.
    verb: String,
    /// Verb foreground. Task rows keep the dim [`VERB_FG`]; system rows tint
    /// it with the event's status color so `unreachable` reads red.
    verb_color: Color,
    /// The task title (or system phrase) — bright fg, wraps, never truncated.
    title: String,
    /// Style for the title. Defaults to bright white; raw fallbacks dim it.
    title_style: Style,
    /// Optional dim spans appended straight after the title (a zen bail tag
    /// `— checks failed`, a fold `(×N)` trailer). Wrap with the title.
    trail: Vec<Span<'static>>,
    /// Optional right-pinned state pill (`[in progress]`, `[done]`, …).
    pill: Option<Pill>,
    /// Right-aligned relative time; finish rows prefix `took Xm · `.
    time: String,
    /// Optional dim second line.
    secondary: Option<SecondaryLine>,
}

impl Row {
    /// A row with the common defaults filled in — no chip, dim verb, bright
    /// title, no trail, no pill, no secondary. Callers set what they need.
    fn new(verb: impl Into<String>, title: impl Into<String>, time: String) -> Self {
        Row {
            chip: Vec::new(),
            verb: verb.into(),
            verb_color: VERB_FG,
            title: title.into(),
            title_style: Style::default().fg(Color::White),
            trail: Vec::new(),
            pill: None,
            time,
            secondary: None,
        }
    }
}

fn render_event(
    ev: &Event,
    app: &mut ActivityApp,
    width: usize,
    now: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
) -> Vec<Line<'static>> {
    match ev {
        Event::Task {
            ts,
            id,
            from,
            to,
            reason,
            ..
        } => render_task_event(
            app,
            *ts,
            id,
            from.clone(),
            to.clone(),
            reason,
            width,
            now,
            started_at,
        ),
        Event::Workspace { ts, name, new, .. } => {
            render_workspace_event(*ts, name, *new, width, now)
        }
        Event::ZenDryRun {
            ts,
            task_id,
            action,
            detail,
            ..
        } => render_zen_dryrun_event(app, *ts, task_id, action, detail, width, now),
        // Heartbeats never reach this branch in normal operation
        // because `ActivityFilter::matches` rejects them, but exhaustive
        // matching keeps a future "show internal events" toggle from
        // silently dropping them.
        Event::Heartbeat { .. } => Vec::new(),
        // A single (unfolded) system event. Folded runs are rendered by
        // `build_lines` calling `render_system_event` directly with a count.
        Event::System(sys) => render_system_event(app, sys, width, now, 1),
        Event::Unknown { ts, raw } => {
            let when = ts.map(|t| relative_time(t, now)).unwrap_or_default();
            // Strip the leading ISO timestamp so the row never shows raw
            // wire syntax with a full timestamp — the relative time on the
            // right is the row's only clock. Lines with no parseable
            // timestamp (the true last resort) show verbatim.
            let body = if ts.is_some() {
                raw.split_once(' ').map(|x| x.1).unwrap_or(raw).to_string()
            } else {
                raw.to_string()
            };
            let mut row = Row::new("", body, when);
            row.chip = system_chip("event");
            row.title_style = Style::default().fg(Color::DarkGray);
            paint_row(row, width)
        }
    }
}

/// Render a system / infra event into feed lines. `count` is the number of
/// consecutive near-duplicate events this row stands for (1 = a lone event);
/// when `count > 1` the row gains a dim `(N attempts)` / `(×N)` trailer and
/// the right-aligned time reads as the most-recent occurrence ("last seen").
fn render_system_event(
    app: &mut ActivityApp,
    sys: &SystemEvent,
    width: usize,
    now: DateTime<Utc>,
    count: usize,
) -> Vec<Line<'static>> {
    let when = relative_time(sys.ts, now);
    let status = sys.status.as_deref();

    // Per-kind chip / verb / verb-color / title. `chip` is the bracketed
    // left-hand subject (a tinted workspace where one applies, else a dimmed
    // neutral label); the verb carries the event's status tint so
    // `unreachable` reads red; `title` is the human summary the row centers
    // on. System rows carry no state pill — the verb color is their signal.
    let (chip, verb, verb_color, title): (Vec<Span<'static>>, &str, Color, String) = match sys.kind
    {
        SystemKind::Ssh => {
            let host = sys.target.as_deref().unwrap_or("host");
            let (verb, color) = if status == Some("failed") {
                ("unreachable", Color::LightRed)
            } else if status == Some("established") {
                ("connected", Color::Green)
            } else {
                ("ssh", Color::Gray)
            };
            let title = sys
                .detail
                .clone()
                .unwrap_or_else(|| "reverse-forward".to_string());
            (system_chip(host), verb, color, title)
        }
        SystemKind::Dispatch => {
            let (name, color) = agent_display(sys.detail.as_deref());
            (
                agent_chip(&name, color),
                "dispatched",
                category_color(StatusCategory::Active),
                system_task_title(app, sys.target.as_deref()),
            )
        }
        SystemKind::Rebase => {
            let (name, color) = agent_display(sys.detail.as_deref());
            (
                agent_chip(&name, color),
                "rebased",
                rebase_status_color(status),
                system_task_title(app, sys.target.as_deref()),
            )
        }
        SystemKind::WorktreeDetach => {
            let (name, color) = agent_display(sys.detail.as_deref());
            let ok = status == Some("ok");
            (
                agent_chip(&name, color),
                "detached",
                if ok { Color::Gray } else { Color::LightRed },
                system_task_title(app, sys.target.as_deref()),
            )
        }
        SystemKind::Send => {
            let (name, color) = agent_display(sys.target.as_deref());
            let stuck = status == Some("stuck");
            (
                agent_chip(&name, color),
                "sent",
                if stuck { Color::Yellow } else { Color::Blue },
                send_title(status),
            )
        }
        SystemKind::Message => {
            let verb = if sys.detail.as_deref() == Some("ack") {
                "ack"
            } else {
                "delivered"
            };
            (
                system_chip("message"),
                verb,
                Color::Blue,
                system_task_title(app, sys.target.as_deref()),
            )
        }
        SystemKind::Closed => (
            system_chip(sys.target.as_deref().unwrap_or("project")),
            "closed",
            Color::Gray,
            sys.detail.clone().unwrap_or_else(|| "project closed".into()),
        ),
        SystemKind::Handoff => (
            system_chip(sys.target.as_deref().unwrap_or("orchestrator")),
            "handoff",
            handoff_status_color(status),
            format!("handoff {}", status.unwrap_or("attempted")),
        ),
        // `[zen] Zen mode turned off` reads as a plain sentence — no verb
        // column, the status carries into the phrase itself.
        SystemKind::Mode => (
            system_chip("zen"),
            "",
            Color::Green,
            format!("Zen mode turned {}", status.unwrap_or("changed")),
        ),
        SystemKind::Supervision => {
            let gave_up = status == Some("gave-up");
            (
                system_chip(sys.target.as_deref().unwrap_or("orchestrator")),
                "supervisor",
                if gave_up { Color::LightRed } else { Color::Yellow },
                humanize_token(status.unwrap_or("supervised")),
            )
        }
        SystemKind::PaneDeath => {
            let alive = status == Some("true");
            (
                system_chip(sys.target.as_deref().unwrap_or("pane")),
                "pane",
                if alive { Color::Gray } else { Color::LightRed },
                if alive {
                    "pane back".to_string()
                } else {
                    "pane died".to_string()
                },
            )
        }
        SystemKind::Other => (
            system_chip(sys.target.as_deref().unwrap_or("")),
            "event",
            Color::Gray,
            sys.detail.clone().unwrap_or_else(|| "event".into()),
        ),
    };

    // Fold trailer + last-seen time. The count is the number of collapsed
    // occurrences; ssh reads more naturally as "attempts".
    let (trail, time) = if count > 1 {
        let label = if matches!(sys.kind, SystemKind::Ssh) {
            format!(" ({count} attempts)")
        } else {
            format!(" (×{count})")
        };
        let trail = vec![Span::styled(label, Style::default().fg(Color::DarkGray))];
        (trail, format!("last {when}"))
    } else {
        (Vec::new(), when)
    };

    let mut row = Row::new(verb, title, time);
    row.chip = chip;
    row.verb_color = verb_color;
    row.trail = trail;
    // The volatile detail already fed the title for most kinds; only append
    // it as a secondary when it isn't the title (dispatch/rebase/detach/
    // message carry a task title up top and a status detail below).
    if let Some(detail) = detail_secondary(sys) {
        row.secondary = Some(SecondaryLine::Detail(detail));
    }
    paint_row(row, width)
}

/// The dim second line for a system row, when its detail isn't already the
/// title. Task-scoped rows (dispatch/rebase/detach) show the status token
/// under the task title; others fold their detail into the title.
fn detail_secondary(sys: &SystemEvent) -> Option<String> {
    match sys.kind {
        SystemKind::Dispatch | SystemKind::Rebase | SystemKind::WorktreeDetach => {
            sys.status.as_deref().map(humanize_token)
        }
        _ => None,
    }
}

/// Resolve a task id to its human title via the cache, falling back to the
/// id (or a neutral placeholder) when the task file is gone.
fn system_task_title(app: &mut ActivityApp, task_id: Option<&str>) -> String {
    match task_id {
        Some(id) => app
            .task_meta(id)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| id.to_string()),
        None => "task".to_string(),
    }
}

fn send_title(status: Option<&str>) -> String {
    match status {
        Some("submitted") | Some("queued") => "message delivered".to_string(),
        Some("stuck") => "delivery stuck".to_string(),
        Some("unverified") => "delivery unverified".to_string(),
        Some(s) => format!("message {s}"),
        None => "message sent".to_string(),
    }
}

fn rebase_status_color(status: Option<&str>) -> Color {
    match status {
        Some("conflict") => Color::LightRed,
        Some("succeeded") => Color::Green,
        _ => Color::Gray,
    }
}

fn handoff_status_color(status: Option<&str>) -> Color {
    match status {
        Some("written") | Some("native-thread") => Color::Green,
        Some("timeout") | Some("send-failed") | Some("unconfirmed") => Color::Yellow,
        _ => Color::Gray,
    }
}

/// `[DRYRUN]` rows for `shelbi zen dry-run` previews. Italic + dim so
/// they read as "what would have happened" rather than blending with
/// real activity above and below. The label is spelled out (not just a
/// glyph) so a grep over a screenshot still finds it.
fn render_zen_dryrun_event(
    app: &mut ActivityApp,
    ts: DateTime<Utc>,
    task_id: &str,
    action: &str,
    detail: &str,
    width: usize,
    now: DateTime<Utc>,
) -> Vec<Line<'static>> {
    let meta = app.task_meta(task_id).cloned();
    let title = meta
        .as_ref()
        .map(|m| m.title.clone())
        .unwrap_or_else(|| task_id.to_string());
    let when = relative_time(ts, now);
    let mut row = Row::new(
        format!("would {}", humanize_dryrun_action(action)),
        title,
        when,
    );
    // A yellow `[DRYRUN]` chip so the row reads as "what Zen would have
    // done", visibly a preview rather than a real move.
    row.chip = identity_chip("DRYRUN", Color::Yellow, true);
    let detail = detail.replace('_', " ");
    if !detail.is_empty() {
        row.secondary = Some(SecondaryLine::Detail(detail));
    }
    paint_row(row, width)
}

fn humanize_dryrun_action(action: &str) -> String {
    match action {
        "consider-auto-promote" => "consider promoting".into(),
        "merge" => "merge".into(),
        "block-merge" => "block merge".into(),
        other => other.replace('-', " "),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_task_event(
    app: &mut ActivityApp,
    ts: DateTime<Utc>,
    id: &str,
    from: Column,
    to: Column,
    reason: &str,
    width: usize,
    now: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
) -> Vec<Line<'static>> {
    let meta = app.task_meta(id).cloned();
    let title = meta
        .as_ref()
        .map(|m| m.title.clone())
        .unwrap_or_else(|| id.to_string());
    let branch = meta.as_ref().and_then(|m| m.branch.clone());
    let workspace = meta.as_ref().and_then(|m| m.assigned_to.clone());

    let when = relative_time(ts, now);

    // Zen-driven events win over the default (from,to) renderer so the
    // user can scan machine-driven rows distinctly.
    if let Some(zr) = parse_zen_reason(reason) {
        return render_zen_event(zr, &title, from, to, width, when);
    }

    // Match on the canonical status ids rather than enum variants so the
    // renderer stays a fixed-vocabulary special-case for the stock flow;
    // any other transition falls through to the generic "moved" arm. Every
    // arm carries a category-tinted state pill naming where the task landed.
    match (from.as_str(), to.as_str()) {
        ("backlog", "todo") => {
            // Board-level promote — the human's own board action, so `[you]`.
            let mut row = Row::new("promoted", title, when);
            row.chip = system_chip("you");
            row.pill = Some(category_pill(to.category()));
            row.secondary = Some(SecondaryLine::Detail("backlog → todo".to_string()));
            paint_row(row, width)
        }
        ("todo", "in-progress") => {
            let (name, color) = agent_display(workspace.as_deref());
            let mut row = Row::new("is building", title, when);
            row.chip = agent_chip(&name, color);
            row.pill = Some(category_pill(to.category()));
            row.secondary = branch.map(SecondaryLine::Branch);
            paint_row(row, width)
        }
        ("in-progress", "review") => {
            let (name, color) = agent_display(workspace.as_deref());
            // Keep the started→review pairing: prefix "took Xm · " onto the
            // right-aligned time when we found the matching start event.
            let took = started_at
                .map(|s| format!("took {} · ", short_duration(ts - s)))
                .unwrap_or_default();
            let mut row = Row::new("finished", title, format!("{took}{when}"));
            row.chip = agent_chip(&name, color);
            row.pill = Some(category_pill(to.category()));
            row.secondary = branch.map(SecondaryLine::Branch);
            paint_row(row, width)
        }
        ("review", "done") => {
            // Board-level acceptance / merge — the human's board action.
            let mut row = Row::new("merged", title, when);
            row.chip = system_chip("you");
            row.pill = Some(category_pill(to.category()));
            row.secondary = Some(SecondaryLine::Detail("moved to done".to_string()));
            paint_row(row, width)
        }
        _ => {
            // Unrecognized transition — render it as a clean "moved" row with
            // the from→to on the dim second line so nothing shows raw wire
            // syntax and no title is dropped.
            let mut row = Row::new("moved", title, when);
            row.chip = system_chip("you");
            row.pill = Some(category_pill(to.category()));
            row.secondary = Some(SecondaryLine::Detail(format!("{from} → {to}")));
            paint_row(row, width)
        }
    }
}

/// Compose a zen-driven event row. Every zen row is machine-driven, so it
/// gets the neutral `◆` marker and an untinted `zen` label — no agent
/// identity — with a category-colored status glyph, a verb, and a `·`-joined
/// detail on the dim second line. Bail reasons add a short colored tag right
/// after the title.
fn render_zen_event(
    zr: ZenReason,
    title: &str,
    from: Column,
    to: Column,
    width: usize,
    when: String,
) -> Vec<Line<'static>> {
    let bail_tag = |text: &'static str, color: Color| {
        vec![Span::styled(text.to_string(), Style::default().fg(color))]
    };

    // Bail rows deliberately carry no state pill — the colored tag after the
    // title, not a "success" pill, is their outcome signal. Promote/merge/
    // other rows show a category-tinted pill for where the task landed.
    #[allow(clippy::type_complexity)]
    let (verb, trail, secondary, pill): (
        &str,
        Vec<Span<'static>>,
        Option<String>,
        Option<Pill>,
    ) = match &zr {
        ZenReason::Promote { .. } => (
            "promoted",
            Vec::new(),
            Some("backlog → todo".to_string()),
            Some(category_pill(to.category())),
        ),
        ZenReason::Merge { sha } => {
            let merged = sha
                .as_deref()
                .map(|s| format!("merged {s}"))
                .unwrap_or_else(|| "merged".to_string());
            (
                "merged",
                Vec::new(),
                Some(join_detail(&["tests green", "ci green", &merged])),
                Some(category_pill(StatusCategory::Done)),
            )
        }
        ZenReason::FailedChecks { cmd, exit } => {
            let parts: Vec<String> = [
                cmd.as_ref().map(|c| format!("`{c}`")),
                exit.as_ref().map(|e| format!("exit {e}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            (
                "bailed on",
                bail_tag(" — checks failed", Color::LightRed),
                (!parts.is_empty()).then(|| parts.join(" · ")),
                None,
            )
        }
        ZenReason::DiffTooLarge { files, lines } => {
            let parts: Vec<String> = [
                files.as_ref().map(|f| format!("{f} files")),
                lines.as_ref().map(|l| format!("{l} lines")),
            ]
            .into_iter()
            .flatten()
            .collect();
            (
                "bailed on",
                bail_tag(" — diff too large", Color::Yellow),
                (!parts.is_empty()).then(|| parts.join(" · ")),
                None,
            )
        }
        ZenReason::DangerPath { paths } => (
            "bailed on",
            bail_tag(" — danger path", Color::Yellow),
            paths
                .as_ref()
                .map(|p| format!("touched: {}", humanize_list(p))),
            None,
        ),
        ZenReason::CiTimeout { duration } => (
            "bailed on",
            bail_tag(" — ci timeout", Color::Yellow),
            duration.as_ref().map(|d| format!("ci timeout after {d}")),
            None,
        ),
        ZenReason::MergeConflict { files } => (
            "bailed on",
            bail_tag(" — merge conflict", Color::Yellow),
            files
                .as_ref()
                .map(|f| format!("conflict in {}", humanize_list(f))),
            None,
        ),
        ZenReason::Other => (
            "moved",
            Vec::new(),
            Some(format!("{from} → {to}")),
            Some(category_pill(to.category())),
        ),
    };

    let mut row = Row::new(verb, title.to_string(), when);
    row.chip = system_chip("zen");
    row.trail = trail;
    row.pill = pill;
    row.secondary = secondary.map(SecondaryLine::Detail);
    paint_row(row, width)
}

/// Comma-list → human-readable list: `"a,b,c"` → `"a, b, c"`. Used for
/// path / file lists that the orchestrator emits as comma-joined
/// `key=value` payloads.
fn humanize_list(s: &str) -> String {
    s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_workspace_event(
    ts: DateTime<Utc>,
    name: &str,
    new: WorkspaceState,
    width: usize,
    now: DateTime<Utc>,
) -> Vec<Line<'static>> {
    let when = relative_time(ts, now);

    // Workspace-state pings are noisier than task transitions and aren't a
    // single agent action, so they read as machine-driven: a dimmed `[name]`
    // chip and a bare verb-phrase, no title. The verb takes the state color
    // so `awaiting input` reads blue; the eye still skims past them in
    // aggregate but can pick out which workspace when scanning.
    let (verb, verb_color, detail) = match new {
        WorkspaceState::Working => ("working", Color::DarkGray, None),
        WorkspaceState::AwaitingInput => ("awaiting input", Color::Blue, None),
        WorkspaceState::Blocked => ("blocked", Color::Yellow, Some("needs human approval")),
        WorkspaceState::Paused => (
            "paused",
            Color::DarkGray,
            Some("waiting for the limit to reset"),
        ),
    };

    let mut row = Row::new(verb, String::new(), when);
    row.chip = system_chip(name);
    row.verb_color = verb_color;
    row.secondary = detail.map(|d| SecondaryLine::Detail(d.to_string()));
    paint_row(row, width)
}

/// Resolve a task's workspace into a display name + tint. Unassigned
/// tasks fall back to `orchestrator` in the default gray.
fn agent_display(workspace: Option<&str>) -> (String, Color) {
    match workspace {
        Some(w) => (w.to_string(), agent_color(w)),
        None => ("orchestrator".to_string(), Color::Gray),
    }
}

const BRANCH_ICON: &str = "⎇ ";

/// Minimum width (cells) of the identity-chip column, so the message column
/// lines up across rows. A chip wider than this (a long workspace name)
/// pushes only its own row's message right by one space instead of being
/// truncated — the chip is never clipped.
const CHIP_W: usize = 9;
/// Gap (cells) between the dim verb and the title it introduces. Wrapped
/// continuation lines of the title indent to this same title column.
const VERB_GAP: usize = 2;
/// Gap (cells) between the wrapping message and the right-pinned pill/time
/// block on the first line.
const RIGHT_GAP: usize = 2;
/// Dim verb color — visible but subordinate to the bright title.
const VERB_FG: Color = Color::Gray;

/// Lay one [`Row`] out as a top-aligned three-column flex, wrapping the
/// message to as many lines as the title needs:
///
/// ```text
/// [alpha]  finished  site: make the home feature-grid mockups feel     [ready for review]  now
///                    real, alive, and animated on hover
///     ⎇ shelbi/home-feature-grid
/// ```
///
/// The identity chip pins top-left; the state pill + relative time pin
/// top-right; the message (dim verb + bright title) wraps in between.
/// Wrapped continuation lines carry neither chip nor pill and indent to
/// align under the title. The title is never truncated with `…`; only a
/// dim secondary line (branch / reason) elides. No background fill.
fn paint_row(row: Row, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);

    // Chip column: at least CHIP_W wide so messages line up; a chip wider
    // than that pushes only its own row's message over by a single space.
    let chip_w: usize = row.chip.iter().map(|s| display_w(&s.content)).sum();
    let msg_x = if chip_w < CHIP_W {
        CHIP_W
    } else {
        chip_w + 1
    };

    // The verb introduces the message; the title starts a fixed gap past it,
    // and wrapped continuation lines align under the title's left edge.
    let verb_w = display_w(&row.verb);
    let title_x = if verb_w > 0 {
        msg_x + verb_w + VERB_GAP
    } else {
        msg_x
    };

    // Right-pinned block: state pill then relative time. Painted on the
    // first line only; continuation lines leave the right margin clear.
    let mut right: Vec<Span<'static>> = Vec::new();
    let mut right_w = 0usize;
    if let Some(pill) = &row.pill {
        let text = format!("[{}]", pill.text);
        right_w += display_w(&text);
        right.push(Span::styled(
            text,
            Style::default().fg(pill.color).add_modifier(Modifier::BOLD),
        ));
    }
    if !row.time.is_empty() {
        if right_w > 0 {
            right.push(Span::raw("  "));
            right_w += 2;
        }
        right_w += display_w(&row.time);
        right.push(Span::styled(row.time.clone(), dim));
    }

    // Wrap the message (title words + dim trail words) into lines. The first
    // line leaves room for the right block; continuation lines use the full
    // width past the title indent.
    let words = message_words(&row.title, row.title_style, &row.trail);
    let first_budget = width
        .saturating_sub(title_x + right_w + if right_w > 0 { RIGHT_GAP } else { 0 })
        .max(1);
    let cont_budget = width.saturating_sub(title_x).max(1);
    let wrapped = wrap_words(&words, first_budget, cont_budget);

    let mut out: Vec<Line<'static>> = Vec::new();
    for (i, line_words) in wrapped.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if i == 0 {
            spans.extend(row.chip.clone());
            spans.push(Span::raw(" ".repeat(msg_x.saturating_sub(chip_w))));
            if verb_w > 0 {
                spans.push(Span::styled(
                    row.verb.clone(),
                    Style::default().fg(row.verb_color),
                ));
                spans.push(Span::raw(" ".repeat(VERB_GAP)));
            }
        } else {
            spans.push(Span::raw(" ".repeat(title_x)));
        }

        let mut line_w = title_x;
        for (j, (word, style)) in line_words.iter().enumerate() {
            if j > 0 {
                spans.push(Span::raw(" "));
                line_w += 1;
            }
            line_w += display_w(word);
            spans.push(Span::styled(word.clone(), *style));
        }

        // Pin the pill + time to the right edge of the first line.
        if i == 0 && right_w > 0 {
            let pad = width.saturating_sub(line_w + right_w);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.extend(right.clone());
        }
        out.push(Line::from(spans));
    }

    if let Some(sec) = row.secondary {
        out.push(paint_secondary(sec, width, title_x));
    }
    out
}

/// Flatten a row's message into styled words for wrapping: the title in its
/// own style, then each trail span's text split into dim words that flow on
/// after it (a bail tag, a `(×N)` fold trailer).
fn message_words(
    title: &str,
    title_style: Style,
    trail: &[Span<'static>],
) -> Vec<(String, Style)> {
    let mut words: Vec<(String, Style)> = Vec::new();
    for w in title.split_whitespace() {
        words.push((w.to_string(), title_style));
    }
    for span in trail {
        for w in span.content.split_whitespace() {
            words.push((w.to_string(), span.style));
        }
    }
    words
}

/// Greedily pack styled words into lines: `first_budget` cells on the first
/// line (which reserves room for the right-pinned pill/time), `cont_budget`
/// on wrapped continuation lines. A single word longer than its budget is
/// hard-broken across lines so a very long token still wraps rather than
/// overrunning the width — no title is ever elided with `…`.
fn wrap_words(
    words: &[(String, Style)],
    first_budget: usize,
    cont_budget: usize,
) -> Vec<Vec<(String, Style)>> {
    let mut lines: Vec<Vec<(String, Style)>> = Vec::new();
    let mut cur: Vec<(String, Style)> = Vec::new();
    let mut cur_w = 0usize;

    for (word, style) in words {
        let budget = if lines.is_empty() { first_budget } else { cont_budget };
        let ww = display_w(word);
        let sep = usize::from(!cur.is_empty());

        // Word won't fit after what's already on this line — break first.
        if !cur.is_empty() && cur_w + sep + ww > budget {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }

        // On a fresh line and still too wide — hard-break the oversized token.
        let budget = if lines.is_empty() { first_budget } else { cont_budget };
        if cur.is_empty() && ww > budget && budget > 0 {
            let chars: Vec<char> = word.chars().collect();
            let mut start = 0;
            while chars.len() - start > budget {
                let chunk: String = chars[start..start + budget].iter().collect();
                lines.push(vec![(chunk, *style)]);
                start += budget;
            }
            let tail: String = chars[start..].iter().collect();
            cur_w = display_w(&tail);
            cur.push((tail, *style));
            continue;
        }

        let sep = usize::from(!cur.is_empty());
        cur_w += sep + ww;
        cur.push((word.clone(), *style));
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// Paint the dim second line under a row. A `Branch` gets the `⎇ ` icon; a
/// `Detail` is a plain muted fragment. Both indent to align under the row's
/// title column and truncate with `…` — the secondary is context, not the
/// headline, so eliding it here is fine.
fn paint_secondary(sec: SecondaryLine, width: usize, indent: usize) -> Line<'static> {
    let (icon, text) = match sec {
        SecondaryLine::Branch(b) => (BRANCH_ICON, b),
        SecondaryLine::Detail(d) => ("", d),
    };
    let budget = width.saturating_sub(indent + display_w(icon)).max(1);
    let text = truncate(&text, budget);
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(
            format!("{icon}{text}"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// Truncate `s` to at most `max` display cells, marking any elision with
/// a trailing `…`.
fn truncate(s: &str, max: usize) -> String {
    if display_w(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn display_w(s: &str) -> usize {
    s.chars().count()
}

/// Compact relative time — `"now"`, `"4m"`, `"2h"`, `"3d"`, then a short
/// date for anything older than a week. Never an ISO timestamp. Returns an
/// empty string for events in the future (clock skew) or with no timestamp.
fn relative_time(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now - ts;
    if delta.num_seconds() < 0 {
        return String::new();
    }
    if delta.num_seconds() < 60 {
        return "now".into();
    }
    if delta.num_minutes() < 60 {
        return format!("{}m", delta.num_minutes());
    }
    if delta.num_hours() < 24 {
        return format!("{}h", delta.num_hours());
    }
    if delta.num_days() < 7 {
        return format!("{}d", delta.num_days());
    }
    let local = ts.with_timezone(&Local);
    if local.year() == Local::now().year() {
        local.format("%b %-d").to_string()
    } else {
        // "Jan 5 2025" — a human date, deliberately not an ISO timestamp.
        local.format("%b %-d %Y").to_string()
    }
}

fn short_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().abs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    let rem = mins % 60;
    if rem == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{rem:02}m")
    }
}

/// Join a list of detail fragments with ` · ` separators, skipping
/// empties so a missing branch or priority doesn't leave a double
/// separator behind.
fn join_detail(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_legacy_task_event_with_defaults() {
        // Pre-workflow line shape (`Plans/workflows.md` §10). The
        // back-compat parser must fill `workflow=default` and derive
        // categories from the canonical 5-status map — that's what the
        // orchestrator's reaction rules now key off, so old lines have to
        // come through with the same shape new ones do.
        let line =
            "2026-06-23T04:19:33.715717+00:00 task=foo todo -> in_progress reason=user:cli:start";
        match parse_event_line(line) {
            Event::Task {
                id,
                workflow,
                from,
                to,
                reason,
                from_category,
                to_category,
                ..
            } => {
                assert_eq!(id, "foo");
                assert_eq!(workflow, "default");
                assert_eq!(from, Column::todo());
                assert_eq!(to, Column::in_progress());
                assert_eq!(reason, "user:cli:start");
                assert_eq!(from_category, StatusCategory::Ready);
                assert_eq!(to_category, StatusCategory::Active);
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn parses_workflow_aware_task_event() {
        // Full shape from §10:
        // `<ts> task=<id> workflow=<name> <from> -> <to> reason=<r>
        //  from_category=<c> to_category=<c>`
        let line = "2026-06-23T04:19:33+00:00 task=ship-it workflow=feature-task \
                    in_progress -> review reason=workspace:ready-marker \
                    from_category=active to_category=handoff";
        match parse_event_line(line) {
            Event::Task {
                id,
                workflow,
                from,
                to,
                reason,
                from_category,
                to_category,
                ..
            } => {
                assert_eq!(id, "ship-it");
                assert_eq!(workflow, "feature-task");
                assert_eq!(from, Column::in_progress());
                assert_eq!(to, Column::review());
                assert_eq!(reason, "workspace:ready-marker");
                assert_eq!(from_category, StatusCategory::Active);
                assert_eq!(to_category, StatusCategory::Handoff);
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn workflow_aware_parser_tolerates_missing_category_fields() {
        // A future writer that drops the category annotations (or an
        // intermediate-format line) must still parse — the parser falls
        // back to deriving them from the canonical column map.
        let line = "2026-06-23T04:19:33+00:00 task=foo workflow=default todo -> in_progress reason=user:cli";
        match parse_event_line(line) {
            Event::Task {
                workflow,
                from_category,
                to_category,
                ..
            } => {
                assert_eq!(workflow, "default");
                assert_eq!(from_category, StatusCategory::Ready);
                assert_eq!(to_category, StatusCategory::Active);
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn task_event_parser_extracts_agent_from_dispatch_reason() {
        // `shelbi task start` writes the resolved agent into `reason=` as
        // a `_agent=<name>` segment. The parser exposes it on the event
        // struct so the activity feed can render an inline `[<agent>]`
        // tag without re-parsing the line.
        let line = "2026-06-23T04:19:33+00:00 task=foo workflow=default \
                    todo -> in_progress \
                    reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer \
                    from_category=ready to_category=active";
        match parse_event_line(line) {
            Event::Task { agent, reason, .. } => {
                assert_eq!(agent.as_deref(), Some("developer"));
                // Reason value is preserved verbatim; the agent field is
                // a parsed convenience layered on top, not a replacement.
                assert!(reason.contains("agent=developer"), "reason: {reason}");
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn task_event_parser_leaves_agent_none_when_field_absent() {
        // Older lines (and transitions emitted from paths that don't
        // spawn a workspace) have no `_agent=` segment. The parser must
        // leave `agent` as `None` rather than guessing.
        let line = "2026-06-23T04:19:33+00:00 task=foo workflow=default \
                    backlog -> todo reason=user:cli \
                    from_category=backlog to_category=ready";
        match parse_event_line(line) {
            Event::Task { agent, .. } => assert!(agent.is_none(), "agent: {agent:?}"),
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn parses_workspace_event() {
        let line = "2026-06-23T04:19:33Z workspace=alpha working -> awaiting_input";
        match parse_event_line(line) {
            Event::Workspace {
                name, prev, new, ..
            } => {
                assert_eq!(name, "alpha");
                assert_eq!(prev, Some(WorkspaceState::Working));
                assert_eq!(new, WorkspaceState::AwaitingInput);
            }
            other => panic!("expected workspace event, got {other:?}"),
        }
    }

    #[test]
    fn parses_first_observation_workspace_event_with_none_prev() {
        let line = "2026-06-23T04:19:33Z workspace=alpha none -> working";
        match parse_event_line(line) {
            Event::Workspace {
                name, prev, new, ..
            } => {
                assert_eq!(name, "alpha");
                assert!(prev.is_none(), "`none` prev must parse as Option::None");
                assert_eq!(new, WorkspaceState::Working);
            }
            other => panic!("expected workspace event, got {other:?}"),
        }
    }

    /// Legacy event-log lines (and any tooling that lags behind the
    /// `worker=` → `workspace=` rename for one release) must still parse
    /// as workspace events. Both forms route to the same `Event::Workspace`
    /// variant — the parser cares about the shape of the rest of the line,
    /// not which prefix introduced it.
    #[test]
    fn parses_legacy_worker_event_form_as_workspace_event() {
        let line = "2026-06-23T04:19:33Z worker=alpha working -> awaiting_input";
        match parse_event_line(line) {
            Event::Workspace {
                name, prev, new, ..
            } => {
                assert_eq!(name, "alpha");
                assert_eq!(prev, Some(WorkspaceState::Working));
                assert_eq!(new, WorkspaceState::AwaitingInput);
            }
            other => panic!("expected workspace event, got {other:?}"),
        }
    }

    #[test]
    fn parses_zen_dryrun_event() {
        let line = "2026-06-24T10:00:00Z zen-dryrun task=fix-typo action=consider-auto-promote detail=mechanically-eligible";
        match parse_event_line(line) {
            Event::ZenDryRun {
                task_id,
                action,
                detail,
                ..
            } => {
                assert_eq!(task_id, "fix-typo");
                assert_eq!(action, "consider-auto-promote");
                assert_eq!(detail, "mechanically-eligible");
            }
            other => panic!("expected zen-dryrun event, got {other:?}"),
        }
    }

    #[test]
    fn zen_dryrun_without_task_or_action_falls_back_to_unknown() {
        // Defensive: a malformed dry-run line (missing the required
        // `task=` and `action=` tokens) must not crash the parser.
        let line = "2026-06-24T10:00:00Z zen-dryrun detail=oops";
        match parse_event_line(line) {
            Event::Unknown { .. } => {}
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn unknown_line_falls_back_to_raw() {
        // A future event shape we don't recognize must still reach the
        // renderer as Unknown — the acceptance criteria require nothing
        // silently dropped.
        let line = "2026-06-23T04:19:33Z something=else";
        match parse_event_line(line) {
            Event::Unknown { ts, raw } => {
                assert!(ts.is_some());
                assert_eq!(raw, line);
            }
            other => panic!("expected unknown event, got {other:?}"),
        }
    }

    #[test]
    fn malformed_line_with_no_timestamp_is_unknown() {
        let line = "not even a timestamp";
        match parse_event_line(line) {
            Event::Unknown { ts, raw } => {
                assert!(ts.is_none());
                assert_eq!(raw, line);
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    // --- System / infra event classification --------------------------

    #[test]
    fn parses_project_scoped_task_transition() {
        // The real on-disk shape carries a leading `project=<name>` scope.
        // It must route to a first-class Task event (the review handoff the
        // feed centers on), not fall into Unknown as raw wire syntax.
        let line = "2026-07-22T14:00:00+00:00 project=shelbi task=foo workflow=app \
                    in_progress -> review reason=workspace:ready-marker \
                    from_category=active to_category=handoff";
        match parse_event_line(line) {
            Event::Task { id, from, to, .. } => {
                assert_eq!(id, "foo");
                assert_eq!(from, Column::in_progress());
                assert_eq!(to, Column::review());
            }
            other => panic!("expected Task, got {other:?}"),
        }
    }

    #[test]
    fn parses_project_scoped_workspace_transition() {
        let line = "2026-07-22T14:00:00+00:00 project=shelbi workspace=alpha working -> awaiting_input";
        match parse_event_line(line) {
            Event::Workspace { name, new, .. } => {
                assert_eq!(name, "alpha");
                assert_eq!(new, WorkspaceState::AwaitingInput);
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn parses_paused_workspace_transition() {
        // `-> paused` (usage-limit) must parse — previously `paused` was not
        // in the state map, so these lines fell through to Unknown.
        let line = "2026-07-22T14:00:00+00:00 project=demo workspace=alpha working -> paused reason=usage-limit";
        match parse_event_line(line) {
            Event::Workspace { new, .. } => assert_eq!(new, WorkspaceState::Paused),
            other => panic!("expected Workspace paused, got {other:?}"),
        }
    }

    #[test]
    fn parses_ssh_reverse_forward_event() {
        let line = "2026-07-22T14:26:16.430059+00:00 ssh reverse-forward \
                    host=devbox mode=tcp status=failed detail=master_open_failed";
        match parse_event_line(line) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::Ssh);
                assert_eq!(sys.target.as_deref(), Some("devbox"));
                assert_eq!(sys.status.as_deref(), Some("failed"));
                assert_eq!(sys.detail.as_deref(), Some("master open failed"));
            }
            other => panic!("expected System(Ssh), got {other:?}"),
        }
    }

    #[test]
    fn parses_the_required_infra_kinds_without_unknown_fallback() {
        // Acceptance — ssh/dispatch/rebase/worktree-detach/closed/handoff
        // each classify to a structured System event, never Unknown.
        let cases: &[(&str, SystemKind, Option<&str>, Option<&str>)] = &[
            (
                "2026-07-22T14:00:00+00:00 dispatch task=t workspace=alpha status=confirmed detail=busy_observed",
                SystemKind::Dispatch,
                Some("t"),
                Some("confirmed"),
            ),
            (
                "2026-07-22T14:00:00+00:00 rebase task=t workspace=alpha branch=b status=up-to-date detail=default=abc",
                SystemKind::Rebase,
                Some("t"),
                Some("up-to-date"),
            ),
            (
                "2026-07-22T14:00:00+00:00 worktree-detach task=t workspace=alpha detached-from=b status=ok",
                SystemKind::WorktreeDetach,
                Some("t"),
                Some("ok"),
            ),
            (
                "2026-07-22T14:00:00+00:00 project=shelbi closed reason=user:quit-shelbi",
                SystemKind::Closed,
                Some("shelbi"),
                None,
            ),
            (
                "2026-07-22T14:00:00+00:00 project=shelbi handoff outcome=written detail=/tmp/h.md",
                SystemKind::Handoff,
                Some("shelbi"),
                Some("written"),
            ),
        ];
        for (line, kind, target, status) in cases {
            match parse_event_line(line) {
                Event::System(sys) => {
                    assert_eq!(sys.kind, *kind, "kind for {line}");
                    assert_eq!(sys.target.as_deref(), *target, "target for {line}");
                    assert_eq!(sys.status.as_deref(), *status, "status for {line}");
                }
                other => panic!("expected System for {line}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parses_pane_death_event() {
        let line = "2026-07-22T14:00:00+00:00 project=demo workspace=alpha pane_alive=false reason=worktree-missing";
        match parse_event_line(line) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::PaneDeath);
                assert_eq!(sys.target.as_deref(), Some("alpha"));
                assert_eq!(sys.status.as_deref(), Some("false"));
            }
            other => panic!("expected System(PaneDeath), got {other:?}"),
        }
    }

    #[test]
    fn parses_send_event_reading_project_from_field() {
        // `send` keeps its `project=` as a later field, not the leading
        // scope, so the parser reads it back out of the k=v tail.
        let line = "2026-07-22T14:00:00+00:00 send project=demo workspace=alpha status=submitted detail=busy_observed";
        match parse_event_line(line) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::Send);
                assert_eq!(sys.project.as_deref(), Some("demo"));
                assert_eq!(sys.target.as_deref(), Some("alpha"));
                assert_eq!(sys.status.as_deref(), Some("submitted"));
            }
            other => panic!("expected System(Send), got {other:?}"),
        }
    }

    #[test]
    fn ssh_fold_key_ignores_volatile_detail() {
        // Dedup is (kind + target + status), ignoring the detail tail — so a
        // flapping host whose detail text jitters still collapses.
        let a = parse_event_line(
            "2026-07-22T14:00:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=master_open_failed",
        );
        let b = parse_event_line(
            "2026-07-22T14:02:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=tcp_forward_failed",
        );
        match (a, b) {
            (Event::System(a), Event::System(b)) => {
                assert_eq!(a.fold_key(), b.fold_key());
            }
            _ => panic!("expected two System events"),
        }
    }

    #[test]
    fn system_rows_never_render_raw_wire_syntax() {
        // Acceptance — no structured row shows a raw ISO timestamp or raw
        // `key=value` wire syntax.
        let mut app = ActivityApp::new("demo");
        let now = Utc.with_ymd_and_hms(2026, 7, 22, 15, 0, 0).unwrap();
        let raw_lines = [
            "2026-07-22T14:26:16.430059+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=master_open_failed",
            "2026-07-22T14:20:00+00:00 dispatch task=fix-login workspace=alpha status=confirmed detail=busy_observed",
            "2026-07-22T14:19:00+00:00 rebase task=fix-login workspace=alpha branch=b status=up-to-date detail=default=abc123",
            "2026-07-22T14:18:00+00:00 worktree-detach task=fix-login workspace=alpha detached-from=b status=ok",
            "2026-07-22T14:17:00+00:00 project=demo closed reason=user:quit-shelbi",
            "2026-07-22T14:16:00+00:00 project=demo handoff outcome=written detail=/tmp/handoff.md",
        ];
        for raw in raw_lines {
            let ev = parse_event_line(raw);
            let lines = render_event(&ev, &mut app, 100, now, None);
            for l in &lines {
                let text = line_text(l);
                assert!(!text.contains("2026-07-22T"), "raw ISO in row: {text:?}");
                assert!(!text.contains("status="), "raw kv in row: {text:?}");
                assert!(!text.contains("reason="), "raw kv in row: {text:?}");
                assert!(!text.contains(" -> "), "raw arrow in row: {text:?}");
            }
        }
    }

    #[test]
    fn unknown_row_strips_leading_iso_timestamp() {
        // A genuinely unrecognized line is the last resort — still cleaned
        // up so it never leads with a raw ISO timestamp.
        let mut app = ActivityApp::new("demo");
        let now = Utc.with_ymd_and_hms(2026, 6, 23, 13, 0, 0).unwrap();
        let ev = parse_event_line("2026-06-23T12:00:00+00:00 frobnicate=xyz weird");
        assert!(matches!(ev, Event::Unknown { .. }), "expected Unknown: {ev:?}");
        let lines = render_event(&ev, &mut app, 80, now, None);
        let l0 = line_text(&lines[0]);
        assert!(!l0.contains("2026-06-23T12:00:00"), "raw ISO leaked: {l0:?}");
        assert!(l0.contains("frobnicate=xyz"), "body missing: {l0:?}");
        // One hour elapsed → compact "1h" relative time on the right.
        assert!(l0.contains("1h"), "relative time missing: {l0:?}");
    }

    #[test]
    fn consecutive_ssh_failures_fold_and_real_events_stay_visible() {
        // Acceptance — a repeating system event collapses to one row with an
        // attempt count and a last-seen time, and the real task activity from
        // the same window stays visible instead of being buried.
        let mut app = ActivityApp::new("demo");
        let now = Utc.with_ymd_and_hms(2026, 7, 22, 15, 0, 0).unwrap();
        // Oldest: a real review handoff.
        app.events.push(parse_event_line(
            "2026-07-22T14:00:00+00:00 project=demo task=fix-login workflow=app \
             in_progress -> review reason=workspace:ready-marker \
             from_category=active to_category=handoff",
        ));
        // Then a flood of 18 identical ssh failures on the same host.
        for i in 0..18 {
            let line = format!(
                "2026-07-22T14:{:02}:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=master_open_failed",
                10 + i
            );
            app.events.push(parse_event_line(&line));
        }
        let lines = build_lines(&mut app, 100, now);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        let joined = text.join("\n");

        let ssh_rows = text.iter().filter(|t| t.contains("devbox")).count();
        assert_eq!(ssh_rows, 1, "ssh flood should fold to one row:\n{joined}");
        assert!(
            joined.contains("18 attempts"),
            "folded attempt count missing:\n{joined}"
        );
        assert!(
            text.iter().any(|t| t.contains("finished")),
            "real review handoff was buried by the flood:\n{joined}"
        );
    }

    #[test]
    fn relative_time_buckets() {
        // Compact form to match the mockup — "now"/"4m"/"2h"/"3d", never an
        // ISO timestamp and never a "… ago" suffix.
        let now = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        assert_eq!(relative_time(now, now), "now");
        assert_eq!(relative_time(now - chrono::Duration::minutes(5), now), "5m");
        assert_eq!(relative_time(now - chrono::Duration::hours(2), now), "2h");
        assert_eq!(relative_time(now - chrono::Duration::days(3), now), "3d");
    }

    #[test]
    fn short_duration_formats() {
        assert_eq!(short_duration(chrono::Duration::seconds(45)), "45s");
        assert_eq!(short_duration(chrono::Duration::minutes(12)), "12m");
        assert_eq!(short_duration(chrono::Duration::hours(2)), "2h");
        assert_eq!(short_duration(chrono::Duration::minutes(125)), "2h05m");
    }

    #[test]
    fn each_phonetic_workspace_has_a_unique_tint() {
        // Identity is now color, not a face — recognizability depends on
        // each workspace getting a distinct tint, so regression-test that
        // no two phonetic names collide on the same color.
        let workspaces = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
        let mut seen: Vec<Color> = Vec::new();
        for w in workspaces {
            let color = agent_color(w);
            assert!(!seen.contains(&color), "duplicate tint for {w}");
            seen.push(color);
        }
    }

    #[test]
    fn unknown_workspace_falls_back_to_default_tint() {
        // A non-phonetic name still renders — just in the neutral gray
        // rather than a unique tint.
        assert_eq!(agent_color("frontend"), Color::Gray);
    }

    #[test]
    fn parse_zen_reason_recognizes_each_kind() {
        assert_eq!(
            parse_zen_reason("orchestrator:zen-promote category=4"),
            Some(ZenReason::Promote {
                category: Some("4".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("orchestrator:zen-merge sha=abc123"),
            Some(ZenReason::Merge {
                sha: Some("abc123".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("zen:failed-checks cmd=\"cargo test\" exit=1"),
            Some(ZenReason::FailedChecks {
                cmd: Some("cargo test".into()),
                exit: Some("1".into()),
            }),
            "quoted command values must survive parsing intact"
        );
        assert_eq!(
            parse_zen_reason("zen:diff-too-large files=12 lines=2543"),
            Some(ZenReason::DiffTooLarge {
                files: Some("12".into()),
                lines: Some("2543".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("zen:danger-path paths=src/db.rs,migrations/001.sql"),
            Some(ZenReason::DangerPath {
                paths: Some("src/db.rs,migrations/001.sql".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("zen:ci-timeout duration=15m"),
            Some(ZenReason::CiTimeout {
                duration: Some("15m".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("zen:merge-conflict files=Cargo.lock,src/main.rs"),
            Some(ZenReason::MergeConflict {
                files: Some("Cargo.lock,src/main.rs".into()),
            })
        );
    }

    #[test]
    fn parse_zen_reason_keeps_future_zen_prefixes_under_zen_treatment() {
        // Anything starting with `zen:` or `orchestrator:zen-` we haven't
        // wired up yet still routes to the zen visual treatment — the
        // user sees the badge + tint and just doesn't get a per-kind
        // detail line. Better than silently rendering as a generic row.
        assert_eq!(
            parse_zen_reason("orchestrator:zen-decline reason-text=needs-tests"),
            Some(ZenReason::Other)
        );
        assert_eq!(parse_zen_reason("zen:auto-promote"), Some(ZenReason::Other));
    }

    #[test]
    fn parse_zen_reason_ignores_non_zen_reasons() {
        assert!(parse_zen_reason("user:cli:start").is_none());
        assert!(parse_zen_reason("workspace:ready-marker").is_none());
        assert!(parse_zen_reason("").is_none());
    }

    #[test]
    fn agent_from_reason_extracts_agent_segment() {
        // On disk the dispatch line carries
        // `reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer` —
        // the leading whitespace from the human reason has been folded
        // to underscores by `append_task_event`'s sanitizer. The helper
        // splits on `_` and surfaces the named segment.
        assert_eq!(
            agent_from_reason("orchestrator:auto-dispatch_workspace=alpha_agent=developer"),
            Some("developer".to_string())
        );
        assert_eq!(
            agent_from_reason("user:cli:start_agent=orchestrator"),
            Some("orchestrator".to_string())
        );
        assert_eq!(agent_from_reason("user:cli"), None);
        assert_eq!(agent_from_reason(""), None);
    }

    #[test]
    fn render_started_row_reads_as_a_sentence_with_chip_verb_and_pill() {
        // Acceptance — a start row reads as a sentence: an identity chip, the
        // `is building` verb, the task title, and an `[in progress]` state
        // pill. (No on-disk task file here, so the unassigned workspace
        // resolves to the `[orchestrator]` chip and the id stands in as title.)
        let mut app = ActivityApp::new("demo");
        let ts = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        let now = ts + chrono::Duration::minutes(1);
        let ev = Event::Task {
            ts,
            id: "demo-task".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::todo(),
            to: Column::in_progress(),
            reason: "orchestrator:auto-dispatch_workspace=alpha_agent=developer".into(),
            agent: Some("developer".into()),
            from_category: Column::todo().category(),
            to_category: Column::in_progress().category(),
            raw: String::new(),
        };
        let lines = render_event(&ev, &mut app, 80, now, None);
        let primary = line_text(&lines[0]);
        assert!(
            primary.starts_with("[orchestrator]"),
            "missing identity chip in: {primary:?}"
        );
        assert!(
            primary.contains("is building"),
            "missing verb in: {primary:?}"
        );
        assert!(
            primary.contains("[in progress]"),
            "missing state pill in: {primary:?}"
        );
    }

    #[test]
    fn render_started_row_pins_the_state_pill_top_right() {
        // The pill + relative time pin to the right edge of the first line.
        let mut app = ActivityApp::new("demo");
        let ts = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        let now = ts + chrono::Duration::minutes(1);
        let ev = Event::Task {
            ts,
            id: "demo-task".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::todo(),
            to: Column::in_progress(),
            reason: "user:cli:start".into(),
            agent: None,
            from_category: Column::todo().category(),
            to_category: Column::in_progress().category(),
            raw: String::new(),
        };
        let lines = render_event(&ev, &mut app, 80, now, None);
        let primary = line_text(&lines[0]);
        assert!(
            primary.contains("is building"),
            "missing verb in: {primary:?}"
        );
        assert!(
            primary.trim_end().ends_with("1m"),
            "relative time not right-pinned: {primary:?}"
        );
    }

    #[test]
    fn agent_row_leads_with_chip_and_dim_branch_line() {
        // Acceptance — an agent row is a tinted `[name]` chip, a dim verb, a
        // bright title, a right-pinned state pill + time, and a dim
        // `⎇ `-led branch second line. No background fill.
        let mut row = Row::new("is building", "Metrics API endpoint", "now".to_string());
        row.chip = agent_chip("bravo", Color::Cyan);
        row.pill = Some(category_pill(StatusCategory::Active));
        row.secondary = Some(SecondaryLine::Branch(
            "shelbi/metrics-api-endpoint".to_string(),
        ));
        let lines = paint_row(row, 80);

        assert_eq!(lines.len(), 2, "primary + one dim branch line");
        let l0 = line_text(&lines[0]);
        assert!(
            l0.starts_with("[bravo]"),
            "row must lead with the identity chip: {l0:?}"
        );
        assert!(l0.contains("is building"), "missing verb: {l0:?}");
        assert!(l0.contains("Metrics API endpoint"), "missing title: {l0:?}");
        assert!(l0.contains("[in progress]"), "missing state pill: {l0:?}");
        assert!(
            l0.trim_end().ends_with("now"),
            "time must be right-pinned: {l0:?}"
        );
        assert!(
            lines.iter().all(|l| l.style.bg.is_none()),
            "no background fill"
        );

        let l1 = line_text(&lines[1]);
        assert!(
            l1.contains("⎇ shelbi/metrics-api-endpoint"),
            "branch second line led by the ⎇ icon: {l1:?}"
        );
    }

    #[test]
    fn long_title_wraps_without_truncation_keeping_chip_and_pill_pinned() {
        // Acceptance — a long title wraps to multiple lines (no `…`), the
        // identity chip stays top-left, and the state pill + time stay
        // top-right. No line overruns the width.
        let title = "A very long task title that will never fit into a narrow terminal";
        let mut row = Row::new("finished", title, "took 34m · 2m".to_string());
        row.chip = agent_chip("alpha", Color::Cyan);
        row.pill = Some(category_pill(StatusCategory::Handoff));
        let lines = paint_row(row, 60);

        assert!(lines.len() >= 2, "long title must wrap: {lines:?}");
        for l in &lines {
            let t = line_text(l);
            assert!(!t.contains('…'), "title must not be truncated: {t:?}");
            assert!(
                display_w(&t) <= 60,
                "row must not overrun width: {} > 60 in {t:?}",
                display_w(&t)
            );
        }
        let l0 = line_text(&lines[0]);
        assert!(l0.starts_with("[alpha]"), "chip pinned top-left: {l0:?}");
        assert!(
            l0.contains("[ready for review]"),
            "pill pinned top-right: {l0:?}"
        );
        assert!(l0.trim_end().ends_with("2m"), "time right-pinned: {l0:?}");
        // The whole title survives across the wrapped lines.
        let joined: String = lines.iter().map(line_text).collect::<Vec<_>>().join(" ");
        for word in title.split_whitespace() {
            assert!(joined.contains(word), "dropped title word {word:?}: {joined:?}");
        }
    }

    #[test]
    fn finished_row_prefixes_took_onto_the_time() {
        // Acceptance — the started→review pairing survives: a finish row's
        // right-aligned time carries a `took Xm · ` prefix.
        let mut app = ActivityApp::new("demo");
        let ts = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        let now = ts + chrono::Duration::minutes(2);
        let started = ts - chrono::Duration::minutes(34);
        let ev = Event::Task {
            ts,
            id: "demo-task".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::in_progress(),
            to: Column::review(),
            reason: "workspace:ready-marker".into(),
            agent: None,
            from_category: Column::in_progress().category(),
            to_category: Column::review().category(),
            raw: String::new(),
        };
        let lines = render_event(&ev, &mut app, 80, now, Some(started));
        let l0 = line_text(&lines[0]);
        assert!(l0.contains("finished"), "missing verb: {l0:?}");
        assert!(l0.contains("took 34m · "), "missing took prefix: {l0:?}");
        assert!(l0.contains("took 34m · 2m"), "relative time kept: {l0:?}");
    }

    #[test]
    fn parse_kv_handles_quotes_and_bare_values() {
        let kv = parse_kv("a=1 b=\"two words\" c=three");
        assert_eq!(kv.get("a").map(String::as_str), Some("1"));
        assert_eq!(kv.get("b").map(String::as_str), Some("two words"));
        assert_eq!(kv.get("c").map(String::as_str), Some("three"));
    }

    /// Helper: concatenate the visible content of every span in a
    /// [`Line`] so tests can assert on rendered text without poking
    /// into private span structure.
    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    /// Helper: build a zen task event the renderer can consume in a
    /// vacuum (no on-disk task file required — we render the id as the
    /// title in that case, which is fine for layout tests).
    fn render_zen_for_test(reason: &str, from: Column, to: Column) -> Vec<Line<'static>> {
        let mut app = ActivityApp::new("demo");
        let ts = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        let now = ts + chrono::Duration::minutes(5);
        let ev = Event::Task {
            ts,
            id: "demo-task".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from_category: from.category(),
            to_category: to.category(),
            from,
            to,
            reason: reason.into(),
            agent: None,
            raw: String::new(),
        };
        render_event(&ev, &mut app, 80, now, None)
    }

    #[test]
    fn render_zen_promote_uses_zen_chip_and_no_bg() {
        let lines = render_zen_for_test(
            "orchestrator:zen-promote category=4",
            Column::backlog(),
            Column::todo(),
        );
        // Two lines: primary + dim secondary. No 3-row badge avatar.
        assert_eq!(lines.len(), 2, "zen row is a primary + one dim second line");
        // Primary: a dimmed `[zen]` chip and the 'promoted' verb.
        let l0 = line_text(&lines[0]);
        assert!(
            l0.starts_with("[zen]"),
            "primary missing zen chip in {l0:?}"
        );
        assert!(
            l0.contains("promoted"),
            "primary missing 'promoted' in {l0:?}"
        );
        // Secondary line carries the "backlog → todo" detail.
        let l1 = line_text(&lines[1]);
        assert!(l1.contains("backlog → todo"), "secondary detail in {l1:?}");
        // No row carries a background fill — the reversed-box look is gone.
        for l in &lines {
            assert_eq!(
                l.style.bg,
                None,
                "no zen row should carry a background fill: {:?}",
                line_text(l)
            );
        }
    }

    #[test]
    fn render_zen_merge_secondary_includes_sha_and_green_checks() {
        let lines = render_zen_for_test(
            "orchestrator:zen-merge sha=abc123",
            Column::review(),
            Column::done(),
        );
        let l0 = line_text(&lines[0]);
        assert!(l0.contains("merged"), "primary should say 'merged': {l0:?}");
        let l1 = line_text(&lines[1]);
        assert!(
            l1.contains("tests green"),
            "secondary missing tests-green: {l1:?}"
        );
        assert!(
            l1.contains("ci green"),
            "secondary missing ci-green: {l1:?}"
        );
        assert!(
            l1.contains("merged abc123"),
            "secondary missing sha: {l1:?}"
        );
    }

    #[test]
    fn render_zen_failed_checks_shows_command_and_exit_in_secondary() {
        let lines = render_zen_for_test(
            "zen:failed-checks cmd=\"cargo test\" exit=1",
            Column::in_progress(),
            Column::review(),
        );
        let l0 = line_text(&lines[0]);
        assert!(
            l0.contains("bailed on"),
            "primary missing 'bailed on': {l0:?}"
        );
        assert!(
            l0.contains("— checks failed"),
            "primary missing bail tag: {l0:?}"
        );
        let l1 = line_text(&lines[1]);
        assert!(
            l1.contains("`cargo test`"),
            "secondary missing failing cmd: {l1:?}"
        );
        assert!(l1.contains("exit 1"), "secondary missing exit code: {l1:?}");
    }

    #[test]
    fn render_zen_diff_too_large_secondary_is_files_lines() {
        let lines = render_zen_for_test(
            "zen:diff-too-large files=12 lines=2543",
            Column::in_progress(),
            Column::review(),
        );
        let l1 = line_text(&lines[1]);
        assert!(l1.contains("12 files"), "got {l1:?}");
        assert!(l1.contains("2543 lines"), "got {l1:?}");
    }

    #[test]
    fn render_zen_danger_path_humanizes_comma_list() {
        let lines = render_zen_for_test(
            "zen:danger-path paths=src/db.rs,migrations/001.sql",
            Column::in_progress(),
            Column::review(),
        );
        let l1 = line_text(&lines[1]);
        assert!(
            l1.contains("touched: src/db.rs, migrations/001.sql"),
            "got {l1:?}"
        );
    }

    #[test]
    fn render_zen_ci_timeout_secondary_has_duration() {
        let lines = render_zen_for_test(
            "zen:ci-timeout duration=15m",
            Column::in_progress(),
            Column::review(),
        );
        let l1 = line_text(&lines[1]);
        assert!(l1.contains("ci timeout after 15m"), "got {l1:?}");
    }

    #[test]
    fn render_zen_merge_conflict_secondary_humanizes_files() {
        let lines = render_zen_for_test(
            "zen:merge-conflict files=Cargo.lock,src/main.rs",
            Column::in_progress(),
            Column::review(),
        );
        let l1 = line_text(&lines[1]);
        assert!(
            l1.contains("conflict in Cargo.lock, src/main.rs"),
            "got {l1:?}"
        );
    }

    #[test]
    fn user_action_rows_do_not_carry_zen_tint() {
        // Regression — `started`, `finished`, default `Promoted`, etc.
        // must keep `Line.style.bg == None` so the zen tint stays a
        // distinguishing visual signal.
        let lines = render_zen_for_test("user:cli:start", Column::todo(), Column::in_progress());
        for l in &lines {
            assert_eq!(
                l.style.bg,
                None,
                "user-action row should not carry zen bg: {:?}",
                line_text(l)
            );
        }
    }

    fn task_event(reason: &str) -> Event {
        Event::Task {
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            id: "demo-task".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::todo(),
            to: Column::in_progress(),
            reason: reason.into(),
            agent: agent_from_reason(reason),
            from_category: Column::todo().category(),
            to_category: Column::in_progress().category(),
            raw: String::new(),
        }
    }

    fn workspace_event() -> Event {
        Event::Workspace {
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            name: "alpha".into(),
            prev: None,
            new: WorkspaceState::Working,
            raw: String::new(),
        }
    }

    fn heartbeat_event() -> Event {
        Event::Heartbeat {
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            project: "demo".into(),
            raw: "2026-06-23T12:00:00+00:00 project=demo heartbeat".into(),
        }
    }

    #[test]
    fn parse_heartbeat_line_round_trips() {
        // Shape: `<ts> project=<name> heartbeat`. Must come back as the
        // `Heartbeat` variant (not `Unknown`) so the filter knows to
        // drop it.
        let line = "2026-06-23T12:00:00+00:00 project=demo heartbeat";
        match parse_event_line(line) {
            Event::Heartbeat { project, .. } => assert_eq!(project, "demo"),
            other => panic!("expected Heartbeat, got {other:?}"),
        }
    }

    #[test]
    fn parse_heartbeat_line_with_counts_round_trips() {
        // The poller appends `zen_eligible=`/`idle_workspaces=` counts after
        // the `heartbeat` keyword. The activity parser keys off the keyword
        // alone, so the row still classifies as `Heartbeat` (and stays
        // filtered out of the feed) regardless of the trailing tokens.
        let line =
            "2026-06-23T12:00:00+00:00 project=demo heartbeat zen_eligible=5 idle_workspaces=4";
        match parse_event_line(line) {
            Event::Heartbeat { project, .. } => assert_eq!(project, "demo"),
            other => panic!("expected Heartbeat, got {other:?}"),
        }
    }

    #[test]
    fn activity_filter_rejects_heartbeat_under_every_pill() {
        // Heartbeats are an orchestrator wake-up signal, not human-facing
        // activity. They must be filtered out regardless of which pill
        // (or no pill) is on — otherwise the feed gets a "nothing
        // happened" row every few minutes.
        let configs = [
            ActivityFilter::default(),
            ActivityFilter {
                zen: true,
                workspaces: false,
            },
            ActivityFilter {
                zen: false,
                workspaces: true,
            },
            ActivityFilter {
                zen: true,
                workspaces: true,
            },
        ];
        for f in configs {
            assert!(
                !f.matches(&heartbeat_event()),
                "heartbeat passed filter: {f:?}"
            );
        }
    }

    #[test]
    fn activity_filter_all_matches_every_event() {
        let f = ActivityFilter::default();
        assert!(f.is_all());
        assert!(f.matches(&task_event("user:cli:start")));
        assert!(f.matches(&task_event("orchestrator:zen-promote category=4")));
        assert!(f.matches(&workspace_event()));
    }

    #[test]
    fn activity_filter_zen_keeps_only_zen_task_events() {
        let f = ActivityFilter {
            zen: true,
            workspaces: false,
        };
        assert!(f.matches(&task_event("orchestrator:zen-promote category=4")));
        assert!(f.matches(&task_event("orchestrator:zen-merge sha=abc")));
        assert!(f.matches(&task_event("zen:failed-checks cmd=\"cargo test\"")));
        assert!(!f.matches(&task_event("user:cli:start")));
        assert!(!f.matches(&workspace_event()));
    }

    #[test]
    fn activity_filter_workspaces_keeps_only_workspace_events() {
        let f = ActivityFilter {
            zen: false,
            workspaces: true,
        };
        assert!(f.matches(&workspace_event()));
        assert!(!f.matches(&task_event("user:cli:start")));
        assert!(!f.matches(&task_event("orchestrator:zen-promote category=4")));
    }

    #[test]
    fn activity_filter_pills_are_multiselect_union() {
        let f = ActivityFilter {
            zen: true,
            workspaces: true,
        };
        assert!(f.matches(&task_event("orchestrator:zen-promote category=4")));
        assert!(f.matches(&workspace_event()));
        // Regular user-action task still filtered out — neither pill matches it.
        assert!(!f.matches(&task_event("user:cli:start")));
    }

    #[test]
    fn toggle_filter_methods_flip_state_and_snap_to_top() {
        let mut app = ActivityApp::new("demo");
        app.scroll = 25;
        app.auto_scroll = false;

        app.toggle_zen_filter();
        assert!(app.filter.zen);
        assert_eq!(app.scroll, 0, "filter toggle should snap scroll to newest");
        assert!(app.auto_scroll);

        app.toggle_workspaces_filter();
        assert!(app.filter.workspaces);

        app.reset_filter();
        assert!(app.filter.is_all(), "`a` resets both pills to off");
    }

    #[test]
    fn started_at_finds_nearest_prior_in_progress_transition() {
        // The "took 18m" line on a review handoff joins the review
        // event to its matching `* -> in_progress` event for the same
        // task. Walk the events list backwards from the review event
        // and return the in_progress event's timestamp.
        let mut app = ActivityApp::new("demo");
        app.events.push(Event::Task {
            ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap(),
            id: "foo".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::todo(),
            to: Column::in_progress(),
            reason: "user:cli".into(),
            agent: None,
            from_category: Column::todo().category(),
            to_category: Column::in_progress().category(),
            raw: String::new(),
        });
        // Unrelated task in between — must not affect the lookup.
        app.events.push(Event::Task {
            ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 5, 0).unwrap(),
            id: "bar".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::todo(),
            to: Column::in_progress(),
            reason: "user:cli".into(),
            agent: None,
            from_category: Column::todo().category(),
            to_category: Column::in_progress().category(),
            raw: String::new(),
        });
        app.events.push(Event::Task {
            ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 18, 0).unwrap(),
            id: "foo".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::in_progress(),
            to: Column::review(),
            reason: "workspace:ready-marker".into(),
            agent: None,
            from_category: Column::in_progress().category(),
            to_category: Column::review().category(),
            raw: String::new(),
        });
        let started = app.started_at(2, "foo");
        assert_eq!(
            started,
            Some(Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap()),
            "must pair the review event with its task's own in_progress event"
        );
    }
}
