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
//! Clicking a row opens the job's sheet on the right, beside the table (the
//! Workspace view's split, with its draggable divider): the job's label, its
//! status and elapsed time, a running command's live terminal (watched, never
//! typed into) or a finished one's saved output, and a Cancel button while it
//! runs. Escape or ✕ closes it. The loads are
//! [`shelbi_app::jobs::open_sheet`]'s, the TUI's too.
//!
//! The awareness store and the history have no change notification, so the
//! view re-reads them every [`REFRESH_INTERVAL`] while it is open, off the UI
//! thread through the window's [`ProjectSource`] (a remote window's source
//! says the far store isn't readable yet, and the view shows that line).
//! Each read also repaints, which is what ticks the running durations and
//! the Finished ages over.

use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, AnyElement, ClickEvent, Context, Entity, FocusHandle, KeyDownEvent, ScrollHandle,
    SharedString, Task, Window,
};
use shelbi_app::jobs::{
    one_line, JobRow, JobsModel, JobsRead, SheetLoad, CANCEL_LABEL, COLUMNS, FINISHED_HEADING, REFRESH_INTERVAL,
    TITLE,
};
use shelbi_app::source::{ProjectHandle, ProjectSource};

use super::workspace_layout::{render_split, ShellSplit};
use crate::assets::icons;
use crate::elements::terminal::{session as term_session, TerminalPane};
use crate::theme::style::{self, ButtonKind, ButtonSize, IconButtonSize, Type, Typography};
use crate::theme::Theme;

/// How often the view drains its refresher.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The fixed columns' widths; Command takes the rest.
const DESCRIPTION_W: f32 = 200.0;
const STATUS_W: f32 = 120.0;
const DURATION_W: f32 = 72.0;
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
    /// The table | sheet split.
    split: ShellSplit,
    focus: FocusHandle,
    needs_focus: bool,
    refresher: shelbi_app::Refresher<JobsRead>,
    last_refresh: Instant,
    scroll: ScrollHandle,
    _poll: Task<()>,
}

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
            split: ShellSplit::default(),
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

    /// Ask for a read now.
    #[cfg(test)]
    pub fn refresh(&mut self) {
        self.refresher.request();
        self.last_refresh = Instant::now();
    }

    /// Open job `id`'s sheet (replacing an open one) and load what it shows
    /// off the UI thread.
    fn open_sheet(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.model.rows().iter().find(|r| r.id == id).cloned() else {
            return;
        };
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

    fn close_sheet(&mut self, cx: &mut Context<Self>) {
        if self.sheet.take().is_some() {
            self.needs_focus = true;
            cx.notify();
        }
    }

    /// The open sheet's row, current from the model.
    fn sheet_row(&self) -> Option<&JobRow> {
        let sheet = self.sheet.as_ref()?;
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

    /// The sheet: its header (the job's label and ✕), its status, the
    /// terminal, and Cancel while the job runs.
    fn render_sheet(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.sheet.as_ref()?;
        let row = self.sheet_row()?.clone();
        let now = super::activity::now();
        let status = if sheet.canceling && row.is_running() {
            "● Canceling…".to_string()
        } else {
            row.sheet_status(now)
        };
        let close = style::icon_button("job-sheet-close", icons::X_MARK, IconButtonSize::Small, false, theme)
            .tooltip(style::tooltip("Close (Esc)"))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_sheet(cx)));
        let body = match &sheet.body {
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
                .gap_3()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(theme.divider)
                .child(button)
                .when_some(sheet.cancel_error.clone(), |d, e| {
                    d.child(div().min_w_0().truncate().type_style(Type::Small).text_color(theme.red).child(e))
                })
        });
        Some(
            div()
                .debug_selector(|| "job-sheet".into())
                .flex()
                .flex_col()
                .size_full()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .h(px(37.0))
                        .pl_4()
                        .pr_2()
                        .child(
                            div()
                                .debug_selector(|| "job-sheet-title".into())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .type_style(Type::BaseMedium)
                                .text_color(theme.text_loud)
                                .child(one_line(&row.description)),
                        )
                        .child(close),
                )
                .child(
                    div()
                        .debug_selector(|| "job-sheet-status".into())
                        .flex_none()
                        .px_4()
                        .pb_2()
                        .type_style(Type::Small)
                        .text_color(theme.tone(row.status_tone()))
                        .child(status),
                )
                .child(div().h(px(1.0)).flex_none().bg(theme.divider))
                .child(body)
                .children(footer)
                .into_any_element(),
        )
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
/// a tooltip), its status (red when it failed), how long it ran, and when it
/// finished.
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
    div()
        .id(SharedString::from(format!("job-row-{}", row.id)))
        .debug_selector(move || format!("job-row-{id}"))
        .cursor_pointer()
        .rounded(px(6.0))
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
                .when(row.failed(), |d| d.type_style(Type::BaseMedium))
                .text_color(theme.tone(row.status_tone()))
                .child(row.status_label()),
        )
        .child(
            div()
                .w(px(DURATION_W))
                .flex_none()
                .truncate()
                .text_color(theme.muted)
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
        let body = render_split("jobs", &self.split, |this| &mut this.split, table, sheet, &theme, cx)
            .flex_1()
            .min_h_0();
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
            .child(body)
    }
}

/// A line in place of the sheet's terminal.
fn message_body(text: &str, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(|| "job-sheet-message".into())
        .flex_1()
        .p_4()
        .type_style(Type::Small)
        .text_color(theme.muted)
        .child(text.to_string())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use shelbi_app::jobs::{EMPTY_TEXT, REMOTE_TEXT};
    use shelbi_app::source::ProjectKey;
    use shelbi_state::awareness::{record_finished_jobs, update_awareness, Job, JobKind, JobRecord, JobStatus};

    const PROJECT: &str = "jobs";
    const LONG: &str = "zsh -c 'cd /Users/me/Workspaces/shelbi/desktop && cargo clippy --all-targets -- -D warnings && cargo test'";

    fn job(id: &str, kind: JobKind, target: &str, label: Option<&str>, started_at: DateTime<Utc>) -> Job {
        Job { id: id.into(), kind, target: target.into(), label: label.map(str::to_string), started_at, runner_pid: None }
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
        let sheet = vcx.debug_bounds("job-sheet").expect("the sheet is drawn");
        let row = vcx.debug_bounds("job-row-j4").unwrap();
        assert!(sheet.left() >= row.right(), "the sheet sits beside the table");
        assert!(vcx.debug_bounds("job-sheet-title").is_some() && vcx.debug_bounds("job-sheet-status").is_some());
        assert!(vcx.debug_bounds("job-sheet-cancel").is_none(), "a finished job has no Cancel");

        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert!(view.read_with(&vcx, |v, cx| v.sheet_text(cx)).is_none(), "Escape closes the sheet");

        click(&mut vcx, "job-row-j4");
        assert!(sheet_shows(&view, &mut vcx, "running 3 tests"));
        click(&mut vcx, "job-sheet-close");
        assert!(view.read_with(&vcx, |v, cx| v.sheet_text(cx)).is_none(), "✕ closes the sheet");
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
