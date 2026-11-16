//! The Jobs screen (`View::Jobs`, palette-only): the project's awareness
//! jobs (what `shelbi job run` and `shelbi job watch` create) in a
//! five-column table, the running ones first, then the finished ones from
//! the job history under a `Finished` row, drawn from the shared
//! [`shelbi_app::jobs`] model the TUI's Jobs page draws too. A failed job's
//! Status is red.
//!
//! There's no Figma frame for it, so it borrows the app's: the Machines
//! view's toolbar (the view's name, loud, on a 37pt row over a divider) and
//! the Settings tables' look (muted SmallMedium headings over the strong
//! rule, Base cells, a divider between rows). The Command cell is cut with
//! an ellipsis so the table never scrolls sideways; hovering it shows the
//! whole command (a long Description likewise).
//!
//! A running job started with `--estimate` draws a thin progress bar along
//! the bottom of its row (elapsed against the estimate, the shared
//! [`shelbi_app::estimate`] fraction the TUI's block bar draws too), full and
//! in the overdue tone once the estimate is used up, when its Status reads
//! `Overdue`. Its Duration reads `4m 12s / 10m`; a finished job keeps its
//! actual duration against the estimate as that text, with no bar.
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
//! glyph and tone) and Duration (with the estimate bar while it runs against
//! one) and its Command as property rows, then a running command's live
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
    div, prelude::*, px, relative, AnyElement, ClickEvent, Context, Entity, EventEmitter, FocusHandle, KeyDownEvent,
    ScrollHandle, SharedString, Task, Window,
};
use shelbi_app::jobs::{
    one_line, JobRow, JobsModel, JobsRead, SheetLoad, CANCEL_LABEL, COLUMNS, FINISHED_HEADING, REFRESH_INTERVAL,
    TITLE,
};
use shelbi_app::source::{ProjectHandle, ProjectSource};

use super::estimate_bar;
use super::floating_pane::{
    self, header_button, property_row, value_text, PaneFrame, PaneHost, PaneIds, PaneMotion, SlideOut, INSET,
};
use crate::assets::icons;
use crate::elements::terminal::metrics::PRIMARY_FAMILY;
use crate::elements::terminal::{session as term_session, TerminalPane};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// How often the view drains its refresher.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The fixed columns' widths; Command takes the rest.
const DESCRIPTION_W: f32 = 200.0;
const STATUS_W: f32 = 120.0;
/// Wide enough for elapsed against an estimate (`1h 4m / 1h 30m`).
const DURATION_W: f32 = 116.0;

/// A row's corner radius; a running row's estimate bar follows it.
const ROW_RADIUS: f32 = 6.0;
const FINISHED_W: f32 = 160.0;

/// The gap between columns.
const COLUMN_GAP: f32 = 20.0;

/// A Description longer than this many characters may not fit its column:
/// it gets a tooltip with the whole text.
const DESCRIPTION_TIP_CHARS: usize = 24;

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
        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
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
    #[cfg(test)]
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
            let button = if sheet.canceling {
                style::disabled_button("job-sheet-cancel", CANCEL_LABEL, ButtonKind::Danger, ButtonSize::Small, theme)
            } else {
                style::button("job-sheet-cancel", CANCEL_LABEL, ButtonKind::Danger, ButtonSize::Small, false, theme)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx)))
            };
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .justify_end()
                .gap_3()
                .px(px(INSET))
                .pt(px(16.0))
                .when_some(sheet.cancel_error.clone(), |d, e| {
                    d.child(
                        div().flex_1().min_w_0().truncate().type_style(Type::Small).text_color(theme.red).child(e),
                    )
                })
                .child(button)
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
        if let Some(message) = self.model.message() {
            return div()
                .flex()
                .flex_1()
                .min_w_0()
                .child(style::empty_state("jobs-empty", message, None, theme).max_w_full())
                .into_any_element();
        }
        let now = super::activity::now();
        let headings = div()
            .flex()
            .flex_row()
            .gap(px(COLUMN_GAP))
            .px(px(12.0))
            .py(px(6.0))
            .child(div().w(px(DESCRIPTION_W)).flex_none().child(COLUMNS[0]))
            .child(div().flex_1().min_w_0().child(COLUMNS[1]))
            .child(div().w(px(STATUS_W)).flex_none().child(COLUMNS[2]))
            .child(div().w(px(DURATION_W)).flex_none().child(COLUMNS[3]))
            .child(div().w(px(FINISHED_W)).flex_none().child(COLUMNS[4]));
        let finished_start = self.model.finished_start();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, row) in self.model.rows().iter().enumerate() {
            if Some(i) == finished_start {
                if i > 0 {
                    rows.push(div().h(px(1.0)).bg(theme.divider_strong).into_any_element());
                }
                rows.push(finished_heading(theme));
                rows.push(div().h(px(1.0)).bg(theme.divider).into_any_element());
            } else if i > 0 {
                rows.push(div().h(px(1.0)).bg(theme.divider).into_any_element());
            }
            let open = self.sheet.as_ref().is_some_and(|s| s.id == row.id);
            rows.push(job_row(row, now, open, theme, cx));
        }
        div()
            .id("jobs-list")
            .debug_selector(|| "jobs-list".into())
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .px_4()
            .py_4()
            .child(super::workflows::table(headings, rows, theme).debug_selector(|| "jobs-table".into()))
            .into_any_element()
    }
}

/// The section row above the finished jobs, in the headings' type.
fn finished_heading(theme: &Theme) -> AnyElement {
    div()
        .debug_selector(|| "jobs-finished-heading".into())
        .px(px(12.0))
        .py(px(6.0))
        .type_style(Type::SmallMedium)
        .text_color(theme.form_hint)
        .child(FINISHED_HEADING)
        .into_any_element()
}

/// One job's row: its description, its command cut to the column (whole in
/// a tooltip), its status (red when it failed), how long it ran (against its
/// estimate when it has one), and when it finished. A running job with an
/// estimate has a bar along the bottom ([`row_estimate_bar`]).
fn job_row(
    row: &JobRow,
    now: chrono::DateTime<chrono::Utc>,
    open: bool,
    theme: &Theme,
    cx: &mut Context<JobsView>,
) -> AnyElement {
    let id = row.id.clone();
    let description = one_line(&row.description);
    let long_description = description.chars().count() > DESCRIPTION_TIP_CHARS;
    let click_id = row.id.clone();
    let overdue = row.overdue(now);
    let duration_color = row.duration_tone(now).map_or(theme.muted, |t| theme.tone(t));
    div()
        .id(SharedString::from(format!("job-row-{}", row.id)))
        .debug_selector(move || format!("job-row-{id}"))
        .relative()
        .cursor_pointer()
        .rounded(px(ROW_RADIUS))
        .when(open, |d| d.bg(theme.background_hover))
        .hover(|d| d.bg(theme.background_hover))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_sheet(&click_id, window, cx)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(COLUMN_GAP))
        .px(px(12.0))
        .py(px(10.0))
        .type_style(Type::Base)
        .child(
            div()
                .id(SharedString::from(format!("job-description-{}", row.id)))
                .w(px(DESCRIPTION_W))
                .flex_none()
                .min_w_0()
                .truncate()
                .text_color(theme.text_loud)
                .when(long_description, |d| d.tooltip(style::tooltip(row.description.clone())))
                .child(description),
        )
        .child(
            div()
                .id(SharedString::from(format!("job-command-{}", row.id)))
                .debug_selector({
                    let id = row.id.clone();
                    move || format!("job-command-{id}")
                })
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.foreground)
                .tooltip(style::tooltip(row.command.clone()))
                .child(one_line(&row.command)),
        )
        .child(
            div()
                .debug_selector({
                    let id = row.id.clone();
                    move || format!("job-status-{id}")
                })
                .w(px(STATUS_W))
                .flex_none()
                .truncate()
                .when(row.failed() || overdue, |d| d.type_style(Type::BaseMedium))
                .text_color(theme.tone(row.status_tone_at(now)))
                .child(row.status_label_at(now)),
        )
        .child(
            div()
                .debug_selector({
                    let id = row.id.clone();
                    move || format!("job-duration-{id}")
                })
                .w(px(DURATION_W))
                .flex_none()
                .truncate()
                .text_color(duration_color)
                .child(row.duration(now)),
        )
        .child(
            div()
                .w(px(FINISHED_W))
                .flex_none()
                .truncate()
                .text_color(theme.muted)
                .child(row.finished(now)),
        )
        .when_some(row.bar(now), |d, bar| d.child(row_estimate_bar(&row.id, bar, theme)))
        .into_any_element()
}

/// The bar along the bottom of a running job's row: the issue cards' bar
/// ([`estimate_bar`]), flush with the row's bottom edge and clipped to its
/// corners, full and in the warning color once the estimate is used up.
fn row_estimate_bar(id: &str, bar: shelbi_app::estimate::EstimateProgress, theme: &Theme) -> AnyElement {
    let selector = format!("job-bar-{id}");
    div()
        .debug_selector(move || selector.clone())
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(ROW_RADIUS))
        .child(estimate_bar::estimate_bar(bar, ROW_RADIUS, theme))
        .into_any_element()
}

impl Render for JobsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_focus {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        let theme = *cx.global::<Theme>();
        let summary = self.model.summary();
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
            // The title row: the view's name, loud, then the count, muted.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(37.0))
                    .px_4()
                    .child(
                        div()
                            .debug_selector(|| "jobs-title".into())
                            .type_style(Type::BaseMedium)
                            .text_color(theme.text_loud)
                            .child(TITLE),
                    )
                    .when_some(summary, |d, summary| {
                        d.child(
                            div()
                                .debug_selector(|| "jobs-summary".into())
                                .type_style(Type::XSmall)
                                .text_color(theme.muted)
                                .child(summary),
                        )
                    }),
            )
            .child(div().h(px(1.0)).flex_none().bg(theme.divider))
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
        assert!(vcx.debug_bounds("jobs-table").is_some());
        assert!(vcx.debug_bounds("jobs-title").is_some());
        assert!(vcx.debug_bounds("job-row-j2").is_some() && vcx.debug_bounds("job-row-j1").is_some());

        // The long command is cut to its cell: the row never outgrows the
        // window, and hovering the cell shows the whole command.
        let row = vcx.debug_bounds("job-row-j2").unwrap();
        let cell = vcx.debug_bounds("job-command-j2").unwrap();
        assert!(cell.right() <= row.right(), "the command cell stays in its row");
        assert!(row.right() <= px(1000.0), "no sideways scroll");
        assert_eq!(row.size.height, vcx.debug_bounds("job-row-j1").unwrap().size.height, "one line, cut");
        assert!(vcx.debug_bounds("tooltip").is_none());
        vcx.simulate_mouse_move(cell.center(), None, gpui::Modifiers::none());
        vcx.executor().advance_clock(Duration::from_millis(600));
        vcx.run_until_parked();
        let tip = vcx.debug_bounds("tooltip").expect("hovering the command shows its tooltip");
        assert!(tip.size.height > row.size.height / 2.0, "{tip:?}");

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
        assert!(vcx.debug_bounds("jobs-table").is_none());
        assert_eq!(view.read_with(&vcx, |v, _| v.model().message()).as_deref(), Some(EMPTY_TEXT));
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn finished_jobs_sit_under_a_heading_after_the_running_ones(cx: &mut TestAppContext) {
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
        for sel in ["job-row-j9", "jobs-finished-heading", "job-row-j8", "job-row-j7"] {
            tops.push(vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel} drawn")).top());
        }
        assert!(tops.windows(2).all(|w| w[0] < w[1]), "running, the heading, then finished: {tops:?}");
        assert!(vcx.debug_bounds("jobs-summary").is_some());
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        assert_eq!(model.summary().as_deref(), Some("1 running · 2 finished"));
        let failed = &model.rows()[2];
        assert_eq!(failed.status_label(), "Failed · exit 3");
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
        // Flush along the row's bottom edge, its full width, like a card's.
        assert_eq!((bar.bottom(), bar.left(), bar.size.width), (row.bottom(), row.left(), row.size.width), "{bar:?} in {row:?}");
        assert!(bar.top() > row.center().y, "along the row's bottom: {bar:?} in {row:?}");
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
