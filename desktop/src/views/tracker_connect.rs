//! Connect to an Issue tracker (`tracker-connect`, Figma "Issue tracker ·
//! Connection flow" 2753:11686): the guided flow the Issues header's ⋮ ▸
//! Connect to issue tracker…, File ▸ Connect to Issue Tracker… and the
//! palette open for the window's project. Its own window, fixed at
//! 800 × 580 with only the traffic lights over its content, like the Add
//! Project wizard; one per project (asking again brings it to the front).
//!
//! [`TrackerFlow`] (shared, toolkit-free) holds the steps and decides what
//! each button does; this view paints it, takes the keys, and runs what the
//! flow asks for off the UI thread through the project's
//! [`TrackerService`]: this machine's, or the far daemon's
//! (`far::Request::ConnectTracker` / `Linear`) for a project on another
//! machine, as Settings ▸ Properties does. The project's current connection
//! is read through the window's [`ProjectHandle`] source the same way.
//!
//! ```text
//!                  Connect to an Issue tracker
//!      Bring external issues into Shelbi and keep work in sync.
//!   ┌──────────────────────────────────────────────────────────┐
//!   │ (gh) GitHub Issues                            [Connect]  │
//!   │      Sync issues from a GitHub repository.               │
//!   └──────────────────────────────────────────────────────────┘
//!   ┌──────────────────────────────────────────────────────────┐
//!   │ (ln) Linear                                   [Connect]  │
//!   │      Sync issues from a Linear team.                     │
//!   └──────────────────────────────────────────────────────────┘
//!          Only one issue tracker can be connected at a time.
//!
//!   (every later step)
//!   Step N of 4                                  [Back] [Continue]
//! ```
//!
//! Keys: Return is the step's primary button, Esc and ⌘W close (a
//! connection already running carries on in the background). The Choose
//! project, Agent rules and Review import steps draw placeholders until
//! `tc-choose` and `tc-import` build them.

use std::collections::HashMap;

use gpui::{
    div, prelude::*, px, AnyWindowHandle, App, ClickEvent, Context, Div, FocusHandle, Focusable, Global, Role,
    Stateful, Subscription, Window, WindowHandle,
};
use shelbi_app::new_project::Tracker;
use shelbi_app::settings::tracker::TrackerService;
use shelbi_app::source::ProjectHandle;
use shelbi_app::tracker_connect::flow::{
    self as copy, Action, Apply, Card, CardState, FlowRequest, Secondary, Step, TrackerFlow,
};
use shelbi_app::tracker_connect::StatusSync;
use shelbi_state::keymap::{Key, KeyChord, Mods};

use crate::app::{CloseWindow, ShelbiWindow};
use crate::assets::{heroicon, icons};
use crate::elements::spinner;
use crate::keys::chord_from_keystroke;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The window's size (the frames').
pub const SIZE: (f32, f32) = (800.0, 580.0);
/// The window's title (the menu bar's Window list).
pub const TITLE: &str = "Connect to Issue Tracker";
/// The cards' and the steps' width inside the 48pt inset.
const CONTENT_W: f32 = 704.0;
/// The room between the subheading and a step's content (the frames' 76pt,
/// less the column's 24pt gap).
const CONTENT_TOP: f32 = 52.0;

/// The open flows, by project: asking again raises the open one.
#[derive(Default)]
struct OpenFlows(HashMap<String, AnyWindowHandle>);

impl Global for OpenFlows {}

/// Open the flow for `handle`'s project (or bring its open one to the
/// front). `from` is the project window that asked: View board shows its
/// board, and a connection reloads it.
pub fn show(handle: ProjectHandle, from: Option<WindowHandle<ShelbiWindow>>, cx: &mut App) {
    let key = handle.key().to_string();
    if let Some(open) = cx.try_global::<OpenFlows>().and_then(|o| o.0.get(&key).copied()) {
        if open.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let service = super::properties::tracker_service(&handle);
    let opened = cx.open_window(super::about::options(TITLE, SIZE, false, cx), |window, cx| {
        cx.new(|cx| TrackerConnectView::new(handle, service, from, window, cx))
    });
    match opened {
        Ok(window) => {
            cx.default_global::<OpenFlows>().0.insert(key, window.into());
            cx.activate(true);
        }
        Err(e) => tracing::error!(error = %e, "could not open Connect to Issue Tracker"),
    }
}

/// The flow's window.
pub struct TrackerConnectView {
    flow: TrackerFlow,
    service: TrackerService,
    handle: ProjectHandle,
    from: Option<WindowHandle<ShelbiWindow>>,
    focus: FocusHandle,
    /// Why the project's connection couldn't be read.
    load_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl TrackerConnectView {
    pub fn new(
        handle: ProjectHandle,
        service: TrackerService,
        from: Option<WindowHandle<ShelbiWindow>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let this_window = window.window_handle();
        let me = cx.weak_entity();
        let subscriptions = vec![cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != this_window {
                return;
            }
            let Some(chord) = chord_from_keystroke(&event.keystroke) else {
                return;
            };
            if me.update(cx, |this, cx| this.key(chord, window, cx)).unwrap_or(false) {
                cx.stop_propagation();
            }
        })];
        let mut view = TrackerConnectView {
            flow: TrackerFlow::new(),
            service,
            handle,
            from,
            focus,
            load_error: None,
            _subscriptions: subscriptions,
        };
        view.load(window, cx);
        view
    }

    /// The flow (tests).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn flow(&self) -> &TrackerFlow {
        &self.flow
    }

    /// Change the flow as a picker would, then run what it asks for (tests,
    /// until `tc-choose` draws the pickers).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn edit_flow(&mut self, edit: impl FnOnce(&mut TrackerFlow), window: &mut Window, cx: &mut Context<Self>) {
        edit(&mut self.flow);
        self.run_due(window, cx);
        cx.notify();
    }

    /// Read the project's connection off the UI thread (over the far link
    /// for a project on another machine).
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.handle.source.clone();
        let read = cx.background_executor().spawn(async move { source.workflow_config() });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update_in(cx, |this, window, cx| this.loaded(config.map(|c| c.issue_tracker().config.clone()), window, cx))
                .ok();
        })
        .detach();
    }

    /// The connection was read (a test hands it in).
    pub fn loaded(
        &mut self,
        config: Result<shelbi_core::IssueTrackerConfig, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match config {
            Ok(cfg) => {
                self.load_error = None;
                self.flow.load(&cfg);
            }
            Err(why) => self.load_error = Some(format!("Couldn't read this project's settings: {why}")),
        }
        self.run_due(window, cx);
        cx.notify();
    }

    /// Close the window (Esc, ⌘W, Close). A running connection carries on;
    /// its answer reaches the project window.
    fn close(&mut self, window: &mut Window) {
        window.remove_window();
    }

    /// A key the flow takes: Return is the primary button, Esc closes.
    fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if chord.mods != Mods::NONE {
            return false;
        }
        match chord.code {
            Key::Enter => self.primary(window, cx),
            Key::Esc => self.close(window),
            _ => return false,
        }
        true
    }

    /// A card's Connect.
    fn connect(&mut self, tracker: Tracker, window: &mut Window, cx: &mut Context<Self>) {
        if self.flow.connect(tracker) {
            self.run_due(window, cx);
        }
        cx.notify();
    }

    /// The footer's primary button.
    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.flow.ready() {
            // Continue says why it can't go on.
            self.flow.advance();
            cx.notify();
            return;
        }
        match self.flow.primary() {
            Action::ViewBoard => {
                self.land(true, cx);
                self.close(window);
                return;
            }
            Action::None => self.run_due(window, cx),
        }
        cx.notify();
    }

    /// The footer's left button.
    fn secondary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.flow.secondary() {
            Some(Secondary::Back) => {
                self.flow.back();
            }
            Some(Secondary::Close) => self.close(window),
            None => {}
        }
        cx.notify();
    }

    /// Run every request the flow has due, each off the UI thread.
    fn run_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(request) = self.flow.due() {
            match request {
                FlowRequest::Linear(req) => {
                    let linear = self.service.linear.clone();
                    let work = cx.background_executor().spawn(async move { linear(req) });
                    cx.spawn_in(window, async move |this, cx| {
                        let answer = work.await;
                        this.update_in(cx, |this, window, cx| {
                            this.flow.linear_answer(answer);
                            this.run_due(window, cx);
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                FlowRequest::Connect(req) => {
                    let connect = self.service.connect.clone();
                    let work = cx.background_executor().spawn(async move { connect(req) });
                    cx.spawn_in(window, async move |this, cx| {
                        let result = work.await;
                        this.update_in(cx, |this, _, cx| {
                            let written = result.is_ok();
                            this.flow.connected(result);
                            if written {
                                this.land(false, cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            }
        }
    }

    /// Tell the project window: a connection was written (it reads its
    /// board again), and on View board, show the board.
    fn land(&self, view_board: bool, cx: &mut App) {
        let Some(from) = self.from else { return };
        let message = self.flow.finished().map(|done| format!("Connected to {}.", done.summary.trim_end_matches('.')));
        from.update(cx, |w, window, cx| w.tracker_connected(view_board, message, window, cx)).ok();
    }
}

impl Focusable for TrackerConnectView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A tracker's mark.
fn mark(tracker: Tracker) -> crate::assets::Icon {
    match tracker {
        Tracker::Linear => icons::LINEAR,
        Tracker::Github | Tracker::FileSystem => icons::GITHUB,
    }
}

/// A card's id (`tracker-card-github`) and its button's.
fn card_id(tracker: Tracker) -> &'static str {
    match tracker {
        Tracker::Github => "tracker-card-github",
        Tracker::Linear => "tracker-card-linear",
        Tracker::FileSystem => "tracker-card-local",
    }
}

fn button_id(tracker: Tracker) -> &'static str {
    match tracker {
        Tracker::Github => "tracker-connect-github",
        Tracker::Linear => "tracker-connect-linear",
        Tracker::FileSystem => "tracker-connect-local",
    }
}

/// The green "Connected" badge in a card's button slot (02a-connected).
fn connected_badge(tracker: Tracker, theme: &Theme) -> Div {
    div()
        .debug_selector(move || format!("{}-connected", card_id(tracker)))
        .flex_none()
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(6.0))
        .bg(theme.green.opacity(0.16))
        .type_style(Type::XSmall)
        .text_color(theme.green)
        .child(copy::CONNECTED)
}

/// A step's placeholder until its subtask draws it.
fn placeholder(id: &'static str, text: &'static str, theme: &Theme) -> Div {
    div()
        .debug_selector(move || id.into())
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(160.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.divider)
        .type_style(Type::Small)
        .text_color(theme.muted)
        .child(text)
}

/// A line of the Importing step's checklist: a check (green) or a spinner.
fn progress_line(id: &'static str, done: bool, text: String, theme: &Theme) -> Div {
    let color = if done { theme.green } else { theme.cyan };
    div()
        .debug_selector(move || id.into())
        .flex()
        .items_center()
        .gap(px(6.0))
        .type_style(Type::Base)
        .text_color(color)
        .child(if done {
            heroicon(icons::CHECK).text_color(color).into_any_element()
        } else {
            spinner::spinner_sized(format!("{id}-spinner"), color, 14.0).into_any_element()
        })
        .child(text)
}

impl TrackerConnectView {
    fn card(&self, card: Card, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let tracker = card.tracker;
        let action = match card.state {
            CardState::Connected => connected_badge(tracker, theme).into_any_element(),
            CardState::Disabled => {
                style::disabled_button(button_id(tracker), copy::CONNECT, ButtonKind::Primary, ButtonSize::Small, theme)
                    .role(Role::Button)
                    .aria_label(format!("{} {}", copy::CONNECT, card.title))
                    .into_any_element()
            }
            CardState::Connect => {
                style::button(button_id(tracker), copy::CONNECT, ButtonKind::Primary, ButtonSize::Small, false, theme)
                    .role(Role::Button)
                    .aria_label(format!("{} {}", copy::CONNECT, card.title))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.connect(tracker, window, cx)))
                    .into_any_element()
            }
        };
        div()
            .id(card_id(tracker))
            .debug_selector(move || card_id(tracker).into())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .w_full()
            .px(px(16.0))
            .py(px(16.0))
            .rounded(px(12.0))
            .bg(theme.toolbar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(heroicon(mark(tracker)).text_color(theme.icon))
                            .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(card.title)),
                    )
                    .child(div().type_style(Type::Small).text_color(theme.muted).child(card.body)),
            )
            .child(action)
    }

    fn chooser(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let cards: Vec<_> = self.flow.cards().into_iter().map(|c| self.card(c, theme, cx)).collect();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.0))
            .w_full()
            .children(cards)
            .child(
                div()
                    .debug_selector(|| "tracker-one-at-a-time".into())
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(copy::ONE_AT_A_TIME),
            )
    }

    fn importing(&self, theme: &Theme) -> Div {
        let mut lines = div().flex().flex_col().gap(px(16.0)).w_full();
        match self.flow.apply() {
            Apply::Idle | Apply::Wanted { .. } | Apply::Running => {
                lines = lines.child(progress_line("tracker-progress-connection", false, copy::CONNECTING.into(), theme));
            }
            Apply::Done(done) => {
                lines = lines.child(progress_line("tracker-progress-connection", true, copy::CONNECTION_SAVED.into(), theme));
                match &done.statuses {
                    StatusSync::NotNeeded => {}
                    StatusSync::Synced(_) => {
                        lines = lines.child(progress_line(
                            "tracker-progress-statuses",
                            true,
                            copy::STATUSES_APPLIED.into(),
                            theme,
                        ))
                    }
                    StatusSync::Failed(why) => {
                        lines = lines.child(
                            div()
                                .debug_selector(|| "tracker-progress-statuses".into())
                                .type_style(Type::Small)
                                .text_color(theme.error)
                                .child(format!("The statuses didn't sync: {why}")),
                        )
                    }
                }
            }
            // The refusal shows as the step's error.
            Apply::Refused(_) => {}
        }
        let running = !matches!(self.flow.apply(), Apply::Refused(_));
        lines.when(running, |d| {
            d.child(div().type_style(Type::Small).text_color(theme.muted).child(copy::BACKGROUND_NOTE))
        })
    }

    fn content(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let tracker = self.flow.tracker();
        match self.flow.step() {
            Step::Chooser => self.chooser(theme, cx),
            Step::ChooseProject => div().flex().flex_col().gap(px(12.0)).w_full().child(placeholder(
                "tracker-choose-placeholder",
                match tracker {
                    Some(Tracker::Linear) => "Workspace, Team and Project pickers (tc-choose).",
                    _ => "Organization, Repository and GitHub Project pickers (tc-choose).",
                },
                theme,
            ))
            .child(div().type_style(Type::Small).text_color(theme.muted).child(copy::ONLY_PROJECT)),
            Step::AgentRules => placeholder("tracker-rules-placeholder", "The two agent rules (tc-import).", theme),
            Step::ReviewImport => placeholder("tracker-review-placeholder", "The first import's preview (tc-import).", theme),
            Step::Importing => self.importing(theme),
        }
    }

    /// The step counter, then the left button and the primary one (every
    /// step after the chooser).
    fn footer(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        let label = self.flow.primary_label()?;
        let secondary = self.flow.secondary().map(|s| {
            let (id, text) = match s {
                Secondary::Back => ("tracker-back", copy::BACK),
                Secondary::Close => ("tracker-close", copy::CLOSE),
            };
            style::button(id, text, ButtonKind::Secondary, ButtonSize::Base, false, theme)
                .role(Role::Button)
                .aria_label(text)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.secondary(window, cx)))
        });
        let primary = if self.flow.ready() {
            style::button("tracker-primary", label, ButtonKind::Primary, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.primary(window, cx)))
        } else {
            style::disabled_button("tracker-primary", label, ButtonKind::Primary, ButtonSize::Base, theme)
        }
        .role(Role::Button)
        .aria_label(label)
        .aria_keyshortcuts("Return");
        Some(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .w_full()
                .child(
                    div()
                        .debug_selector(|| "tracker-step".into())
                        .type_style(Type::SmallMedium)
                        .text_color(theme.muted)
                        .children(self.flow.counter()),
                )
                .child(div().flex_1())
                .children(secondary)
                .child(primary),
        )
    }
}

impl Render for TrackerConnectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let error = self.load_error.clone().or_else(|| self.flow.error());
        let content = self.content(&theme, cx);
        let footer = self.footer(&theme, cx);
        // The frames hang each step's content a fixed way under the heading;
        // only Importing's progress sits in the middle.
        let centered = self.flow.step() == Step::Importing;
        div()
            .id("tracker-connect")
            .debug_selector(|| "tracker-connect".into())
            .role(Role::Dialog)
            .aria_label(TITLE)
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(24.0))
            .p(px(48.0))
            .bg(theme.canvas)
            .font_family(style::UI_FAMILY)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .w_full()
                    .text_center()
                    .child(
                        div()
                            .debug_selector(|| "tracker-heading".into())
                            .type_style(Type::Title)
                            .text_color(theme.text_loud)
                            .child(self.flow.heading()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "tracker-subheading".into())
                            .type_style(Type::Base)
                            .text_color(theme.foreground)
                            .child(self.flow.subheading()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .when(centered, |d| d.justify_center())
                    .when(!centered, |d| d.pt(px(CONTENT_TOP)))
                    .gap(px(16.0))
                    .w(px(CONTENT_W))
                    .child(content)
                    .children(error.map(|e| {
                        div()
                            .debug_selector(|| "tracker-error".into())
                            .type_style(Type::Small)
                            .text_color(theme.error)
                            .child(e)
                    })),
            )
            .children(footer)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{Entity, TestAppContext, VisualTestContext};
    use shelbi_app::source::ProjectKey;
    use shelbi_app::tracker_connect::{ConnectError, ConnectTracker, Connected, TrackerChoice};
    use shelbi_core::{GithubConnection, IssueTrackerBackend, IssueTrackerConfig};

    use super::*;

    struct Rig {
        cx: VisualTestContext,
        view: Entity<TrackerConnectView>,
    }

    /// A service that records each connection and writes it.
    fn recording(asked: Arc<Mutex<Vec<ConnectTracker>>>) -> TrackerService {
        TrackerService {
            connect: Arc::new(move |req| {
                asked.lock().unwrap().push(req.clone());
                Ok(Connected {
                    backend: req.choice.backend(),
                    previous: IssueTrackerBackend::FileSystem,
                    file: "alpha.yaml".into(),
                    summary: "GitHub acme/app".into(),
                    statuses: StatusSync::NotNeeded,
                })
            }),
            linear: Arc::new(shelbi_app::linear_setup::fixture::answer),
            sync_statuses: Arc::new(|| StatusSync::NotNeeded),
        }
    }

    fn rig(cx: &mut TestAppContext, handle: ProjectHandle, service: TrackerService) -> Rig {
        super::super::about::tests::init(cx);
        let options = gpui::WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                gpui::Point::default(),
                gpui::size(px(SIZE.0), px(SIZE.1)),
            ))),
            ..Default::default()
        };
        let window = cx.update(|cx| {
            cx.open_window(options, |window, cx| cx.new(|cx| TrackerConnectView::new(handle, service, None, window, cx)))
                .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut vcx).unwrap();
        vcx.run_until_parked();
        Rig { cx: vcx, view }
    }

    /// The local project `alpha` (its source reads nothing: each test hands
    /// in the connection).
    fn local(cx: &mut TestAppContext, service: TrackerService, cfg: IssueTrackerConfig) -> Rig {
        let mut r = rig(cx, ProjectHandle::null(ProjectKey::local("alpha")), service);
        r.update(|v, window, cx| v.loaded(Ok(cfg), window, cx));
        r
    }

    impl Rig {
        fn update(&mut self, f: impl FnOnce(&mut TrackerConnectView, &mut Window, &mut Context<TrackerConnectView>)) {
            let view = self.view.clone();
            self.cx.update(|window, cx| view.update(cx, |v, cx| f(v, window, cx)));
            self.cx.run_until_parked();
        }

        fn flow<R>(&mut self, f: impl FnOnce(&TrackerFlow) -> R) -> R {
            self.view.read_with(&self.cx, |v, _| f(v.flow()))
        }

        fn drawn(&mut self, selector: &'static str) -> bool {
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
            self.cx.run_until_parked();
            self.cx.debug_bounds(selector).is_some()
        }

        fn click(&mut self, selector: &'static str) {
            assert!(self.drawn(selector), "{selector} is drawn");
            let at = self.cx.debug_bounds(selector).unwrap().center();
            self.cx.simulate_click(at, gpui::Modifiers::none());
            self.cx.run_until_parked();
        }

        fn keys(&mut self, keys: &str) {
            self.cx.simulate_keystrokes(keys);
            self.cx.run_until_parked();
        }
    }

    fn github(repo: &str) -> IssueTrackerConfig {
        IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection { repo: repo.into() }),
            ..IssueTrackerConfig::default()
        }
    }

    #[gpui::test]
    fn the_chooser_offers_github_and_linear_and_connect_moves_on(cx: &mut TestAppContext) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut r = local(cx, recording(asked.clone()), IssueTrackerConfig::default());
        assert!(r.drawn("tracker-card-github") && r.drawn("tracker-card-linear"));
        assert!(!r.drawn("tracker-card-jira"), "no Jira");
        assert!(r.drawn("tracker-one-at-a-time"));
        assert!(!r.drawn("tracker-step") && !r.drawn("tracker-primary"), "the chooser has no footer");
        assert_eq!(r.flow(|f| f.heading()), copy::CHOOSER_HEADING);

        r.click("tracker-connect-github");
        assert_eq!(r.flow(|f| (f.step(), f.counter())), (Step::ChooseProject, Some("Step 1 of 4".to_string())));
        assert!(r.drawn("tracker-step") && r.drawn("tracker-back") && r.drawn("tracker-primary"));
        assert!(r.drawn("tracker-choose-placeholder"));
        // Continue waits for a repository, and says why.
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::ChooseProject);
        assert!(r.drawn("tracker-error"));
        r.update(|v, window, cx| v.edit_flow(|f| f.set_repo("acme/app"), window, cx));
        assert!(!r.drawn("tracker-error"));
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::AgentRules);
        r.click("tracker-back");
        assert_eq!(r.flow(|f| f.step()), Step::ChooseProject);
        r.keys("enter");
        r.keys("enter");
        assert_eq!(r.flow(|f| (f.step(), f.primary_label())), (Step::ReviewImport, Some(copy::IMPORT)));
        // Import writes the connection, through the service, once.
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::Importing);
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            [ConnectTracker { choice: TrackerChoice::Github { repo: "acme/app".into() }, force: false }]
        );
        assert!(r.flow(|f| f.finished().is_some()));
        assert!(r.drawn("tracker-progress-connection") && r.drawn("tracker-close"));
        assert_eq!(r.flow(|f| f.primary_label()), Some(copy::VIEW_BOARD));
    }

    #[gpui::test]
    fn a_connected_tracker_shows_connected_and_the_other_card_is_disabled(cx: &mut TestAppContext) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut r = local(cx, recording(asked.clone()), github("acme/app"));
        assert!(r.drawn("tracker-card-github-connected"));
        assert!(!r.drawn("tracker-connect-github"), "a badge in its button's place");
        assert!(r.drawn("tracker-connect-linear"));
        r.click("tracker-connect-linear");
        assert_eq!(r.flow(|f| f.step()), Step::Chooser, "the disabled Connect does nothing");
        assert!(asked.lock().unwrap().is_empty());
    }

    #[gpui::test]
    fn esc_closes_the_window(cx: &mut TestAppContext) {
        let mut r = local(cx, recording(Arc::default()), IssueTrackerConfig::default());
        let windows = r.cx.cx.update(|cx| cx.windows().len());
        r.keys("escape");
        assert_eq!(r.cx.cx.update(|cx| cx.windows().len()), windows - 1);
    }

    /// A project on another machine: the flow reads its connection over the
    /// host's link, and the connection runs on the far daemon
    /// (`far::Request::ConnectTracker`), which refuses it there because the
    /// far board has issues (so nothing runs `gh`).
    #[gpui::test]
    fn a_far_project_reads_and_connects_through_its_daemon(cx: &mut TestAppContext) {
        let far = crate::remote_fixture::FarHost::new("tracker-flow");
        let mut remote = crate::app::RemoteWindow::new(far.session(), far.listing(), far.link.clone(), far.remote.clone());
        let handle = remote.handle_for_test(crate::remote_fixture::PROJECT);
        assert!(handle.link.is_remote());
        let service = super::super::properties::tracker_service(&handle);
        let mut r = rig(cx, handle, service);
        assert!(r.flow(|f| f.loaded()), "the far project's connection was read");
        assert!(r.drawn("tracker-connect-github"), "the far project is on its local board");

        r.click("tracker-connect-github");
        r.update(|v, window, cx| v.edit_flow(|f| f.set_repo("acme/app"), window, cx));
        r.keys("enter");
        r.keys("enter");
        r.keys("enter");
        let asked: Vec<_> = far.asked.lock().unwrap().requests.clone();
        assert!(
            asked.iter().any(|q| matches!(q, shelbi_app::far::Request::ConnectTracker(c)
                if c.choice == TrackerChoice::Github { repo: "acme/app".into() })),
            "{asked:?}"
        );
        assert!(
            r.flow(|f| matches!(f.apply(), Apply::Refused(ConnectError::NotEmpty { issues: 2, .. }))),
            "{:?}",
            r.flow(|f| f.apply().clone())
        );
        assert_eq!(r.flow(|f| f.primary_label()), Some(copy::SWITCH_ANYWAY));
        assert!(r.drawn("tracker-error"));
    }
}
