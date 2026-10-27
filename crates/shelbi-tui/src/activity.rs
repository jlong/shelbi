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
//! The parsing, title resolution, filtering and row content live in
//! [`shelbi_app::activity`], shared with the desktop app: this module owns the
//! terminal half (the pill row, scrolling, and laying a shared
//! [`FeedRow`] out in cells), so both clients show the same rows, in the same
//! order, with the same glyphs.

use std::fs;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub use shelbi_app::activity::{
    parse_event_line, ActivityData, ActivityFilter, ActivityKind, ActivityMetaCache, Event,
};
use shelbi_app::activity::{
    build_feed, read_tail, resolve_task_meta_into, FeedItem, FeedRow, Secondary, TaskCache,
};
use shelbi_app::theme::Tone;
use shelbi_state::keymap::{load_keymaps, ActivityAction, DisplayStyle, Keymaps};
use shelbi_state::{events_log_path, ZenModeState};

use crate::theme::palette;
use crate::keymap::format_chord_or_unbound;

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
    /// The render snapshot — the latest resolved metadata the feed draws from. On
    /// the shell path it is folded in by [`apply_activity_data`](Self::apply_activity_data);
    /// on the standalone path it is the snapshot [`resolve_task_meta`](Self::resolve_task_meta)
    /// produces from `meta_cache`.
    task_cache: TaskCache,
    /// The persistent title-resolution cache for the **standalone** process path
    /// (`shelbi __activity`): the incremental/frozen/backoff state that
    /// [`resolve_task_meta_into`] carries across passes. The single-process shell
    /// keeps its own equivalent cache (see [`read_activity_data`](Self::read_activity_data))
    /// and never touches this field.
    meta_cache: ActivityMetaCache,
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
            task_cache: TaskCache::new(),
            meta_cache: ActivityMetaCache::default(),
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

    /// Take new bindings (the shell's, after a Settings ▸ Keybindings
    /// save) for the footer hints and the handler.
    pub fn set_keymaps(&mut self, keymaps: Keymaps) {
        self.keymaps = keymaps;
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
                self.fail_status(format!("zen toggle failed: {e}"));
            }
        }
    }

    /// Set the status line to a user-facing error AND record it to the project's
    /// persistent error log (best-effort, tagged `activity`), so it survives the
    /// next status-line overwrite / a restart and lights the sidebar's
    /// unread-errors button. The status line keeps showing the raw message.
    fn fail_status(&mut self, message: String) {
        crate::error_report::log_error(&self.project_name, "activity", &message);
        self.status_line = message;
    }

    /// Toggle the Zen pill — flips between including-only-zen and not.
    /// Snaps scroll back to the newest so the user's eye stays on the
    /// freshly filtered head instead of a now-empty viewport.
    pub fn toggle_zen_filter(&mut self) {
        self.filter.toggle_kind(ActivityKind::Zen);
        self.snap_to_top();
    }

    /// Toggle the Workspaces pill — same shape as [`Self::toggle_zen_filter`].
    pub fn toggle_workspaces_filter(&mut self) {
        self.filter.toggle_kind(ActivityKind::Workspaces);
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
                self.fail_status(format!("events.log path failed: {e}"));
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
                self.fail_status(format!("read events.log: {e}"));
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

        // Resolve titles for the (possibly newly-loaded) events in one batched
        // request, off the per-frame render path. Only reached when the log
        // actually grew (the early returns above cover the idle case), so an idle
        // feed makes no request at all.
        self.resolve_task_meta();
    }

    pub fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_millis(500) {
            self.refresh();
        }
    }

    /// Read the activity feed off the UI thread: the IO half of a refresh for
    /// the single-process TUI shell, which cannot afford a blocking disk / `gh`
    /// read on its event loop. A full re-read of `events.log` with titles
    /// resolved incrementally into the shell-owned persistent `cache`
    /// ([`shelbi_app::activity::read_activity_data`]); folded in by
    /// [`apply_activity_data`](Self::apply_activity_data).
    ///
    /// Runs on the shell's dedicated activity worker, *separate* from the
    /// sidebar/board read, so even the first cold hydration pass never delays the
    /// sidebar painting.
    pub(crate) fn read_activity_data(
        project_name: &str,
        cache: &Mutex<ActivityMetaCache>,
    ) -> ActivityData {
        shelbi_app::activity::read_activity_data(project_name, cache)
    }

    /// Fold a freshly-read [`ActivityData`] in — the in-memory half of a refresh.
    /// Replaces the event list and task-meta cache; scroll / auto-scroll / filter
    /// state live on the app and are untouched, so the feed stays where the user
    /// left it. The incremental `log_offset` / `log_mtime` are reset so a later
    /// call to [`refresh`](Self::refresh) (if any) re-reads cleanly.
    pub(crate) fn apply_activity_data(&mut self, data: ActivityData) {
        if let Some(msg) = data.status {
            self.fail_status(msg);
        }
        self.events = data.events;
        self.task_cache = data.task_cache;
        // A full re-read superseded the incremental offset; forget it so a mixed
        // caller (shell apply then a standalone-style refresh) doesn't skip lines.
        self.log_offset = 0;
        self.log_mtime = None;
        self.last_refresh = Instant::now();
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

    /// Resolve every task id the loaded feed references that is stale or unknown,
    /// folding the result into the standalone process's persistent `meta_cache`
    /// and republishing the render snapshot. Delegates to the shared
    /// [`resolve_task_meta_into`] so this standalone path and the single-process
    /// shell's [`read_activity_data`](Self::read_activity_data) share **one** rule:
    /// the incremental batch bound, the frozen terminal/miss entries, and the
    /// exponential backoff on an errored fetch. Called on the refresh cadence, not
    /// per render frame, so a draw never issues an API call and an idle feed makes
    /// no request (O(1) per tick, not O(N)).
    fn resolve_task_meta(&mut self) {
        let snapshot = resolve_task_meta_into(
            &self.project_name,
            &self.events,
            &mut self.meta_cache,
            Instant::now(),
        );
        self.task_cache = snapshot;
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
                .fg(palette().ansi_cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}", app.project_name),
            Style::default().fg(palette().ansi_dark_gray),
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
        (PillKind::All, "All", app.filter.is_all(), palette().ansi_cyan),
        (PillKind::Zen, "Zen", app.filter.has_kind(ActivityKind::Zen), zen_fg()),
        (
            PillKind::Workspaces,
            "Workspaces",
            app.filter.has_kind(ActivityKind::Workspaces),
            palette().ansi_magenta,
        ),
    ];

    let inactive = Style::default().fg(palette().ansi_dark_gray);
    let sep_style = Style::default().fg(palette().ansi_dark_gray);

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
        Style::default().fg(palette().ansi_dark_gray),
    )));
    f.render_widget(footer, area);
}

/// Build the full feed as a flat `Vec<Line>` ready to hand to a scrollable
/// `Paragraph`: the shared [`build_feed`] (newest first, filtered, folded, with
/// a day header whenever the local-time day changes), each row laid out by
/// [`paint_row`] and followed by a blank line.
fn build_lines(app: &mut ActivityApp, width: usize, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let feed = build_feed(&app.events, &app.task_cache, &app.filter, now);
    let dim = Style::default().fg(palette().ansi_dark_gray);
    if let Some(empty) = feed.empty {
        return vec![Line::from(Span::styled(empty, dim))];
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    for item in &feed.items {
        match item {
            FeedItem::Day(label) => {
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                lines.push(date_header(label, width));
                lines.push(Line::raw(""));
            }
            FeedItem::Row(row) => {
                lines.extend(paint_row(row, width));
                lines.push(Line::raw(""));
            }
        }
    }
    lines
}

fn date_header(label: &str, width: usize) -> Line<'static> {
    let label_str = format!("── {label} ");
    let trail_w = width.saturating_sub(label_str.chars().count());
    let trail = "─".repeat(trail_w);
    Line::from(Span::styled(
        format!("{label_str}{trail}"),
        Style::default().fg(palette().ansi_dark_gray),
    ))
}

/// Foreground for the Zen filter pill. Kept from the old zen styling —
/// the badge/background tint is gone, but green still reads "zen".
fn zen_fg() -> Color {
    palette().ansi_green
}

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
/// A shared row tone as a terminal color.
fn tone(t: Tone) -> Color {
    t.pick(palette())
}

/// Lay one shared [`FeedRow`] out as a top-aligned three-column flex,
/// wrapping the message to as many lines as the title needs:
///
/// ```text
/// [alpha]  finished  site: make the home feature-grid mockups feel     [ready for review]  now
///                    real, alive, and animated on hover
///                    ⎇ shelbi/home-feature-grid
/// ```
///
/// The identity chip pins top-left; the state pill + relative time pin
/// top-right; the message (dim verb + bright title) wraps in between.
/// Wrapped continuation lines carry neither chip nor pill and indent to
/// align under the title. The title is never truncated with `…`; only a
/// dim secondary line (branch / reason) elides. No background fill.
fn paint_row(row: &FeedRow, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(palette().ansi_dark_gray);

    // Chip column: at least CHIP_W wide so messages line up; a chip wider
    // than that pushes only its own row's message over by a single space.
    let chip: Vec<Span<'static>> = row
        .chip
        .iter()
        .map(|c| {
            let mut style = Style::default().fg(tone(c.tone));
            if c.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            Span::styled(c.text(), style)
        })
        .collect();
    let chip_w: usize = chip.iter().map(|s| display_w(&s.content)).sum();
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
        let text = pill.bracketed();
        right_w += display_w(&text);
        right.push(Span::styled(
            text,
            Style::default().fg(tone(pill.tone)).add_modifier(Modifier::BOLD),
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
    let words = message_words(row);
    let first_budget = width
        .saturating_sub(title_x + right_w + if right_w > 0 { RIGHT_GAP } else { 0 })
        .max(1);
    let cont_budget = width.saturating_sub(title_x).max(1);
    let wrapped = wrap_words(&words, first_budget, cont_budget);

    let mut out: Vec<Line<'static>> = Vec::new();
    for (i, line_words) in wrapped.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if i == 0 {
            spans.extend(chip.clone());
            spans.push(Span::raw(" ".repeat(msg_x.saturating_sub(chip_w))));
            if verb_w > 0 {
                spans.push(Span::styled(
                    row.verb.clone(),
                    Style::default().fg(tone(row.verb_tone)),
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

    if let Some(sec) = &row.secondary {
        out.push(paint_secondary(sec, width, title_x));
    }
    out
}

/// Flatten a row's message into styled words for wrapping: the title in its
/// own tone, then the trail's text split into words that flow on after it (a
/// bail tag, a `(×N)` fold trailer).
fn message_words(row: &FeedRow) -> Vec<(String, Style)> {
    let title_style = Style::default().fg(tone(row.title_tone));
    let mut words: Vec<(String, Style)> = row
        .title
        .split_whitespace()
        .map(|w| (w.to_string(), title_style))
        .collect();
    if let Some((text, t)) = &row.trail {
        let style = Style::default().fg(tone(*t));
        words.extend(text.split_whitespace().map(|w| (w.to_string(), style)));
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

/// Paint the dim second line under a row. A branch gets the `⎇ ` icon; a
/// detail is a plain muted fragment. Both indent to align under the row's
/// title column and truncate with `…`: the secondary is context, not the
/// headline, so eliding it here is fine.
fn paint_secondary(sec: &Secondary, width: usize, indent: usize) -> Line<'static> {
    let (icon, text) = sec.parts();
    let budget = width.saturating_sub(indent + display_w(icon)).max(1);
    let text = truncate(text, budget);
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(
            format!("{icon}{text}"),
            Style::default().fg(palette().ansi_dark_gray),
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

// ---------------------------------------------------------------------------
// Tests

/// One event's painted lines (empty for a heartbeat), for the row tests.
#[cfg(test)]
fn render_event(
    ev: &Event,
    app: &mut ActivityApp,
    width: usize,
    now: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
) -> Vec<Line<'static>> {
    shelbi_app::activity::event_row(ev, &app.task_cache, now, started_at)
        .map(|row| paint_row(&row, width))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use shelbi_app::activity::{Chip, Pill, RowTarget};
    use shelbi_core::{Column, DEFAULT_WORKFLOW_NAME};

    /// The feed (title, pills, task moves, workspace state changes, system
    /// rows in success / failure / warning colors, the zen dry-run chip, the
    /// footer) is readable on a light terminal.
    #[test]
    fn renders_readable_in_light() {
        use ratatui::{backend::TestBackend, Terminal};
        use shelbi_app::theme::Appearance;
        let mut app = ActivityApp::new("demo");
        for line in [
            "2026-06-23T04:19:33+00:00 task=foo workflow=default todo -> in_progress reason=user:cli",
            "2026-07-22T14:00:00+00:00 project=demo task=fix-login workflow=app in_progress -> review reason=workspace:ready-marker from_category=active to_category=handoff",
            "2026-07-22T14:00:00+00:00 project=shelbi workspace=alpha working -> awaiting_input",
            "2026-07-22T14:00:00+00:00 project=demo workspace=bravo working -> paused reason=usage-limit",
            "2026-07-22T14:00:00+00:00 project=demo workspace=charlie pane_alive=false reason=worktree-missing",
            "2026-07-22T14:00:00+00:00 dispatch task=t workspace=delta status=confirmed detail=busy_observed",
            "2026-07-22T14:00:00+00:00 merge task=t workspace=echo base=main status=failed detail=not_mergeable",
            "2026-07-22T14:00:00+00:00 push task=t workspace=foxtrot branch=b status=blocked detail=remote_has_unrebased_commits",
            "2026-07-22T14:00:00+00:00 rebase task=t workspace=alpha branch=b status=up-to-date detail=default=abc",
            "2026-07-22T14:00:00+00:00 project=shelbi task=t ci pr=42 check=build conclusion=failure head_sha=deadbeef",
            "2026-07-22T14:00:00+00:00 project=shelbi task=t ci pr=42 check=all conclusion=success head_sha=deadbeef",
            "2026-07-22T14:00:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=master_open_failed",
            "2026-07-22T14:00:00+00:00 send project=demo workspace=alpha status=submitted detail=busy_observed",
            "2026-07-22T14:00:00+00:00 project=shelbi handoff outcome=written detail=/tmp/h.md",
            "2026-06-24T10:00:00Z zen-dryrun task=fix-typo action=consider-auto-promote detail=mechanically-eligible",
            "2026-06-23T12:00:00+00:00 project=demo heartbeat zen_eligible=5 idle_workspaces=4",
            "2026-06-23T12:00:00+00:00 frobnicate=xyz weird",
        ] {
            app.events.push(parse_event_line(line));
        }
        app.status_line = "refreshed".into();
        crate::theme::with_appearance(Appearance::Light, || {
            let mut term = Terminal::new(TestBackend::new(140, 48)).unwrap();
            term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
            crate::theme::assert_light_readable("activity", term.backend().buffer());
        });
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
    fn render_started_row_reads_as_a_sentence_with_chip_verb_and_pill() {
        // Acceptance — a start row reads as a sentence: an identity chip, the
        // `is building` verb, the task title, and an `[in progress]` state
        // pill. (No on-disk task file here, so the unassigned workspace
        // resolves to the `[orchestrator]` chip and the id stands in as title.)
        let mut app = ActivityApp::new("demo");
        let ts = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        let now = ts + chrono::Duration::minutes(1);
        let ev = Event::Issue {
            project: None,
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
        let ev = Event::Issue {
            project: None,
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
        let row = test_row(
            "is building",
            "Metrics API endpoint",
            "now",
            "bravo",
            Pill {
                text: "in progress".into(),
                tone: Tone::Yellow,
            },
            Some(Secondary::Branch("shelbi/metrics-api-endpoint".to_string())),
        );
        let lines = paint_row(&row, 80);

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
        let row = test_row(
            "finished",
            title,
            "took 34m · 2m",
            "alpha",
            Pill {
                text: "ready for review".into(),
                tone: Tone::Magenta,
            },
            None,
        );
        let lines = paint_row(&row, 60);

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
        let ev = Event::Issue {
            project: None,
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

    /// A shared row with an agent chip, built directly (not from an event) so
    /// the layout tests control every field.
    fn test_row(
        verb: &str,
        title: &str,
        time: &str,
        agent: &str,
        pill: Pill,
        secondary: Option<Secondary>,
    ) -> FeedRow {
        FeedRow {
            chip: Some(Chip {
                label: agent.into(),
                tone: Tone::Cyan,
                bold: true,
            }),
            verb: verb.into(),
            verb_tone: Tone::Gray,
            title: title.into(),
            title_tone: Tone::White,
            trail: None,
            pill: Some(pill),
            time: time.into(),
            secondary,
            target: Some(RowTarget::Task("t".into())),
        }
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
        let ev = Event::Issue {
            project: None,
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
    fn render_zen_merge_secondary_includes_pr_and_green_checks() {
        // The on-disk form: `append_task_event` folds the emitted
        // `orchestrator:zen-merge pr=1343` reason's space into an underscore.
        let lines = render_zen_for_test(
            "orchestrator:zen-merge_pr=1343",
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
            l1.contains("merged #1343"),
            "secondary missing pr number: {l1:?}"
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

    #[test]
    fn toggle_filter_methods_flip_state_and_snap_to_top() {
        let mut app = ActivityApp::new("demo");
        app.scroll = 25;
        app.auto_scroll = false;

        app.toggle_zen_filter();
        assert!(app.filter.has_kind(crate::activity::ActivityKind::Zen));
        assert_eq!(app.scroll, 0, "filter toggle should snap scroll to newest");
        assert!(app.auto_scroll);

        app.toggle_workspaces_filter();
        assert!(app.filter.has_kind(crate::activity::ActivityKind::Workspaces));

        app.reset_filter();
        assert!(app.filter.is_all(), "`a` resets both pills to off");
    }
}
