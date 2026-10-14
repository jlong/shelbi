//! The Activity feed, rendered from the shared feed model
//! (`shelbi_app::activity`): the same rows, order and glyphs as the TUI's
//! Activity view (`crates/shelbi-tui/src/activity.rs`), laid out natively.
//!
//! The view is a thin gpui shell over [`ActivityState`] (`activity_state.rs`).
//! Its data comes from the shared activity reader on its own background
//! refresher (`shelbi_app::spawn_worker` over
//! `shelbi_app::activity::read_activity_window`), re-read on the TUI's 750ms
//! cadence so new events appear live. The read is windowed: it parses only the
//! newest lines of `events.log`, and "Load older" re-reads with a window one
//! page larger. Task titles resolve incrementally into a cache the refresher
//! keeps across reads, never on the UI thread.
//!
//! Clicking (or Enter on) a row opens what it is about: a task row asks the
//! window to open the task's detail ([`ActivityEvent::OpenTask`]); a workspace
//! row runs `CommandKind::FocusWorkspace` through the window's dispatch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{
    div, prelude::*, px, svg, App, ClickEvent, Context, EventEmitter, FocusHandle, FontWeight,
    Hsla, KeyDownEvent, ScrollHandle, SharedString, Task, Window,
};
use shelbi_app::activity::{
    read_activity_window, ActivityData, ActivityKind, ActivityMetaCache, FeedItem, FeedRow,
    RowTarget,
};
use shelbi_app::CommandKind;
use shelbi_state::keymap::{format_chord, ActivityAction, DisplayStyle};

use super::activity_state::{ActivityState, Intent, Picker};
use crate::assets::icons;
use crate::keys::chord_from_keystroke;
use crate::theme::Theme;

/// The TUI's activity refresh cadence.
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
/// How often the view drains its refresher.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// The identity-chip column's width, so messages line up across rows (the
/// TUI's 9-cell chip column). A wider chip pushes only its own row over.
const CHIP_WIDTH: f32 = 84.0;
const ICON: f32 = 12.0;

/// What the feed asks of the window.
#[derive(Debug, Clone)]
pub enum ActivityEvent {
    /// Show the task's detail (on the Issues board).
    OpenTask(String),
    /// Run this registry command through `commands::dispatch_command`.
    Command(CommandKind),
}

pub struct ActivityView {
    state: ActivityState,
    focus: FocusHandle,
    needs_focus: bool,
    refresher: shelbi_app::Refresher<ActivityData>,
    /// The line window the refresher reads with; "load older" grows it.
    window: Arc<AtomicUsize>,
    last_refresh: Instant,
    scroll: ScrollHandle,
    display_style: DisplayStyle,
    _poll: Task<()>,
}

impl EventEmitter<ActivityEvent> for ActivityView {}

impl ActivityView {
    pub fn new(project: String, cx: &mut Context<Self>) -> Self {
        let (keymaps, diagnostics) = shelbi_state::keymap::load_keymaps(Some(&project));
        for d in diagnostics {
            tracing::warn!("keys.yaml: {d:?}");
        }
        let state = ActivityState::new(project.clone(), keymaps);
        let window = Arc::new(AtomicUsize::new(state.window()));
        // The title cache lives as long as the view, so each read only
        // resolves ids it hasn't seen (the TUI shell's incremental rule).
        let cache = Arc::new(Mutex::new(ActivityMetaCache::default()));
        let (p, w) = (project.clone(), window.clone());
        let refresher = shelbi_app::spawn_worker(move |_| {
            read_activity_window(&p, &cache, w.load(Ordering::Acquire))
        });
        refresher.request();
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                break;
            }
        });
        ActivityView {
            state,
            focus: cx.focus_handle(),
            needs_focus: true,
            refresher,
            window,
            last_refresh: Instant::now(),
            scroll: ScrollHandle::new(),
            display_style: DisplayStyle::detect(),
            _poll: poll,
        }
    }

    /// Take keyboard focus on the next frame (the window calls this when the
    /// view is shown).
    pub fn request_focus(&mut self) {
        self.needs_focus = true;
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if let Some(data) = self.refresher.latest() {
            self.state.apply(data, Utc::now());
            cx.notify();
        }
        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresh();
        }
    }

    fn refresh(&mut self) {
        self.refresher.request();
        self.last_refresh = Instant::now();
    }

    fn apply_intents(&mut self, intents: Vec<Intent>, cx: &mut Context<Self>) {
        for intent in intents {
            match intent {
                Intent::Open(RowTarget::Task(id)) => cx.emit(ActivityEvent::OpenTask(id)),
                Intent::Open(RowTarget::Workspace(workspace)) => {
                    cx.emit(ActivityEvent::Command(CommandKind::FocusWorkspace { workspace }))
                }
                Intent::Refresh => self.refresh(),
                Intent::LoadWindow(lines) => {
                    self.window.store(lines, Ordering::Release);
                    self.refresh();
                }
                Intent::Reveal(ix) => self.scroll.scroll_to_item(ix),
            }
        }
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(chord) = chord_from_keystroke(&ev.keystroke) else {
            return;
        };
        let Some(intents) = self.state.handle_key(chord, Utc::now()) else {
            return;
        };
        cx.stop_propagation();
        self.apply_intents(intents, cx);
    }

    fn load_older(&mut self, cx: &mut Context<Self>) {
        if let Some(intent) = self.state.load_older() {
            self.apply_intents(vec![intent], cx);
        }
    }

    /// Drain the background sources now (the offscreen check's dispatcher
    /// doesn't run the poll timer).
    #[cfg(feature = "visual-check")]
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        self.tick(cx);
    }

    /// The view's state, for the offscreen views check.
    #[cfg(feature = "visual-check")]
    pub fn state(&self) -> &ActivityState {
        &self.state
    }

    #[cfg(feature = "visual-check")]
    pub fn state_mut(&mut self) -> &mut ActivityState {
        &mut self.state
    }

    // ---- rendering --------------------------------------------------------

    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = &self.state.filter;
        let pill = |id: &'static str, label: &'static str, active: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_2()
                .py(px(2.0))
                .rounded_md()
                .cursor_pointer()
                .text_xs()
                .when(active, |d| {
                    d.bg(theme.selection)
                        .text_color(theme.white)
                        .font_weight(FontWeight::BOLD)
                })
                .when(!active, |d| d.text_color(theme.muted).hover(|s| s.bg(theme.search)))
                .child(label)
        };
        let mut pills = div().flex().flex_row().items_center().gap_1().child(
            pill("kind-all", "All", filter.is_all()).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.state.clear_kinds(Utc::now());
                    cx.notify();
                },
            )),
        );
        for kind in ActivityKind::ALL {
            let id = match kind {
                ActivityKind::Tasks => "kind-tasks",
                ActivityKind::Zen => "kind-zen",
                ActivityKind::Workspaces => "kind-workspaces",
                ActivityKind::System => "kind-system",
            };
            pills = pills.child(pill(id, kind.label(), filter.has_kind(kind)).on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.state.toggle_kind(kind, Utc::now());
                    cx.notify();
                }),
            ));
        }
        let project_label = filter
            .project
            .clone()
            .unwrap_or_else(|| "All projects".to_string());
        let workspace_label = filter
            .workspace
            .clone()
            .unwrap_or_else(|| "All workspaces".to_string());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.white)
                            .child("Activity"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted)
                            .child(self.state.project.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(pills)
                    .child(
                        super::issues::chip(theme, icons::FUNNEL, project_label)
                            .id("project-picker")
                            .debug_selector(|| "project-picker".into())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.state.toggle_picker(Picker::Project);
                                cx.notify();
                            })),
                    )
                    .child(
                        super::issues::chip(theme, icons::USER, workspace_label)
                            .id("workspace-picker")
                            .debug_selector(|| "workspace-picker".into())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.state.toggle_picker(Picker::Workspace);
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_feed(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let feed = self.state.feed();
        let placeholder = |text: SharedString| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme.muted)
                .child(text)
                .into_any_element()
        };
        if !self.state.is_loaded() {
            return placeholder("Loading…".into());
        }
        if let Some(empty) = feed.empty {
            // A filter can hide every loaded row while older ones may match.
            if !(self.state.has_older() && !self.state.filter.is_unfiltered()) {
                return placeholder(empty.into());
            }
        }
        let mut list = div()
            .id("activity-feed")
            .debug_selector(|| "activity-feed".into())
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .pb_2();
        for (ix, item) in feed.items.iter().enumerate() {
            list = list.child(match item {
                FeedItem::Day(label) => day_header(theme, label).into_any_element(),
                FeedItem::Row(row) => {
                    let selected = self.state.selected == Some(ix);
                    feed_row(theme, row, selected)
                        .id(("activity-row", ix))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let intents = this.state.click(ix);
                            this.apply_intents(intents, cx);
                        }))
                        .into_any_element()
                }
            });
        }
        if let Some(empty) = feed.empty {
            list = list.child(
                div()
                    .py_4()
                    .text_color(theme.muted)
                    .child(empty),
            );
        }
        list.child(self.render_paging(theme, cx)).into_any_element()
    }

    fn render_paging(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let loaded = format!("{} events loaded", self.state.loaded_events());
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .py_3()
            .text_xs()
            .text_color(theme.muted);
        if self.state.is_loading_older() {
            row.child("Loading older events…").child(loaded)
        } else if self.state.has_older() {
            row.child(
                super::issues::chip(theme, icons::ARROW_PATH, "Load older".into())
                    .id("load-older")
                    .debug_selector(|| "load-older".into())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.load_older(cx))),
            )
            .child(loaded)
        } else {
            row.child("Start of the log").child(loaded)
        }
    }

    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
        let km = self.state.keymaps();
        let h = |a| {
            km.activity
                .first_chord_for(a)
                .map_or_else(|| "–".to_string(), |c| format_chord(c, self.display_style))
        };
        let hints = format!(
            "{}/{} select   enter open   {}/{} page   {} top   {} bottom   {} refresh   {}/{}/{} filter",
            h(ActivityAction::ScrollDown),
            h(ActivityAction::ScrollUp),
            h(ActivityAction::PageUp),
            h(ActivityAction::PageDown),
            h(ActivityAction::ScrollHome),
            h(ActivityAction::ScrollEnd),
            h(ActivityAction::Refresh),
            h(ActivityAction::ResetFilter),
            h(ActivityAction::ToggleZenFilter),
            h(ActivityAction::ToggleWorkspacesFilter),
        );
        div()
            .flex()
            .flex_row()
            .gap_4()
            .px_4()
            .py_1()
            .border_t_1()
            .border_color(theme.divider)
            .text_xs()
            .text_color(theme.muted)
            .child(div().flex_1().truncate().child(hints))
            .when_some(self.state.status().map(str::to_string), |d, s| {
                d.child(div().text_color(theme.ansi_light_red).child(s))
            })
    }

    fn render_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let picker = self.state.picker?;
        let (title, all_label, current, options, left) = match picker {
            Picker::Project => (
                "Project",
                "All projects",
                self.state.filter.project.clone(),
                self.state.projects().to_vec(),
                px(16.0),
            ),
            Picker::Workspace => (
                "Workspace",
                "All workspaces",
                self.state.filter.workspace.clone(),
                self.state.workspaces().to_vec(),
                px(180.0),
            ),
        };
        let choice = move |this: &mut Self, value: Option<String>| match picker {
            Picker::Project => this.state.set_project(value, Utc::now()),
            Picker::Workspace => this.state.set_workspace(value, Utc::now()),
        };
        let mut rows = vec![(all_label.to_string(), None)];
        rows.extend(options.into_iter().map(|o| (o.clone(), Some(o))));
        let items: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(ix, (label, value))| {
                let selected = value == current;
                picker_row(theme, selected, label)
                    .id(("picker-option", ix))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        choice(this, value.clone());
                        cx.notify();
                    }))
            })
            .collect();
        let dim: Hsla = theme.background.opacity(0.35);
        Some(
            div()
                .id("picker-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(dim)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.state.picker = None;
                    cx.notify();
                }))
                .child(
                    super::issues::panel(theme)
                        .id("picker-panel")
                        .debug_selector(|| "picker-panel".into())
                        .absolute()
                        .top(px(76.0))
                        .left(left)
                        .w(px(260.0))
                        .max_h(px(360.0))
                        .overflow_y_scroll()
                        .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                        .child(super::issues::panel_title(theme, title))
                        .children(items),
                ),
        )
    }
}

impl Render for ActivityView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_focus {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        let theme = *cx.global::<Theme>();
        div()
            .id("activity")
            .track_focus(&self.focus)
            .key_context("Activity")
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.canvas)
            .text_sm()
            .text_color(theme.gray)
            .child(self.render_header(&theme, cx))
            .child(self.render_feed(&theme, cx))
            .child(self.render_footer(&theme))
            .children(self.render_picker(&theme, cx))
    }
}

// ---- pieces -----------------------------------------------------------------

/// `── Today ─────`: the day label, then a rule to the edge.
fn day_header(theme: &Theme, label: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .pt_4()
        .pb_2()
        .text_xs()
        .text_color(theme.ansi_dark_gray)
        .child(div().w(px(12.0)).h(px(1.0)).bg(theme.divider))
        .child(label.to_string())
        .child(div().flex_1().h(px(1.0)).bg(theme.divider))
}

/// One feed row, laid out like the TUI's: the identity chip pinned left, the
/// dim verb, the title (wrapping, never truncated) with its trail and the dim
/// secondary line under it, and the state pill + time pinned right.
fn feed_row(theme: &Theme, row: &FeedRow, selected: bool) -> gpui::Div {
    let tone = |t| theme.tone(t);
    let chip = div()
        .flex_none()
        .min_w(px(CHIP_WIDTH))
        .pr_2()
        .when_some(row.chip.as_ref(), |d, chip| {
            d.text_color(tone(chip.tone))
                .when(chip.bold, |d| d.font_weight(FontWeight::BOLD))
                .child(chip.text())
        });
    let verb = div()
        .flex_none()
        .when(!row.verb.is_empty(), |d| {
            d.pr_2().text_color(tone(row.verb_tone)).child(row.verb.clone())
        });
    let message = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(2.0))
        .child(
            div()
                .text_color(tone(row.title_tone))
                .child(row.title.clone())
                .when_some(row.trail.as_ref(), |d, (text, t)| {
                    d.child(div().text_color(tone(*t)).child(text.clone()))
                })
                .flex()
                .flex_row()
                .flex_wrap(),
        )
        .when_some(row.secondary.as_ref(), |d, sec| {
            let (icon, text) = sec.parts();
            d.child(
                div()
                    .text_xs()
                    .text_color(theme.ansi_dark_gray)
                    .truncate()
                    .child(format!("{icon}{text}")),
            )
        });
    let right = div()
        .flex()
        .flex_row()
        .flex_none()
        .gap_2()
        .pl_2()
        .when_some(row.pill.as_ref(), |d, pill| {
            d.child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(tone(pill.tone))
                    .child(pill.bracketed()),
            )
        })
        .when(!row.time.is_empty(), |d| {
            d.child(div().text_color(theme.ansi_dark_gray).child(row.time.clone()))
        });
    div()
        .flex()
        .flex_row()
        .items_start()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(theme.selection))
        .when(!selected, |d| d.hover(|s| s.bg(theme.search)))
        .child(chip)
        .child(verb)
        .child(message)
        .child(right)
}

fn picker_row(theme: &Theme, selected: bool, label: String) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .mx_1()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(theme.selection))
        .child(
            div().w(px(ICON)).when(selected, |d| {
                d.child(svg().path(icons::CHECK).size(px(ICON)).text_color(theme.white))
            }),
        )
        .child(label)
}
