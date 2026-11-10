//! Settings ▸ Orchestrator (`awareness-setting`): how events reach the
//! project's orchestrator, over the shared
//! [`OrchestratorForm`](shelbi_app::settings::orchestrator::OrchestratorForm)
//! the TUI's page draws too.
//!
//! ```text
//!  Orchestrator
//!  Event delivery
//!  (•) Simple heartbeat system (v1)
//!      A periodic heartbeat keeps the orchestrator up to date.
//!  ( ) Awareness system (v2)
//!      Notifies the orchestrator only on board changes, reminders,
//!      finished jobs and overdue tasks.
//!  Applies when the orchestrator restarts.      [ Restart orchestrator ]
//! ```
//!
//! The project's config is read off the UI thread through the window's
//! source (`workflow_config`, which carries `orchestrator.awareness:`), and
//! read again each time the page is shown. Picking an option saves it at
//! once through the window's config store (the project's daemon, here or on
//! the far machine), as the Workflows page's edits do. Restart orchestrator
//! asks the project's daemon to hand off and replace the orchestrator
//! session, workers left running.

use std::sync::Arc;

use gpui::{div, prelude::*, px, ClickEvent, Context, Div, FontWeight, Role, SharedString, Stateful, Toggled, Window};
use shelbi_app::settings::orchestrator::{
    self as orch, mode_description, mode_label, OrchestratorForm, RestartStatus, SaveStatus, APPLIES_NOTE,
    EVENT_DELIVERY_LABEL, MODES, RESTART_LABEL,
};
use shelbi_app::source::ProjectHandle;
use shelbi_core::AwarenessMode;
use shelbi_state::workflow_config::WorkflowConfig;

use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The page title's size (Desktop/Heading, as the Workspaces page's).
const TITLE_SIZE: f32 = 18.0;

/// Restarts a project's orchestrator (blocking; run off the UI thread):
/// [`orch::restart`] through the window's link, a recorder under test.
pub type Restarter = Arc<dyn Fn(&ProjectHandle) -> Result<(), String> + Send + Sync>;

/// The Orchestrator page.
pub struct OrchestratorPage {
    form: OrchestratorForm,
    handle: ProjectHandle,
    restarter: Restarter,
    /// Re-read the config on the next draw (the page was shown again).
    reload: bool,
}

/// A mode's radio row's selector.
pub fn mode_selector(mode: AwarenessMode) -> &'static str {
    match mode {
        AwarenessMode::Heartbeat => "orchestrator-mode-heartbeat",
        AwarenessMode::Awareness => "orchestrator-mode-awareness",
    }
}

impl OrchestratorPage {
    pub fn new(handle: &ProjectHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut page = OrchestratorPage {
            form: OrchestratorForm::default(),
            handle: handle.clone(),
            restarter: Arc::new(|handle: &ProjectHandle| orch::restart(handle.name(), Some(&handle.link))),
            reload: false,
        };
        // A test's page never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.load(window, cx);
        }
        page
    }

    /// Read the project's config off the UI thread and show its mode.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.handle.source.clone();
        let read = cx.background_executor().spawn(async move { source.workflow_config() });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update(cx, |this, cx| {
                this.form.load(config);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the config when next drawn (the page was shown again), and
    /// forget the last visit's save and restart lines. A test's page reads
    /// no disk unless told to.
    pub fn reload_when_drawn(&mut self) {
        self.form.clear_status();
        self.reload = !cfg!(test);
    }

    #[cfg(test)]
    pub fn form(&self) -> &OrchestratorForm {
        &self.form
    }

    /// Restart through `restarter` instead (a test's recorder).
    #[cfg(test)]
    pub fn set_restarter(&mut self, restarter: Restarter) {
        self.restarter = restarter;
    }

    /// Pick `mode` and save it off the UI thread.
    fn choose(&mut self, mode: AwarenessMode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.form.choose(mode) else {
            return;
        };
        cx.notify();
        let store = self.handle.clone();
        let save = cx.background_executor().spawn(async move { orch::save(&store, &config) });
        cx.spawn_in(window, async move |this, cx| {
            let done: Result<WorkflowConfig, String> = save.await;
            this.update(cx, |this, cx| {
                this.form.saved_result(done);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Restart the orchestrator off the UI thread.
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.form.begin_restart() {
            return;
        }
        cx.notify();
        let handle = self.handle.clone();
        let restarter = self.restarter.clone();
        let run = cx.background_executor().spawn(async move { restarter(&handle) });
        cx.spawn_in(window, async move |this, cx| {
            let done = run.await;
            this.update(cx, |this, cx| {
                this.form.restart_result(done);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// A radio row: the radio, the mode's label over its description.
fn radio_row(mode: AwarenessMode, on: bool, enabled: bool, theme: &Theme) -> Stateful<Div> {
    let id = mode_selector(mode);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(Role::RadioButton)
        .aria_label(mode_label(mode))
        .aria_selected(on)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .flex()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .w_full()
        .py(px(4.0))
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.6))
        .child(super::add_project::radio(on, theme))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(mode_label(mode)))
                .child(div().type_style(Type::Small).text_color(theme.form_hint).child(mode_description(mode))),
        )
}

impl Render for OrchestratorPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if std::mem::take(&mut self.reload) {
            self.load(window, cx);
        }
        let current = self.form.mode();
        let editable = self.form.editable();
        let radios = MODES.map(|mode| {
            radio_row(mode, mode == current, editable, &theme).when(editable, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose(mode, window, cx)))
            })
        });
        let restarting = self.form.restarting();
        let restart = style::button("orchestrator-restart", RESTART_LABEL, ButtonKind::Secondary, ButtonSize::Small, false, &theme)
            .role(Role::Button)
            .aria_label(RESTART_LABEL)
            .when(restarting, |d| d.opacity(0.6))
            .when(!restarting, |d| {
                d.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.restart(window, cx)))
            });
        let line = |selector: &'static str, text: String, color| {
            div()
                .debug_selector(move || selector.into())
                .type_style(Type::XSmall)
                .text_color(color)
                .child(text)
        };
        let unreadable = self.form.unreadable().map(|e| line("orchestrator-unreadable", e.to_string(), theme.error));
        let status = self.form.status_message().map(|text| {
            let color = if matches!(self.form.status(), SaveStatus::Failed(_)) { theme.error } else { theme.form_hint };
            line("orchestrator-status", text, color)
        });
        let restart_line = self.form.restart_status().message().map(|text| {
            let color = if matches!(self.form.restart_status(), RestartStatus::Failed(_)) {
                theme.error
            } else {
                theme.form_hint
            };
            line("orchestrator-restart-status", text, color)
        });
        div()
            .id("settings-orchestrator")
            .debug_selector(|| "settings-orchestrator".into())
            .flex()
            .flex_col()
            .w_full()
            .gap(px(16.0))
            .child(
                div()
                    .type_style(Type::Heading)
                    .text_size(px(TITLE_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_loud)
                    .child("Orchestrator"),
            )
            .children(unreadable)
            .child(
                div()
                    .id("orchestrator-event-delivery")
                    .debug_selector(|| "orchestrator-event-delivery".into())
                    .role(Role::RadioGroup)
                    .aria_label(EVENT_DELIVERY_LABEL)
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud)
                            .child(SharedString::from(EVENT_DELIVERY_LABEL)),
                    )
                    .children(radios),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(16.0))
                    .w_full()
                    .child(
                        div()
                            .debug_selector(|| "orchestrator-applies".into())
                            .flex_1()
                            .min_w_0()
                            .type_style(Type::Small)
                            .text_color(theme.form_hint)
                            .child(APPLIES_NOTE),
                    )
                    .child(restart),
            )
            .children(status)
            .children(restart_line)
    }
}
