//! The Reminders screen (`View::Reminders`, palette-only, and left out of
//! the palette's default list until the user types: `rem…` finds it): the
//! project's `shelbi remind` reminders in a four-column table (Id, When,
//! Reminder, Repeats), the pending ones soonest first under `Pending`, then
//! the recently fired ones newest first under `Fired`, drawn from the shared
//! [`shelbi_app::reminders`] model the TUI's Reminders page draws too.
//!
//! It is the Jobs screen's layout, built from the same pieces
//! ([`super::list_screen`]): the title row with its count, the bordered
//! table with section headings, and the clickable rows. A row opens its
//! sheet the way a job's opens: the shared floating pane
//! ([`super::floating_pane`], the issue pane's chrome) over the full-width
//! table, at the window's task pane width (a drag of its left edge sets it
//! for every pane), sliding in and back out. Its Header is `id · text` with
//! Close; it shows the reminder's status, when it is due (absolute and
//! relative), its repeat, when it was created, its whole text and, for a
//! pending one, a Cancel reminder button (`shelbi remind cancel`). Another
//! row's click shows that reminder; Escape or Close closes it.
//!
//! Like the Jobs screen, it re-reads the store and the reminder history
//! every [`REFRESH_INTERVAL`] while it is open, off the UI thread through
//! the window's [`ProjectSource`] (a remote window's source says the far
//! store isn't readable yet), so a reminder appears, fires, or goes on its
//! own; each read also repaints, which ticks the relative times over.

use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, AnyElement, ClickEvent, Context, EventEmitter, FocusHandle, KeyDownEvent, ScrollHandle,
    SharedString, Task, Window,
};
use shelbi_app::jobs::one_line;
use shelbi_app::reminders::{
    ReminderRow, RemindersModel, RemindersRead, SheetState, CANCEL_LABEL, COLUMNS, REFRESH_INTERVAL, TITLE,
};
use shelbi_app::source::{ProjectHandle, ProjectSource};

use super::floating_pane::{
    self, header_button, property_row, value_text, PaneFrame, PaneHost, PaneIds, PaneMotion, SlideOut, INSET,
};
use super::list_screen;
use crate::assets::icons;
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// How often the view drains its refresher.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The fixed columns' widths; Reminder takes the rest.
const ID_W: f32 = 44.0;
const WHEN_W: f32 = 132.0;
const REPEATS_W: f32 = 110.0;

/// The sheet's ids: tests and the visual check find it by them.
const IDS: PaneIds = PaneIds {
    pane: "reminder-sheet",
    closing: "reminder-sheet-closing",
    resize: "reminder-sheet-resize",
    inert: "reminder-sheet-inert",
    slide_in: "reminder-sheet-in",
    slide_out: "reminder-sheet-out",
};

pub struct RemindersView {
    source: std::sync::Arc<dyn ProjectSource>,
    model: RemindersModel,
    /// The open reminder's sheet.
    sheet: Option<SheetState>,
    /// A closed sheet sliding back out.
    closing: SlideOut<SheetState>,
    /// The sheet's width (the window's task pane width) and a drag of its
    /// edge.
    pane_frame: PaneFrame,
    focus: FocusHandle,
    needs_focus: bool,
    refresher: shelbi_app::Refresher<RemindersRead>,
    last_refresh: Instant,
    scroll: ScrollHandle,
    _poll: Task<()>,
}

/// Something the window acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum RemindersEvent {
    /// A drag of the sheet's left edge ended on this width: the window saves
    /// it as its task pane width, which the other panes follow too.
    PaneWidth(f32),
}

impl EventEmitter<RemindersEvent> for RemindersView {}

impl PaneHost for RemindersView {
    fn pane_frame(&mut self) -> &mut PaneFrame {
        &mut self.pane_frame
    }

    fn pane_resized(&mut self, width: f32, cx: &mut Context<Self>) {
        cx.emit(RemindersEvent::PaneWidth(width));
    }
}

impl RemindersView {
    pub fn new(handle: &ProjectHandle, cx: &mut Context<Self>) -> Self {
        let source = handle.source.clone();
        let reader = source.clone();
        let refresher = shelbi_app::spawn_worker(move |_| reader.reminders());
        refresher.request();
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                break;
            }
        });
        RemindersView {
            source,
            model: RemindersModel::Loading,
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

    /// What the view shows.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn model(&self) -> &RemindersModel {
        &self.model
    }

    /// The open sheet's row, for tests.
    #[cfg(test)]
    pub fn sheet_row(&self) -> Option<&ReminderRow> {
        self.sheet.as_ref().map(|s| &s.row)
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if let Some(read) = self.refresher.latest() {
            self.model = RemindersModel::from_read(read);
            if self.sheet.as_mut().is_some_and(|sheet| !sheet.follow(&self.model)) {
                self.close_sheet(cx);
            }
            // Also when nothing changed: the relative times tick over.
            cx.notify();
        }
        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresh();
        }
    }

    /// Read now and fold the read in (the offscreen check's dispatcher
    /// doesn't run the poll timer, and tests don't wait out the cadence).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        self.tick(cx);
    }

    /// Ask for a read now.
    pub fn refresh(&mut self) {
        self.refresher.request();
        self.last_refresh = Instant::now();
    }

    /// Open reminder `key`'s sheet, as a click on its row does (the
    /// offscreen check has no pointer).
    #[cfg(feature = "visual-check")]
    pub fn open_sheet_for_check(&mut self, key: &str, cx: &mut Context<Self>) {
        self.open_sheet(key, cx);
    }

    /// Open reminder `key`'s sheet (replacing an open one; its own row
    /// again keeps the sheet as it is).
    fn open_sheet(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.sheet.as_ref().is_some_and(|s| s.row.key == key) {
            return;
        }
        if let Some(row) = self.model.row(key).cloned() {
            // One opened while another slides out shows at once.
            self.closing.clear();
            self.sheet = Some(SheetState::new(row));
            cx.notify();
        }
    }

    /// Close the sheet (Close, Escape, or its reminder canceled): it slides
    /// back out, inert.
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

    /// `shelbi remind cancel` for the open sheet's reminder, off the UI
    /// thread: it goes on the next read, and its sheet with it.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.sheet.as_mut().and_then(SheetState::begin_cancel) else {
            return;
        };
        let source = self.source.clone();
        let background = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let done = background.spawn(async move { source.cancel_reminder(&id) }).await;
            let _ = this.update(cx, |view, cx| {
                if let (Err(e), Some(sheet)) = (done, view.sheet.as_mut()) {
                    sheet.cancel_failed(e);
                }
                // Read now, so the row goes without waiting out the cadence.
                view.refresh();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The sheet, while one is open or sliding out: the floating pane with
    /// its Header (`id · text`, Close), the reminder's properties, its whole
    /// text, and Cancel reminder while it is pending.
    fn render_sheet(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (sheet, motion) = match (&self.sheet, self.closing.get()) {
            (Some(sheet), _) => (sheet, PaneMotion::In),
            (None, Some(sheet)) => (sheet, PaneMotion::Out),
            (None, None) => return None,
        };
        // The pane floats over the table, so it paints from the opaque
        // theme, as the issue pane does.
        let theme = &theme.opaque();
        let row = &sheet.row;
        let now = super::activity::now();
        let close = header_button("reminder-sheet-close", icons::X_MARK, false, theme)
            .tooltip(style::tooltip("Close (Esc)"))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_sheet(cx)));
        let title = format!("{} · {}", row.id, one_line(&row.text));
        let header = floating_pane::header("reminder-sheet-title", title, vec![close.into_any_element()], theme);
        let status = property_row(
            theme,
            "Status",
            value_text(theme)
                .debug_selector(|| "reminder-sheet-status".into())
                .text_color(theme.tone(row.status_tone()))
                .child(sheet.status(now)),
        );
        let facts = row.details(now).into_iter().map(|(label, value)| {
            let selector = format!("reminder-detail-{}", label.to_lowercase());
            property_row(theme, label, value_text(theme).debug_selector(move || selector).child(value))
        });
        let properties = div()
            .debug_selector(|| "reminder-sheet-properties".into())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.0))
            .w_full()
            .min_w_0()
            .p(px(INSET))
            .child(status)
            .children(facts);
        // The whole text, wrapped, under a rule: what the reminder says.
        let text = div()
            .id("reminder-sheet-body")
            .debug_selector(|| "reminder-sheet-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .mx(px(INSET))
            .pt(px(16.0))
            .border_t_1()
            .border_color(theme.divider)
            .child(
                div()
                    .debug_selector(|| "reminder-sheet-text".into())
                    .px(px(8.0))
                    .type_style(Type::Base)
                    .text_color(theme.text_loud)
                    .child(row.text.clone()),
            );
        let footer = row.can_cancel().then(|| {
            list_screen::action(
                "reminder",
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
            .child(properties)
            .child(text)
            .children(footer)
            .into_any_element();
        Some(floating_pane::render(&self.pane_frame, IDS, motion, header, body, |pane, _| pane, theme, cx))
    }

    fn render_body(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if let Some(message) = self.model.message() {
            // The empty line's two sentences on lines of their own: wrapped
            // as one, the command's closing "`." breaks onto a line alone.
            let message = match self.model {
                RemindersModel::Empty => message.replacen(". ", ".\n", 1),
                _ => message,
            };
            return list_screen::message("reminders-empty", message, theme);
        }
        let now = super::activity::now();
        let headings = list_screen::headings_row()
            .child(div().w(px(ID_W)).flex_none().child(COLUMNS[0]))
            .child(div().w(px(WHEN_W)).flex_none().child(COLUMNS[1]))
            .child(div().flex_1().min_w_0().child(COLUMNS[2]))
            .child(div().w(px(REPEATS_W)).flex_none().child(COLUMNS[3]));
        let rows = self
            .model
            .rows()
            .iter()
            .map(|row| {
                let open = self.sheet.as_ref().is_some_and(|s| s.row.key == row.key);
                reminder_row(row, now, open, theme, cx)
            })
            .collect();
        let rows = list_screen::sectioned("reminders", rows, &self.model.headings(), theme);
        list_screen::list("reminders", &self.scroll, headings, rows, theme)
    }
}

/// One reminder's row: its id, when it is due (blue) or when it fired
/// (muted), its text cut to the column (whole in a tooltip), its repeat.
fn reminder_row(
    row: &ReminderRow,
    now: chrono::DateTime<chrono::Utc>,
    open: bool,
    theme: &Theme,
    cx: &mut Context<RemindersView>,
) -> AnyElement {
    let key = row.key.clone();
    let when_color = row.when_tone().map_or(theme.muted, |t| theme.tone(t));
    list_screen::row("reminder", &row.key, open, theme)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_sheet(&key, cx)))
        .child(div().w(px(ID_W)).flex_none().truncate().text_color(theme.muted).child(row.id.clone()))
        .child(
            div()
                .debug_selector({
                    let key = row.key.clone();
                    move || format!("reminder-when-{key}")
                })
                .w(px(WHEN_W))
                .flex_none()
                .truncate()
                .text_color(when_color)
                .child(row.when(now)),
        )
        .child(
            div()
                .id(SharedString::from(format!("reminder-text-{}", row.key)))
                .debug_selector({
                    let key = row.key.clone();
                    move || format!("reminder-text-{key}")
                })
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text_loud)
                .tooltip(style::tooltip(row.text.clone()))
                .child(one_line(&row.text)),
        )
        .child(div().w(px(REPEATS_W)).flex_none().truncate().text_color(theme.muted).child(row.repeats()))
        .into_any_element()
}

impl Render for RemindersView {
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
            .id("reminders")
            .debug_selector(|| "reminders".into())
            .track_focus(&self.focus)
            .key_context("Reminders")
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
            .child(list_screen::title_row("reminders", TITLE, summary, &theme))
            .child(div().h(px(1.0)).flex_none().bg(theme.divider))
            .child(div().flex().flex_col().flex_1().min_h_0().min_w_0().child(table))
            .children(sheet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::column_swap::SLIDE;
    use crate::views::floating_pane::EDGE;
    use crate::views::pane_width::DEFAULT_W;
    use chrono::Utc;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use shelbi_app::reminders::{EMPTY_TEXT, REMOTE_TEXT};
    use shelbi_app::source::ProjectKey;
    use shelbi_state::awareness::{record_fired_reminders, update_awareness, FiredReminder, Reminder};

    const PROJECT: &str = "reminders";

    fn reminder(id: &str, text: &str, due_in_mins: i64, every: Option<u64>) -> Reminder {
        let now = Utc::now();
        Reminder {
            id: id.into(),
            due_at: now + chrono::Duration::minutes(due_in_mins),
            text: text.into(),
            created_at: now - chrono::Duration::hours(1),
            every: every.map(Duration::from_secs),
        }
    }

    fn set_reminders(reminders: Vec<Reminder>) {
        update_awareness(PROJECT, |store| {
            store.reminders = reminders;
            Ok(())
        })
        .unwrap();
    }

    fn open(cx: &mut TestAppContext, handle: ProjectHandle) -> (Entity<RemindersView>, VisualTestContext) {
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
                |_, cx| cx.new(|cx| RemindersView::new(&handle, cx)),
            )
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut vcx).unwrap();
        (view, vcx)
    }

    /// Ask for a read and wait for it to land and draw.
    fn read_until(
        view: &Entity<RemindersView>,
        cx: &mut VisualTestContext,
        done: impl Fn(&RemindersModel) -> bool,
    ) -> bool {
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

    /// A selector built at runtime (a fired row's key), as `debug_bounds`
    /// wants it.
    fn sel(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    /// Run a slide (in or out) to its end and draw.
    fn slid(vcx: &mut VisualTestContext) {
        vcx.executor().advance_clock(SLIDE);
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn click(vcx: &mut VisualTestContext, selector: &'static str) {
        let at = vcx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
        vcx.simulate_click(at, gpui::Modifiers::none());
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn pending_then_fired_under_their_headings_and_following_the_store(cx: &mut TestAppContext) {
        let _home = shelbi_app::settings::agents::fixture::Home::new("dt-reminders", PROJECT, crate::test_env::lock());
        let fired_one = reminder("r8", "check deploy", -20, None);
        set_reminders(vec![
            reminder("r9", "ask about devbox relay", 120, Some(86_400)),
            reminder("r12", "check PR #2311 CI", 14, None),
        ]);
        record_fired_reminders(PROJECT, vec![FiredReminder::fired(&fired_one, fired_one.due_at)], Utc::now()).unwrap();
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 3), "the first read lands");
        let ids: Vec<String> = view.read_with(&vcx, |v, _| v.model().rows().iter().map(|r| r.id.clone()).collect());
        assert_eq!(ids, ["r12", "r9", "r8"], "pending soonest first, then fired");
        let fired_key = view.read_with(&vcx, |v, _| v.model().rows()[2].key.clone());
        let mut tops = Vec::new();
        for selector in [
            "reminders-pending-heading".to_string(),
            "reminder-row-r12".to_string(),
            "reminder-row-r9".to_string(),
            "reminders-fired-heading".to_string(),
            format!("reminder-row-{fired_key}"),
        ] {
            tops.push(vcx.debug_bounds(sel(selector.clone())).unwrap_or_else(|| panic!("{selector} drawn")).top());
        }
        assert!(tops.windows(2).all(|w| w[0] < w[1]), "headings above their rows: {tops:?}");
        assert!(vcx.debug_bounds("reminders-title").is_some() && vcx.debug_bounds("reminders-summary").is_some());
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        assert_eq!(model.summary().as_deref(), Some("2 pending · 1 fired"));
        let row = vcx.debug_bounds("reminder-row-r12").unwrap();
        assert!(row.right() <= px(1000.0), "no sideways scroll");

        // A reminder added elsewhere appears; a canceled one goes.
        set_reminders(vec![reminder("r9", "ask about devbox relay", 120, Some(86_400)), reminder("r15", "new", 5, None)]);
        assert!(
            read_until(&view, &mut vcx, |m| m.rows().first().is_some_and(|r| r.id == "r15") && m.pending_count() == 2),
            "the new reminder appears, the canceled one goes"
        );
        assert!(vcx.debug_bounds("reminder-row-r12").is_none());
        set_reminders(vec![]);
        std::fs::remove_file(shelbi_state::awareness::reminder_history_path(PROJECT).unwrap()).unwrap();
        assert!(read_until(&view, &mut vcx, |m| *m == RemindersModel::Empty), "nothing left");
        assert!(vcx.debug_bounds("reminders-empty").is_some());
        assert_eq!(view.read_with(&vcx, |v, _| v.model().message()).as_deref(), Some(EMPTY_TEXT));
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_rows_sheet_shows_its_details_and_cancel_removes_a_pending_reminder(cx: &mut TestAppContext) {
        let _home =
            shelbi_app::settings::agents::fixture::Home::new("dt-reminders-sheet", PROJECT, crate::test_env::lock());
        set_reminders(vec![reminder("r9", "ask about devbox relay", 120, Some(86_400)), reminder("r12", "check CI", 14, None)]);
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 2));
        assert!(vcx.debug_bounds("reminder-sheet").is_none());

        click(&mut vcx, "reminder-row-r9");
        vcx.executor().advance_clock(SLIDE);
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        let sheet = vcx.debug_bounds("reminder-sheet").expect("the sheet is drawn");
        let row = vcx.debug_bounds("reminder-row-r9").unwrap();
        // It floats over the table, which keeps its full width under it, in
        // from the screen's right edge.
        assert!(sheet.left() < row.right() && sheet.right() > row.center().x, "over the table: {sheet:?} {row:?}");
        assert_eq!(sheet.right(), px(1000.0 - EDGE), "{sheet:?}");
        assert_eq!(sheet.size.width, px(DEFAULT_W), "the shared pane's width");
        assert!(row.right() > px(1000.0 - 40.0), "the table keeps its width: {row:?}");
        for sel in [
            "reminder-sheet-title",
            "reminder-sheet-status",
            "reminder-sheet-text",
            "reminder-detail-due",
            "reminder-detail-repeats",
            "reminder-detail-created",
            "reminder-sheet-cancel",
        ] {
            assert!(vcx.debug_bounds(sel).is_some(), "{sel} drawn");
        }

        // Another row's click (on the part the pane leaves showing) shows
        // that reminder; its own row keeps it.
        let uncovered = |vcx: &mut VisualTestContext, selector: &'static str| {
            let row = vcx.debug_bounds(selector).unwrap();
            gpui::point(row.left() + px(20.0), row.center().y)
        };
        let at = uncovered(&mut vcx, "reminder-row-r12");
        vcx.simulate_click(at, gpui::Modifiers::none());
        vcx.run_until_parked();
        assert_eq!(view.read_with(&vcx, |v, _| v.sheet_row().map(|r| r.id.clone())).as_deref(), Some("r12"));
        let at = uncovered(&mut vcx, "reminder-row-r9");
        vcx.simulate_click(at, gpui::Modifiers::none());
        vcx.simulate_click(at, gpui::Modifiers::none());
        vcx.run_until_parked();
        assert_eq!(view.read_with(&vcx, |v, _| v.sheet_row().map(|r| r.id.clone())).as_deref(), Some("r9"));

        // Escape closes it, sliding out: drawn, inert, for one slide.
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert!(view.read_with(&vcx, |v, _| v.sheet_row().is_none()), "Escape closes the sheet");
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(vcx.debug_bounds("reminder-sheet-closing").is_some(), "it slides out");
        assert!(vcx.debug_bounds("reminder-sheet").is_none());
        slid(&mut vcx);
        assert!(vcx.debug_bounds("reminder-sheet-closing").is_none(), "gone once the slide is up");
        // Close does the same.
        click(&mut vcx, "reminder-row-r9");
        slid(&mut vcx);
        click(&mut vcx, "reminder-sheet-close");
        assert!(view.read_with(&vcx, |v, _| v.sheet_row().is_none()), "Close closes the sheet");
        slid(&mut vcx);

        // Cancel removes it from the store, and the sheet closes with it.
        click(&mut vcx, "reminder-row-r9");
        slid(&mut vcx);
        click(&mut vcx, "reminder-sheet-cancel");
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 1), "the reminder is canceled");
        let pending: Vec<String> = shelbi_state::awareness::read_awareness(PROJECT)
            .unwrap()
            .reminders
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(pending, ["r12"], "the store lost it, as `shelbi remind cancel` would");
        assert!(view.read_with(&vcx, |v, _| v.sheet_row().is_none()), "its sheet closes");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_fired_reminders_sheet_has_no_cancel(cx: &mut TestAppContext) {
        let _home =
            shelbi_app::settings::agents::fixture::Home::new("dt-reminders-fired", PROJECT, crate::test_env::lock());
        let r = reminder("r8", "check deploy", -20, None);
        record_fired_reminders(PROJECT, vec![FiredReminder::fired(&r, r.due_at)], Utc::now()).unwrap();
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 1));
        let key = view.read_with(&vcx, |v, _| v.model().rows()[0].key.clone());
        click(&mut vcx, sel(format!("reminder-row-{key}")));
        slid(&mut vcx);
        assert!(vcx.debug_bounds("reminder-sheet").is_some());
        assert!(vcx.debug_bounds("reminder-detail-fired").is_some());
        assert!(vcx.debug_bounds("reminder-sheet-cancel").is_none(), "a fired reminder has no Cancel");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_remote_window_says_reminders_are_not_available(cx: &mut TestAppContext) {
        let handle = ProjectHandle::null(ProjectKey::remote("devbox", "web"));
        let (view, mut vcx) = open(cx, handle);
        assert!(read_until(&view, &mut vcx, |m| *m == RemindersModel::Unavailable));
        assert!(vcx.debug_bounds("reminders-empty").is_some());
        assert_eq!(view.read_with(&vcx, |v, _| v.model().message()).as_deref(), Some(REMOTE_TEXT));
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn dragging_the_sheets_left_edge_resizes_it_and_reports_the_width(cx: &mut TestAppContext) {
        let _home =
            shelbi_app::settings::agents::fixture::Home::new("dt-reminders-resize", PROJECT, crate::test_env::lock());
        set_reminders(vec![reminder("r9", "ask about devbox relay", 120, Some(86_400))]);
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        let saved = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = saved.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, ev: &RemindersEvent, _| {
                let RemindersEvent::PaneWidth(w) = ev;
                sink.borrow_mut().push(*w);
            })
            .detach()
        });
        // The window's saved width.
        view.update(&mut vcx, |v, cx| v.set_pane_width(600.0, cx));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 1));
        click(&mut vcx, "reminder-row-r9");
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(vcx.debug_bounds("reminder-sheet").unwrap().size.width, px(600.0), "the saved width");

        let edge = vcx.debug_bounds("reminder-sheet-resize").unwrap().center();
        let to = gpui::point(edge.x - px(100.0), edge.y);
        vcx.simulate_mouse_down(edge, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.simulate_mouse_move(to, Some(gpui::MouseButton::Left), gpui::Modifiers::none());
        vcx.simulate_mouse_up(to, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.run_until_parked();
        assert_eq!(view.read_with(&vcx, |v, _| v.pane_width()), 700.0);
        assert_eq!(vcx.debug_bounds("reminder-sheet").unwrap().size.width, px(700.0));
        assert_eq!(*saved.borrow(), [700.0], "the window gets the new width");
        view.update(&mut vcx, |v, _| v.stop_workers());
    }
}
