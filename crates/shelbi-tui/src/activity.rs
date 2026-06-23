//! Activity feed — human-friendly view of `~/.shelbi/events.log`.
//!
//! Renders the same append-only event stream `shelbi events tail`
//! consumes, but reformatted as a date-bucketed reverse-chronological
//! feed: who started what, who finished what, who's idle, who's waiting.
//! Per-agent avatars sit to the left of every row attributed to a
//! worker so the eye can group runs without re-reading names.
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
//! - `~/.shelbi/projects/<p>/tasks/<id>.md` — task title + priority +
//!   branch for the row's secondary line. Cached by id, invalidated
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

use shelbi_core::Column;
use shelbi_state::{events_log_path, WorkerState};

/// Avatar size in character cells. Each cell covers two vertical
/// pixels with the half-block glyphs (`▀▄█`), so the effective canvas
/// is 4×6 pixels — small enough that 20+ rows still fit on a typical
/// terminal, large enough that each face stays recognizable.
const AVATAR_W: usize = 4;
const AVATAR_H: usize = 3;

/// Gap (in cells) between the avatar column and the row's primary
/// text. Keeping it as a single constant means every event-type
/// renderer lines up against the same left margin.
const AVATAR_GAP: usize = 2;

/// Phonetic-letter creature avatars. One per phonetic-alphabet worker
/// name (alpha…foxtrot). Selected by name lookup in [`avatar_for`];
/// unknown names fall back to a single-line ` · ` placeholder.
const ALPHA_AVATAR: [&str; AVATAR_H] = ["▄▀▀▄", "█▄▄█", " ▀▀ "];
const BRAVO_AVATAR: [&str; AVATAR_H] = ["▄██▄", "█▄▄█", "▀  ▀"];
const CHARLIE_AVATAR: [&str; AVATAR_H] = ["▄▀▀▄", "▌▄▄▐", "▀  ▀"];
const DELTA_AVATAR: [&str; AVATAR_H] = ["▄▄▄▄", "▌▄▄▐", "▐  ▌"];
const ECHO_AVATAR: [&str; AVATAR_H] = ["▄▀▀▄", "█  █", "▀▀▀▀"];
const FOXTROT_AVATAR: [&str; AVATAR_H] = ["▄  ▄", "█▀▀█", "▐▄▄▌"];

/// Pick the (rows, tint-color) avatar for a worker name. Mapping is
/// hard-coded to the six declared phonetic-letter workers — every
/// other name falls back to `None` and the default fg color, so a
/// project that names workers `frontend` / `backend` still renders,
/// just without a unique face.
fn avatar_for(name: &str) -> (Option<[&'static str; AVATAR_H]>, Color) {
    match name {
        "alpha" => (Some(ALPHA_AVATAR), Color::Cyan),
        "bravo" => (Some(BRAVO_AVATAR), Color::Magenta),
        "charlie" => (Some(CHARLIE_AVATAR), Color::Green),
        "delta" => (Some(DELTA_AVATAR), Color::Yellow),
        "echo" => (Some(ECHO_AVATAR), Color::Blue),
        "foxtrot" => (Some(FOXTROT_AVATAR), Color::LightRed),
        _ => (None, Color::Gray),
    }
}

/// One parsed line out of `events.log`. The raw line is kept on every
/// variant so the renderer can fall back to the original text if a
/// later format change introduces a shape we don't recognize — nothing
/// in the file should ever be silently dropped.
#[derive(Debug, Clone)]
pub enum Event {
    Task {
        ts: DateTime<Utc>,
        id: String,
        from: Column,
        to: Column,
        reason: String,
        raw: String,
    },
    Worker {
        ts: DateTime<Utc>,
        name: String,
        prev: Option<WorkerState>,
        new: WorkerState,
        raw: String,
    },
    /// Recognized timestamp but the rest doesn't match the task/worker
    /// shape — render the original line verbatim so nothing vanishes.
    Unknown {
        ts: Option<DateTime<Utc>>,
        raw: String,
    },
}

impl Event {
    pub fn ts(&self) -> Option<DateTime<Utc>> {
        match self {
            Event::Task { ts, .. } | Event::Worker { ts, .. } => Some(*ts),
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
    priority: u32,
    branch: Option<String>,
    assigned_to: Option<String>,
    mtime: Option<SystemTime>,
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
}

impl ActivityApp {
    pub fn new(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
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
                            priority: tf.task.priority,
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
            Event::Task { id, to, ts, .. } if id == task_id && *to == Column::InProgress => {
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
        // Format: `<id> <from> -> <to> reason=<reason>`
        let mut tokens = rest.splitn(5, ' ');
        let id = tokens.next().unwrap_or("");
        let from_s = tokens.next().unwrap_or("");
        let arrow = tokens.next().unwrap_or("");
        let to_s = tokens.next().unwrap_or("");
        let reason_s = tokens.next().unwrap_or("");
        if arrow == "->" {
            if let (Ok(from), Ok(to)) = (from_s.parse::<Column>(), to_s.parse::<Column>()) {
                let reason = reason_s
                    .strip_prefix("reason=")
                    .unwrap_or(reason_s)
                    .to_string();
                return Event::Task {
                    ts,
                    id: id.to_string(),
                    from,
                    to,
                    reason,
                    raw,
                };
            }
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    if let Some(rest) = rest.strip_prefix("worker=") {
        // Format: `<name> <prev> -> <new>`
        let mut tokens = rest.splitn(4, ' ');
        let name = tokens.next().unwrap_or("");
        let prev_s = tokens.next().unwrap_or("");
        let arrow = tokens.next().unwrap_or("");
        let new_s = tokens.next().unwrap_or("");
        if arrow == "->" {
            if let Some(new) = parse_worker_state(new_s) {
                let prev = if prev_s == "none" {
                    None
                } else {
                    parse_worker_state(prev_s)
                };
                return Event::Worker {
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

fn parse_worker_state(s: &str) -> Option<WorkerState> {
    match s {
        "working" => Some(WorkerState::Working),
        "awaiting_input" => Some(WorkerState::AwaitingInput),
        "blocked" => Some(WorkerState::Blocked),
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
            Constraint::Length(2), // title
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_title(f, app, outer[0]);
    render_body(f, app, outer[1]);
    render_footer(f, app, outer[2]);
}

fn render_title(f: &mut Frame, app: &ActivityApp, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            "Activity",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}", app.project_name),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(vec![title, Line::raw("")]), area);
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

fn render_footer(f: &mut Frame, _app: &ActivityApp, area: Rect) {
    let footer = Paragraph::new(Line::from(Span::styled(
        "j/k scroll · u/d page · g top · G bottom · r refresh",
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

    // Newest indices first. Cloning the event out is cheap (small
    // strings); it dodges a borrow conflict between iterating
    // `app.events` and calling `app.task_meta` inside the loop body.
    let order: Vec<usize> = (0..app.events.len()).rev().collect();

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
            if *to == Column::Review {
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

/// What sits in the avatar column. Three shapes — a full 3-row
/// creature face for worker-attributed task transitions, a single
/// glyph (★ / ◆ / ✓) for task-only events, or nothing (raw fallback).
enum AvatarSlot {
    Face {
        rows: [&'static str; AVATAR_H],
        color: Color,
    },
    Glyph {
        ch: &'static str,
        color: Color,
    },
    None,
}

impl AvatarSlot {
    /// Resolve a worker name into either a creature face (when the
    /// name is one of the six phonetic-letter workers) or `Glyph`
    /// stand-in keyed by the fallback symbol.
    fn for_worker(worker: Option<&str>, fallback_glyph: &'static str, default_color: Color) -> Self {
        match worker {
            Some(w) => match avatar_for(w) {
                (Some(rows), color) => AvatarSlot::Face { rows, color },
                (None, _) => AvatarSlot::Glyph {
                    ch: fallback_glyph,
                    color: default_color,
                },
            },
            None => AvatarSlot::Glyph {
                ch: fallback_glyph,
                color: default_color,
            },
        }
    }
}

/// The primary + secondary text for one event row.
struct RowText {
    /// Top line — bold worker name (if any), verb, italic title.
    /// Pre-spanned so workers and verbs can carry different styles.
    primary: Vec<Span<'static>>,
    /// Optional muted detail line under the primary.
    secondary: Option<String>,
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
            raw,
        } => render_task_event(app, *ts, id, *from, *to, reason, raw, width, now, started_at),
        Event::Worker {
            ts, name, new, ..
        } => render_worker_event(*ts, name, *new, width, now),
        Event::Unknown { ts, raw } => {
            let when = ts.map(|t| relative_time(t, now)).unwrap_or_default();
            let row = RowText {
                primary: vec![Span::styled(
                    format!("? {raw}"),
                    Style::default().fg(Color::DarkGray),
                )],
                secondary: None,
            };
            paint_row(AvatarSlot::None, row, width, when)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_task_event(
    app: &mut ActivityApp,
    ts: DateTime<Utc>,
    id: &str,
    from: Column,
    to: Column,
    _reason: &str,
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
    let priority = meta.as_ref().map(|m| m.priority);
    let branch = meta.as_ref().and_then(|m| m.branch.clone());
    let worker = meta.as_ref().and_then(|m| m.assigned_to.clone());

    let when = relative_time(ts, now);
    let title_quoted = format!("\u{201C}{title}\u{201D}");

    match (from, to) {
        (Column::Backlog, Column::Todo) => {
            // Promoted — no agent attribution.
            let row = RowText {
                primary: vec![
                    Span::styled(
                        "Promoted".to_string(),
                        Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        title_quoted,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ],
                secondary: Some(join_detail(&["backlog → todo", &fmt_priority(priority)])),
            };
            paint_row(
                AvatarSlot::Glyph {
                    ch: GLYPH_PROMOTED,
                    color: Color::Cyan,
                },
                row,
                width,
                when,
            )
        }
        (Column::Todo, Column::InProgress) => {
            let (worker_span, slot) = worker_attribution(
                worker.as_deref(),
                GLYPH_STARTED,
                Color::Green,
            );
            let row = RowText {
                primary: vec![
                    worker_span,
                    Span::raw("  "),
                    Span::styled("started".to_string(), Style::default().fg(Color::Gray)),
                    Span::raw("  "),
                    Span::styled(
                        title_quoted,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ],
                secondary: Some(join_detail(&[
                    &branch
                        .as_ref()
                        .map(|b| format!("branch: {b}"))
                        .unwrap_or_default(),
                    &fmt_priority(priority),
                ])),
            };
            paint_row(slot, row, width, when)
        }
        (Column::InProgress, Column::Review) => {
            let (worker_span, slot) = worker_attribution(
                worker.as_deref(),
                GLYPH_FINISHED,
                Color::Cyan,
            );
            let took = started_at.map(|s| format!("took {}", short_duration(ts - s)));
            let row = RowText {
                primary: vec![
                    worker_span,
                    Span::raw("  "),
                    Span::styled("finished".to_string(), Style::default().fg(Color::Gray)),
                    Span::raw("  "),
                    Span::styled(
                        title_quoted,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(
                        " — ready for review".to_string(),
                        Style::default().fg(Color::Cyan),
                    ),
                ],
                secondary: Some(join_detail(&[
                    &took.unwrap_or_default(),
                    &branch
                        .as_ref()
                        .map(|b| format!("branch: {b}"))
                        .unwrap_or_default(),
                ])),
            };
            paint_row(slot, row, width, when)
        }
        (Column::Review, Column::Done) => {
            let row = RowText {
                primary: vec![
                    Span::styled(
                        title_quoted,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(" accepted".to_string(), Style::default().fg(Color::Gray)),
                ],
                secondary: Some("moved to done".to_string()),
            };
            paint_row(
                AvatarSlot::Glyph {
                    ch: GLYPH_ACCEPTED,
                    color: Color::Cyan,
                },
                row,
                width,
                when,
            )
        }
        _ => {
            // Unrecognized transition — render raw line so nothing
            // silently disappears from the feed.
            let summary = format!("task {id}: {from} → {to}");
            let row = RowText {
                primary: vec![Span::styled(summary, Style::default().fg(Color::Gray))],
                secondary: Some(raw.to_string()),
            };
            paint_row(AvatarSlot::None, row, width, when)
        }
    }
}

fn render_worker_event(
    ts: DateTime<Utc>,
    name: &str,
    new: WorkerState,
    width: usize,
    now: DateTime<Utc>,
) -> Vec<Line<'static>> {
    let when = relative_time(ts, now);

    // Worker-state events are noisier than task transitions — render
    // the primary muted so the eye skims past them in aggregate but
    // can still see them when looking for context.
    let muted_name = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(Color::DarkGray);

    let (verb, detail) = match new {
        WorkerState::Working => ("is working", None),
        WorkerState::AwaitingInput => ("is waiting for input", None),
        WorkerState::Blocked => (
            "is blocked on a permission prompt",
            Some("needs human approval"),
        ),
    };

    let row = RowText {
        primary: vec![
            Span::styled(name.to_string(), muted_name),
            Span::raw(" "),
            Span::styled(verb.to_string(), muted),
        ],
        secondary: detail.map(|d| d.to_string()),
    };

    // Even for muted rows, keep the avatar visible so the eye can
    // still group "alpha's run" without re-reading the name.
    let slot = match avatar_for(name) {
        (Some(rows), color) => AvatarSlot::Face { rows, color },
        (None, _) => AvatarSlot::Glyph {
            ch: GLYPH_DOT,
            color: Color::DarkGray,
        },
    };
    paint_row(slot, row, width, when)
}

/// Build the "<worker>" Span and matching avatar slot for a task
/// transition. Falls back to the supplied glyph + color when the
/// task isn't assigned to anyone (or is assigned to a non-phonetic
/// name we don't have art for).
fn worker_attribution(
    worker: Option<&str>,
    fallback_glyph: &'static str,
    fallback_color: Color,
) -> (Span<'static>, AvatarSlot) {
    let label = worker.unwrap_or("orchestrator").to_string();
    let span = Span::styled(
        label,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let slot = AvatarSlot::for_worker(worker, fallback_glyph, fallback_color);
    (span, slot)
}

const GLYPH_PROMOTED: &str = "★";
const GLYPH_STARTED: &str = "▲";
const GLYPH_FINISHED: &str = "◆";
const GLYPH_ACCEPTED: &str = "✓";
const GLYPH_DOT: &str = "·";

/// Paint one event into terminal lines, aligning the avatar/glyph
/// column on the left, the primary text in the middle, and the
/// relative timestamp right-aligned on the first line.
///
/// Output line count varies by slot kind:
/// - `Face` → 3 rows of avatar art (lines 1-3), primary text on line
///   1, optional secondary on line 2.
/// - `Glyph` → 1 row of avatar art (line 1), primary text on line 1,
///   optional secondary on line 2.
/// - `None` → no avatar column; primary + optional secondary indented
///   under the same left margin.
fn paint_row(
    slot: AvatarSlot,
    row: RowText,
    width: usize,
    when: String,
) -> Vec<Line<'static>> {
    let when_w = display_w(&when);
    let when_style = Style::default().fg(Color::DarkGray);
    let indent_w = AVATAR_W + AVATAR_GAP;
    let mut out: Vec<Line<'static>> = Vec::new();

    let (avatar_row1, avatar_row2, avatar_row3, color, has_third_row) = match slot {
        AvatarSlot::Face { rows, color } => (
            rows[0].to_string(),
            rows[1].to_string(),
            rows[2].to_string(),
            color,
            true,
        ),
        AvatarSlot::Glyph { ch, color } => {
            // Pad the single glyph to AVATAR_W cells so the primary
            // text aligns with the face-avatar rows.
            let padded = pad_to(ch, AVATAR_W);
            (padded, pad_to("", AVATAR_W), pad_to("", AVATAR_W), color, false)
        }
        AvatarSlot::None => (
            pad_to("", AVATAR_W),
            pad_to("", AVATAR_W),
            pad_to("", AVATAR_W),
            Color::DarkGray,
            false,
        ),
    };
    let avatar_style = Style::default().fg(color);

    // Row 1: avatar + primary + right-aligned timestamp.
    let primary_w: usize = row.primary.iter().map(|s| display_w(&s.content)).sum();
    let used = display_w(&avatar_row1) + AVATAR_GAP + primary_w;
    let pad = width.saturating_sub(used + when_w);
    let mut line1: Vec<Span<'static>> = vec![
        Span::styled(avatar_row1, avatar_style),
        Span::raw(" ".repeat(AVATAR_GAP)),
    ];
    line1.extend(row.primary);
    line1.push(Span::raw(" ".repeat(pad)));
    line1.push(Span::styled(when, when_style));
    out.push(Line::from(line1));

    // Row 2: avatar + secondary (if any). For glyph/none slots, the
    // avatar row is blank so the secondary text indents cleanly.
    let row2 = match (row.secondary, has_third_row, !avatar_row2.trim().is_empty()) {
        (Some(detail), _, _) => {
            let mut spans: Vec<Span<'static>> = vec![
                Span::styled(avatar_row2, avatar_style),
                Span::raw(" ".repeat(AVATAR_GAP)),
            ];
            spans.push(Span::styled(detail, when_style));
            Some(Line::from(spans))
        }
        (None, true, _) => {
            // No secondary but we still need the second avatar row to
            // keep the face intact.
            Some(Line::from(vec![Span::styled(avatar_row2, avatar_style)]))
        }
        (None, false, false) => None,
        (None, false, true) => {
            // Glyph slot with no secondary: skip the row entirely.
            None
        }
    };
    if let Some(l) = row2 {
        out.push(l);
    }

    // Row 3: avatar only (faces); skipped for glyph/none.
    if has_third_row {
        out.push(Line::from(vec![Span::styled(avatar_row3, avatar_style)]));
    }

    // For slots with no avatar art at all, indent the secondary line
    // we already emitted above (or the primary if there was no
    // secondary) so the column stays anchored. The `indent_w`
    // constant is used by the empty `pad_to` calls above so this is
    // automatic — but if the primary line came out shorter than
    // `indent_w` we don't add extra padding here; the renderer's
    // left margin already gives us breathing room.
    let _ = indent_w;
    out
}

/// Pad `s` on the right with ASCII spaces to a total visible width
/// of `w` cells. Char-based count is fine here — avatar art and the
/// glyph fallbacks are all single-cell.
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

fn fmt_priority(p: Option<u32>) -> String {
    p.map(|v| format!("#{v}")).unwrap_or_default()
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
    fn parses_task_event() {
        let line = "2026-06-23T04:19:33.715717+00:00 task=foo todo -> in_progress reason=user:cli:start";
        match parse_event_line(line) {
            Event::Task {
                id, from, to, reason, ..
            } => {
                assert_eq!(id, "foo");
                assert_eq!(from, Column::Todo);
                assert_eq!(to, Column::InProgress);
                assert_eq!(reason, "user:cli:start");
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn parses_worker_event() {
        let line = "2026-06-23T04:19:33Z worker=alpha working -> awaiting_input";
        match parse_event_line(line) {
            Event::Worker { name, prev, new, .. } => {
                assert_eq!(name, "alpha");
                assert_eq!(prev, Some(WorkerState::Working));
                assert_eq!(new, WorkerState::AwaitingInput);
            }
            other => panic!("expected worker event, got {other:?}"),
        }
    }

    #[test]
    fn parses_first_observation_worker_event_with_none_prev() {
        let line = "2026-06-23T04:19:33Z worker=alpha none -> working";
        match parse_event_line(line) {
            Event::Worker { name, prev, new, .. } => {
                assert_eq!(name, "alpha");
                assert!(prev.is_none(), "`none` prev must parse as Option::None");
                assert_eq!(new, WorkerState::Working);
            }
            other => panic!("expected worker event, got {other:?}"),
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
    fn each_phonetic_worker_has_unique_avatar() {
        // The recognizability of the feed depends on each worker
        // getting a distinct face — regression-test that no two
        // phonetic names accidentally share avatar art.
        let workers = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
        let mut seen: Vec<[&str; AVATAR_H]> = Vec::new();
        for w in workers {
            let (avatar, _) = avatar_for(w);
            let av = avatar.unwrap_or_else(|| panic!("{w} must have an avatar"));
            assert!(!seen.contains(&av), "duplicate avatar for {w}");
            seen.push(av);
        }
    }

    #[test]
    fn unknown_worker_has_no_avatar_but_default_color() {
        let (avatar, color) = avatar_for("frontend");
        assert!(avatar.is_none(), "non-phonetic worker has no avatar");
        assert_eq!(color, Color::Gray);
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
            from: Column::Todo,
            to: Column::InProgress,
            reason: "user:cli".into(),
            raw: String::new(),
        });
        // Unrelated task in between — must not affect the lookup.
        app.events.push(Event::Task {
            ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 5, 0).unwrap(),
            id: "bar".into(),
            from: Column::Todo,
            to: Column::InProgress,
            reason: "user:cli".into(),
            raw: String::new(),
        });
        app.events.push(Event::Task {
            ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 18, 0).unwrap(),
            id: "foo".into(),
            from: Column::InProgress,
            to: Column::Review,
            reason: "worker:review-marker".into(),
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
