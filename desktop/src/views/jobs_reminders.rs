//! The Jobs and Reminders screen: one palette-only screen with two
//! segments, `View::Jobs` and `View::Reminders` (the palette's Jobs and
//! Reminders entries each open it on theirs), under the Issues screen's
//! toolbar band:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ 2 running · 14 finished    ┌────────┬─────────────┐              │ 52px band
//! │                            │ ▤ Jobs │ ◷ Reminders │              │
//! │                            └────────┴─────────────┘              │
//! ├──────────────────────────────────────────────────────────────────┤
//! │   Running …   (the selected segment's screen, its sheet over it) │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The `[Jobs | Reminders]` control sits on the toolbar's center line,
//! whatever stands beside it; at its left the selected segment's counts
//! (`2 running · 14 finished`, `4 pending · 1 fired`) in the toolbar's label
//! style; its right is empty for now, laid out to take actions.
//!
//! Each segment is its own view ([`JobsView`], [`RemindersView`]), built the
//! first time it shows and kept, with its scroll, while the other does. Only
//! the shown one reads its store; a switch drops an open sheet at once (no
//! slide-out nobody would see), and the segment shown again reads at once.
//! Which segment shows is the window's view (`View::Jobs` or
//! `View::Reminders`), so a tab or window restore lands on it: a click on
//! the control switches here and tells the window
//! ([`JobsRemindersEvent::Selected`]), which shows that view.

use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Context, Entity, EventEmitter, Subscription, Window};
use shelbi_app::nav::View;
use shelbi_app::source::ProjectHandle;

use super::jobs::{JobsEvent, JobsView};
use super::reminders::{RemindersEvent, RemindersView};
use crate::assets::icons;
use crate::elements::controls;
use crate::theme::style::{self, Type, Typography, TOOLBAR_EDGE, TOOLBAR_PAD_L, TOOLBAR_PAD_R};
use crate::theme::Theme;

/// The screen's two segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Jobs,
    Reminders,
}

impl Segment {
    /// The window view that shows this segment.
    pub fn view(self) -> View {
        match self {
            Segment::Jobs => View::Jobs,
            Segment::Reminders => View::Reminders,
        }
    }

    /// The segment's name in the control and in its selectors.
    fn key(self) -> &'static str {
        match self {
            Segment::Jobs => "jobs",
            Segment::Reminders => "reminders",
        }
    }
}

/// Something the window acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum JobsRemindersEvent {
    /// A sheet's edge drag ended on this width: the window saves it as its
    /// task pane width, which every pane follows.
    PaneWidth(f32),
    /// The control switched to this segment's view: the window shows it.
    Selected(View),
}

impl EventEmitter<JobsRemindersEvent> for JobsRemindersView {}

pub struct JobsRemindersView {
    project: ProjectHandle,
    segment: Segment,
    jobs: Option<Entity<JobsView>>,
    reminders: Option<Entity<RemindersView>>,
    /// The window's task pane width, for a segment built later.
    pane_width: Option<f32>,
    _subscriptions: Vec<Subscription>,
}

impl JobsRemindersView {
    pub fn new(project: &ProjectHandle, segment: Segment, cx: &mut Context<Self>) -> Self {
        let mut view = JobsRemindersView {
            project: project.clone(),
            segment,
            jobs: None,
            reminders: None,
            pane_width: None,
            _subscriptions: Vec::new(),
        };
        view.ensure(segment, cx);
        view
    }

    /// The segment that shows.
    #[cfg(test)]
    pub fn segment(&self) -> Segment {
        self.segment
    }

    /// Show `segment` (the window's view moved to it, or the control was
    /// clicked): the other's sheet goes and it stops reading; this one is
    /// built if it hasn't been, reads at once, and takes the keyboard.
    pub fn set_segment(&mut self, segment: Segment, cx: &mut Context<Self>) {
        if segment == self.segment {
            return;
        }
        match self.segment {
            Segment::Jobs => {
                if let Some(jobs) = &self.jobs {
                    jobs.update(cx, |v, cx| {
                        v.dismiss_sheet(cx);
                        v.set_active(false);
                    });
                }
            }
            Segment::Reminders => {
                if let Some(reminders) = &self.reminders {
                    reminders.update(cx, |v, cx| {
                        v.dismiss_sheet(cx);
                        v.set_active(false);
                    });
                }
            }
        }
        self.segment = segment;
        self.ensure(segment, cx);
        match segment {
            Segment::Jobs => {
                if let Some(jobs) = &self.jobs {
                    jobs.update(cx, |v, _| {
                        v.set_active(true);
                        v.request_focus();
                    });
                }
            }
            Segment::Reminders => {
                if let Some(reminders) = &self.reminders {
                    reminders.update(cx, |v, _| {
                        v.set_active(true);
                        v.request_focus();
                    });
                }
            }
        }
        cx.notify();
    }

    /// A click on the control's `segment`.
    fn select(&mut self, segment: Segment, cx: &mut Context<Self>) {
        if segment == self.segment {
            return;
        }
        self.set_segment(segment, cx);
        cx.emit(JobsRemindersEvent::Selected(segment.view()));
    }

    /// Build `segment`'s view if it hasn't been.
    fn ensure(&mut self, segment: Segment, cx: &mut Context<Self>) {
        let width = self.pane_width;
        match segment {
            Segment::Jobs if self.jobs.is_none() => {
                let jobs = cx.new(|cx| JobsView::new(&self.project, cx));
                if let Some(width) = width {
                    jobs.update(cx, |v, cx| v.set_pane_width(width, cx));
                }
                self._subscriptions.push(cx.subscribe(&jobs, |_, _, ev: &JobsEvent, cx| match ev {
                    JobsEvent::PaneWidth(w) => cx.emit(JobsRemindersEvent::PaneWidth(*w)),
                }));
                // Its reads change the toolbar's counts.
                self._subscriptions.push(cx.observe(&jobs, |_, _, cx| cx.notify()));
                self.jobs = Some(jobs);
            }
            Segment::Reminders if self.reminders.is_none() => {
                let reminders = cx.new(|cx| RemindersView::new(&self.project, cx));
                if let Some(width) = width {
                    reminders.update(cx, |v, cx| v.set_pane_width(width, cx));
                }
                self._subscriptions.push(cx.subscribe(&reminders, |_, _, ev: &RemindersEvent, cx| match ev {
                    RemindersEvent::PaneWidth(w) => cx.emit(JobsRemindersEvent::PaneWidth(*w)),
                }));
                self._subscriptions.push(cx.observe(&reminders, |_, _, cx| cx.notify()));
                self.reminders = Some(reminders);
            }
            _ => {}
        }
    }

    /// The window's saved task pane width, which both segments' sheets
    /// share.
    pub fn set_pane_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.pane_width = Some(width);
        if let Some(jobs) = &self.jobs {
            jobs.update(cx, |v, cx| v.set_pane_width(width, cx));
        }
        if let Some(reminders) = &self.reminders {
            reminders.update(cx, |v, cx| v.set_pane_width(width, cx));
        }
    }

    pub fn request_focus(&mut self, cx: &mut Context<Self>) {
        match self.segment {
            Segment::Jobs => {
                if let Some(jobs) = &self.jobs {
                    jobs.update(cx, |v, _| v.request_focus());
                }
            }
            Segment::Reminders => {
                if let Some(reminders) = &self.reminders {
                    reminders.update(cx, |v, _| v.request_focus());
                }
            }
        }
    }

    /// The window is dropping the screen (a project switch): stop both
    /// segments' reads without waiting on one in flight.
    pub fn stop_workers(&mut self, cx: &mut Context<Self>) {
        if let Some(jobs) = &self.jobs {
            jobs.update(cx, |v, _| v.stop_workers());
        }
        if let Some(reminders) = &self.reminders {
            reminders.update(cx, |v, _| v.stop_workers());
        }
    }

    /// The Jobs segment's view, once built.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn jobs(&self) -> Option<Entity<JobsView>> {
        self.jobs.clone()
    }

    /// The Reminders segment's view, once built.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn reminders(&self) -> Option<Entity<RemindersView>> {
        self.reminders.clone()
    }

    /// The selected segment's counts.
    fn summary(&self, cx: &gpui::App) -> Option<String> {
        match self.segment {
            Segment::Jobs => self.jobs.as_ref().and_then(|v| v.read(cx).summary()),
            Segment::Reminders => self.reminders.as_ref().and_then(|v| v.read(cx).summary()),
        }
    }

    /// The toolbar: the counts, the control on the center line, and the
    /// (empty) actions side.
    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let segment = self.segment;
        let item = |s: Segment, cx: &mut Context<Self>| {
            let (icon, label) = match s {
                Segment::Jobs => (icons::QUEUE_LIST, shelbi_app::jobs::TITLE),
                Segment::Reminders => (icons::CLOCK, shelbi_app::reminders::TITLE),
            };
            controls::icon_segment(format!("{}-segment", s.key()), icon, label, s == segment, true, theme)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(s, cx)))
        };
        let summary = self.summary(cx).map(|text| {
            let key = segment.key();
            div()
                .debug_selector(move || format!("{key}-summary"))
                .min_w_0()
                .truncate()
                .type_style(Type::SmallMedium)
                .text_color(theme.form_hint)
                .child(text)
        });
        style::toolbar_band(theme)
            .debug_selector(|| "jobs-reminders-toolbar".into())
            // Three columns: the two sides share what the control leaves,
            // equally, so the control sits on the band's center line. The
            // band's room (its padding, and the edge on its left) is 3px
            // wider at the right, so the control steps 3px right to land
            // on it.
            .child(
                div()
                    .debug_selector(|| "jobs-reminders-toolbar-left".into())
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .children(summary),
            )
            .child(
                div().flex_none().ml(px(TOOLBAR_PAD_R - TOOLBAR_PAD_L - TOOLBAR_EDGE)).child(
                    controls::segmented_track(theme)
                        .debug_selector(|| "jobs-reminders-segments".into())
                        .child(item(Segment::Jobs, cx))
                        .child(item(Segment::Reminders, cx)),
                ),
            )
            .child(
                // Room for the screen's actions.
                div()
                    .debug_selector(|| "jobs-reminders-toolbar-right".into())
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .flex_1()
                    .min_w_0(),
            )
            .into_any_element()
    }
}

impl Render for JobsRemindersView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let toolbar = self.render_toolbar(&theme, cx);
        let content = match self.segment {
            Segment::Jobs => self.jobs.clone().map(|v| v.into_any_element()),
            Segment::Reminders => self.reminders.clone().map(|v| v.into_any_element()),
        };
        div()
            .debug_selector(|| "jobs-reminders".into())
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(toolbar)
            .child(div().flex().flex_col().flex_1().min_h_0().min_w_0().children(content))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::views::column_swap::SLIDE;
    use chrono::Utc;
    use gpui::{TestAppContext, VisualTestContext};
    use shelbi_state::awareness::{update_awareness, Reminder};

    const PROJECT: &str = "jobs-reminders";

    fn open(cx: &mut TestAppContext, segment: Segment) -> (Entity<JobsRemindersView>, VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let handle = shelbi_orchestrator::source::local_handle(PROJECT);
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                        gpui::point(px(0.0), px(0.0)),
                        gpui::size(px(1000.0), px(600.0)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| JobsRemindersView::new(&handle, segment, cx)),
            )
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut vcx).unwrap();
        (view, vcx)
    }

    fn draw(vcx: &mut VisualTestContext) {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }

    /// Drain the Reminders segment's reads until `done`.
    fn reminders_until(
        view: &Entity<JobsRemindersView>,
        vcx: &mut VisualTestContext,
        done: impl Fn(&RemindersView) -> bool,
    ) -> bool {
        let reminders = view.read_with(vcx, |v, _| v.reminders()).expect("the Reminders segment");
        reminders.update(vcx, |v, _| v.refresh());
        for _ in 0..200 {
            reminders.update(vcx, |v, cx| v.poll(cx));
            draw(vcx);
            if reminders.read_with(vcx, |v, _| done(v)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn click(vcx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
        vcx.simulate_click(at, gpui::Modifiers::none());
        draw(vcx);
    }

    fn center_x(b: gpui::Bounds<gpui::Pixels>) -> f32 {
        f32::from(b.origin.x) + f32::from(b.size.width) / 2.0
    }

    #[gpui::test]
    fn the_control_switches_segments_and_drops_the_open_sheet(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-jobs-reminders", PROJECT, crate::test_env::lock());
        let now = Utc::now();
        update_awareness(PROJECT, |store| {
            store.reminders = vec![Reminder {
                id: "r1".into(),
                due_at: now + chrono::Duration::minutes(30),
                text: "check CI".into(),
                created_at: now - chrono::Duration::minutes(5),
                every: None,
            }];
            Ok(())
        })
        .unwrap();
        let (view, mut vcx) = open(cx, Segment::Reminders);
        let selected = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = selected.clone();
        let _sub = vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, ev: &JobsRemindersEvent, _| {
                if let JobsRemindersEvent::Selected(v) = ev {
                    seen.borrow_mut().push(v.clone());
                }
            })
        });
        assert!(reminders_until(&view, &mut vcx, |v| v.model().rows().len() == 1), "the reminder loads");
        assert!(view.read_with(&vcx, |v, _| v.jobs()).is_none(), "Jobs is built when it first shows");

        // The toolbar: the Issues band's height, the counts at its left, the
        // control on its center line.
        let toolbar = vcx.debug_bounds("jobs-reminders-toolbar").expect("the toolbar");
        assert_eq!(toolbar.size.height, px(style::TOOLBAR_H));
        assert_eq!(toolbar.size.width, px(1000.0));
        let control = vcx.debug_bounds("jobs-reminders-segments").expect("the control");
        assert!((center_x(control) - 500.0).abs() < 0.5, "centered: {control:?}");
        assert!(vcx.debug_bounds("jobs-segment-icon").is_some() && vcx.debug_bounds("reminders-segment-icon").is_some());
        let summary = vcx.debug_bounds("reminders-summary").expect("the counts");
        assert!(summary.left() < control.left(), "the counts sit left of the control");
        let list = vcx.debug_bounds("reminders-list").expect("the agenda");
        assert!(list.top() >= toolbar.bottom(), "the agenda sits under the toolbar");

        // A row's sheet, then a switch: the sheet goes at once, and the
        // hidden segment stops reading.
        let row = vcx.debug_bounds("reminder-row-r1").expect("the row");
        click(&mut vcx, gpui::point(row.left() + px(20.0), row.center().y));
        vcx.executor().advance_clock(SLIDE);
        draw(&mut vcx);
        assert!(vcx.debug_bounds("reminder-sheet").is_some(), "the sheet opens");
        let at = vcx.debug_bounds("jobs-segment").expect("the Jobs segment").center();
        click(&mut vcx, at);
        assert_eq!(view.read_with(&vcx, |v, _| v.segment()), Segment::Jobs);
        assert_eq!(*selected.borrow(), vec![View::Jobs], "the window is told");
        assert!(vcx.debug_bounds("reminders").is_none() && vcx.debug_bounds("jobs").is_some());
        let reminders = view.read_with(&vcx, |v, _| v.reminders()).unwrap();
        assert!(reminders.read_with(&vcx, |v, _| !v.sheet_open() && !v.is_active()));
        let jobs = view.read_with(&vcx, |v, _| v.jobs()).expect("the Jobs segment, built");
        assert!(jobs.read_with(&vcx, |v, _| v.is_active()));
        let control = vcx.debug_bounds("jobs-reminders-segments").expect("the control");
        assert!((center_x(control) - 500.0).abs() < 0.5, "still centered: {control:?}");

        // Back: the reminder is there with no sheet over it, and Jobs rests.
        let at = vcx.debug_bounds("reminders-segment").unwrap().center();
        click(&mut vcx, at);
        assert_eq!(*selected.borrow(), vec![View::Jobs, View::Reminders]);
        assert!(vcx.debug_bounds("reminder-row-r1").is_some() && vcx.debug_bounds("reminder-sheet").is_none());
        assert!(reminders.read_with(&vcx, |v, _| v.is_active()) && jobs.read_with(&vcx, |v, _| !v.is_active()));
        // The selected segment again changes nothing.
        let at = vcx.debug_bounds("reminders-segment").unwrap().center();
        click(&mut vcx, at);
        assert_eq!(selected.borrow().len(), 2);
        view.update(&mut vcx, |v, cx| v.stop_workers(cx));
    }
}
