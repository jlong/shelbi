//! Settings ▸ Issues' tracker section (`linear-settings-ui`), over the
//! shared [`TrackerForm`] the TUI's page draws too: the three boards as
//! radios (one external integration at most: GitHub or Linear), the GitHub
//! repo or the Linear fields ([`linear_fields`]), and Cancel / Connect. The
//! page ([`super::issues_settings`]) draws its title and help over it.
//!
//! ```text
//!  (•) Local board   ( ) GitHub Issues   ( ) Linear
//!  ...the chosen board's fields...
//!  Shelbi shows and creates issues in ENG, assigned to you.
//!  [ ] Always require human review before beginning work
//!      You must move them out of the backlog status before work begins. ...
//!  [x] Only work on issues assigned to me (ada)
//!      Issues must be assigned to your user before work begins.
//!                                               [Cancel] [Connect]
//! ```
//!
//! The two agent rules (`issue_tracker.agent_rules`) show for an external
//! tracker only, and save with the connection like its fields.
//!
//! Nothing is written until Connect (Save for the same board's new
//! filters): the connection runs off the UI thread through the section's
//! [`TrackerService`] (this machine's, or the far daemon's for a project on
//! another one), and the page reads the project again when it lands
//! ([`TrackerEvent::Connected`]). A switch refused because the current
//! board has issues shows the refusal's migrate hint and offers Switch
//! anyway.

use gpui::{div, prelude::*, px, ClickEvent, Context, Entity, EventEmitter, Focusable, Role, Subscription, Window};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use shelbi_app::linear_setup::LinearRequest;
use shelbi_app::new_project::{self as np, Tracker};
use shelbi_app::settings::tracker::{
    self as tr, choice_title, TrackerForm, TrackerService, CANCEL, CHOICES, SWITCH_ANYWAY,
};
use shelbi_app::source::ProjectHandle;
use shelbi_app::tracker_connect::rules_copy;
use shelbi_core::IssueTrackerConfig;

use super::add_project::radio;
use super::linear_fields::{self, LinearUi, Pick};
use crate::elements::controls;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The tracker service for `handle`'s project: this machine's, or its far
/// daemon's.
pub fn tracker_service(handle: &ProjectHandle) -> TrackerService {
    if handle.link.is_remote() {
        TrackerService::far(&handle.link)
    } else {
        shelbi_orchestrator::tracker_connect::local_service(handle.name())
    }
}

/// What the section tells the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerEvent {
    /// A connection was written: read the project again.
    Connected,
}

/// A board's radio id.
pub fn choice_selector(tracker: Tracker) -> &'static str {
    match tracker {
        Tracker::FileSystem => "tracker-choice-local",
        Tracker::Github => "tracker-choice-github",
        Tracker::Linear => "tracker-choice-linear",
    }
}

/// The tracker section.
pub struct TrackerSection {
    form: TrackerForm,
    service: TrackerService,
    linear: LinearUi,
    repo_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TrackerEvent> for TrackerSection {}

impl TrackerSection {
    pub fn new(service: TrackerService, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let linear = LinearUi::new(window, cx);
        let repo_input = cx.new(|cx| InputState::new(window, cx).placeholder("owner/repo"));
        let subscriptions = vec![
            cx.subscribe_in(&repo_input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.form.set_repo(&text);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.connect(false, window, cx),
                _ => {}
            }),
            cx.subscribe_in(&linear.key_input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.form.set_key_input(&text);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.msg(linear_fields::Msg::Verify, window, cx),
                _ => {}
            }),
        ];
        TrackerSection { form: TrackerForm::default(), service, linear, repo_input, _subscriptions: subscriptions }
    }

    /// The section's state (tests and the visual check).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn form(&self) -> &TrackerForm {
        &self.form
    }

    /// Swap the tracker service (a test's or the visual check's canned one).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn set_service(&mut self, service: TrackerService) {
        self.service = service;
    }

    /// The project's connection was read (or read again).
    pub fn load(&mut self, cfg: &IssueTrackerConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.form.load(cfg);
        self.after_change(window, cx);
    }

    fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo = self.form.issues().repo().to_string();
        if self.repo_input.read(cx).value() != repo.as_str() {
            self.repo_input.update(cx, |s, cx| s.set_value(repo, window, cx));
        }
        let key = self.form.linear().key_input().to_string();
        self.linear.sync_key(&key, window, cx);
        self.run_linear(window, cx);
        cx.notify();
    }

    /// Run the Linear reads the choice has due off the UI thread, each
    /// answer back to the form.
    fn run_linear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(request) = self.form.linear_due() {
            self.spawn_linear(request, window, cx);
        }
        if let Some(request) = self.form.github_due() {
            let github = self.service.github.clone();
            let task = cx.background_executor().spawn(async move { github(request) });
            cx.spawn_in(window, async move |this, cx| {
                let answer = task.await;
                this.update_in(cx, |this, _, cx| {
                    this.form.github_answer(answer);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Flip an agent rule (a checkbox click).
    pub fn toggle_rule(&mut self, assigned_only: bool, cx: &mut Context<Self>) {
        if assigned_only {
            self.form.toggle_assigned_only();
        } else {
            self.form.toggle_require_review();
        }
        cx.notify();
    }

    /// The agent rules: a checkbox each, its help under it.
    fn rules(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let rules = self.form.rules();
        let editable = self.form.editable();
        let assigned = self.form.assigned_title();
        let rule = |id: &'static str, title: String, help: &'static str, on: bool, assigned_only: bool, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    controls::checkbox(id, title, on, false, theme)
                        .role(Role::CheckBox)
                        .aria_selected(on)
                        .when(!editable, |d| d.opacity(0.6))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_rule(assigned_only, cx))),
                )
                .child(div().pl(px(30.0)).type_style(Type::Small).text_color(theme.form_hint).child(help))
        };
        div()
            .debug_selector(|| "tracker-rules".into())
            .flex()
            .flex_col()
            .gap(px(12.0))
            .max_w(px(640.0))
            .child(rule(
                "tracker-rule-review",
                rules_copy::REVIEW_TITLE.to_string(),
                rules_copy::REVIEW_HELP,
                rules.require_review,
                false,
                cx,
            ))
            .child(rule("tracker-rule-assigned", assigned, rules_copy::ASSIGNED_HELP, rules.assigned_only, true, cx))
    }

    fn spawn_linear(&mut self, request: LinearRequest, window: &mut Window, cx: &mut Context<Self>) {
        let linear = self.service.linear.clone();
        let task = cx.background_executor().spawn(async move { linear(request) });
        cx.spawn_in(window, async move |this, cx| {
            let answer = task.await;
            this.update_in(cx, |this, window, cx| {
                this.form.linear_answer(answer);
                this.after_change(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Pick a board.
    pub fn select(&mut self, tracker: Tracker, window: &mut Window, cx: &mut Context<Self>) {
        self.linear.menu = None;
        self.form.select(tracker);
        self.after_change(window, cx);
    }

    /// A click in the Linear fields.
    pub fn msg(&mut self, msg: linear_fields::Msg, window: &mut Window, cx: &mut Context<Self>) {
        use linear_fields::Msg;
        match msg {
            Msg::Verify => {
                self.form.submit_key();
            }
            Msg::ChangeKey => {
                self.form.change_key();
                let focus = self.linear.key_input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            Msg::CancelChangeKey => self.form.cancel_change_key(),
            Msg::Retry => self.form.retry_linear(),
            Msg::Team(team) => self.form.select_team(&team),
            Msg::Filter(filter, value) => self.form.select_filter(filter, Some(&value)),
        }
        self.after_change(window, cx);
    }

    fn menu(&mut self, pick: Option<Pick>, cx: &mut Context<Self>) {
        match pick {
            Some(p) => self.linear.toggle(p),
            None => self.linear.menu = None,
        }
        cx.notify();
    }

    /// Connect (or Save); `force` is Switch anyway.
    pub fn connect(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.form.submit(force) else {
            cx.notify();
            return;
        };
        self.linear.menu = None;
        let connect = self.service.connect.clone();
        let task = cx.background_executor().spawn(async move { connect(request) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                let ok = result.is_ok();
                this.form.connected(result);
                if ok {
                    cx.emit(TrackerEvent::Connected);
                }
                this.after_change(window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.linear.menu = None;
        self.form.cancel();
        self.after_change(window, cx);
    }

    /// One board's radio and its name.
    fn choice(&self, tracker: Tracker, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let id = choice_selector(tracker);
        let on = self.form.tracker() == tracker;
        let editable = self.form.editable();
        div()
            .id(id)
            .debug_selector(move || id.into())
            .role(Role::RadioButton)
            .aria_label(choice_title(tracker))
            .aria_selected(on)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .when(editable, |d| d.cursor_pointer())
            .when(!editable, |d| d.opacity(0.6))
            .child(radio(on, theme))
            .child(div().type_style(Type::Base).text_color(theme.form_text).child(choice_title(tracker)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select(tracker, window, cx)))
    }

    fn repo_field(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.repo_input.read(cx).focus_handle(cx).is_focused(window);
        let field = controls::textbox(
            div().flex_1().min_w_0().child(
                Input::new(&self.repo_input)
                    .appearance(false)
                    .h_full()
                    .px_0()
                    .type_style(Type::Base)
                    .text_color(theme.form_text),
            ),
            focused,
            theme,
        )
        .debug_selector(|| "tracker-github-repo".into());
        controls::labeled_control(
            np::GITHUB_REPO_LABEL,
            div().w(px(controls::CONTROL_W)).max_w_full().child(field),
            Some(np::GITHUB_BODY.into()),
            theme,
        )
    }
}

impl Render for TrackerSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let form = &self.form;
        let radios = div()
            .id("tracker-choices")
            .role(Role::RadioGroup)
            .aria_label(tr::TITLE)
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(24.0))
            .children(CHOICES.map(|t| self.choice(t, &theme, cx)));
        let body = match form.tracker() {
            Tracker::FileSystem => div()
                .type_style(Type::Small)
                .text_color(theme.form_hint)
                .child(np::FILE_SYSTEM_BODY)
                .into_any_element(),
            Tracker::Github => self.repo_field(&theme, window, cx).into_any_element(),
            Tracker::Linear => linear_fields::fields(
                linear_fields::Host::Settings,
                form.linear(),
                &self.linear,
                linear_fields::Ring::default(),
                form.editable(),
                linear_fields::Handlers {
                    msg: |this: &mut Self, msg, window, cx| this.msg(msg, window, cx),
                    menu: |this: &mut Self, pick, cx| this.menu(pick, cx),
                },
                &theme,
                window,
                cx,
            )
            .max_w(px(640.0))
            .into_any_element(),
        };
        let rules = form.shows_rules().then(|| self.rules(&theme, cx));
        let form = &self.form;
        let error = form.error().map(|e| {
            div()
                .debug_selector(|| "tracker-error".into())
                .max_w(px(640.0))
                .type_style(Type::Small)
                .text_color(theme.error)
                .child(e)
        });
        let status = form.status_line().map(|s| {
            div()
                .debug_selector(|| "tracker-status".into())
                .type_style(Type::Small)
                .text_color(theme.form_hint)
                .child(s)
        });
        let cancel = if form.cancel_enabled() {
            style::button("tracker-cancel", CANCEL, ButtonKind::Secondary, ButtonSize::Base, false, &theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cancel(window, cx)))
        } else {
            style::disabled_button("tracker-cancel", CANCEL, ButtonKind::Secondary, ButtonSize::Base, &theme)
        };
        let force = form.offers_force().then(|| {
            style::button("tracker-force", SWITCH_ANYWAY, ButtonKind::Danger, ButtonSize::Base, false, &theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.connect(true, window, cx)))
        });
        let label = form.primary_label();
        let primary = if form.primary_enabled() {
            style::button("tracker-connect", label, ButtonKind::Primary, ButtonSize::Base, false, &theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.connect(false, window, cx)))
        } else {
            style::disabled_button("tracker-connect", label, ButtonKind::Primary, ButtonSize::Base, &theme)
        };
        div()
            .id("tracker-section")
            .debug_selector(|| "tracker-section".into())
            .role(Role::Group)
            .aria_label(tr::TITLE)
            .flex()
            .flex_col()
            .gap(px(16.0))
            .w_full()
            .child(radios)
            .child(body)
            .children(rules)
            .children(error)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .max_w(px(640.0))
                    .child(div().flex_1().min_w_0().children(status))
                    .child(div().flex().flex_row().gap(px(8.0)).child(cancel).children(force).child(primary)),
            )
    }
}
