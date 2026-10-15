//! The Machines view: every machine the project declares, with its
//! workspaces, rendered from the shared [`shelbi_app::view::MachinesModel`].
//!
//! Each machine's header shows where it is (`local` or `ssh <host>`), its live
//! reachability (remotes only), its tags, and its recorded remote `shelbi`
//! binary. Each workspace row shows its observed state, the agent and task
//! `shelbi workspace list` reports, the task's branch, and for a review slot
//! the review server's state and URL (click the URL to open it).
//!
//! Two background sources, neither on the UI thread:
//!
//! - a refresher running the shared reader
//!   (`shelbi_orchestrator::machines_model::read_machines_model`, also the
//!   TUI's), re-read on a short cadence;
//! - a [`ReachabilityProber`] (`shelbi_app::reachability`, also the TUI's)
//!   probing each remote over SSH on its own thread. A remote reads
//!   "checking…" until its first probe lands, and a down host shows as
//!   unreachable without ever blocking the window.
//!
//! Clicking (or Enter on) a workspace runs `CommandKind::FocusWorkspace`
//! through the window's dispatch.

use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, ClickEvent, Context, EventEmitter, FocusHandle, FontWeight,
    KeyDownEvent, ScrollHandle, SharedString, Task, Window,
};
use shelbi_app::reachability::{ReachabilityProber, REACHABILITY_BACKOFF_MAX, REACHABILITY_CADENCE};
use shelbi_app::theme::Tone;
use shelbi_app::view::{
    reachability_label, remote_binary_label, workspace_state_badge, MachineRow, MachineWorkspace,
    MachinesModel, SlotAssignment,
};
use shelbi_app::CommandKind;

use super::machines_state::{Intent, MachinesState};
use crate::assets::{heroicon, icons};
use crate::keys::chord_from_keystroke;
use crate::theme::style;
use crate::theme::Theme;

/// How often the machines read re-runs. It reads the board index and each
/// workspace's status (and, for a remote review slot, its serving marker over
/// SSH), all off the UI thread.
const REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
/// How often the view drains its refresher and the prober.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What the view asks of the window.
#[derive(Debug, Clone)]
pub enum MachinesEvent {
    /// Run this registry command through `commands::dispatch_command`.
    Command(CommandKind),
}

pub struct MachinesView {
    state: MachinesState,
    focus: FocusHandle,
    needs_focus: bool,
    refresher: shelbi_app::Refresher<Result<MachinesModel, String>>,
    prober: ReachabilityProber,
    last_refresh: Instant,
    scroll: ScrollHandle,
    _poll: Task<()>,
}

impl EventEmitter<MachinesEvent> for MachinesView {}

impl MachinesView {
    pub fn new(project: String, cx: &mut Context<Self>) -> Self {
        let p = project.clone();
        let refresher = shelbi_app::spawn_worker(move |_| {
            shelbi_orchestrator::machines_model::read_machines_model(&p).map_err(|e| e.to_string())
        });
        refresher.request();
        let prober = ReachabilityProber::spawn(
            REACHABILITY_CADENCE,
            REACHABILITY_BACKOFF_MAX,
            |_machine: &str, host: &str| {
                shelbi_orchestrator::machine::probe_host_reachability(host)
            },
        );
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                break;
            }
        });
        MachinesView {
            state: MachinesState::new(project),
            focus: cx.focus_handle(),
            needs_focus: true,
            refresher,
            prober,
            last_refresh: Instant::now(),
            scroll: ScrollHandle::new(),
            _poll: poll,
        }
    }

    pub fn request_focus(&mut self) {
        self.needs_focus = true;
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(read) = self.refresher.latest() {
            self.state.apply(read);
            // A no-op unless the declared remotes changed.
            self.prober.set_targets(self.state.probe_targets());
            changed = true;
        }
        changed |= self.state.set_reachability(self.prober.snapshot());
        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresh();
        }
        if changed {
            cx.notify();
        }
    }

    fn refresh(&mut self) {
        self.refresher.request();
        self.last_refresh = Instant::now();
    }

    fn apply_intents(&mut self, intents: Vec<Intent>, cx: &mut Context<Self>) {
        for intent in intents {
            match intent {
                Intent::Focus(workspace) => {
                    cx.emit(MachinesEvent::Command(CommandKind::FocusWorkspace { workspace }))
                }
                Intent::Refresh => self.refresh(),
            }
        }
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(chord) = chord_from_keystroke(&ev.keystroke) else {
            return;
        };
        let Some(intents) = self.state.handle_key(chord) else {
            return;
        };
        cx.stop_propagation();
        self.apply_intents(intents, cx);
    }

    /// Drain the background sources now (the offscreen check's dispatcher
    /// doesn't run the poll timer).
    #[cfg(feature = "visual-check")]
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        self.tick(cx);
    }

    /// The view's state, for the offscreen views check.
    #[cfg(feature = "visual-check")]
    pub fn state(&self) -> &MachinesState {
        &self.state
    }

    // ---- rendering --------------------------------------------------------

    fn render_body(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let placeholder = |text: String| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme.text_quiet)
                .child(text)
                .into_any_element()
        };
        if !self.state.is_loaded() {
            return placeholder("Loading…".into());
        }
        if let Some(error) = self.state.error() {
            return placeholder(format!("Couldn't read the project: {error}"));
        }
        let model = self.state.model();
        if model.machines.is_empty() {
            return placeholder("No machines declared for this project.".into());
        }
        let mut body = div()
            .id("machines-list")
            .debug_selector(|| "machines-list".into())
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .py_2()
            .gap_4();
        for (mi, machine) in model.machines.iter().enumerate() {
            // Each machine is a card on the main surface.
            let mut group = div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .p_2()
                .rounded_lg()
                .bg(theme.surface_raised)
                .shadow(style::card_shadow(theme))
                .child(machine_header(theme, machine));
            if machine.workspaces.is_empty() {
                group = group.child(
                    div()
                        .pl_4()
                        .text_xs()
                        .text_color(theme.text_quiet)
                        .child("(no workspaces)"),
                );
            }
            for (wi, ws) in machine.workspaces.iter().enumerate() {
                let selected = self.state.is_selected(mi, wi);
                group = group.child(
                    workspace_row(theme, ws, selected)
                        .id(("machine-workspace", mi * 1000 + wi))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let intents = this.state.click(mi, wi);
                            this.apply_intents(intents, cx);
                        })),
                );
            }
            body = body.child(group);
        }
        body.into_any_element()
    }
}

impl Render for MachinesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_focus {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        let theme = *cx.global::<Theme>();
        let count = self.state.model().workspace_count();
        div()
            .id("machines")
            .track_focus(&self.focus)
            .key_context("Machines")
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.surface)
            .text_sm()
            .text_color(theme.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.rule)
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_loud)
                            .child("Machines"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.text_quiet)
                            .child(self.state.title().to_string()),
                    ),
            )
            .child(self.render_body(&theme, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .px_4()
                    .py_1p5()
                    .border_t_1()
                    .border_color(theme.rule)
                    .text_xs()
                    .text_color(theme.text_quiet)
                    .child(div().flex_1().child("↑/↓ select   enter open   r refresh"))
                    .child(format!(
                        "{count} workspace{}",
                        if count == 1 { "" } else { "s" }
                    )),
            )
    }
}

// ---- pieces -----------------------------------------------------------------

/// `gpu   ssh gpu.lan   ● reachable   [gpu, linux]   /usr/local/bin/shelbi v0.10.0`
fn machine_header(theme: &Theme, m: &MachineRow) -> impl IntoElement {
    let location = if m.is_local {
        "local".to_string()
    } else {
        match &m.host {
            Some(h) => format!("ssh {h}"),
            None => "ssh".to_string(),
        }
    };
    let toned = |text: String, tone: Tone| style::chip(text, Theme::tone_hue(tone), theme);
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_2()
        .px_2()
        .pt_1()
        .pb_2()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_loud)
                .child(m.name.clone()),
        )
        .child(div().text_xs().text_color(theme.text_quiet).child(location))
        // Live reachability next to the ssh label: remotes only, the hub is
        // never probed.
        .when(!m.is_local, |d| {
            let (text, tone) = reachability_label(&m.reachability, 80);
            d.child(toned(text, tone))
        })
        .children(m.tags.iter().map(|tag| style::chip(tag.clone(), crate::theme::Hue::Neutral, theme)))
        .when(!m.is_local, |d| {
            let (text, tone) = remote_binary_label(m.remote.as_ref());
            d.child(div().text_xs().text_color(theme.tone(tone)).child(text))
        })
}

/// One workspace: name, observed state, agent, task, branch, review server.
fn workspace_row(theme: &Theme, w: &MachineWorkspace, selected: bool) -> gpui::Div {
    let (badge, badge_tone) = workspace_state_badge(w.state);
    let task = match &w.assignment {
        SlotAssignment::Idle => div().text_color(theme.text_quiet).child("idle"),
        assignment => div()
            .flex()
            .flex_row()
            .min_w_0()
            .gap_2()
            .child(div().flex_none().text_color(theme.text).child(assignment.cell()))
            .when_some(w.task_title.clone(), |d, title| {
                d.child(div().min_w_0().truncate().text_color(theme.text_loud).child(title))
            }),
    };
    let review = w.review.as_ref().map(|r| {
        let tone = if r.label() == "serving" {
            Tone::Cyan
        } else {
            Tone::DarkGray
        };
        let mut cell = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_2()
            .child(div().text_color(theme.tone(tone)).child(r.label()));
        if let Some(url) = r.url.clone() {
            let open = url.clone();
            cell = cell.child(
                div()
                    .id(SharedString::from(format!("review-url-{}", w.name)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .text_color(theme.link)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.link_hover))
                    .child(url)
                    .child(
                        heroicon(icons::ARROW_TOP_RIGHT_ON_SQUARE)
                            .text_color(theme.icon),
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&open);
                    }),
            );
        }
        cell
    });
    div()
        .flex()
        .flex_row()
        .items_center()
        .relative()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer()
        .when(selected, |d| d.bg(theme.row_selected).child(style::selection_indicator(theme)))
        .when(!selected, |d| d.hover(|s| s.bg(theme.fill_subtle)))
        .child(
            div()
                .w(px(150.0))
                .flex_none()
                .truncate()
                .text_color(theme.text_loud)
                .child(w.name.clone()),
        )
        .child(
            div()
                .w(px(96.0))
                .flex_none()
                .flex()
                .child(style::chip(badge, Theme::tone_hue(badge_tone), theme)),
        )
        .child(
            div()
                .w(px(96.0))
                .flex_none()
                .truncate()
                .text_color(theme.text)
                .child(w.agent.clone().unwrap_or_else(|| "-".to_string())),
        )
        .child(div().flex_1().min_w_0().child(task))
        .when_some(w.branch.clone(), |d, branch| {
            d.child(
                div()
                    .max_w(px(240.0))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(theme.text_quiet)
                    .child(format!("{}{branch}", shelbi_app::activity::BRANCH_ICON)),
            )
        })
        .children(review)
}
