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
//! The awareness store and the history have no change notification, so the
//! view re-reads them every [`REFRESH_INTERVAL`] while it is open, off the UI
//! thread through the window's [`ProjectSource`] (a remote window's source
//! says the far store isn't readable yet, and the view shows that line).
//! Each read also repaints, which is what ticks the running durations and
//! the Finished ages over.

use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, AnyElement, Context, FocusHandle, ScrollHandle, SharedString, Task, Window,
};
use shelbi_app::jobs::{
    one_line, JobRow, JobsModel, JobsRead, COLUMNS, FINISHED_HEADING, REFRESH_INTERVAL, TITLE,
};
use shelbi_app::source::{ProjectHandle, ProjectSource};

use crate::theme::style::{self, Type, Typography};
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
    model: JobsModel,
    focus: FocusHandle,
    needs_focus: bool,
    refresher: shelbi_app::Refresher<JobsRead>,
    last_refresh: Instant,
    scroll: ScrollHandle,
    _poll: Task<()>,
}

impl JobsView {
    pub fn new(handle: &ProjectHandle, cx: &mut Context<Self>) -> Self {
        let source = handle.source.clone();
        Self::over(source, cx)
    }

    fn over(source: std::sync::Arc<dyn ProjectSource>, cx: &mut Context<Self>) -> Self {
        let refresher = shelbi_app::spawn_worker(move |_| source.jobs());
        refresher.request();
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                break;
            }
        });
        JobsView {
            model: JobsModel::Loading,
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

    /// Ask for a read now.
    #[cfg(test)]
    pub fn refresh(&mut self) {
        self.refresher.request();
        self.last_refresh = Instant::now();
    }

    fn render_body(&self, theme: &Theme) -> AnyElement {
        if let Some(message) = self.model.message() {
            return style::empty_state("jobs-empty", message, None, theme)
                .max_w_full()
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
            rows.push(job_row(row, now, theme));
        }
        div()
            .id("jobs-list")
            .debug_selector(|| "jobs-list".into())
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
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
fn job_row(row: &JobRow, now: chrono::DateTime<chrono::Utc>, theme: &Theme) -> AnyElement {
    let id = row.id.clone();
    let description = one_line(&row.description);
    let long_description = description.chars().count() > DESCRIPTION_TIP_CHARS;
    div()
        .debug_selector(move || format!("job-row-{id}"))
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
        div()
            .id("jobs")
            .debug_selector(|| "jobs".into())
            .track_focus(&self.focus)
            .key_context("Jobs")
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
            .child(self.render_body(&theme))
    }
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
}
