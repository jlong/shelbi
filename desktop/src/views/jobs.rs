//! The Jobs screen (`View::Jobs`, palette-only): the project's awareness
//! jobs (what `shelbi job run` and `shelbi job watch` create), drawn from the
//! shared [`shelbi_app::jobs`] model the TUI's Jobs page draws too. It is the
//! Jobs segment of the Jobs and Reminders screen
//! ([`super::jobs_reminders`]), whose toolbar carries its name and counts.
//!
//! There's no Figma frame for it, so it borrows the app's vocabulary: the
//! board's cards, the issue pane's chips and the Changelog's centered
//! column (at most [`CONTENT_MAX_W`] wide, so a wide window doesn't stretch
//! a row into a scan across the screen).
//!
//! ```text
//! 2 running · 3 finished        [ ▤ Jobs | ◷ Reminders ]
//! ─────────────────────────────────────────────────────────────
//!   Running 2
//!   ╭─────────────────────────────────────────────────────────╮
//!   │ ◠ bundle desktop app                           4m / 12m │
//!   │ ████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░    35% │
//!   │ [Command] [👤 alpha] zsh -c ./bundle.sh …      2:06 PM │
//!   │ › Bundling Shelbi.app                                   │
//!   ╰─────────────────────────────────────────────────────────╯
//!   Today
//!   ╭─────────────────────────────────────────────────────────╮
//!   │ ✓ site build    npm run build                  2m 12m ago│
//!   │ ✕ desktop tests exit 101 · FAILED…             7m  1h ago│
//!   ╰─────────────────────────────────────────────────────────╯
//! ```
//!
//! Running jobs are the hero: one card each (the board card's surface,
//! corners and shadow), with a live spinner (the progress color, the warning
//! color once overdue), the label, an `Overdue` chip once it is past its
//! estimate, and the elapsed time against the estimate; under it a rounded estimate bar with the share used
//! (or how far over); a meta line with the job's kind, who started it, its
//! command and when it started; and a well with what it is doing: a
//! command's last output line in monospace, or what a watch waits for.
//!
//! Finished jobs are compact history, newest first, grouped by the day they
//! finished (`Today`, `Yesterday`, `Mon, Nov 16`), each day on one card: a
//! status mark (a green check, a red ✕, a yellow stop for a cancel, a
//! neutral mark for a watch that ended), the label, the command (a failed
//! job's exit code and last line instead, in red), a watch's kind, how long it
//! ran and how long ago it finished. Labels, commands and output lines cut
//! with an ellipsis so the screen never scrolls sideways; hovering one shows
//! it whole.
//!
//! With no jobs at all the screen says what jobs are and offers the
//! command that starts one, in the bootstrap screen's copyable command box.
//!
//! Clicking a row opens the job's sheet, drawn exactly like the issue pane
//! (the shared [`super::floating_pane`]): floating over the table's right,
//! [`EDGE`](super::floating_pane::EDGE) in from the top, right and bottom,
//! at the window's task pane width (its left edge resizes it, and the board's
//! pane follows), sliding in and back out. The table keeps its full width,
//! scroll and selection under it; clicking another row shows that job in the
//! pane.
//!
//! ```text
//! ┌ Jobs ──────────────────────────────────────────────────────┐
//! │ j7 bundle …  running  4m 12s / 12m  ┌──────────────────────┐ │
//! │ j6 checks …  overdue  …             │ j7 · bundle desktop ✕│ │
//! │ (the table, full width under the    │ Status    ● Running  │ │
//! │  pane, visible in its margins)      │ Duration  4m / 12m ▬ │ │
//! │                                     │ Command   zsh -c …   │ │
//! │                                     │ ┌ terminal ────────┐ │ │
//! │                                     │ │ …                │ │ │
//! │                                     │ └──────────────────┘ │ │
//! │                                     │         [Cancel job] │ │
//! │                                     └──────────────────────┘ │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! The Header reads `id · label`, then ✕. Under it, the job's Status (its
//! glyph and tone), Duration (with the estimate bar while it runs against
//! one), when it started and who started it, and its Command as property
//! rows, then a running command's live
//! terminal (watched, never typed into) or a finished one's saved output in
//! a framed well, and Cancel while it runs. Escape or ✕ closes it. The loads
//! are [`shelbi_app::jobs::open_sheet`]'s, the TUI's too.
//!
//! The awareness store and the history have no change notification, so the
//! view re-reads them every [`REFRESH_INTERVAL`] while it is open, off the UI
//! thread through the window's [`ProjectSource`] (a remote window's source
//! says the far store isn't readable yet, and the view shows that line).
//! Each read also repaints, which is what ticks the running durations and
//! the Finished ages over.

use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, relative, AnyElement, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Hsla,
    KeyDownEvent, ScrollHandle, SharedString, Task, Window,
};
use shelbi_app::jobs::{
    clock_label, one_line, JobRow, JobsModel, JobsRead, SheetLoad, CANCEL_LABEL, EMPTY_COMMAND, EMPTY_DETAIL,
    EMPTY_TITLE, REFRESH_INTERVAL, RUNNING_HEADING,
};
use shelbi_app::source::{ProjectHandle, ProjectSource};
use shelbi_app::theme::Tone;
use shelbi_state::awareness::{JobKind, JobStatus};

use super::estimate_bar;
use super::floating_pane::{
    self, header_button, property_row, value_text, PaneFrame, PaneHost, PaneIds, PaneMotion, SlideOut, INSET,
};
use super::list_screen::{self, section_heading, tint, CARD_RADIUS};
use crate::assets::{glyph, glyphs, heroicon, icons};
use crate::elements::spinner::spinner_sized;
use crate::elements::terminal::metrics::PRIMARY_FAMILY;
use crate::elements::terminal::{session as term_session, TerminalPane};
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// How often the view drains its refresher.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// A running card's padding.
const CARD_PAD: f32 = 14.0;
/// A running card's estimate bar: thick enough to read at a glance.
const CARD_BAR_H: f32 = 6.0;
/// The bar's words (`35%`, `12m over`).
const CARD_BAR_LABEL_W: f32 = 64.0;
/// The running card's live spinner.
const LIVE_GLYPH: f32 = 14.0;
/// A history row's status mark.
const MARK: f32 = 18.0;
/// The history's trailing columns, right-aligned so they line up across
/// rows and days.
const KIND_W: f32 = 96.0;
/// Wide enough for a duration against an estimate (`1h 4m / 1h 30m`).
const HISTORY_DURATION_W: f32 = 108.0;
const AGO_W: f32 = 64.0;
/// A command's and an output line's monospace size.
const MONO_SIZE: f32 = 12.0;
const MONO_LINE: f32 = 18.0;
/// A label or line longer than this many characters may be cut: it gets a
/// tooltip with the whole text.
const TIP_CHARS: usize = 40;

pub struct JobsView {
    project: String,
    source: std::sync::Arc<dyn ProjectSource>,
    model: JobsModel,
    /// The open job's sheet.
    sheet: Option<JobSheet>,
    /// A closed sheet sliding back out.
    closing: SlideOut<JobSheet>,
    /// The sheet's width (the window's task pane width) and a drag of its
    /// edge.
    pane_frame: PaneFrame,
    focus: FocusHandle,
    needs_focus: bool,
    refresher: shelbi_app::Refresher<JobsRead>,
    last_refresh: Instant,
    /// Whether its segment of the Jobs and Reminders screen shows: hidden,
    /// it asks for no reads (the shown segment's poll is the only one).
    active: bool,
    scroll: ScrollHandle,
    _poll: Task<()>,
}

/// Something the window acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum JobsEvent {
    /// A drag of the sheet's left edge ended on this width: the window saves
    /// it as its task pane width (`WindowState::task_pane_width`), which the
    /// board's pane follows too.
    PaneWidth(f32),
}

impl EventEmitter<JobsEvent> for JobsView {}

impl PaneHost for JobsView {
    fn pane_frame(&mut self) -> &mut PaneFrame {
        &mut self.pane_frame
    }

    fn pane_resized(&mut self, width: f32, cx: &mut Context<Self>) {
        cx.emit(JobsEvent::PaneWidth(width));
    }
}

/// The sheet's ids: tests and the visual check find it by them.
const IDS: PaneIds = PaneIds {
    pane: "job-sheet",
    closing: "job-sheet-closing",
    resize: "job-sheet-resize",
    inert: "job-sheet-inert",
    slide_in: "job-sheet-in",
    slide_out: "job-sheet-out",
};

/// The terminal well's corners.
const WELL_RADIUS: f32 = 8.0;
/// The terminal well's room around the grid.
const WELL_PAD: f32 = 8.0;
/// The Duration row's estimate bar.
const BAR_W: f32 = 96.0;
const BAR_THICK: f32 = 4.0;

/// One job's side sheet.
struct JobSheet {
    id: String,
    /// The row it opened on, for when the job leaves the model (pruned).
    opened: JobRow,
    body: SheetBody,
    /// A cancel was asked for and the row still says running.
    canceling: bool,
    /// Why the cancel failed, when it did.
    cancel_error: Option<String>,
    _load: Option<Task<()>>,
}

enum SheetBody {
    Loading,
    /// The live terminal, or the saved output replayed.
    Terminal(Entity<TerminalPane>),
    Message(String),
}

impl JobsView {
    pub fn new(handle: &ProjectHandle, cx: &mut Context<Self>) -> Self {
        Self::over(handle.name().to_string(), handle.source.clone(), cx)
    }

    fn over(project: String, source: std::sync::Arc<dyn ProjectSource>, cx: &mut Context<Self>) -> Self {
        let reader = source.clone();
        let refresher = shelbi_app::spawn_worker(move |_| reader.jobs());
        refresher.request();
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                break;
            }
        });
        JobsView {
            project,
            source,
            model: JobsModel::Loading,
            sheet: None,
            closing: SlideOut::default(),
            pane_frame: PaneFrame::default(),
            focus: cx.focus_handle(),
            needs_focus: true,
            refresher,
            last_refresh: Instant::now(),
            active: true,
            scroll: ScrollHandle::new(),
            _poll: poll,
        }
    }

    /// The window is dropping this view (a project switch): stop its reads
    /// without waiting on one in flight.
    pub fn stop_workers(&mut self) {
        self.refresher.stop_without_waiting();
    }

    pub fn request_focus(&mut self) {
        self.needs_focus = true;
    }

    /// Show or hide its segment of the Jobs and Reminders screen. Hidden, it
    /// stops asking for reads; shown again, it reads at once.
    pub fn set_active(&mut self, active: bool) {
        if active && !self.active {
            self.refresh();
        }
        self.active = active;
    }

    /// The counts the toolbar shows while its segment is selected.
    pub fn summary(&self) -> Option<String> {
        self.model.summary()
    }

    /// Drop the sheet at once, without sliding it out (a switch to the
    /// other segment: nothing would see it go).
    pub fn dismiss_sheet(&mut self, cx: &mut Context<Self>) {
        self.pane_frame.cancel_resize();
        self.closing.clear();
        if self.sheet.take().is_some() {
            cx.notify();
        }
    }

    /// Whether it asks for reads (its segment shows).
    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The window's saved task pane width, which the sheet shares.
    pub fn set_pane_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if self.pane_frame.set_saved(width) {
            cx.notify();
        }
    }

    /// The sheet's chosen width.
    #[cfg(test)]
    pub fn pane_width(&self) -> f32 {
        self.pane_frame.wanted()
    }

    /// Whether a closed sheet is still sliding out.
    #[cfg(test)]
    pub fn sheet_closing(&self) -> bool {
        self.closing.get().is_some()
    }

    /// Open job `id`'s sheet, as a click on its row does.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn open_job(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_sheet(id, window, cx);
    }

    /// What the view shows.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn model(&self) -> &JobsModel {
        &self.model
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if let Some(read) = self.refresher.latest() {
            self.model = JobsModel::from_read(read);
            // Also when nothing changed: the ages tick over.
            cx.notify();
        }
        if self.active && self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresher.request();
            self.last_refresh = Instant::now();
        }
    }

    /// Read now and fold the read in (the offscreen check's dispatcher
    /// doesn't run the poll timer, and tests don't wait out the cadence).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        self.tick(cx);
    }

    /// Whether the open sheet has its terminal (loaded, not a message).
    #[cfg(feature = "visual-check")]
    pub fn sheet_has_terminal(&self) -> bool {
        self.sheet.as_ref().is_some_and(|s| matches!(s.body, SheetBody::Terminal(_)))
    }

    /// The open sheet's job and what its terminal shows (`None` while it
    /// loads or shows a message), for tests.
    #[cfg(test)]
    pub fn sheet_text(&self, cx: &gpui::App) -> Option<(String, Option<String>)> {
        let sheet = self.sheet.as_ref()?;
        let text = match &sheet.body {
            SheetBody::Terminal(pane) => Some(pane.read(cx).terminal_text()),
            _ => None,
        };
        Some((sheet.id.clone(), text))
    }

    /// The open sheet's Header title.
    #[cfg(test)]
    pub fn sheet_title(&self) -> Option<String> {
        let row = self.sheet_row()?;
        Some(format!("{} · {}", row.id, one_line(&row.description)))
    }

    /// Ask for a read now.
    pub fn refresh(&mut self) {
        self.refresher.request();
        self.last_refresh = Instant::now();
    }

    /// Open job `id`'s sheet (replacing an open one) and load what it shows
    /// off the UI thread.
    fn open_sheet(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Its own row again: the sheet stays as it is.
        if self.sheet.as_ref().is_some_and(|s| s.id == id) {
            return;
        }
        let Some(row) = self.model.rows().iter().find(|r| r.id == id).cloned() else {
            return;
        };
        // A sheet opened while another slides out shows at once.
        self.closing.clear();
        let (project, load_row) = (self.project.clone(), row.clone());
        let id = row.id.clone();
        let background = cx.background_executor().clone();
        let load = cx.spawn_in(window, async move |this, cx| {
            let load = background
                .spawn(async move { shelbi_app::jobs::open_sheet(&project, &load_row) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| view.sheet_loaded(&id, load, window, cx));
        });
        self.sheet = Some(JobSheet {
            id: row.id.clone(),
            opened: row,
            body: SheetBody::Loading,
            canceling: false,
            cancel_error: None,
            _load: Some(load),
        });
        cx.notify();
    }

    fn sheet_loaded(&mut self, id: &str, load: SheetLoad, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.sheet.as_mut().filter(|s| s.id == id) else {
            return;
        };
        let name = shelbi_state::awareness::job_session_name(&self.project, id);
        sheet.body = match load {
            SheetLoad::Live(c) => {
                let size = shelbi_term::Size::new(c.size.cols, c.size.rows);
                match term_session::wire(c.conn, c.events, size, name) {
                    Ok(connected) => SheetBody::Terminal(cx.new(|cx| TerminalPane::watching(connected, window, cx))),
                    Err(e) => SheetBody::Message(e),
                }
            }
            SheetLoad::Saved(bytes) => SheetBody::Terminal(cx.new(|cx| TerminalPane::replay(name, bytes, window, cx))),
            SheetLoad::Message(m) => SheetBody::Message(m),
        };
        sheet._load = None;
        cx.notify();
    }

    /// Close the sheet (✕, Escape): it slides back out, inert.
    fn close_sheet(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.sheet.take() else {
            return;
        };
        // A drag of its edge goes with it.
        self.pane_frame.cancel_resize();
        self.closing.start(sheet, |this: &mut Self| &mut this.closing, |_, _| {}, cx);
        self.needs_focus = true;
        cx.notify();
    }

    /// The open sheet's row, current from the model.
    fn sheet_row(&self) -> Option<&JobRow> {
        self.row_of(self.sheet.as_ref()?)
    }

    /// `sheet`'s row, current from the model.
    fn row_of<'a>(&'a self, sheet: &'a JobSheet) -> Option<&'a JobRow> {
        Some(self.model.rows().iter().find(|r| r.id == sheet.id).unwrap_or(&sheet.opened))
    }

    /// `shelbi job cancel` for the open sheet's job, off the UI thread: its
    /// row moves under Finished on the next read.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        let running = self.sheet_row().is_some_and(JobRow::can_cancel);
        let Some(sheet) = self.sheet.as_mut().filter(|s| running && !s.canceling) else {
            return;
        };
        sheet.canceling = true;
        sheet.cancel_error = None;
        let (source, id) = (self.source.clone(), sheet.id.clone());
        let background = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let done = background.spawn(async move { source.cancel_job(&id).map(|()| id) }).await;
            let _ = this.update(cx, |view, cx| {
                if let (Err(e), Some(sheet)) = (done, view.sheet.as_mut()) {
                    sheet.canceling = false;
                    sheet.cancel_error = Some(e);
                }
                // Read now, so the row moves without waiting out the cadence.
                view.refresher.request();
                view.last_refresh = Instant::now();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The sheet, while one is open or sliding out: the floating pane with
    /// its Header (`id · label`, ✕), the job's properties, the terminal,
    /// and Cancel while the job runs.
    fn render_sheet(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (sheet, motion) = match (&self.sheet, self.closing.get()) {
            (Some(sheet), _) => (sheet, PaneMotion::In),
            (None, Some(sheet)) => (sheet, PaneMotion::Out),
            (None, None) => return None,
        };
        let row = self.row_of(sheet)?.clone();
        // The pane floats over the table, so it paints from the opaque
        // theme, as the issue pane does.
        let theme = &theme.opaque();
        let now = super::activity::now();
        let close = header_button("job-sheet-close", icons::X_MARK, false, theme)
            .tooltip(style::tooltip("Close (Esc)"))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_sheet(cx)));
        let title = format!("{} · {}", row.id, one_line(&row.description));
        let header = floating_pane::header("job-sheet-title", title, vec![close.into_any_element()], theme);
        let terminal = match &sheet.body {
            SheetBody::Loading => message_body("Loading…", theme),
            SheetBody::Message(m) => message_body(m, theme),
            SheetBody::Terminal(pane) => div()
                .debug_selector(|| "job-sheet-terminal".into())
                .flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(pane.clone())
                .into_any_element(),
        };
        // The terminal in its well: the terminal's own surface, framed.
        let well = div()
            .debug_selector(|| "job-sheet-well".into())
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .mx(px(INSET))
            .overflow_hidden()
            .rounded(px(WELL_RADIUS))
            .border_1()
            .border_color(theme.divider)
            .bg(theme.terminal_canvas())
            // The grid clear of the frame.
            .p(px(WELL_PAD))
            .child(terminal);
        let footer = row.can_cancel().then(|| {
            list_screen::action(
                "job",
                CANCEL_LABEL,
                sheet.canceling,
                sheet.cancel_error.clone(),
                cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx)),
                theme,
            )
        });
        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .pb(px(INSET))
            .child(sheet_properties(&row, sheet.canceling, now, theme))
            .child(well)
            .children(footer)
            .into_any_element();
        Some(floating_pane::render(&self.pane_frame, IDS, motion, header, body, |pane, _| pane, theme, cx))
    }

    fn render_body(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.model == JobsModel::Empty {
            return list_screen::empty_state("jobs-empty", icons::QUEUE_LIST, EMPTY_TITLE, EMPTY_DETAIL, EMPTY_COMMAND, theme);
        }
        if let Some(message) = self.model.message() {
            return list_screen::message("jobs-empty", message, theme);
        }
        let now = super::activity::now();
        let open = |row: &JobRow| self.sheet.as_ref().is_some_and(|s| s.id == row.id);
        let (running, finished) = (self.model.running(), self.model.finished());
        let mut column = list_screen::content();
        if !running.is_empty() {
            column = column.child(section_heading("jobs-running-heading", RUNNING_HEADING.to_string(), Some(running.len()), false, theme));
            for row in running {
                column = column.child(running_card(row, now, open(row), theme, cx));
            }
        }
        let days = self.model.finished_days(now, &chrono::Local);
        for (n, (start, label)) in days.iter().enumerate() {
            let end = days.get(n + 1).map_or(finished.len(), |(next, _)| *next);
            let spaced = n > 0 || !running.is_empty();
            column = column.child(section_heading(format!("jobs-day-{n}-heading"), label.clone(), None, spaced, theme));
            let rows = finished[*start..end].iter().map(|row| history_row(row, now, open(row), theme, cx));
            column = column.child(list_screen::group_card(format!("jobs-day-{n}"), rows, theme));
        }
        list_screen::scrolling("jobs", &self.scroll, column)
    }
}

/// A debug selector `job-<part>-<id>`.
fn selector(part: &'static str, id: &str) -> impl FnOnce() -> String {
    let s = format!("job-{part}-{id}");
    move || s
}

/// A small chip on a card: X-Small Medium on a tint, 4pt corners.
fn chip(text: String, fill: Hsla, color: Hsla) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .h(px(20.0))
        .px(px(6.0))
        .rounded(px(4.0))
        .bg(fill)
        .type_style(Type::XSmallMedium)
        .text_color(color)
        .whitespace_nowrap()
        .child(text)
}

/// The kind chip (`Command`, `PR #2265`, `CI d482e3f`, `PID 4242`).
fn kind_chip(row: &JobRow, theme: &Theme) -> Div {
    chip(row.kind_label(), theme.fill_on(theme.background, theme.card), theme.foreground)
        .debug_selector(selector("kind", &row.id))
}

/// Who started it: a person for a worker's workspace, the robot for the
/// orchestrator, its full name (agent, task) on hover.
fn started_by_chip(row: &JobRow, theme: &Theme) -> impl IntoElement {
    let icon = if row.started_by.is_some() {
        heroicon(icons::USER_SMALL)
    } else {
        heroicon(icons::ROBOT).size(px(14.0))
    };
    div()
        .id(SharedString::from(format!("job-by-{}", row.id)))
        .debug_selector(selector("by", &row.id))
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .text_color(theme.muted)
        .child(icon.text_color(theme.icon))
        .child(row.started_by_label())
        .tooltip(style::tooltip(format!("Started by {}", row.started_by_detail())))
}

/// Monospace text at the cards' code size.
fn mono(d: Div) -> Div {
    d.font_family(PRIMARY_FAMILY).text_size(px(MONO_SIZE)).line_height(px(MONO_LINE))
}

/// A running job's card: the live spinner, the label, the status chip and
/// elapsed against the estimate; the estimate bar; the kind, who started
/// it, the command and when; and what it is doing now.
fn running_card(
    row: &JobRow,
    now: chrono::DateTime<chrono::Utc>,
    open: bool,
    theme: &Theme,
    cx: &mut Context<JobsView>,
) -> AnyElement {
    let overdue = row.overdue(now);
    let accent = if overdue { theme.warning } else { theme.progress };
    let label = one_line(&row.description);
    let long_label = label.chars().count() > TIP_CHARS;
    let status_tone = row.status_tone_at(now);
    let duration_color = row.duration_tone(now).map_or(theme.foreground, |t| theme.tone(t));
    let click_id = row.id.clone();

    let title = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .min_w_0()
        .child(
            div()
                .debug_selector(selector("live", &row.id))
                .flex_none()
                .child(spinner_sized(SharedString::from(format!("job-live-{}", row.id)), accent, LIVE_GLYPH)),
        )
        .child(
            div()
                .id(SharedString::from(format!("job-description-{}", row.id)))
                .flex_1()
                .min_w_0()
                .truncate()
                .type_style(Type::BaseMedium)
                .text_color(theme.text_loud)
                .when(long_label, |d| d.tooltip(style::tooltip(row.description.clone())))
                .child(label),
        )
        // The spinner says it is running; only an overdue job gets a word.
        .when(overdue, |d| {
            d.child(
                chip(row.status_label_at(now), tint(status_tone, theme), theme.tone(status_tone))
                    .debug_selector(selector("status", &row.id)),
            )
        })
        .child(
            div()
                .debug_selector(selector("duration", &row.id))
                .flex_none()
                .type_style(Type::Small)
                .text_color(duration_color)
                .child(row.duration(now)),
        );

    let bar = row.bar(now).map(|bar| {
        let fill = estimate_bar::fill_color(&bar, theme);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .debug_selector(selector("bar", &row.id))
                    .flex_1()
                    .h(px(CARD_BAR_H))
                    .rounded_full()
                    .bg(theme.search)
                    .child(div().h_full().w(relative(bar.fraction())).rounded_full().bg(fill)),
            )
            .child(
                div()
                    .debug_selector(selector("bar-label", &row.id))
                    .flex_none()
                    .w(px(CARD_BAR_LABEL_W))
                    .text_right()
                    .type_style(Type::XSmall)
                    .text_color(if overdue { theme.warning } else { theme.muted })
                    .child(row.bar_label(now).unwrap_or_default()),
            )
    });

    let command = (row.kind == JobKind::Command).then(|| {
        mono(div())
            .id(SharedString::from(format!("job-command-{}", row.id)))
            .debug_selector(selector("command", &row.id))
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(theme.muted)
            .tooltip(style::tooltip(row.command.clone()))
            .child(one_line(&row.command))
    });
    let meta = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .type_style(Type::XSmall)
        .text_color(theme.muted)
        .child(kind_chip(row, theme))
        .child(started_by_chip(row, theme))
        .map(|d| match command {
            Some(c) => d.child(c),
            None => d.child(div().flex_1()),
        })
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .child(format!("started {}", clock_label(row.started_at, now, &chrono::Local))),
        );

    let activity = row.activity_line();
    let is_output = row.activity_is_output();
    let long_activity = activity.chars().count() > TIP_CHARS * 2;
    let well = div()
        .id(SharedString::from(format!("job-activity-{}", row.id)))
        .debug_selector(selector("activity", &row.id))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .h(px(30.0))
        .px(px(10.0))
        .rounded(px(6.0))
        .bg(theme.fill_on(theme.background, theme.card))
        .when(long_activity, |d| d.tooltip(style::tooltip(activity.clone())))
        .child(if is_output {
            mono(div()).flex_none().text_color(theme.icon).child("\u{203A}")
        } else {
            div().flex_none().child(heroicon(icons::CLOCK).size(px(14.0)).text_color(theme.icon))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .map(|d| if is_output { mono(d) } else { d.type_style(Type::XSmall) })
                .text_color(if is_output { theme.foreground } else { theme.muted })
                .child(activity),
        );

    div()
        .id(SharedString::from(format!("job-row-{}", row.id)))
        .debug_selector(selector("row", &row.id))
        .relative()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .min_w_0()
        .p(px(CARD_PAD))
        .rounded(px(CARD_RADIUS))
        .bg(theme.card)
        // The open job's card wears the focus ring, as the board's selected
        // card does.
        .shadow(if open && theme.translucent() {
            style::focus_shadow_inset(theme)
        } else if open {
            style::focus_shadow(theme)
        } else {
            style::card_shadow(theme)
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_sheet(&click_id, window, cx)))
        .child(title)
        .children(bar)
        .child(meta)
        .child(well)
        .into_any_element()
}

/// A finished job's status mark: a tinted disc with a glyph. A success is
/// a green check, a failure a red ✕, a cancel a yellow stop; a watch that
/// ended without a verdict (exited, merged, closed) is neutral.
fn status_mark(row: &JobRow, theme: &Theme) -> AnyElement {
    let status = match row.state {
        shelbi_app::jobs::JobState::Finished { status, .. } => status,
        shelbi_app::jobs::JobState::Running => JobStatus::Exited,
    };
    let small = |icon| heroicon(icon).size(px(12.0));
    let (fill, mark): (Hsla, AnyElement) = match status {
        JobStatus::Ok => (tint(Tone::Green, theme), glyph(glyphs::CHECK).text_color(theme.green).into_any_element()),
        JobStatus::Failed => (tint(Tone::Red, theme), small(icons::X_MARK_MENU).text_color(theme.red).into_any_element()),
        JobStatus::Canceled => (tint(Tone::Yellow, theme), small(icons::MINUS).text_color(theme.yellow).into_any_element()),
        JobStatus::Merged => (tint(Tone::Gray, theme), small(icons::PULL_REQUEST).text_color(theme.foreground).into_any_element()),
        JobStatus::Closed => (tint(Tone::Gray, theme), small(icons::X_MARK_MENU).text_color(theme.muted).into_any_element()),
        JobStatus::Exited => (tint(Tone::Gray, theme), glyph(glyphs::DOT).text_color(theme.muted).into_any_element()),
    };
    div()
        .id(SharedString::from(format!("job-status-{}", row.id)))
        .debug_selector(selector("status", &row.id))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(MARK))
        .rounded_full()
        .bg(fill)
        .child(mark)
        .tooltip(style::tooltip(row.status_label()))
        .into_any_element()
}

/// One finished job's row: its mark, label, command (or, failed, why in
/// red), kind, how long it ran and how long ago it finished.
fn history_row(
    row: &JobRow,
    now: chrono::DateTime<chrono::Utc>,
    open: bool,
    theme: &Theme,
    cx: &mut Context<JobsView>,
) -> AnyElement {
    let label = one_line(&row.description);
    let long_label = label.chars().count() > TIP_CHARS;
    let click_id = row.id.clone();
    let duration_color = row.duration_tone(now).map_or(theme.muted, |t| theme.tone(t));
    let detail = match row.failure_detail() {
        Some(why) => div()
            .id(SharedString::from(format!("job-failure-{}", row.id)))
            .debug_selector(selector("failure", &row.id))
            .flex_1()
            .min_w_0()
            .truncate()
            .type_style(Type::Small)
            .text_color(theme.red)
            .when(why.chars().count() > TIP_CHARS, |d| d.tooltip(style::tooltip(why.clone())))
            .child(why),
        None if row.kind == JobKind::Command => mono(div())
            .id(SharedString::from(format!("job-command-{}", row.id)))
            .debug_selector(selector("command", &row.id))
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(theme.muted)
            .tooltip(style::tooltip(row.command.clone()))
            .child(one_line(&row.command)),
        None => div().id(SharedString::from(format!("job-detail-{}", row.id))).flex_1(),
    };
    list_screen::compact_row(format!("job-row-{}", row.id), open, theme)
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_sheet(&click_id, window, cx)))
        .child(status_mark(row, theme))
        .child(
            div()
                .id(SharedString::from(format!("job-description-{}", row.id)))
                // Up to 40% of the row, and shrinking before the trailing
                // columns do in a narrow window.
                .flex_initial()
                .min_w_0()
                .max_w(relative(0.4))
                .truncate()
                .type_style(Type::Base)
                .text_color(theme.text_loud)
                .when(long_label, |d| d.tooltip(style::tooltip(row.description.clone())))
                .child(label),
        )
        .child(detail)
        // A watch's kind (`PR #2251`); a command's monospace line already
        // says what it is.
        .child(
            div()
                .flex_none()
                .w(px(KIND_W))
                .flex()
                .flex_row()
                .justify_end()
                .when(row.kind != JobKind::Command, |d| d.child(kind_chip(row, theme))),
        )
        .child(
            div()
                .debug_selector(selector("duration", &row.id))
                .flex_none()
                .w(px(HISTORY_DURATION_W))
                .text_right()
                .truncate()
                .type_style(Type::Small)
                .text_color(duration_color)
                .child(row.duration(now)),
        )
        .child(
            div()
                .id(SharedString::from(format!("job-ago-{}", row.id)))
                .debug_selector(selector("ago", &row.id))
                .flex_none()
                .w(px(AGO_W))
                .text_right()
                .whitespace_nowrap()
                .type_style(Type::Small)
                .text_color(theme.muted)
                .tooltip(style::tooltip(format!("Finished {}", row.finished(now))))
                .child(row.finished_ago(now)),
        )
        .into_any_element()
}

impl Render for JobsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_focus {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        let theme = *cx.global::<Theme>();
        let table = self.render_body(&theme, cx);
        let sheet = self.render_sheet(&theme, cx);
        div()
            .id("jobs")
            .debug_selector(|| "jobs".into())
            .track_focus(&self.focus)
            .key_context("Jobs")
            // Escape closes the sheet, wherever focus is in the view: ahead
            // of the terminal, which would otherwise take the key.
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && this.sheet.is_some() {
                    this.close_sheet(cx);
                    cx.stop_propagation();
                }
            }))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.nested(theme.canvas))
            .type_style(Type::Base)
            .text_color(theme.foreground)
            // The table at its full width, whether or not the sheet is open:
            // the sheet floats over it.
            .child(div().flex().flex_col().flex_1().min_h_0().min_w_0().child(table))
            .children(sheet)
    }
}

/// The sheet's properties, at the content's inset: Status (its glyph, in
/// its tone), Duration (against the estimate, with its bar while the job
/// runs against one) and Command (cut to one line, whole in a tooltip).
fn sheet_properties(row: &JobRow, canceling: bool, now: chrono::DateTime<chrono::Utc>, theme: &Theme) -> AnyElement {
    let status = if canceling && row.is_running() {
        "● Canceling…".to_string()
    } else {
        row.status_glyph_label(now)
    };
    let duration_color = row.duration_tone(now).map_or(theme.foreground, |t| theme.tone(t));
    let bar = row.bar(now).map(|bar| {
        let fill = estimate_bar::fill_color(&bar, theme);
        div()
            .debug_selector(|| "job-sheet-bar".into())
            .flex_none()
            .w(px(BAR_W))
            .h(px(BAR_THICK))
            .rounded_full()
            .bg(theme.search)
            .child(div().h_full().w(relative(bar.fraction())).rounded_full().bg(fill))
    });
    let command = row.command.clone();
    div()
        .debug_selector(|| "job-sheet-properties".into())
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(6.0))
        .w_full()
        .min_w_0()
        .p(px(INSET))
        .child(property_row(
            theme,
            "Status",
            value_text(theme)
                .debug_selector(|| "job-sheet-status".into())
                .when(row.failed() || row.overdue(now), |d| d.type_style(Type::SmallMedium))
                .text_color(theme.tone(row.status_tone_at(now)))
                .child(status),
        ))
        .child(property_row(
            theme,
            "Duration",
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .min_w_0()
                .child(
                    value_text(theme)
                        .debug_selector(|| "job-sheet-duration".into())
                        .text_color(duration_color)
                        .child(row.duration(now)),
                )
                .children(bar),
        ))
        .child(property_row(
            theme,
            "Started",
            value_text(theme).debug_selector(|| "job-sheet-started".into()).child(row.started(now)),
        ))
        .child(property_row(
            theme,
            "Started by",
            value_text(theme).debug_selector(|| "job-sheet-started-by".into()).child(row.started_by_detail()),
        ))
        .child(property_row(
            theme,
            "Command",
            value_text(theme)
                .id("job-sheet-command")
                .debug_selector(|| "job-sheet-command".into())
                .font_family(PRIMARY_FAMILY)
                .text_size(px(12.0))
                .line_height(px(18.0))
                .tooltip(style::tooltip(command))
                .child(one_line(&row.command)),
        ))
        .into_any_element()
}

/// A line in place of the sheet's terminal.
fn message_body(text: &str, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(|| "job-sheet-message".into())
        .flex_1()
        .p(px(4.0))
        .type_style(Type::Small)
        .text_color(theme.muted)
        .child(text.to_string())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::column_swap::SLIDE;
    use crate::views::floating_pane::EDGE;
    use crate::views::pane_width::DEFAULT_W;
    use chrono::{DateTime, Utc};
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use shelbi_app::jobs::{EMPTY_TEXT, REMOTE_TEXT};
    use shelbi_app::source::ProjectKey;
    use shelbi_state::awareness::{record_finished_jobs, update_awareness, Job, JobKind, JobRecord, JobStatus};

    const PROJECT: &str = "jobs";
    const LONG: &str = "zsh -c 'cd /Users/me/Workspaces/shelbi/desktop && cargo clippy --all-targets -- -D warnings && cargo test'";

    fn job(id: &str, kind: JobKind, target: &str, label: Option<&str>, started_at: DateTime<Utc>) -> Job {
        Job { id: id.into(), kind, target: target.into(), label: label.map(str::to_string), started_at, runner_pid: None, estimate: None, next_alert_at: None, started_by: None }
    }

    fn set_jobs(jobs: Vec<Job>) {
        update_awareness(PROJECT, |store| {
            store.jobs = jobs;
            Ok(())
        })
        .unwrap();
    }

    fn open(cx: &mut TestAppContext, handle: ProjectHandle) -> (Entity<JobsView>, VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                        gpui::point(px(0.0), px(0.0)),
                        gpui::size(px(1000.0), px(600.0)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| JobsView::new(&handle, cx)),
            )
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut vcx).unwrap();
        (view, vcx)
    }

    /// Ask for a read and wait for it to land and draw.
    fn read_until(view: &Entity<JobsView>, cx: &mut VisualTestContext, done: impl Fn(&JobsModel) -> bool) -> bool {
        view.update(cx, |v, _| v.refresh());
        for _ in 0..200 {
            view.update(cx, |v, cx| v.poll(cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            if view.read_with(cx, |v, _| done(v.model())) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[gpui::test]
    fn the_table_lists_jobs_and_follows_the_store(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs", PROJECT, crate::test_env::lock());
        let now = Utc::now();
        set_jobs(vec![
            job("j1", JobKind::Pr, "2265", None, now - chrono::Duration::minutes(72)),
            job("j2", JobKind::Command, LONG, Some("desktop checks"), now - chrono::Duration::minutes(4)),
        ]);
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 2), "the first read lands");
        let ids: Vec<String> = view.read_with(&vcx, |v, _| v.model().rows().iter().map(|r| r.id.clone()).collect());
        assert_eq!(ids, ["j2", "j1"], "newest first");
        assert!(vcx.debug_bounds("jobs-list").is_some());
        assert!(vcx.debug_bounds("jobs-running-heading").is_some());
        assert!(vcx.debug_bounds("job-row-j2").is_some() && vcx.debug_bounds("job-row-j1").is_some());

        // The long command is cut to its cell: the card never outgrows the
        // window, and hovering the cell shows the whole command.
        let row = vcx.debug_bounds("job-row-j2").unwrap();
        let cell = vcx.debug_bounds("job-command-j2").unwrap();
        assert!(cell.right() <= row.right(), "the command cell stays in its row");
        assert!(row.right() <= px(1000.0), "no sideways scroll");
        assert_eq!(row.size.height, vcx.debug_bounds("job-row-j1").unwrap().size.height, "one line, cut");
        assert!(vcx.debug_bounds("job-bar-j2").is_none(), "no estimate, no bar");
        assert!(vcx.debug_bounds("tooltip").is_none());
        vcx.simulate_mouse_move(cell.center(), None, gpui::Modifiers::none());
        vcx.executor().advance_clock(Duration::from_millis(600));
        vcx.run_until_parked();
        let tip = vcx.debug_bounds("tooltip").expect("hovering the command shows its tooltip");
        assert!(tip.size.height > cell.size.height, "the whole command, wrapped: {tip:?}");

        // A job started outside the screen appears; one that finishes goes.
        set_jobs(vec![
            job("j1", JobKind::Pr, "2265", None, now - chrono::Duration::minutes(72)),
            job("j3", JobKind::Command, "sleep 60", Some("x"), Utc::now()),
        ]);
        assert!(
            read_until(&view, &mut vcx, |m| m.rows().first().is_some_and(|r| r.id == "j3") && m.rows().len() == 2),
            "the new job appears, the finished one goes"
        );
        assert!(vcx.debug_bounds("job-row-j2").is_none());
        set_jobs(vec![]);
        assert!(read_until(&view, &mut vcx, |m| *m == JobsModel::Empty), "every job finished");
        assert!(vcx.debug_bounds("jobs-empty").is_some());
        assert!(vcx.debug_bounds("jobs-list").is_none());
        assert!(vcx.debug_bounds("jobs-empty-command").is_some(), "the empty state offers the command");
        assert_eq!(view.read_with(&vcx, |v, _| v.model().message()).as_deref(), Some(EMPTY_TEXT));
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn finished_jobs_sit_under_a_day_heading_after_the_running_ones(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-history", PROJECT, crate::test_env::lock());
        let now = Utc::now();
        let ago = |m: i64| now - chrono::Duration::minutes(m);
        set_jobs(vec![job("j9", JobKind::Command, "sleep 600", Some("bundle"), ago(3))]);
        let finished = |id: &str, status, exit, m: i64| JobRecord {
            exit_code: exit,
            ..JobRecord::finished(&job(id, JobKind::Command, "cargo test", Some(id), ago(m + 2)), status, ago(m))
        };
        record_finished_jobs(
            PROJECT,
            vec![finished("j7", JobStatus::Failed, Some(3), 30), finished("j8", JobStatus::Ok, Some(0), 10)],
            now,
        )
        .unwrap();
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 3), "the first read lands");
        let ids: Vec<String> = view.read_with(&vcx, |v, _| v.model().rows().iter().map(|r| r.id.clone()).collect());
        assert_eq!(ids, ["j9", "j8", "j7"], "running, then newest finished first");
        let mut tops = Vec::new();
        for sel in ["jobs-running-heading", "job-row-j9", "jobs-day-0-heading", "job-row-j8", "job-row-j7"] {
            tops.push(vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel} drawn")).top());
        }
        assert!(tops.windows(2).all(|w| w[0] < w[1]), "running, the day's heading, then finished: {tops:?}");
        // The history is compact: one short line a job, every one alike.
        let (j8, j7) = (vcx.debug_bounds("job-row-j8").unwrap(), vcx.debug_bounds("job-row-j7").unwrap());
        assert_eq!((j8.size.height, j7.size.height), (px(list_screen::COMPACT_ROW_H), px(list_screen::COMPACT_ROW_H)));
        assert!(j8.size.height < vcx.debug_bounds("job-row-j9").unwrap().size.height / 2.0, "the running card is the hero");
        // Its columns line up across rows.
        for part in ["status", "duration", "ago"] {
            let mut at = |id: &str| vcx.debug_bounds(Box::leak(format!("job-{part}-{id}").into_boxed_str())).unwrap();
            let (a, b) = (at("j8"), at("j7"));
            assert_eq!(a.right(), b.right(), "{part} lines up");
        }
        // The failure reads inline, in red; a success shows its command.
        assert!(vcx.debug_bounds("job-failure-j7").is_some());
        assert!(vcx.debug_bounds("job-failure-j8").is_none() && vcx.debug_bounds("job-command-j8").is_some());
        // The counts the toolbar shows.
        assert_eq!(view.read_with(&vcx, |v, _| v.summary()).as_deref(), Some("1 running · 2 finished"));
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        let failed = &model.rows()[2];
        assert_eq!(failed.status_label(), "Failed · exit 3");
        assert_eq!(failed.failure_detail().as_deref(), Some("exit 3"));
        assert_eq!(failed.finished_ago(Utc::now()), "30m ago");
        let theme = vcx.update(|_, cx| *cx.global::<Theme>());
        assert_eq!(theme.tone(failed.status_tone()), theme.red, "a failure reads red");
        assert!(vcx.debug_bounds("job-status-j7").is_some());
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_running_job_with_an_estimate_draws_a_bar_that_turns_overdue(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-estimate", PROJECT, crate::test_env::lock());
        let now = Utc::now();
        let ago = |m: i64| now - chrono::Duration::minutes(m);
        let estimated = |id: &str, started, mins: u64| {
            let mut j = job(id, JobKind::Command, "cargo test", Some(id), started);
            j.set_estimate(Duration::from_secs(mins * 60));
            j
        };
        // A quarter of the way in, past it, and no estimate at all.
        set_jobs(vec![estimated("j1", ago(10), 40), estimated("j2", ago(45), 30), job("j3", JobKind::Pid, "1", None, ago(1))]);
        let done = JobRecord {
            exit_code: Some(0),
            ..JobRecord::finished(&estimated("j0", ago(80), 10), JobStatus::Ok, ago(60))
        };
        record_finished_jobs(PROJECT, vec![done], now).unwrap();
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 4), "the first read lands");

        let row = vcx.debug_bounds("job-row-j1").unwrap();
        let bar = vcx.debug_bounds("job-bar-j1").expect("an estimated running job draws a bar");
        // A thick rounded bar inside the card, under its title.
        assert_eq!(bar.size.height, px(CARD_BAR_H), "{bar:?}");
        assert!(bar.left() > row.left() && bar.right() < row.right(), "inside the card: {bar:?} in {row:?}");
        let title = vcx.debug_bounds("job-duration-j1").unwrap();
        assert!(bar.top() > title.bottom(), "under the title: {bar:?}");
        assert!(bar.size.width > row.size.width / 2.0, "most of the card's width: {bar:?}");
        assert!(vcx.debug_bounds("job-bar-label-j1").is_some());
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        let quarter = model.rows().iter().find(|r| r.id == "j1").unwrap().bar(Utc::now()).unwrap();
        assert!((quarter.fraction() - 0.25).abs() < 0.02, "a quarter full: {}", quarter.fraction());

        // Past its estimate: full (warning-colored, see estimate_bar), and
        // the Status says so.
        assert!(vcx.debug_bounds("job-bar-j2").is_some());
        let late = model.rows().iter().find(|r| r.id == "j2").unwrap().clone();
        assert_eq!(late.status_label_at(Utc::now()), "Overdue");
        let over = late.bar(Utc::now()).unwrap();
        assert!(over.overdue() && over.fraction() == 1.0);
        assert_eq!(late.bar_label(Utc::now()).as_deref(), Some("15m over"));
        assert!(vcx.debug_bounds("job-status-j2").is_some(), "the Overdue chip");
        assert!(vcx.debug_bounds("job-status-j1").is_none(), "on time: the spinner says it all");
        assert!(late.duration(Utc::now()).ends_with("/ 30m"));

        // No estimate, or finished: no bar; the finished one keeps its text.
        assert!(vcx.debug_bounds("job-bar-j3").is_none());
        assert!(vcx.debug_bounds("job-bar-j0").is_none());
        let finished = model.rows().iter().find(|r| r.id == "j0").unwrap();
        assert_eq!(finished.duration(Utc::now()), "20m / 10m");
        assert!(vcx.debug_bounds("job-duration-j0").is_some());
        let row = vcx.debug_bounds("job-row-j2").unwrap();
        assert!(row.right() <= px(1000.0), "no sideways scroll");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_running_card_shows_its_last_output_line_kind_and_starter(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-card", PROJECT, crate::test_env::lock());
        let now = Utc::now();
        let mut built = job("j1", JobKind::Command, "cargo build", Some("bundle"), now - chrono::Duration::minutes(2));
        built.started_by = Some(shelbi_state::awareness::JobStarter {
            workspace: "alpha".into(),
            agent: Some("developer".into()),
            task: None,
        });
        set_jobs(vec![built, job("j2", JobKind::Pr, "2265", None, now - chrono::Duration::minutes(5))]);
        write_log("j1", "\u{1b}[32m   Compiling\u{1b}[0m shelbi v0.42.0\nBundling Shelbi.app\n");
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(
            read_until(&view, &mut vcx, |m| m.rows().iter().any(|r| r.last_line.is_some())),
            "the first read lands, with the job's output"
        );
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        // The command started 2m ago, the watch 5m ago: newest first.
        let (cmd, pr) = (&model.rows()[0], &model.rows()[1]);
        assert_eq!((cmd.id.as_str(), pr.id.as_str()), ("j1", "j2"));
        assert_eq!((cmd.activity_line().as_str(), cmd.activity_is_output()), ("Bundling Shelbi.app", true));
        assert_eq!((cmd.kind_label().as_str(), cmd.started_by_label().as_str()), ("Command", "alpha"));
        assert_eq!(pr.activity_line(), "Waiting for PR #2265 to merge or close");
        assert_eq!((pr.kind_label().as_str(), pr.started_by_label().as_str()), ("PR #2265", "orchestrator"));
        for part in ["live", "duration", "kind", "by", "activity"] {
            for id in ["j1", "j2"] {
                let sel: &'static str = Box::leak(format!("job-{part}-{id}").into_boxed_str());
                let b = vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel} drawn"));
                let card = vcx.debug_bounds(Box::leak(format!("job-row-{id}").into_boxed_str())).unwrap();
                assert!(b.left() >= card.left() && b.right() <= card.right(), "{sel} inside its card");
            }
        }
        // A watch has no command line to show; the command job does.
        assert!(vcx.debug_bounds("job-command-j1").is_some() && vcx.debug_bounds("job-command-j2").is_none());
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_narrow_window_cuts_every_long_line_and_never_scrolls_sideways(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-narrow", PROJECT, crate::test_env::lock());
        let now = Utc::now();
        let label = "a very long label that names everything this job does and then some more words";
        set_jobs(vec![job("j1", JobKind::Command, LONG, Some(label), now - chrono::Duration::minutes(2))]);
        write_log("j1", &format!("{}\n", "x".repeat(300)));
        let mut failed = JobRecord::finished(
            &job("j0", JobKind::Command, LONG, Some(label), now - chrono::Duration::minutes(9)),
            JobStatus::Failed,
            now - chrono::Duration::minutes(8),
        );
        failed.exit_code = Some(1);
        failed.detail = Some("e".repeat(300));
        record_finished_jobs(PROJECT, vec![failed], now).unwrap();
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        vcx.simulate_resize(gpui::size(px(520.0), px(600.0)));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 2 && m.rows()[0].last_line.is_some()));
        let root = vcx.debug_bounds("jobs").unwrap();
        for sel in ["job-row-j1", "job-activity-j1", "job-command-j1", "job-row-j0", "job-failure-j0", "job-ago-j0"] {
            let b = vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel} drawn"));
            assert!(b.right() <= root.right(), "{sel} stays in the window: {b:?} in {root:?}");
        }
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_remote_window_says_jobs_are_not_available(cx: &mut TestAppContext) {
        let handle = ProjectHandle::null(ProjectKey::remote("devbox", "web"));
        let (view, mut vcx) = open(cx, handle);
        assert!(read_until(&view, &mut vcx, |m| *m == JobsModel::Unavailable));
        assert!(vcx.debug_bounds("jobs-empty").is_some());
        assert_eq!(view.read_with(&vcx, |v, _| v.model().message()).as_deref(), Some(REMOTE_TEXT));
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    fn write_log(id: &str, text: &str) {
        let path = shelbi_state::awareness::job_output_path(PROJECT, id).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn click(vcx: &mut VisualTestContext, selector: &'static str) {
        let at = vcx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
        vcx.simulate_click(at, gpui::Modifiers::none());
        vcx.run_until_parked();
    }

    /// Let the sheet's slide in finish (it runs on the executor's clock).
    fn slid_in(vcx: &mut VisualTestContext) {
        vcx.executor().advance_clock(SLIDE);
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }

    /// Draw until the sheet's terminal shows `want`.
    fn sheet_shows(view: &Entity<JobsView>, cx: &mut VisualTestContext, want: &str) -> bool {
        for _ in 0..300 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            let text = cx.update(|_, cx| view.read(cx).sheet_text(cx));
            if text.and_then(|(_, t)| t).is_some_and(|t| t.contains(want)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[gpui::test]
    fn clicking_a_finished_job_opens_its_saved_output_and_escape_or_x_closes_it(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-sheet", PROJECT, crate::test_env::lock());
        let now = Utc::now();
        let record = JobRecord {
            exit_code: Some(3),
            ..JobRecord::finished(
                &job("j4", JobKind::Command, "cargo test", Some("desktop tests"), now - chrono::Duration::minutes(3)),
                JobStatus::Failed,
                now - chrono::Duration::minutes(1),
            )
        };
        record_finished_jobs(PROJECT, vec![record], now).unwrap();
        write_log("j4", "running 3 tests\ntest result: FAILED\n");
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 1));
        assert!(vcx.debug_bounds("job-sheet").is_none());

        click(&mut vcx, "job-row-j4");
        assert!(sheet_shows(&view, &mut vcx, "test result: FAILED"), "the saved output shows");
        assert!(vcx.debug_bounds("job-sheet").is_some(), "the sheet is drawn");
        assert!(vcx.debug_bounds("job-sheet-title").is_some() && vcx.debug_bounds("job-sheet-status").is_some());
        assert!(vcx.debug_bounds("job-sheet-cancel").is_none(), "a finished job has no Cancel");

        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert!(view.read_with(&vcx, |v, cx| v.sheet_text(cx)).is_none(), "Escape closes the sheet");
        // It slides out, then goes.
        assert!(view.read_with(&vcx, |v, _| v.sheet_closing()));
        vcx.executor().advance_clock(SLIDE);
        vcx.run_until_parked();
        assert!(!view.read_with(&vcx, |v, _| v.sheet_closing()));
        assert!(vcx.debug_bounds("job-sheet-closing").is_none());

        click(&mut vcx, "job-row-j4");
        assert!(sheet_shows(&view, &mut vcx, "running 3 tests"));
        slid_in(&mut vcx);
        click(&mut vcx, "job-sheet-close");
        assert!(view.read_with(&vcx, |v, cx| v.sheet_text(cx)).is_none(), "✕ closes the sheet");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    fn two_finished_jobs() {
        let now = Utc::now();
        let record = |id: &str, label: &str, mins: i64| JobRecord {
            exit_code: Some(0),
            ..JobRecord::finished(
                &job(id, JobKind::Command, "cargo test", Some(label), now - chrono::Duration::minutes(mins + 2)),
                JobStatus::Ok,
                now - chrono::Duration::minutes(mins),
            )
        };
        record_finished_jobs(PROJECT, vec![record("j1", "site build", 9), record("j2", "desktop tests", 3)], now).unwrap();
        write_log("j1", "built the site\n");
        write_log("j2", "running 12 tests\n");
    }

    /// Click `selector`'s left end: what the floating sheet leaves uncovered.
    fn click_left(vcx: &mut VisualTestContext, selector: &'static str) {
        let b = vcx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn"));
        vcx.simulate_click(gpui::point(b.left() + px(24.0), b.center().y), gpui::Modifiers::none());
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn the_sheet_floats_over_the_full_width_table_like_the_issue_pane(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-float", PROJECT, crate::test_env::lock());
        two_finished_jobs();
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 2));
        let row_before = vcx.debug_bounds("job-row-j2").unwrap();

        click(&mut vcx, "job-row-j2");
        assert!(sheet_shows(&view, &mut vcx, "running 12 tests"));
        let sheet = vcx.debug_bounds("job-sheet").expect("the sheet is drawn");
        let root = vcx.debug_bounds("jobs").unwrap();
        let edge = px(EDGE);
        // The issue pane's place: EDGE in from the top, right and bottom, at
        // the task pane's 640pt.
        assert_eq!(sheet.top(), root.top() + edge, "{sheet:?}");
        assert_eq!(sheet.right(), root.right() - edge, "{sheet:?}");
        assert_eq!(sheet.bottom(), root.bottom() - edge, "{sheet:?}");
        assert_eq!(sheet.size.width, px(DEFAULT_W));
        // The table keeps its full width under it.
        let row = vcx.debug_bounds("job-row-j2").unwrap();
        assert_eq!(row, row_before, "the table doesn't narrow");
        assert!(sheet.left() < row.right(), "the sheet covers the table's right");
        let heading = vcx.debug_bounds("job-sheet-title").unwrap();
        assert_eq!(heading.top() - sheet.top() + heading.size.height / 2.0, px(floating_pane::HEADER_H / 2.0));
        let title = view.read_with(&vcx, |v, _| v.sheet_title());
        assert_eq!(title.as_deref(), Some("j2 · desktop tests"), "the Header reads id · label");
        assert!(vcx.debug_bounds("job-sheet-well").is_some_and(|w| w.left() > sheet.left() && w.right() < sheet.right()));

        // Another row, where the sheet leaves it uncovered, shows that job.
        click_left(&mut vcx, "job-row-j1");
        assert!(sheet_shows(&view, &mut vcx, "built the site"), "the sheet follows the click");
        assert_eq!(view.read_with(&vcx, |v, _| v.sheet_title()).as_deref(), Some("j1 · site build"));
        assert_eq!(vcx.debug_bounds("job-sheet").unwrap(), sheet, "in the same place");
        // Its own row again keeps it as it is.
        click_left(&mut vcx, "job-row-j1");
        assert_eq!(view.read_with(&vcx, |v, cx| v.sheet_text(cx)).map(|(id, _)| id).as_deref(), Some("j1"));

        // Reduce Motion: closing leaves nothing sliding.
        click(&mut vcx, "job-sheet-close");
        assert!(!view.read_with(&vcx, |v, _| v.sheet_closing()));
        assert!(vcx.debug_bounds("job-sheet").is_none() && vcx.debug_bounds("job-sheet-closing").is_none());
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn the_sheet_slides_in_and_back_out(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-slide", PROJECT, crate::test_env::lock());
        two_finished_jobs();
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 2));
        let right = vcx.debug_bounds("jobs").unwrap().right() - px(EDGE);
        let draw = |vcx: &mut VisualTestContext| {
            vcx.update(|window, cx| window.draw(cx).clear(cx));
        };

        vcx.update(|window, cx| view.update(cx, |v, cx| v.open_job("j2", window, cx)));
        draw(&mut vcx);
        let start = vcx.debug_bounds("job-sheet").unwrap();
        assert!(start.left() >= right, "it starts past the right edge: {start:?}");
        vcx.executor().advance_clock(SLIDE / 2);
        draw(&mut vcx);
        let mid = vcx.debug_bounds("job-sheet").unwrap();
        assert!(mid.left() < start.left() && mid.right() > right, "on its way in: {mid:?}");
        vcx.executor().advance_clock(SLIDE);
        draw(&mut vcx);
        assert_eq!(vcx.debug_bounds("job-sheet").unwrap().right(), right, "in place");

        vcx.simulate_keystrokes("escape");
        draw(&mut vcx);
        assert!(vcx.debug_bounds("job-sheet").is_none());
        let leaving = vcx.debug_bounds("job-sheet-closing").expect("the closed sheet slides out");
        assert_eq!(leaving.right(), right, "from where it was");
        vcx.executor().advance_clock(SLIDE / 2);
        draw(&mut vcx);
        let out = vcx.debug_bounds("job-sheet-closing").unwrap();
        assert!(out.left() > leaving.left(), "on its way out: {out:?}");
        vcx.executor().advance_clock(SLIDE);
        vcx.run_until_parked();
        draw(&mut vcx);
        assert!(vcx.debug_bounds("job-sheet-closing").is_none(), "then gone");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn dragging_the_sheets_left_edge_resizes_it_and_reports_the_width(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-resize", PROJECT, crate::test_env::lock());
        two_finished_jobs();
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        let saved = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = saved.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, ev: &JobsEvent, _| {
                let JobsEvent::PaneWidth(w) = ev;
                sink.borrow_mut().push(*w);
            })
            .detach()
        });
        // The window's saved width.
        view.update(&mut vcx, |v, cx| v.set_pane_width(600.0, cx));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 2));
        click(&mut vcx, "job-row-j2");
        assert_eq!(vcx.debug_bounds("job-sheet").unwrap().size.width, px(600.0), "the saved width");

        let edge = vcx.debug_bounds("job-sheet-resize").unwrap().center();
        let to = gpui::point(edge.x - px(100.0), edge.y);
        vcx.simulate_mouse_down(edge, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.simulate_mouse_move(to, Some(gpui::MouseButton::Left), gpui::Modifiers::none());
        vcx.simulate_mouse_up(to, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.run_until_parked();
        assert_eq!(view.read_with(&vcx, |v, _| v.pane_width()), 700.0);
        assert_eq!(vcx.debug_bounds("job-sheet").unwrap().size.width, px(700.0));
        assert_eq!(*saved.borrow(), [700.0], "the window gets the new width");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn cancel_in_a_running_jobs_sheet_stops_the_job(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-cancel", PROJECT, crate::test_env::lock());
        // A running job whose terminal is gone shows its output so far.
        set_jobs(vec![job("j6", JobKind::Command, "make", Some("build"), Utc::now())]);
        write_log("j6", "step 1 of 3\n");
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 1));
        click(&mut vcx, "job-row-j6");
        assert!(sheet_shows(&view, &mut vcx, "step 1 of 3"));
        slid_in(&mut vcx);

        click(&mut vcx, "job-sheet-cancel");
        assert!(
            read_until(&view, &mut vcx, |m| m.rows().first().is_some_and(|r| !r.is_running())),
            "the job is canceled and moves under Finished"
        );
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        assert_eq!(model.rows()[0].status_label(), "Canceled");
        assert!(vcx.debug_bounds("job-sheet-cancel").is_none(), "no Cancel once it has stopped");
        assert!(view.read_with(&vcx, |v, cx| v.sheet_text(cx)).is_some(), "the sheet stays open on it");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

}
