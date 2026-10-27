//! The Machines view: every machine the project declares, with its
//! workspaces, rendered from the shared [`shelbi_app::view::MachinesModel`].
//!
//! Drawn to the Figma Machines frame (`dt-figma-machines`). Each machine's
//! heading shows where it is (`local` or `ssh <host>`) and its live
//! reachability (remotes only), with its tags and recorded remote `shelbi`
//! binary on the line under it. Each workspace row shows its observed state,
//! the runner its task's agent launches on, the task as `<status>: <title>`,
//! the task's branch, and for a review slot a server still starting and the
//! review URL (click the URL to open it).
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
    div, prelude::*, px, ClickEvent, Context, EventEmitter, FocusHandle,
    KeyDownEvent, ScrollHandle, SharedString, Task, Window,
};
use shelbi_app::reachability::{ReachabilityProber, REACHABILITY_BACKOFF_MAX, REACHABILITY_CADENCE};
use shelbi_app::theme::Tone;
use shelbi_app::view::{
    reachability_label, remote_binary_label, workspace_state_badge, MachineRow, MachineWorkspace,
    MachinesModel,
};
use shelbi_app::CommandKind;

use super::machines_state::{Intent, MachinesState};
use crate::assets::{heroicon, icons};
use crate::keys::chord_from_keystroke;
use crate::theme::style::{self, ButtonKind, ButtonSize};
use crate::theme::Theme;
use crate::theme::style::{Type, Typography};

/// How often the machines read re-runs. It reads the board index and each
/// workspace's status (and, for a remote review slot, its serving marker over
/// SSH), all off the UI thread.
const REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
/// How often the view drains its refresher and the prober.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Probe `host` over SSH, or (`--visual-figma`, whose fixture hosts don't
/// exist) answer the stubbed reachability without a probe. Settings ▸
/// Workspaces' prober probes through it too.
pub(super) fn probe(host: &str) -> shelbi_core::Reachability {
    #[cfg(feature = "visual-check")]
    if let Some(stub) = FIXTURE_HOSTS.lock().unwrap().clone() {
        return stub;
    }
    shelbi_orchestrator::machine::probe_host_reachability(host)
}

#[cfg(feature = "visual-check")]
static FIXTURE_HOSTS: std::sync::Mutex<Option<shelbi_core::Reachability>> = std::sync::Mutex::new(None);

/// Answer every remote `reachability` without probing it, for the rest of
/// the process or the next call (`--visual-figma`).
#[cfg(feature = "visual-check")]
pub fn stub_reachability(reachability: shelbi_core::Reachability) {
    *FIXTURE_HOSTS.lock().unwrap() = Some(reachability);
}

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
            |_machine: &str, host: &str| probe(host),
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

    /// The window is dropping this view (a project switch): stop its
    /// background reads without waiting on one in flight, so the UI thread
    /// never joins a slow read.
    pub fn stop_workers(&mut self) {
        self.refresher.stop_without_waiting();
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

    #[cfg(feature = "visual-check")]
    pub fn state_mut(&mut self) -> &mut MachinesState {
        &mut self.state
    }

    // ---- rendering --------------------------------------------------------

    fn render_body(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let placeholder = |text: String| {
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
        if let Some(error) = self.state.error() {
            return placeholder(format!("Couldn't read the project: {error}"));
        }
        let model = self.state.model();
        if model.machines.is_empty() {
            return style::empty_state(
                "machines-empty",
                "No machines are declared for this project yet. Add them under `machines:` in its config, then refresh.",
                Some(
                    style::icon_label_button("machines-refresh", icons::ARROW_PATH, "Refresh", ButtonKind::Secondary, ButtonSize::Base, theme)
                        .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                            this.refresh();
                            cx.notify();
                        })),
                ),
                theme,
            )
            .into_any_element();
        }
        // The Machines frame: machines 16 apart, each a heading (plus a
        // remote's metadata line) over its workspace rows, 2 apart.
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
            let mut group = div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(2.0))
                .child(machine_header(theme, machine))
                .when_some(machine_metadata(theme, machine), |d, line| d.child(line));
            if machine.workspaces.is_empty() {
                group = group.child(
                    div()
                        .pl_4()
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
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
        let divider = || div().h(px(1.0)).flex_none().bg(theme.divider);
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
            .bg(theme.nested(theme.canvas))
            .type_style(Type::Base)
            .text_color(theme.foreground)
            // The toolbar: the view's name, loud, then the project's, muted.
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
                            .type_style(Type::BaseMedium)
                            .text_color(theme.text_loud)
                            .child("Machines"),
                    )
                    .child(
                        div()
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .child(self.state.title().to_string()),
                    ),
            )
            .child(divider())
            .child(self.render_body(&theme, cx))
            .child(divider())
            // The shortcuts: key hints left, the workspace count right.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(26.0))
                    .px_4()
                    .type_style(Type::XSmall)
                    .text_color(theme.muted)
                    .child(div().flex_1().min_w_0().truncate().child("↑/↓ select   enter open   r refresh"))
                    .child(div().flex_none().child(format!(
                        "{count} workspace{}",
                        if count == 1 { "" } else { "s" }
                    ))),
            )
    }
}

// ---- pieces -----------------------------------------------------------------

/// The column widths of a workspace row: name, state, runner (the
/// Machines frame's).
const NAME_W: f32 = 150.0;
const STATE_W: f32 = 76.0;
const RUNNER_W: f32 = 96.0;

/// A machine's heading: `gpu   ssh gpu.lan   ● reachable`. The name is loud,
/// where it is muted, and a remote's live reachability follows in its tone
/// (green reachable, red with the reason when not, muted while checking).
fn machine_header(theme: &Theme, m: &MachineRow) -> impl IntoElement {
    let location = if m.is_local {
        "local".to_string()
    } else {
        match &m.host {
            Some(h) => format!("ssh {h}"),
            None => "ssh".to_string(),
        }
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_3()
        .h(px(24.0))
        .min_w_0()
        .child(
            div()
                .flex_none()
                .type_style(Type::BaseMedium)
                .text_color(theme.text_loud)
                .child(m.name.clone()),
        )
        .child(div().flex_none().type_style(Type::XSmall).text_color(theme.muted).child(location))
        // Remotes only: the hub is never probed.
        .when(!m.is_local, |d| {
            let (text, tone) = reachability_label(&m.reachability, 80);
            // The label is cut at 80 characters; the whole reason is a hover
            // away.
            let (full, _) = reachability_label(&m.reachability, usize::MAX);
            d.child(
                div()
                    .id(gpui::SharedString::from(format!("reachability-{}", m.name)))
                    .min_w_0()
                    .truncate()
                    .type_style(Type::XSmall)
                    .text_color(machines_tone(theme, tone))
                    .when(full != text, |d| d.tooltip(style::tooltip(full)))
                    .child(text),
            )
        })
}

/// The line under a machine's heading: its tags, then (remotes) the
/// recorded `shelbi` binary, `[gpu, linux]   /usr/local/bin/shelbi v0.10.0`.
/// `None` for a local machine with no tags.
fn machine_metadata(theme: &Theme, m: &MachineRow) -> Option<gpui::Div> {
    if m.is_local && m.tags.is_empty() {
        return None;
    }
    let binary = (!m.is_local).then(|| {
        let (text, tone) = remote_binary_label(m.remote.as_ref());
        // A resolved binary is metadata; incompatible or missing is a problem.
        let color = if tone == Tone::Green {
            theme.muted
        } else {
            machines_tone(theme, tone)
        };
        div().min_w_0().truncate().text_color(color).child(text)
    });
    Some(
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(24.0))
            .min_w_0()
            .type_style(Type::XSmall)
            .when(!m.tags.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_color(theme.workflow_badge_text)
                        .child(format!("[{}]", m.tags.join(", "))),
                )
            })
            .children(binary),
    )
}

/// A shared model's [`Tone`] in the Machines frame's colors: green and cyan
/// are the frame's `theme.green` / `theme.cyan`, idle is muted, the rest
/// keep their log hues.
fn machines_tone(theme: &Theme, tone: Tone) -> gpui::Hsla {
    match tone {
        Tone::Green => theme.theme_green,
        Tone::Cyan => theme.theme_cyan,
        Tone::DarkGray => theme.muted,
        Tone::Yellow => theme.warning,
        tone => theme.tone(tone),
    }
}

/// The state cell: the observed state, except that a review slot serving its
/// task reads `review` (the Machines frame); a slot still starting its server
/// says so in the review cell.
fn state_cell(w: &MachineWorkspace) -> (&'static str, Tone) {
    match w.state {
        Some(shelbi_state::WorkspaceState::Serving) => ("review", Tone::Cyan),
        state => workspace_state_badge(state),
    }
}

/// One workspace: name, state, runner, `<status>: <task title>`, a review
/// slot's server, and the branch right-aligned.
fn workspace_row(theme: &Theme, w: &MachineWorkspace, selected: bool) -> gpui::Div {
    let (state, state_tone) = state_cell(w);
    let review = w.review.as_ref().and_then(|r| {
        // A serving slot already reads `review` in the state cell; say only
        // what that doesn't: a server still starting, and its URL.
        let starting = (r.label() != "serving").then(|| r.label());
        if starting.is_none() && r.url.is_none() {
            return None;
        }
        let mut cell = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_2()
            .type_style(Type::XSmall)
            .when_some(starting, |d, label| d.child(div().text_color(theme.muted).child(label)));
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
                    .child(heroicon(icons::ARROW_TOP_RIGHT_ON_SQUARE).text_color(theme.icon))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&open);
                    }),
            );
        }
        Some(cell)
    });
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_3()
        .h(px(30.0))
        .pl_4()
        .pr_2()
        .rounded(px(6.0))
        .cursor_pointer()
        .when(selected, |d| d.bg(theme.selection))
        .when(!selected, |d| d.hover(|s| s.bg(theme.background)))
        .child(
            div()
                .w(px(NAME_W))
                .flex_none()
                .truncate()
                .text_color(theme.text_loud)
                .child(w.name.clone()),
        )
        .child(
            div()
                .w(px(STATE_W))
                .flex_none()
                .truncate()
                .text_color(machines_tone(theme, state_tone))
                .child(state),
        )
        .child(
            div()
                .w(px(RUNNER_W))
                .flex_none()
                .truncate()
                .text_color(theme.table_text)
                .child(w.runner.clone().unwrap_or_else(|| "—".to_string())),
        )
        // The task by title; its id (what `shelbi workspace list` prints) on
        // hover.
        .child({
            let cell = w.task_cell();
            let ids = w.assignment.cell();
            div()
                .id(SharedString::from(format!("machine-task-{}", w.name)))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.table_text)
                .when(ids != cell, |d| d.tooltip(style::tooltip(ids)))
                .child(cell)
        })
        .children(review)
        .when_some(w.branch.clone(), |d, branch| {
            d.child(
                div()
                    .max_w(px(240.0))
                    .flex_none()
                    .truncate()
                    .type_style(Type::XSmall)
                    .text_color(theme.dark_gray)
                    .child(format!("{}{branch}", shelbi_app::activity::BRANCH_ICON)),
            )
        })
}
