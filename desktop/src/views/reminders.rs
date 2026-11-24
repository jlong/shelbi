//! The Reminders screen (`View::Reminders`, palette-only, and left out of
//! the palette's default list until the user types: `rem…` finds it): the
//! project's `shelbi remind` reminders as an agenda, drawn from the shared
//! [`shelbi_app::reminders`] model the TUI's Reminders page draws too.
//!
//! A reminder is a point in time coming up, so the screen reads like a day
//! planner, in the Jobs screen's vocabulary ([`super::list_screen`]: its
//! title row, centered column, section headings, cards and empty state):
//!
//! ```text
//!  ⏰ Reminders  3 pending · 5 fired
//!  ──────────────────────────────────────────────────────────────
//!    Next up
//!    ╭──────────────────────────────────────────────────────────╮
//!    │ (◷) in 12m                              10:45 AM  [r7]   │
//!    │     Check PR #2424 CI and merge if green                 │
//!    │     ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░           set 48m ago │
//!    ╰──────────────────────────────────────────────────────────╯
//!    Today
//!    ╭──────────────────────────────────────────────────────────╮
//!    │  2:00 PM  ◷ Look at the devbox relay           in 3h 27m │
//!    │  5:30 PM  ↻ Nightly: check stale…  every 1d    in 6h 57m │
//!    ╰──────────────────────────────────────────────────────────╯
//!    Tomorrow
//!    ╭──────────────────────────────────────────────────────────╮
//!    │  9:00 AM  ◷ Ping the Homebrew tap PR             in 22h  │
//!    ╰──────────────────────────────────────────────────────────╯
//!    Fired 3
//!    ╭──────────────────────────────────────────────────────────╮
//!    │ Today      10:02 AM  ✓ Re-check alpha's build job        │
//!    │             8:15 AM  ✓ Look at release v0.14 CI          │
//!    │ Yesterday  11:40 PM  ✓ Morning summary for the user      │
//!    ╰──────────────────────────────────────────────────────────╯
//! ```
//!
//! The soonest pending reminder is the hero: a card with its glyph (a clock,
//! or the repeat arrows for one that repeats) in a tinted disc, a live
//! countdown (`in 12m`, ticking over with each read), its due time, its
//! repeat and its id, its whole text (up to two lines), and a bar from when
//! it was set to when it is due. The rest are grouped by their local day
//! (`Today`, `Tomorrow`, `Thursday`, `Mon, Dec 7`), one compact row each: the
//! time of day in a gutter, the glyph, the text, the repeat and how long
//! until it is due, right-aligned. A reminder past its time that the hub
//! hasn't fired yet (a hub that was down) wears the warning color, like a
//! job past its estimate, and the ones after Next up gather under `Overdue`.
//! Fired reminders are a dimmed history, newest first, the day in the
//! gutter of each day's first row. With none at all, the empty state shows
//! the clock and the command that sets one, ready to copy.
//!
//! Clicking any reminder opens its sheet: the shared floating pane
//! ([`super::floating_pane`], the issue pane's chrome) over the full-width
//! agenda, at the window's task pane width (a drag of its left edge sets it
//! for every pane), sliding in and back out. Its Header is `id · text` with
//! Close; it shows the reminder's status, when it is due (absolute and
//! relative), its repeat, when it was created, its whole text and, for a
//! pending one, a Cancel reminder button (`shelbi remind cancel`). Another
//! reminder's click shows that one; Escape or Close closes it.
//!
//! Like the Jobs screen, it re-reads the store and the reminder history
//! every [`REFRESH_INTERVAL`] while it is open, off the UI thread through
//! the window's [`ProjectSource`] (a remote window's source says the far
//! store isn't readable yet), so a reminder appears, fires, or goes on its
//! own; each read also repaints, which ticks the countdowns over.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use gpui::{
    div, prelude::*, px, relative, AnyElement, ClickEvent, Context, EventEmitter, FocusHandle, Hsla, KeyDownEvent,
    ScrollHandle, SharedString, Task, Window,
};
use shelbi_app::jobs::one_line;
use shelbi_app::reminders::{
    ReminderRow, RemindersModel, RemindersRead, SheetState, CANCEL_LABEL, EMPTY_COMMAND, EMPTY_DETAIL, EMPTY_TITLE,
    FIRED_HEADING, NEXT_UP_HEADING, REFRESH_INTERVAL, TITLE,
};
use shelbi_app::source::{ProjectHandle, ProjectSource};
use shelbi_app::theme::Tone;

use super::floating_pane::{
    self, header_button, property_row, value_text, PaneFrame, PaneHost, PaneIds, PaneMotion, SlideOut, INSET,
};
use super::list_screen::{self, section_heading, tint, CARD_RADIUS};
use crate::assets::{glyph, glyphs, heroicon, icons, Icon};
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// How often the view drains its refresher.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The Next up card's padding, its glyph's disc, and its bar.
const CARD_PAD: f32 = 14.0;
const DISC: f32 = 32.0;
const CARD_BAR_H: f32 = 6.0;
/// The bar's words (`set 48m ago`).
const CARD_BAR_LABEL_W: f32 = 96.0;
/// A row's glyph.
const GLYPH: f32 = 14.0;
/// The agenda's time gutter (`11:40 PM`), and the fired history's day
/// gutter before it (`Yesterday`, `Mon, Nov 16`).
const TIME_W: f32 = 64.0;
const DAY_W: f32 = 84.0;
/// The countdown column, right-aligned so it lines up across rows and days
/// (`in 23h 59m`, `due 20m ago`).
const COUNTDOWN_W: f32 = 88.0;
/// A text longer than this many characters may be cut: it gets a tooltip
/// with the whole text.
const TIP_CHARS: usize = 40;

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
                .text_color(theme.tone(row.status_tone_at(now)))
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
        if self.model == RemindersModel::Empty {
            return list_screen::empty_state("reminders-empty", icons::CLOCK, EMPTY_TITLE, EMPTY_DETAIL, EMPTY_COMMAND, theme);
        }
        if let Some(message) = self.model.message() {
            return list_screen::message("reminders-empty", message, theme);
        }
        let now = super::activity::now();
        let open = |row: &ReminderRow| self.sheet.as_ref().is_some_and(|s| s.row.key == row.key);
        let (pending, fired) = (self.model.pending(), self.model.fired());
        let mut column = list_screen::content();
        let mut spaced = false;
        if let Some(next) = self.model.next_up() {
            column = column.child(section_heading("reminders-next-heading", NEXT_UP_HEADING.to_string(), None, false, theme));
            column = column.child(next_up_card(next, now, open(next), theme, cx));
            spaced = true;
        }
        let days = self.model.agenda_days(now, &Local);
        for (n, (start, label)) in days.iter().enumerate() {
            let end = days.get(n + 1).map_or(pending.len(), |(next, _)| *next);
            column = column.child(section_heading(format!("reminders-day-{n}-heading"), label.clone(), None, spaced, theme));
            let rows = pending[*start..end].iter().map(|row| agenda_row(row, now, open(row), theme, cx));
            column = column.child(list_screen::group_card(format!("reminders-day-{n}"), rows, theme));
            spaced = true;
        }
        if !fired.is_empty() {
            column = column.child(section_heading(
                "reminders-fired-heading",
                FIRED_HEADING.to_string(),
                Some(fired.len()),
                spaced,
                theme,
            ));
            let days = self.model.fired_days(now, &Local);
            let rows = fired.iter().enumerate().map(|(i, row)| {
                let day = days.iter().find(|(at, _)| *at == i).map(|(_, label)| label.clone());
                fired_row(row, day, open(row), theme, cx)
            });
            column = column.child(list_screen::group_card("reminders-fired".to_string(), rows, theme));
        }
        list_screen::scrolling("reminders", &self.scroll, column)
    }
}

/// A debug selector `reminder-<part>-<key>`.
fn selector(part: &'static str, key: &str) -> impl FnOnce() -> String {
    let s = format!("reminder-{part}-{key}");
    move || s
}

/// A reminder's glyph: the repeat arrows for one that repeats, else the
/// clock.
fn glyph_icon(row: &ReminderRow) -> Icon {
    if row.every.is_some() {
        icons::ARROW_PATH
    } else {
        icons::CLOCK
    }
}

/// A pending reminder's accent at `now`: blue, the warning color once past
/// its time.
fn accent(row: &ReminderRow, now: DateTime<Utc>, theme: &Theme) -> (Tone, Hsla) {
    let tone = row.tone_at(now).unwrap_or(Tone::Blue);
    let color = if row.overdue(now) { theme.warning } else { theme.tone(tone) };
    (tone, color)
}

/// The soonest pending reminder's card: its glyph in a tinted disc, the
/// countdown, the due time, repeat and id; its whole text (two lines at
/// most); the bar from when it was set to when it is due.
fn next_up_card(
    row: &ReminderRow,
    now: DateTime<Utc>,
    open: bool,
    theme: &Theme,
    cx: &mut Context<RemindersView>,
) -> AnyElement {
    let (tone, color) = accent(row, now, theme);
    let key = row.key.clone();
    let disc = div()
        .debug_selector(selector("glyph", &row.key))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(DISC))
        .rounded_full()
        .bg(tint(tone, theme))
        .child(heroicon(glyph_icon(row)).size(px(16.0)).text_color(color));
    let top = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .min_w_0()
        .child(
            div()
                .debug_selector(selector("countdown", &row.key))
                .flex_1()
                .min_w_0()
                .truncate()
                .type_style(Type::LargeSemibold)
                .text_color(color)
                .child(row.countdown(now)),
        )
        .when(row.every.is_some(), |d| {
            d.child(
                div()
                    .debug_selector(selector("repeats", &row.key))
                    .flex_none()
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(row.repeats()),
            )
        })
        .child(
            div()
                .debug_selector(selector("due", &row.key))
                .flex_none()
                .type_style(Type::Small)
                .text_color(theme.foreground)
                .child(row.due_clock(now, &Local)),
        )
        .child(
            div()
                .debug_selector(selector("id", &row.key))
                .flex()
                .flex_none()
                .items_center()
                .h(px(20.0))
                .px(px(6.0))
                .rounded(px(4.0))
                .bg(theme.fill_on(theme.background, theme.card))
                .type_style(Type::XSmallMedium)
                .text_color(theme.muted)
                .child(row.id.clone()),
        );
    let text = div()
        .id(SharedString::from(format!("reminder-text-{}", row.key)))
        .debug_selector(selector("text", &row.key))
        .min_w_0()
        .line_clamp(2)
        .type_style(Type::BaseMedium)
        .text_color(theme.text_loud)
        .tooltip(style::tooltip(row.text.clone()))
        .child(row.text.clone());
    let bar = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .debug_selector(selector("bar", &row.key))
                .flex_1()
                .h(px(CARD_BAR_H))
                .rounded_full()
                .bg(theme.search)
                .child(div().h_full().w(relative(row.progress(now))).rounded_full().bg(color)),
        )
        .child(
            div()
                .debug_selector(selector("bar-label", &row.key))
                .flex_none()
                .w(px(CARD_BAR_LABEL_W))
                .text_right()
                .whitespace_nowrap()
                .type_style(Type::XSmall)
                .text_color(theme.muted)
                .child(row.set_label(now)),
        );
    div()
        .id(SharedString::from(format!("reminder-row-{}", row.key)))
        .debug_selector(selector("row", &row.key))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .min_w_0()
        .p(px(CARD_PAD))
        .rounded(px(CARD_RADIUS))
        .bg(theme.card)
        // The open reminder's card wears the focus ring, as the board's
        // selected card does.
        .shadow(if open && theme.translucent() {
            style::focus_shadow_inset(theme)
        } else if open {
            style::focus_shadow(theme)
        } else {
            style::card_shadow(theme)
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_sheet(&key, cx)))
        .child(disc)
        .child(div().flex().flex_col().flex_1().min_w_0().gap(px(8.0)).child(top).child(text).child(bar))
        .into_any_element()
}

/// A pending reminder's agenda row: the time of day, its glyph, its text
/// (whole in a tooltip when cut), its repeat, and how long until it is due.
fn agenda_row(
    row: &ReminderRow,
    now: DateTime<Utc>,
    open: bool,
    theme: &Theme,
    cx: &mut Context<RemindersView>,
) -> AnyElement {
    let (_, color) = accent(row, now, theme);
    let overdue = row.overdue(now);
    let key = row.key.clone();
    let text = one_line(&row.text);
    let long = text.chars().count() > TIP_CHARS;
    list_screen::compact_row(format!("reminder-row-{}", row.key), open, theme)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_sheet(&key, cx)))
        .type_style(Type::Small)
        .child(
            div()
                .debug_selector(selector("time", &row.key))
                .flex_none()
                .w(px(TIME_W))
                .text_right()
                .whitespace_nowrap()
                .text_color(if overdue { theme.warning } else { theme.muted })
                .child(row.gutter_time(&Local)),
        )
        .child(
            div()
                .debug_selector(selector("glyph", &row.key))
                .flex_none()
                .child(heroicon(glyph_icon(row)).size(px(GLYPH)).text_color(if overdue { theme.warning } else { theme.icon })),
        )
        .child(
            div()
                .id(SharedString::from(format!("reminder-text-{}", row.key)))
                .debug_selector(selector("text", &row.key))
                .flex_1()
                .min_w_0()
                .truncate()
                .type_style(Type::Base)
                .text_color(theme.text_loud)
                .when(long, |d| d.tooltip(style::tooltip(row.text.clone())))
                .child(text),
        )
        .when(row.every.is_some(), |d| {
            d.child(
                div()
                    .debug_selector(selector("repeats", &row.key))
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(theme.muted)
                    .child(row.repeats()),
            )
        })
        .child(
            div()
                .debug_selector(selector("countdown", &row.key))
                .flex_none()
                .w(px(COUNTDOWN_W))
                .text_right()
                .whitespace_nowrap()
                .text_color(color)
                .child(row.countdown(now)),
        )
        .into_any_element()
}

/// A fired reminder's history row, dimmed: the day (on each day's first
/// row), the time it fired, a check, its text and its repeat.
fn fired_row(
    row: &ReminderRow,
    day: Option<String>,
    open: bool,
    theme: &Theme,
    cx: &mut Context<RemindersView>,
) -> AnyElement {
    let key = row.key.clone();
    let text = one_line(&row.text);
    let long = text.chars().count() > TIP_CHARS;
    list_screen::compact_row(format!("reminder-row-{}", row.key), open, theme)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_sheet(&key, cx)))
        .type_style(Type::Small)
        .text_color(theme.muted)
        .child(
            div()
                .debug_selector(selector("day", &row.key))
                .flex_none()
                .w(px(DAY_W))
                .truncate()
                .when_some(day, |d, day| d.child(day)),
        )
        .child(
            div()
                .debug_selector(selector("time", &row.key))
                .flex_none()
                .w(px(TIME_W))
                .text_right()
                .whitespace_nowrap()
                .child(row.gutter_time(&Local)),
        )
        .child(
            div()
                .debug_selector(selector("glyph", &row.key))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(GLYPH))
                .child(glyph(glyphs::CHECK).text_color(theme.muted)),
        )
        .child(
            div()
                .id(SharedString::from(format!("reminder-text-{}", row.key)))
                .debug_selector(selector("text", &row.key))
                .flex_1()
                .min_w_0()
                .truncate()
                .type_style(Type::Base)
                .text_color(theme.muted)
                .when(long, |d| d.tooltip(style::tooltip(row.text.clone())))
                .child(text),
        )
        .when(row.every.is_some(), |d| {
            d.child(
                div()
                    .debug_selector(selector("repeats", &row.key))
                    .flex_none()
                    .whitespace_nowrap()
                    .child(row.repeats()),
            )
        })
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
            .child(list_screen::title_row("reminders", Some(icons::CLOCK), TITLE, summary, &theme))
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
    use shelbi_app::reminders::REMOTE_TEXT;
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
    fn next_up_then_the_agenda_then_fired_and_following_the_store(cx: &mut TestAppContext) {
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
            "reminders-next-heading".to_string(),
            "reminder-row-r12".to_string(),
            "reminders-day-0-heading".to_string(),
            "reminder-row-r9".to_string(),
            "reminders-fired-heading".to_string(),
            format!("reminder-row-{fired_key}"),
        ] {
            tops.push(vcx.debug_bounds(sel(selector.clone())).unwrap_or_else(|| panic!("{selector} drawn")).top());
        }
        assert!(tops.windows(2).all(|w| w[0] < w[1]), "headings above their rows: {tops:?}");
        // The title wears the clock, with the counts.
        for s in ["reminders-title-icon", "reminders-title", "reminders-summary"] {
            assert!(vcx.debug_bounds(s).is_some(), "{s} drawn");
        }
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        assert_eq!(model.summary().as_deref(), Some("2 pending · 1 fired"));
        // Next up counts down to it, with its due time and a set→due bar;
        // the agenda row has its time, its repeat and its countdown.
        for s in [
            "reminder-countdown-r12",
            "reminder-due-r12",
            "reminder-id-r12",
            "reminder-bar-r12",
            "reminder-bar-label-r12",
            "reminder-time-r9",
            "reminder-glyph-r9",
            "reminder-repeats-r9",
            "reminder-countdown-r9",
        ] {
            assert!(vcx.debug_bounds(s).is_some(), "{s} drawn");
        }
        assert!(vcx.debug_bounds("reminder-repeats-r12").is_none(), "a one-shot reminder has no repeat");
        let next = model.next_up().unwrap();
        assert!(next.countdown(Utc::now()).starts_with("in 1"), "{}", next.countdown(Utc::now()));
        // The fired row is history: its day, its time, no countdown.
        assert!(vcx.debug_bounds(sel(format!("reminder-day-{fired_key}"))).is_some());
        assert!(vcx.debug_bounds(sel(format!("reminder-countdown-{fired_key}"))).is_none());
        let row = vcx.debug_bounds("reminder-row-r12").unwrap();
        assert!(row.right() <= px(1000.0), "no sideways scroll");

        // A reminder added elsewhere appears; a canceled one goes.
        set_reminders(vec![reminder("r9", "ask about devbox relay", 120, Some(86_400)), reminder("r15", "new", 5, None)]);
        assert!(
            read_until(&view, &mut vcx, |m| m.rows().first().is_some_and(|r| r.id == "r15") && m.pending_count() == 2),
            "the new reminder appears, the canceled one goes"
        );
        assert!(vcx.debug_bounds("reminder-row-r12").is_none());
        assert!(vcx.debug_bounds("reminder-countdown-r15").is_some(), "the new one is up next");
        set_reminders(vec![]);
        std::fs::remove_file(shelbi_state::awareness::reminder_history_path(PROJECT).unwrap()).unwrap();
        assert!(read_until(&view, &mut vcx, |m| *m == RemindersModel::Empty), "nothing left");
        // The empty state: the clock, and the command that sets one.
        for s in ["reminders-empty", "reminders-empty-icon", "reminders-empty-command"] {
            assert!(vcx.debug_bounds(s).is_some(), "{s} drawn");
        }
        view.update(&mut vcx, |v, _| v.stop_workers());
    }

    #[gpui::test]
    fn a_reminder_past_its_time_shows_overdue(cx: &mut TestAppContext) {
        let _home =
            shelbi_app::settings::agents::fixture::Home::new("dt-reminders-overdue", PROJECT, crate::test_env::lock());
        set_reminders(vec![
            reminder("r3", "missed while the hub was down", -30, None),
            reminder("r4", "also missed", -10, None),
            reminder("r5", "still ahead", 30, None),
        ]);
        let (view, mut vcx) = open(cx, shelbi_orchestrator::source::local_handle(PROJECT));
        assert!(read_until(&view, &mut vcx, |m| m.rows().len() == 3));
        let model = view.read_with(&vcx, |v, _| v.model().clone());
        let now = Utc::now();
        assert_eq!(model.next_up().map(|r| r.id.as_str()), Some("r3"));
        assert!(model.next_up().unwrap().overdue(now));
        assert_eq!(model.next_up().unwrap().countdown(now), "due 30m ago");
        let days = model.agenda_days(now, &Local);
        assert_eq!(days[0], (1, shelbi_app::reminders::OVERDUE_HEADING.to_string()));
        for s in ["reminder-countdown-r3", "reminder-row-r4", "reminders-day-0-heading", "reminder-row-r5"] {
            assert!(vcx.debug_bounds(s).is_some(), "{s} drawn");
        }
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
