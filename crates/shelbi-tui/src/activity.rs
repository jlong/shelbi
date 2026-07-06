//! Activity feed — human-friendly view of `~/.shelbi/events.log`.
//!
//! Renders the same append-only event stream `shelbi events tail`
//! consumes, but reformatted as a date-bucketed reverse-chronological
//! feed: who started what, who finished what, who's idle, who's waiting.
//! Identity is conveyed by color — a `●` dot and the agent name in the
//! workspace's tint — so the eye can group runs without re-reading names.
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
    /// Recognized timestamp but the rest doesn't match the task/workspace
    /// shape — render the original line verbatim so nothing vanishes.
    Unknown {
        ts: Option<DateTime<Utc>>,
        raw: String,
    },
}

impl Event {
    pub fn ts(&self) -> Option<DateTime<Utc>> {
        match self {
            Event::Task { ts, .. }
            | Event::Workspace { ts, .. }
            | Event::ZenDryRun { ts, .. }
            | Event::Heartbeat { ts, .. } => Some(*ts),
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

/// Parse one `events.log` line into an [`Event`]. Best-effort: any
/// unrecognized shape lands in [`Event::Unknown`] so the renderer
/// still prints the raw text rather than silently dropping the line.
pub fn parse_event_line(line: &str) -> Event {
    let raw = line.to_string();
    let mut parts = line.splitn(2, ' ');
    let ts_str = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    let ts = DateTime::parse_from_rfc3339(ts_str)
        .ok()
        .map(|t| t.with_timezone(&Utc));

    let Some(ts) = ts else {
        return Event::Unknown { ts: None, raw };
    };

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

    if let Some(rest) = rest.strip_prefix("project=") {
        // Heartbeat shape: `project=<name> heartbeat zen_eligible=<N>
        // idle_workspaces=<M>`. We recognize it here so the parser doesn't
        // fall through to `Unknown` (which would render a `?`-prefixed raw
        // line if the filter were ever bypassed). We match on the keyword
        // token alone — the trailing `zen_eligible=`/`idle_workspaces=`
        // counts are for the orchestrator's react rule, not the activity
        // feed, so they don't change how this row is classified. Other
        // `project=...` lines fall through to the existing Unknown handling.
        let mut tokens = rest.split_whitespace();
        let name = tokens.next().unwrap_or("");
        let action = tokens.next().unwrap_or("");
        if !name.is_empty() && action == "heartbeat" {
            return Event::Heartbeat {
                ts,
                project: name.to_string(),
                raw,
            };
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    // Workspace state transition lines. New emissions use `workspace=<name>`;
    // legacy lines (and one-release tooling that still emits the old form)
    // use `worker=<name>` — we accept both. Phase 2 will drop the `worker=`
    // fallback once enough time has passed.
    let ws_rest = rest
        .strip_prefix("workspace=")
        .or_else(|| rest.strip_prefix("worker="));
    if let Some(rest) = ws_rest {
        // Format: `<name> <prev> -> <new>`
        let mut tokens = rest.splitn(4, ' ');
        let name = tokens.next().unwrap_or("");
        let prev_s = tokens.next().unwrap_or("");
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

    Event::Unknown { ts: Some(ts), raw }
}

fn parse_workspace_state(s: &str) -> Option<WorkspaceState> {
    match s {
        "working" => Some(WorkspaceState::Working),
        "awaiting_input" => Some(WorkspaceState::AwaitingInput),
        "blocked" => Some(WorkspaceState::Blocked),
        _ => None,
    }
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

    for idx in order {
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

/// The leading identity marker for a feed row. Agent-attributed rows get
/// a tinted `●` dot; machine-driven rows (zen, dry-run, workspace-state
/// noise, board-level promotes) get a neutral `◆` with no agent tint, so
/// the two streams stay visibly separate.
#[derive(Clone, Copy)]
enum Marker {
    /// `●` painted in the agent's tint.
    Agent(Color),
    /// Neutral `◆` — no agent identity.
    System,
}

impl Marker {
    fn glyph(self) -> &'static str {
        match self {
            Marker::Agent(_) => MARK_AGENT,
            Marker::System => MARK_SYSTEM,
        }
    }

    fn color(self) -> Color {
        match self {
            Marker::Agent(c) => c,
            Marker::System => Color::Gray,
        }
    }
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

/// One fully-specified feed row, ready for [`paint_row`] to lay out.
/// The title truncates to fit, the time right-aligns, and the optional
/// secondary prints as a dim indented second line — no background fill.
struct Row {
    marker: Marker,
    /// Identity spans between the marker and the status glyph — the agent
    /// name (tinted, bold) plus an optional dim `·role·` tag, or a neutral
    /// system label. Padded to [`IDENT_W`] so verbs line up across rows.
    identity: Vec<Span<'static>>,
    /// Category-tinted status glyph conveying the verb (`▶`/`✓`/`✔`/…).
    /// The empty string omits it.
    glyph: &'static str,
    glyph_color: Color,
    /// Dim verb word (started / finished / promoted / …).
    verb: String,
    /// The task title — bright fg, truncated with `…`.
    title: String,
    /// Style for the title. Defaults to bright white; special-case rows
    /// (raw fallbacks) dim it.
    title_style: Style,
    /// Optional spans appended straight after the title (e.g. a zen bail
    /// tag `— checks failed`). Counted against the title's width budget.
    trail: Vec<Span<'static>>,
    /// Right-aligned relative time; finish rows prefix `took Xm · `.
    time: String,
    /// Optional dim second line.
    secondary: Option<SecondaryLine>,
}

impl Row {
    /// A row with the common defaults filled in — bright title, no glyph,
    /// no trail, no secondary. Callers set the fields they care about.
    fn new(
        marker: Marker,
        verb: impl Into<String>,
        title: impl Into<String>,
        time: String,
    ) -> Self {
        Row {
            marker,
            identity: Vec::new(),
            glyph: "",
            glyph_color: Color::DarkGray,
            verb: verb.into(),
            title: title.into(),
            title_style: Style::default().fg(Color::White),
            trail: Vec::new(),
            time,
            secondary: None,
        }
    }
}

/// Identity spans for an agent-attributed row: the workspace name in its
/// tint (bold) plus an optional dim `·role·` tag.
fn agent_identity(name: &str, color: Color, role: Option<&str>) -> Vec<Span<'static>> {
    let mut out = vec![Span::styled(
        name.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if let Some(role) = role {
        out.push(Span::raw(" "));
        out.push(Span::styled(
            format!("·{role}·"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    out
}

/// Identity spans for a machine-driven row: a neutral (untinted) label,
/// or nothing at all when the event carries no natural label.
fn system_identity(label: &str) -> Vec<Span<'static>> {
    if label.is_empty() {
        return Vec::new();
    }
    vec![Span::styled(
        label.to_string(),
        Style::default().fg(Color::Gray),
    )]
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
            agent,
            raw,
            ..
        } => render_task_event(
            app,
            *ts,
            id,
            from.clone(),
            to.clone(),
            reason,
            agent.as_deref(),
            raw,
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
        Event::Unknown { ts, raw } => {
            let when = ts.map(|t| relative_time(t, now)).unwrap_or_default();
            let mut row = Row::new(Marker::System, "", raw.to_string(), when);
            row.title_style = Style::default().fg(Color::DarkGray);
            paint_row(row, width)
        }
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
        Marker::System,
        format!("would {}", humanize_dryrun_action(action)),
        title,
        when,
    );
    // Neutral `[DRYRUN]` label + a subdued move glyph so the row reads as
    // "what Zen would have done", visibly a preview rather than a real move.
    row.identity = vec![Span::styled(
        "[DRYRUN]".to_string(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )];
    row.glyph = GLYPH_PROMOTED;
    row.glyph_color = Color::Yellow;
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
    agent: Option<&str>,
    raw: &str,
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
    // any other transition falls through to the generic "moved" arm.
    match (from.as_str(), to.as_str()) {
        ("backlog", "todo") => {
            // Board-level promote — no agent attribution, neutral marker.
            let mut row = Row::new(Marker::System, "promoted", title, when);
            row.glyph = GLYPH_PROMOTED;
            row.glyph_color = category_color(to.category());
            row.secondary = Some(SecondaryLine::Detail("backlog → todo".to_string()));
            paint_row(row, width)
        }
        ("todo", "in-progress") => {
            let (name, color) = agent_display(workspace.as_deref());
            let mut row = Row::new(Marker::Agent(color), "started", title, when);
            row.identity = agent_identity(&name, color, agent);
            row.glyph = GLYPH_STARTED;
            row.glyph_color = category_color(to.category());
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
            let mut row = Row::new(
                Marker::Agent(color),
                "finished",
                title,
                format!("{took}{when}"),
            );
            row.identity = agent_identity(&name, color, agent);
            row.glyph = GLYPH_FINISHED;
            row.glyph_color = category_color(to.category());
            row.secondary = branch.map(SecondaryLine::Branch);
            paint_row(row, width)
        }
        ("review", "done") => {
            // Board-level acceptance — no agent attribution.
            let mut row = Row::new(Marker::System, "accepted", title, when);
            row.glyph = GLYPH_DONE;
            row.glyph_color = category_color(to.category());
            row.secondary = Some(SecondaryLine::Detail("moved to done".to_string()));
            paint_row(row, width)
        }
        _ => {
            // Unrecognized transition — keep the raw line so nothing
            // silently disappears from the feed.
            let mut row = Row::new(Marker::System, "moved", title, when);
            row.glyph = GLYPH_MOVED;
            row.glyph_color = category_color(to.category());
            row.secondary = Some(SecondaryLine::Detail(format!("{from} → {to}: {raw}")));
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

    #[allow(clippy::type_complexity)]
    let (verb, glyph, glyph_color, trail, secondary): (
        &str,
        &'static str,
        Color,
        Vec<Span<'static>>,
        Option<String>,
    ) = match &zr {
        ZenReason::Promote { .. } => (
            "promoted",
            GLYPH_PROMOTED,
            category_color(to.category()),
            Vec::new(),
            Some("backlog → todo".to_string()),
        ),
        ZenReason::Merge { sha } => {
            let merged = sha
                .as_deref()
                .map(|s| format!("merged {s}"))
                .unwrap_or_else(|| "merged".to_string());
            (
                "merged",
                GLYPH_DONE,
                category_color(StatusCategory::Done),
                Vec::new(),
                Some(join_detail(&["tests green", "ci green", &merged])),
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
                GLYPH_MOVED,
                Color::LightRed,
                bail_tag(" — checks failed", Color::LightRed),
                (!parts.is_empty()).then(|| parts.join(" · ")),
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
                GLYPH_MOVED,
                Color::Yellow,
                bail_tag(" — diff too large", Color::Yellow),
                (!parts.is_empty()).then(|| parts.join(" · ")),
            )
        }
        ZenReason::DangerPath { paths } => (
            "bailed on",
            GLYPH_MOVED,
            Color::Yellow,
            bail_tag(" — danger path", Color::Yellow),
            paths
                .as_ref()
                .map(|p| format!("touched: {}", humanize_list(p))),
        ),
        ZenReason::CiTimeout { duration } => (
            "bailed on",
            GLYPH_MOVED,
            Color::Yellow,
            bail_tag(" — ci timeout", Color::Yellow),
            duration.as_ref().map(|d| format!("ci timeout after {d}")),
        ),
        ZenReason::MergeConflict { files } => (
            "bailed on",
            GLYPH_MOVED,
            Color::Yellow,
            bail_tag(" — merge conflict", Color::Yellow),
            files
                .as_ref()
                .map(|f| format!("conflict in {}", humanize_list(f))),
        ),
        ZenReason::Other => (
            "moved",
            GLYPH_MOVED,
            category_color(to.category()),
            Vec::new(),
            Some(format!("{from} → {to}")),
        ),
    };

    let mut row = Row::new(Marker::System, verb, title.to_string(), when);
    row.identity = system_identity("zen");
    row.glyph = glyph;
    row.glyph_color = glyph_color;
    row.trail = trail;
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
    // single agent action, so they read as machine-driven: neutral ◆ marker,
    // muted (untinted) name label, no title. The eye skims past them in
    // aggregate but can still pick out which workspace when scanning.
    let (verb, glyph, glyph_color, detail) = match new {
        WorkspaceState::Working => ("working", GLYPH_IDLE, Color::DarkGray, None),
        WorkspaceState::AwaitingInput => ("awaiting input", GLYPH_AWAITING, Color::Blue, None),
        WorkspaceState::Blocked => (
            "blocked",
            GLYPH_AWAITING,
            Color::Yellow,
            Some("needs human approval"),
        ),
        WorkspaceState::Paused => (
            "paused",
            GLYPH_AWAITING,
            Color::DarkGray,
            Some("waiting for the limit to reset"),
        ),
    };

    let mut row = Row::new(Marker::System, verb, String::new(), when);
    row.identity = vec![Span::styled(
        name.to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )];
    row.glyph = glyph;
    row.glyph_color = glyph_color;
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

const MARK_AGENT: &str = "●";
const MARK_SYSTEM: &str = "◆";
const BRANCH_ICON: &str = "⎇ ";

/// Status glyphs, keyed by the verb/category they convey. Colored by the
/// destination category at the call site so `started` reads active-yellow,
/// `finished` handoff-magenta, `done` green, and so on.
const GLYPH_STARTED: &str = "▶";
const GLYPH_FINISHED: &str = "✓";
const GLYPH_DONE: &str = "✔";
const GLYPH_IDLE: &str = "●";
const GLYPH_AWAITING: &str = "⏸";
const GLYPH_MOVED: &str = "⟳";
const GLYPH_PROMOTED: &str = "⇢";

/// Fixed width (cells) of the identity segment (name + `·role·`) so the
/// status glyph and verb line up across rows regardless of name length.
const IDENT_W: usize = 18;
/// Verbs pad to this width so titles line up across rows.
const VERB_W: usize = 8;
/// Left indent (cells) of the dim second line.
const SECONDARY_INDENT: usize = 4;
/// Dim verb color — visible but subordinate to the bright title.
const VERB_FG: Color = Color::Gray;

/// Lay one [`Row`] out into terminal lines:
///
/// ```text
/// ● name ·role·   ▶ verb    Title, truncated…            took Xm · 2m ago
///     ⎇ branch/name
/// ```
///
/// The marker leads (a tinted `●` for agents, a neutral `◆` for machine
/// rows); the identity is padded to [`IDENT_W`] and the verb to [`VERB_W`]
/// so glyphs, verbs, and titles line up across rows. The title truncates
/// with `…` so the right-aligned time never collides with it, and the
/// optional secondary prints as a dim indented second line. No row is
/// painted with a background fill.
fn paint_row(row: Row, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans: Vec<Span<'static>> = Vec::new();

    // Marker: ● in the agent tint, or a neutral ◆, plus a trailing gap.
    let marker = row.marker.glyph();
    spans.push(Span::styled(
        format!("{marker} "),
        Style::default().fg(row.marker.color()),
    ));
    let marker_w = display_w(marker) + 1;

    // Identity segment, padded to a fixed width so verbs align.
    let id_w: usize = row.identity.iter().map(|s| display_w(&s.content)).sum();
    spans.extend(row.identity);
    let id_pad = IDENT_W.saturating_sub(id_w).max(1);
    spans.push(Span::raw(" ".repeat(id_pad)));

    // Status glyph (optional) + padded verb.
    let mut glyph_w = 0;
    if !row.glyph.is_empty() {
        spans.push(Span::styled(
            row.glyph.to_string(),
            Style::default().fg(row.glyph_color),
        ));
        spans.push(Span::raw(" "));
        glyph_w = display_w(row.glyph) + 1;
    }
    let verb = pad_to(&row.verb, VERB_W);
    let verb_w = display_w(&verb);
    spans.push(Span::styled(verb, Style::default().fg(VERB_FG)));
    spans.push(Span::raw("  "));

    // The title takes whatever's left; `trail` and the right-aligned time
    // both count against its budget so nothing overruns `width`.
    let prefix_w = marker_w + id_w + id_pad + glyph_w + verb_w + 2;
    let trail_w: usize = row.trail.iter().map(|s| display_w(&s.content)).sum();
    let time_w = display_w(&row.time);
    let time_gap = usize::from(time_w > 0);
    let title_budget = width
        .saturating_sub(prefix_w + trail_w + time_w + time_gap)
        .max(1);
    let title = truncate(&row.title, title_budget);
    let title_w = display_w(&title);
    spans.push(Span::styled(title, row.title_style));
    spans.extend(row.trail);

    // Pad so the time hugs the right edge.
    let used = prefix_w + title_w + trail_w;
    let pad = width.saturating_sub(used + time_w);
    spans.push(Span::raw(" ".repeat(pad)));
    if time_w > 0 {
        spans.push(Span::styled(row.time, dim));
    }

    let mut out = vec![Line::from(spans)];
    if let Some(sec) = row.secondary {
        out.push(paint_secondary(sec, width));
    }
    out
}

/// Paint the dim second line under a row. A `Branch` gets the `⎇ ` icon; a
/// `Detail` is a plain muted fragment. Both indent under the row and
/// truncate with `…`.
fn paint_secondary(sec: SecondaryLine, width: usize) -> Line<'static> {
    let (icon, text) = match sec {
        SecondaryLine::Branch(b) => (BRANCH_ICON, b),
        SecondaryLine::Detail(d) => ("", d),
    };
    let budget = width
        .saturating_sub(SECONDARY_INDENT + display_w(icon))
        .max(1);
    let text = truncate(&text, budget);
    Line::from(vec![
        Span::raw(" ".repeat(SECONDARY_INDENT)),
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

/// Pad `s` on the right with ASCII spaces to a total visible width
/// of `w` cells. Char-based count is fine here — the verbs and glyphs
/// it pads are all single-cell.
fn pad_to(s: &str, w: usize) -> String {
    let have = display_w(s);
    if have >= w {
        return s.to_string();
    }
    let mut out = String::from(s);
    out.extend(std::iter::repeat(' ').take(w - have));
    out
}

fn display_w(s: &str) -> usize {
    s.chars().count()
}

/// "5m ago", "2h ago", "3d ago". Returns an empty string for events
/// in the future (clock skew) or with no timestamp.
fn relative_time(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now - ts;
    if delta.num_seconds() < 0 {
        return String::new();
    }
    if delta.num_seconds() < 60 {
        return "just now".into();
    }
    if delta.num_minutes() < 60 {
        return format!("{}m ago", delta.num_minutes());
    }
    if delta.num_hours() < 24 {
        return format!("{}h ago", delta.num_hours());
    }
    if delta.num_days() < 7 {
        return format!("{}d ago", delta.num_days());
    }
    let local = ts.with_timezone(&Local);
    if local.year() == Local::now().year() {
        local.format("%b %-d").to_string()
    } else {
        local.format("%Y-%m-%d").to_string()
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

    #[test]
    fn relative_time_buckets() {
        let now = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(
            relative_time(now - chrono::Duration::minutes(5), now),
            "5m ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::hours(2), now),
            "2h ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::days(3), now),
            "3d ago"
        );
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
    fn render_started_row_includes_role_tag_after_agent_name() {
        // Acceptance — dispatch rows surface the agent role as a dim
        // `·<role>·` tag right after the name, so the user can read which
        // role is on the workspace without going back to the workflow YAML.
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
            primary.contains("·developer·"),
            "missing role tag in: {primary:?}"
        );
        assert!(primary.contains("started"), "missing verb in: {primary:?}");
    }

    #[test]
    fn render_started_row_without_agent_field_skips_role_tag() {
        // Backward compat: a legacy event line with no `agent` field
        // renders cleanly, no empty `··` role tag left behind.
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
        assert!(!primary.contains('·'), "stray role tag in: {primary:?}");
        assert!(primary.contains("started"), "missing verb in: {primary:?}");
    }

    #[test]
    fn agent_row_leads_with_tinted_dot_and_dim_branch_line() {
        // Acceptance — an agent row is a colored ● dot + name, a dim role
        // tag, a status glyph, a verb, a bright title with the time
        // right-aligned, and a dim `⎇ `-led branch second line. No bg fill.
        let mut row = Row::new(
            Marker::Agent(Color::Cyan),
            "started",
            "Metrics API endpoint",
            "just now".to_string(),
        );
        row.identity = agent_identity("bravo", Color::Cyan, Some("developer"));
        row.glyph = GLYPH_STARTED;
        row.glyph_color = category_color(StatusCategory::Active);
        row.secondary = Some(SecondaryLine::Branch(
            "shelbi/metrics-api-endpoint".to_string(),
        ));
        let lines = paint_row(row, 80);

        assert_eq!(lines.len(), 2, "primary + one dim branch line");
        let l0 = line_text(&lines[0]);
        assert!(
            l0.starts_with(MARK_AGENT),
            "row must lead with the agent dot: {l0:?}"
        );
        assert!(l0.contains("bravo"), "missing agent name: {l0:?}");
        assert!(l0.contains("·developer·"), "missing dim role tag: {l0:?}");
        assert!(l0.contains(GLYPH_STARTED), "missing status glyph: {l0:?}");
        assert!(l0.contains("started"), "missing verb: {l0:?}");
        assert!(l0.contains("Metrics API endpoint"), "missing title: {l0:?}");
        assert!(
            l0.ends_with("just now"),
            "time must be right-aligned: {l0:?}"
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
    fn narrow_width_truncates_title_and_preserves_right_aligned_time() {
        let mut row = Row::new(
            Marker::Agent(Color::Cyan),
            "finished",
            "A very long task title that will never fit into a narrow terminal",
            "took 34m · 2m ago".to_string(),
        );
        row.identity = agent_identity("alpha", Color::Cyan, None);
        row.glyph = GLYPH_FINISHED;
        let lines = paint_row(row, 60);
        let l0 = line_text(&lines[0]);
        assert!(
            l0.contains('…'),
            "long title should be truncated with …: {l0:?}"
        );
        assert!(
            l0.contains("took 34m · 2m ago"),
            "time preserved intact: {l0:?}"
        );
        assert!(
            display_w(&l0) <= 60,
            "row must not overrun width: {} > 60 in {l0:?}",
            display_w(&l0)
        );
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
        assert!(l0.contains("2m ago"), "relative time kept: {l0:?}");
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
    fn render_zen_promote_uses_neutral_marker_and_no_bg() {
        let lines = render_zen_for_test(
            "orchestrator:zen-promote category=4",
            Column::backlog(),
            Column::todo(),
        );
        // Two lines: primary + dim secondary. No 3-row badge avatar.
        assert_eq!(lines.len(), 2, "zen row is a primary + one dim second line");
        // Primary: neutral ◆ marker, untinted 'zen' label, 'promoted' verb.
        let l0 = line_text(&lines[0]);
        assert!(
            l0.contains(MARK_SYSTEM),
            "primary missing system marker in {l0:?}"
        );
        assert!(
            !l0.contains(MARK_AGENT),
            "zen row must not use the agent dot in {l0:?}"
        );
        assert!(l0.contains("zen"), "primary missing zen label in {l0:?}");
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
