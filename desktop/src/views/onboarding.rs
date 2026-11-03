//! The Orchestrator view's onboarding (`orch-onboarding`): the three steps
//! a project whose orchestrator was never set up shows in place of the
//! orchestrator's terminal, after the Figma frames (`00 · Orchestrator ·
//! Welcome` 2709:10719, `01 · … · Choose provider and model` 2709:11068,
//! `02 · … · Review instructions` 2709:11492). The state, its copy and its
//! saves are `shelbi_app::onboarding`'s, shared with the TUI; this view
//! only draws it and turns clicks and keys into its calls.
//!
//! 1. A 56pt disc with the chat bubble, the title and the intro over a
//!    520pt column, and Get started (Primary, Large).
//! 2. Over a 620pt column: the title and its sentence, Harness and Default
//!    model (each a 36pt picker with its helper under it), a rule, and
//!    Continue at the right.
//! 3. The title and its sentence, `Orchestrator instructions` with
//!    `Markdown` at the right over a 400pt monospace editor, a rule, and
//!    Back (left) and Launch Orchestrator (right).
//!
//! Esc goes Back from step 3 (and 2); Enter presses Get started on step 1.
//! The reads and saves run off the UI thread; once Launch Orchestrator's
//! save lands the view emits [`OnboardingEvent::Launched`] and the window
//! starts the orchestrator and shows its terminal.

use gpui::{
    div, prelude::*, px, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, Subscription, Window,
};
use gpui_kit::component::input::{Escape, InputEvent, TextareaState};
use shelbi_app::onboarding::{
    self as model, ContinueOutcome, LaunchOutcome, OnboardingState, Setup, BACK, CONTINUE, FIELD_HELP, GET_STARTED,
    HARNESS_BODY, HARNESS_LABEL, HARNESS_TITLE, INSTRUCTIONS_BODY, INSTRUCTIONS_FORMAT, INSTRUCTIONS_LABEL,
    INSTRUCTIONS_TITLE, INTRO_BODY, INTRO_TITLE, LAUNCH, LOADING, MODEL_LABEL,
};
use shelbi_app::view::{OnboardingModel, OnboardingStep};

use super::agent_editor::markdown_area;
pub use super::harness_picker::Menu;
use super::harness_picker::{self as pickers, Fields, Handlers, Host};
use super::workflow_editor::error_line;
use crate::assets::{glyph, glyphs};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The main area's inset around the steps (`Main content`'s 40pt).
const INSET: f32 = 40.0;
/// Step 1's bottom inset (80pt: the intro sits a little above center).
const INTRO_BOTTOM: f32 = 80.0;
/// The intro's column (`Welcome and setup`).
const INTRO_W: f32 = 520.0;
/// Steps 2 and 3's column (`Agent setup`, `Instructions setup`).
const COLUMN_W: f32 = 620.0;
/// The intro's disc and the gaps between its pieces.
const DISC: f32 = 56.0;
/// The instructions editor (`Instructions · Editable Markdown`).
const EDITOR_H: f32 = 400.0;

/// What the view tells the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingEvent {
    /// Launch Orchestrator saved and marked the project: start the
    /// orchestrator and show its terminal.
    Launched,
}


pub struct OnboardingView {
    state: OnboardingState,
    menu: Option<Menu>,
    menu_anchor: style::Anchor,
    instructions: Entity<TextareaState>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OnboardingEvent> for OnboardingView {}

impl Focusable for OnboardingView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OnboardingView {
    /// `project`'s onboarding; its settings are read off the UI thread.
    pub fn new(project: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let instructions = cx.new(|cx| TextareaState::new(window, cx));
        let subscriptions = vec![cx.subscribe_in(&instructions, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                this.state.type_instructions(&text);
                cx.notify();
            }
        })];
        let view = OnboardingView {
            state: OnboardingState::new(project),
            menu: None,
            menu_anchor: Default::default(),
            instructions,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        let project = project.to_string();
        let read = cx.background_executor().spawn(async move { model::load(&project) });
        cx.spawn_in(window, async move |this, cx| {
            let setup = read.await;
            this.update_in(cx, |this, window, cx| this.loaded(setup, window, cx)).ok();
        })
        .detach();
        view
    }

    #[cfg(test)]
    pub fn state(&self) -> &OnboardingState {
        &self.state
    }

    /// The instructions editor (a test types into it).
    #[cfg(test)]
    pub fn instructions_field(&self) -> Entity<TextareaState> {
        self.instructions.clone()
    }

    fn loaded(&mut self, setup: Result<Setup, String>, window: &mut Window, cx: &mut Context<Self>) {
        self.state.loaded(setup);
        let text = self.state.instructions().to_string();
        self.instructions.update(cx, |s, cx| s.set_value(text, window, cx));
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Actions (public for the tests and the window's keys)

    pub fn get_started(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.get_started();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        // The editor's text is the draft (a value set in code sends no
        // Change), so Back keeps exactly what it shows.
        if self.state.step() == OnboardingStep::Instructions {
            let text = self.instructions.read(cx).value().to_string();
            self.state.type_instructions(&text);
        }
        self.state.back();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn select_runner(&mut self, kind: shelbi_core::RunnerKind, cx: &mut Context<Self>) {
        self.menu = None;
        self.state.select_runner(kind);
        cx.notify();
    }

    pub fn select_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        self.menu = None;
        self.state.select_model(model.as_deref());
        cx.notify();
    }

    pub fn continue_(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(job) = self.state.begin_continue() else {
            cx.notify();
            return;
        };
        let save = cx.background_executor().spawn(async move { model::run_continue(job) });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = save.await;
            this.update_in(cx, |this, window, cx| this.continued(outcome, window, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn continued(&mut self, outcome: ContinueOutcome, window: &mut Window, cx: &mut Context<Self>) {
        self.state.continued(outcome);
        if self.state.step() == OnboardingStep::Instructions {
            let handle = self.instructions.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    pub fn launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.instructions.read(cx).value().to_string();
        self.state.type_instructions(&text);
        let Some(job) = self.state.begin_launch() else {
            cx.notify();
            return;
        };
        let save = cx.background_executor().spawn(async move { model::run_launch(job) });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = save.await;
            this.update_in(cx, |this, _, cx| this.launched(outcome, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn launched(&mut self, outcome: LaunchOutcome, cx: &mut Context<Self>) {
        if self.state.launched(outcome) {
            cx.emit(OnboardingEvent::Launched);
        }
        cx.notify();
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = if self.menu == Some(menu) { None } else { Some(menu) };
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "enter" && self.state.step() == OnboardingStep::Intro {
            self.get_started(window, cx);
            cx.stop_propagation();
        }
    }
}

impl Render for OnboardingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let m = self.state.model();
        let intro = m.step == OnboardingStep::Intro;
        let step = match m.step {
            OnboardingStep::Intro => self.render_intro(&theme, cx).into_any_element(),
            OnboardingStep::Harness => self.render_harness(&m, &theme, cx).into_any_element(),
            OnboardingStep::Instructions => self.render_instructions(&m, &theme, window, cx).into_any_element(),
        };
        div()
            .id("onboarding")
            .debug_selector(|| "onboarding".into())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .pt(px(INSET))
            .px(px(INSET))
            .pb(px(if intro { INTRO_BOTTOM } else { INSET }))
            .bg(theme.canvas)
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .on_key_down(cx.listener(Self::on_key))
            // Esc, the editor's included, goes Back.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.state.step() != OnboardingStep::Intro {
                    this.back(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .id("onboarding-pane")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .overflow_y_scroll()
                    .child(step),
            )
    }
}

impl OnboardingView {
    /// `00 · Orchestrator · Welcome`.
    fn render_intro(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let disc = div()
            .debug_selector(|| "onboarding-disc".into())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(DISC))
            .rounded_full()
            .bg(theme.search)
            .child(glyph(glyphs::CHAT_BUBBLE).text_color(theme.accent));
        let text = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w(px(INTRO_W))
            .text_center()
            .child(
                div()
                    .debug_selector(|| "onboarding-title".into())
                    .type_style(Type::Title)
                    .text_color(theme.text_loud)
                    .child(INTRO_TITLE),
            )
            .child(
                // 18 / 1.4, regular: the frame's intro (no named style).
                div()
                    .debug_selector(|| "onboarding-intro".into())
                    .type_style(Type::Base)
                    .text_size(px(18.0))
                    .line_height(px(18.0 * 1.4))
                    .text_color(theme.muted)
                    .child(INTRO_BODY),
            );
        let start = style::button("onboarding-get-started", GET_STARTED, ButtonKind::Primary, ButtonSize::Large, false, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.get_started(window, cx)));
        div()
            .debug_selector(|| "onboarding-intro-step".into())
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap(px(24.0))
            .w(px(INTRO_W))
            .child(disc)
            .child(text)
            .child(start)
    }

    /// A step's title and the sentence under it (`Step heading`).
    fn heading(title: &'static str, body: &'static str, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(
                div()
                    .debug_selector(|| "onboarding-title".into())
                    .type_style(Type::Title)
                    .text_color(theme.text_loud)
                    .child(title),
            )
            .child(div().type_style(Type::Base).text_color(theme.foreground).child(body))
    }

    /// A field's label (Base Semibold, loud).
    fn label(text: &'static str, theme: &Theme) -> Div {
        div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(text)
    }

    /// The 1pt rule between a step's fields and its actions (`Divider`).
    fn rule(theme: &Theme) -> Div {
        div().flex_none().h(px(1.0)).w_full().bg(theme.divider)
    }

    /// `01 · Orchestrator · Choose provider and model`.
    fn render_harness(&self, m: &OnboardingModel, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let fields = pickers::fields(
            Fields {
                host: Host::Onboarding,
                harness_label: HARNESS_LABEL,
                model_label: MODEL_LABEL,
                help: FIELD_HELP,
                harnesses: &m.harnesses,
                models: &m.models,
                model_choice: m.model_label.clone(),
                loading: m.loading,
                enabled: !m.loading && !m.busy,
                model_enabled: m.model_enabled,
                model_note: m.model_note.clone(),
                width: COLUMN_W,
                ring: (false, false),
            },
            self.menu,
            &self.menu_anchor,
            Handlers {
                select_runner: |this: &mut Self, kind, cx| this.select_runner(kind, cx),
                select_model: |this: &mut Self, model, cx| this.select_model(model, cx),
                toggle: |this: &mut Self, menu, cx| this.toggle_menu(menu, cx),
                close: |this: &mut Self, _| this.menu = None,
            },
            theme,
            cx,
        );
        let cont = if m.loading || m.busy {
            let label = if m.busy { "Saving…" } else { CONTINUE };
            style::disabled_button("onboarding-continue", label, ButtonKind::Primary, ButtonSize::Large, theme)
        } else {
            style::button("onboarding-continue", CONTINUE, ButtonKind::Primary, ButtonSize::Large, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.continue_(window, cx)))
        };
        div()
            .debug_selector(|| "onboarding-harness-step".into())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(32.0))
            .w(px(COLUMN_W))
            .child(Self::heading(HARNESS_TITLE, HARNESS_BODY, theme))
            .child(fields)
            .children(m.error.as_deref().map(|e| error_line("onboarding-error".into(), e, theme)))
            .child(Self::rule(theme))
            .child(div().flex().flex_row().w_full().justify_end().child(cont))
    }

    /// `02 · Orchestrator · Review instructions`.
    fn render_instructions(&self, m: &OnboardingModel, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let editor = if m.loading {
            div()
                .flex()
                .h(px(EDITOR_H))
                .w_full()
                .items_center()
                .justify_center()
                .text_color(theme.form_hint)
                .child(LOADING)
        } else {
            markdown_area(&self.instructions, "onboarding-instructions", EDITOR_H, window, cx)
        };
        let back = if m.busy {
            style::disabled_button("onboarding-back", BACK, ButtonKind::Secondary, ButtonSize::Large, theme)
        } else {
            style::button("onboarding-back", BACK, ButtonKind::Secondary, ButtonSize::Large, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back(window, cx)))
        };
        let launch = if m.busy || m.loading {
            let label = if m.busy { "Launching…" } else { LAUNCH };
            style::disabled_button("onboarding-launch", label, ButtonKind::Primary, ButtonSize::Large, theme)
        } else {
            style::button("onboarding-launch", LAUNCH, ButtonKind::Primary, ButtonSize::Large, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.launch(window, cx)))
        };
        div()
            .debug_selector(|| "onboarding-instructions-step".into())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(24.0))
            .w(px(COLUMN_W))
            .child(Self::heading(INSTRUCTIONS_TITLE, INSTRUCTIONS_BODY, theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .child(Self::label(INSTRUCTIONS_LABEL, theme))
                            .child(div().type_style(Type::Small).text_color(theme.muted).child(INSTRUCTIONS_FORMAT)),
                    )
                    .child(editor),
            )
            .children(m.error.as_deref().map(|e| error_line("onboarding-error".into(), e, theme)))
            .child(Self::rule(theme))
            .child(div().flex().flex_row().w_full().justify_between().child(back).child(launch))
    }
}
