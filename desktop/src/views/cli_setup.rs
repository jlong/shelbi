//! The first launch's CLI setup (`dt-wizard-cli-setup`, Figma "CLI Check"
//! 2478:8507, "CLI Install" 2480:8522, "CLI Ready" 2480:8577): the
//! unnumbered screens before the Add Project wizard's 01.
//!
//! A first launch (no window to restore, `main`) opens this window at the
//! wizard's 800 × 580. It runs bootstrap ([`crate::platform::Bootstrap`]),
//! which finds the CLI or, when there is none, installs the release matching
//! the app (`platform::install`), then starts the daemon and checks the
//! control connection, and shows where it is: "Checking for Shelbi CLI…",
//! "Installing CLI…" (once an install starts), "Ready" with the bar full
//! only once bootstrap reached `Ready`. Ready advances by itself: with no
//! project registered, the window becomes the Add Project wizard (the same
//! window, its root replaced, [`add_project::replace_root`]); with one
//! (made from a terminal meanwhile), the launch's normal windows open
//! instead. Either way the project windows reuse this bootstrap's result
//! ([`crate::app::share_ready`]) rather than running their own.
//!
//! A failure shows bootstrap's own failure screen for it (an install's says
//! what broke and the other ways to install; an incompatible or misconfigured
//! CLI keeps its diagnosis), with Try Again (also Return), which runs
//! bootstrap again. Nothing is retried on a timer, and a retry can only start
//! from a failure, so there is never a second job.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, AnyElement, AnyWindowHandle, App, Context, FocusHandle, Focusable, KeyDownEvent, Window,
};

use crate::app::CloseWindow;
use crate::assets::{heroicon, icons};
use crate::platform::version::APP_VERSION;
use crate::platform::{relaunch, Bootstrap, BootstrapEvent, BootstrapState, SystemPlatform};
use crate::theme::style::{self, Type, Typography};
use crate::theme::{self, Theme};
use crate::views::add_project::{self, SIZE};
use crate::views::bootstrap::{self as boot_view, ScreenAction};
use crate::window_state::LaunchWindow;

/// The screens' heading.
pub const TITLE: &str = "Let's get Shelbi ready";
pub const CHECKING: &str = "Checking for Shelbi CLI…";
pub const INSTALLING: &str = "Installing CLI…";
pub const READY: &str = "Ready";
/// The progress bar's width (the frames').
const BAR_W: f32 = 358.0;
/// How long Ready shows before the window moves on.
const READY_DWELL: Duration = Duration::from_millis(700);

/// What a screen shows while bootstrap is in flight or done.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Status {
    pub label: &'static str,
    /// The bar's fill, 0 to 1 (the frames' ten segments: Check 1, Install 7,
    /// Ready 10).
    pub fraction: f32,
    /// Ready: the check mark shows.
    pub done: bool,
}

/// `state`'s screen. `installing`: this run started an install, so the
/// checks after it (finding the new CLI, the daemon, the control
/// connection) are still the install's.
pub fn status(state: &BootstrapState, installing: bool) -> Status {
    use crate::platform::install::Step;
    let (label, fraction) = match state {
        BootstrapState::Ready(_) => {
            return Status {
                label: READY,
                fraction: 1.0,
                done: true,
            }
        }
        BootstrapState::InstallingCli { step } => (
            INSTALLING,
            match step {
                Step::Downloading => 0.2,
                Step::Verifying => 0.4,
                Step::Installing => 0.6,
            },
        ),
        BootstrapState::FindingCli if installing => (INSTALLING, 0.7),
        BootstrapState::StartingDaemon { .. } if installing => (INSTALLING, 0.8),
        BootstrapState::Connecting { .. } if installing => (INSTALLING, 0.9),
        BootstrapState::ResolvingEnv | BootstrapState::FindingCli => (CHECKING, 0.1),
        BootstrapState::StartingDaemon { .. } => (CHECKING, 0.5),
        BootstrapState::Connecting { .. } => (CHECKING, 0.8),
        // Drawn as its failure screen; the bar would stay where it was.
        BootstrapState::Failed(_) => (CHECKING, 0.1),
    };
    Status {
        label,
        fraction,
        done: false,
    }
}

/// Where Ready leads.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// No project yet: the Add Project wizard's 01, in this window.
    AddProject,
    /// A project exists: the launch's windows (this one closes).
    Restore(Vec<LaunchWindow>),
}

/// Starts a bootstrap run.
pub type StartFn = dyn Fn(&mut App) -> async_channel::Receiver<BootstrapEvent>;
/// The windows a launch would restore now (blocking: run off the UI thread).
pub type PlanFn = dyn Fn() -> Vec<LaunchWindow> + Send + Sync;
/// Goes where Ready leads, from this window.
pub type NextFn = dyn Fn(Next, AnyWindowHandle, &mut App);

/// What the view runs; a test swaps in its own (scripted bootstrap events,
/// a canned plan, a recorder for where it went).
#[derive(Clone)]
pub struct Deps {
    pub start: Rc<StartFn>,
    pub plan: Arc<PlanFn>,
    pub next: Rc<NextFn>,
    pub dwell: Duration,
}

impl Deps {
    /// The real machine.
    pub fn real() -> Self {
        Deps {
            start: Rc::new(|cx| {
                let client_id = crate::app::client_id(cx);
                Bootstrap::spawn_as(SystemPlatform::default(), APP_VERSION.to_string(), client_id)
            }),
            plan: Arc::new(|| crate::read_launch_plan().1),
            next: Rc::new(go),
            dwell: READY_DWELL,
        }
    }
}

/// The real [`Next`]: replace this window's root with the wizard, or open
/// the launch's windows and close this one (after, so the app always has a
/// window).
fn go(next: Next, handle: AnyWindowHandle, cx: &mut App) {
    match next {
        Next::AddProject => {
            let _ = handle.update(cx, |_, window, cx| add_project::replace_root(window, cx));
        }
        Next::Restore(plan) => {
            crate::app::open_launch_windows(plan, cx);
            cx.activate(true);
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }
}

/// Open the setup window (the first launch).
pub fn show(cx: &mut App) {
    let opened = cx.open_window(super::about::options("Shelbi", SIZE, false, cx), |window, cx| {
        cx.new(|cx| CliSetupView::new(Deps::real(), window, cx))
    });
    match opened {
        Ok(handle) => {
            add_project::opened(handle.into(), cx);
            cx.activate(true);
        }
        Err(e) => tracing::error!(error = %e, "could not open the setup window"),
    }
}

/// The CLI setup screens.
pub struct CliSetupView {
    state: BootstrapState,
    /// This run started an install.
    installing: bool,
    /// Bumped per run: a run's late events never reach a newer one.
    run: u64,
    /// Ready was reached and the window is moving on.
    advancing: bool,
    focus: FocusHandle,
    window: AnyWindowHandle,
    deps: Deps,
}

impl CliSetupView {
    pub fn new(deps: Deps, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        theme::window::apply_to(window, cx);
        let mut this = CliSetupView {
            state: BootstrapState::ResolvingEnv,
            installing: false,
            run: 0,
            advancing: false,
            focus,
            window: window.window_handle(),
            deps,
        };
        this.start(cx);
        this
    }

    /// The state shown (tests).
    #[cfg(test)]
    pub fn state(&self) -> &BootstrapState {
        &self.state
    }

    /// Start a bootstrap run and listen to it. The listener outlives the
    /// view: after Ready, the daemon's relaunch and detach pushes still
    /// reach the app (the project windows reuse this run's control handle).
    fn start(&mut self, cx: &mut Context<Self>) {
        self.run += 1;
        self.state = BootstrapState::ResolvingEnv;
        self.installing = false;
        let run = self.run;
        let events = (self.deps.start)(cx);
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                match event {
                    BootstrapEvent::State(state) => {
                        let _ = this.update(cx, |this, cx| this.apply(run, state, cx));
                    }
                    BootstrapEvent::RelaunchRequired(notice) => {
                        tracing::info!(reason = %notice.reason, "relaunch required");
                        cx.update(|cx| cx.refresh_windows());
                    }
                    BootstrapEvent::Detach(reason) => {
                        tracing::info!(%reason, "detached by the daemon");
                        cx.update(|cx| cx.quit());
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn apply(&mut self, run: u64, state: BootstrapState, cx: &mut Context<Self>) {
        if run != self.run {
            return;
        }
        if matches!(state, BootstrapState::InstallingCli { .. }) {
            self.installing = true;
        }
        match &state {
            BootstrapState::Ready(ready) => {
                tracing::info!(cli = %ready.cli.path.display(), installed = self.installing, "setup ready");
                crate::app::share_ready(ready.clone(), cx);
                self.advance(cx);
            }
            BootstrapState::Failed(failure) => tracing::warn!(?failure, "setup failed"),
            _ => {}
        }
        self.state = state;
        cx.notify();
    }

    /// After Ready shows: read where the launch goes (off the UI thread) and
    /// go there.
    fn advance(&mut self, cx: &mut Context<Self>) {
        if self.advancing {
            return;
        }
        self.advancing = true;
        let plan = self.deps.plan.clone();
        let read = cx.background_executor().spawn(async move { plan() });
        let dwell = cx.background_executor().timer(self.deps.dwell);
        let (next, handle) = (self.deps.next.clone(), self.window);
        cx.spawn(async move |_, cx| {
            dwell.await;
            let plan = read.await;
            let to = if plan.is_empty() {
                Next::AddProject
            } else {
                Next::Restore(plan)
            };
            cx.update(|cx| next(to, handle, cx));
        })
        .detach();
    }

    /// A failure screen's button.
    pub(crate) fn action(&mut self, action: ScreenAction, cx: &mut Context<Self>) {
        if !matches!(self.state, BootstrapState::Failed(_)) {
            return;
        }
        match action {
            ScreenAction::Retry => self.start(cx),
            ScreenAction::Relaunch => match relaunch::relaunch() {
                Ok(()) => cx.quit(),
                Err(e) => tracing::error!("relaunch failed: {e}"),
            },
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.key == "enter" && !k.modifiers.modified() {
            if let BootstrapState::Failed(failure) = &self.state {
                let action = boot_view::failure_screen(failure).action;
                self.action(action, cx);
            }
        }
    }

    fn progress(&self, theme: &Theme) -> AnyElement {
        let status = status(&self.state, self.installing);
        // Check holds the bar under the label at 12; Install and Ready at 16.
        let gap = if status.label == CHECKING { 12.0 } else { 16.0 };
        let fill = (BAR_W * status.fraction.clamp(0.0, 1.0)).round();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .gap(px(gap))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(20.0))
                    .gap(px(4.0))
                    .child(
                        div()
                            .debug_selector(|| "cli-setup-status".into())
                            .type_style(Type::BaseMedium)
                            .text_color(theme.text_loud)
                            .child(status.label),
                    )
                    .when(status.done, |row| {
                        row.child(heroicon(icons::CHECK).text_color(theme.accent))
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "cli-setup-bar".into())
                    .flex()
                    .flex_row()
                    .w(px(BAR_W))
                    .h(px(4.0))
                    .rounded(px(2.0))
                    .overflow_hidden()
                    // `surface/depth-20`, the track; `progress/default`, the
                    // fill.
                    .bg(theme.background)
                    .child(div().w(px(fill)).h_full().rounded_full().bg(theme.accent)),
            )
            .into_any_element()
    }
}

impl Focusable for CliSetupView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CliSetupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let root = div()
            .id("cli-setup")
            .debug_selector(|| "cli-setup".into())
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.canvas)
            .font_family(style::UI_FAMILY);
        if let BootstrapState::Failed(failure) = &self.state {
            let weak = cx.entity().downgrade();
            // Below the traffic lights; the screen scrolls when it's long.
            return root
                .pt(px(28.0))
                .child(boot_view::render_failure(
                    &boot_view::failure_screen(failure),
                    &theme,
                    move |action, _, cx| {
                        let _ = weak.update(cx, |this, cx| this.action(action, cx));
                    },
                ))
                .into_any_element();
        }
        root.items_center()
            .gap(px(24.0))
            .pt(px(64.0))
            .px(px(64.0))
            .pb(px(32.0))
            .child(
                div()
                    .debug_selector(|| "cli-setup-title".into())
                    .flex_none()
                    .w_full()
                    .text_center()
                    .type_style(Type::Title)
                    .text_color(theme.text_loud)
                    .child(TITLE),
            )
            .child(self.progress(&theme))
            // The frames' spacer under the details.
            .child(div().flex_none().h(px(32.0)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::platform::bootstrap::Failure;
    use crate::platform::discovery::{Candidate, CliSource};
    use crate::platform::install::{self, InstallError, InstallErrorKind};
    use crate::window_state::WindowState;

    fn cli() -> Candidate {
        Candidate {
            path: "/Users/me/bin/shelbi".into(),
            source: CliSource::KnownLocation,
        }
    }

    #[test]
    fn the_screens_follow_bootstrap() {
        let checking = status(&BootstrapState::FindingCli, false);
        assert_eq!((checking.label, checking.fraction, checking.done), (CHECKING, 0.1, false));
        let daemon = status(&BootstrapState::StartingDaemon { cli: cli() }, false);
        assert_eq!(daemon.label, CHECKING, "a found CLI never says installing");
        let install = status(&BootstrapState::InstallingCli { step: install::Step::Installing }, true);
        assert_eq!((install.label, install.fraction), (INSTALLING, 0.6));
        // The checks after an install are still the install's.
        let after = status(&BootstrapState::Connecting { cli: cli() }, true);
        assert_eq!((after.label, after.fraction), (INSTALLING, 0.9));
        let ready = status(&BootstrapState::Ready(crate::app::inert_ready()), true);
        assert_eq!((ready.label, ready.fraction, ready.done), (READY, 1.0, true));
    }

    /// A setup window over scripted bootstrap runs: each run's events are
    /// sent by the test; the plan and where Ready went are recorded.
    struct Rig {
        cx: VisualTestContext,
        view: gpui::Entity<CliSetupView>,
        runs: Rc<RefCell<Vec<async_channel::Sender<BootstrapEvent>>>>,
        went: Rc<RefCell<Vec<Next>>>,
    }

    fn rig(cx: &mut TestAppContext, plan: Vec<LaunchWindow>) -> Rig {
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::init(cx);
        });
        let runs: Rc<RefCell<Vec<async_channel::Sender<BootstrapEvent>>>> = Rc::default();
        let went: Rc<RefCell<Vec<Next>>> = Rc::default();
        let deps = Deps {
            start: Rc::new({
                let runs = runs.clone();
                move |_| {
                    let (tx, rx) = async_channel::unbounded();
                    runs.borrow_mut().push(tx);
                    rx
                }
            }),
            plan: Arc::new(move || plan.clone()),
            next: Rc::new({
                let went = went.clone();
                move |next, _, _| went.borrow_mut().push(next)
            }),
            dwell: Duration::ZERO,
        };
        let (view, cx) = cx.add_window_view(|window, cx| CliSetupView::new(deps, window, cx));
        Rig {
            cx: cx.clone(),
            view,
            runs,
            went,
        }
    }

    impl Rig {
        fn send(&mut self, state: BootstrapState) {
            let tx = self.runs.borrow().last().unwrap().clone();
            tx.send_blocking(BootstrapEvent::State(state)).unwrap();
            self.cx.run_until_parked();
        }

        fn shown(&mut self) -> Status {
            self.view.read_with(&self.cx, |v, _| status(v.state(), v.installing))
        }
    }

    #[gpui::test]
    fn a_found_cli_checks_then_opens_add_project(cx: &mut TestAppContext) {
        let mut r = rig(cx, Vec::new());
        r.send(BootstrapState::FindingCli);
        assert_eq!(r.shown().label, CHECKING);
        r.send(BootstrapState::StartingDaemon { cli: cli() });
        r.send(BootstrapState::Connecting { cli: cli() });
        assert_eq!(r.shown().label, CHECKING);
        assert!(r.went.borrow().is_empty(), "nothing before Ready");
        r.send(BootstrapState::Ready(crate::app::inert_ready()));
        assert!(r.shown().done);
        assert_eq!(*r.went.borrow(), [Next::AddProject]);
        assert_eq!(r.runs.borrow().len(), 1);
    }

    #[gpui::test]
    fn an_install_shows_installing_until_ready(cx: &mut TestAppContext) {
        let mut r = rig(cx, Vec::new());
        r.send(BootstrapState::FindingCli);
        r.send(BootstrapState::InstallingCli { step: install::Step::Downloading });
        assert_eq!(r.shown().label, INSTALLING);
        r.send(BootstrapState::InstallingCli { step: install::Step::Installing });
        r.send(BootstrapState::FindingCli);
        r.send(BootstrapState::StartingDaemon { cli: cli() });
        let s = r.shown();
        assert_eq!((s.label, s.done), (INSTALLING, false), "a download isn't readiness");
        assert!(r.went.borrow().is_empty());
        r.send(BootstrapState::Ready(crate::app::inert_ready()));
        assert_eq!(*r.went.borrow(), [Next::AddProject]);
    }

    #[gpui::test]
    fn a_failed_install_retries_once_per_try_and_late_events_are_dropped(cx: &mut TestAppContext) {
        let mut r = rig(cx, Vec::new());
        r.send(BootstrapState::InstallingCli { step: install::Step::Downloading });
        let failed = BootstrapState::Failed(Failure::InstallFailed(InstallError::new(
            InstallErrorKind::Network,
            "offline",
        )));
        // A retry before the failure does nothing (the run is still going).
        r.view.update(&mut r.cx, |v, cx| v.action(ScreenAction::Retry, cx));
        assert_eq!(r.runs.borrow().len(), 1);
        r.send(failed.clone());
        assert!(matches!(r.view.read_with(&r.cx, |v, _| v.state().clone()), BootstrapState::Failed(_)));
        // Return is Try Again; a second press lands on the new run, in flight.
        r.cx.simulate_keystrokes("enter");
        r.cx.simulate_keystrokes("enter");
        r.cx.run_until_parked();
        assert_eq!(r.runs.borrow().len(), 2, "one new job");
        assert_eq!(r.shown().label, CHECKING);
        // The first run's stragglers don't reach the second.
        let old = r.runs.borrow()[0].clone();
        old.send_blocking(BootstrapEvent::State(failed)).unwrap();
        r.cx.run_until_parked();
        assert!(!matches!(r.view.read_with(&r.cx, |v, _| v.state().clone()), BootstrapState::Failed(_)));
        r.send(BootstrapState::Ready(crate::app::inert_ready()));
        assert_eq!(*r.went.borrow(), [Next::AddProject]);
    }

    #[gpui::test]
    fn a_returning_user_restores_the_launch_windows(cx: &mut TestAppContext) {
        let plan = vec![LaunchWindow {
            slot: 0,
            project: "shelbi".into(),
            state: WindowState::default(),
        }];
        let mut r = rig(cx, plan.clone());
        r.send(BootstrapState::Ready(crate::app::inert_ready()));
        assert_eq!(*r.went.borrow(), [Next::Restore(plan)]);
    }

    #[gpui::test]
    fn ready_is_shared_with_the_project_windows(cx: &mut TestAppContext) {
        let mut r = rig(cx, Vec::new());
        r.cx.update(|_, cx| {
            cx.set_global(crate::app::Shell::new("client".into(), None, Default::default()))
        });
        r.send(BootstrapState::Ready(crate::app::inert_ready()));
        let shared = r.cx.update(|_, cx| crate::app::shared_ready(cx).is_some());
        assert!(shared);
    }
}
