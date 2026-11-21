//! Connect to an Issue tracker (`tracker-connect`, Figma "Issue tracker ·
//! Connection flow" 2753:11686): the guided flow the Issues header's ⋮ ▸
//! Connect to issue tracker…, File ▸ Connect to Issue Tracker… and the
//! palette open for the window's project. Its own window, fixed at
//! 800 × 580 with only the traffic lights over its content, like the Add
//! Project wizard; one per project (asking again brings it to the front).
//!
//! [`TrackerFlow`] (shared, toolkit-free) holds the steps and decides what
//! each button does; this view paints it (the steps through the agent rules
//! with [`super::tracker_steps`], which the Add Project wizard's Issues step
//! paints too), takes the keys, and runs what the
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
//!
//!   (every later step: no step counter)
//!                                                [Back] [Continue]
//! ```
//!
//! A card's Connect checks its tracker's authorization (the card says
//! Checking…): the GitHub CLI's sign-in, or a working Linear API key.
//! Authorized, the chooser shows the card Connected (a green badge, the
//! other card's Connect disabled; 02a-github-connected) for a beat
//! ([`copy::CONNECTED_BEAT_MS`]), then moves on to Choose project; Back from
//! there returns to it, with Continue in the footer. Not authorized, the
//! Authorize step asks first. Linear's is Add API key (02b-linear-api-key),
//! which Choose your Linear project's Change account opens too:
//!
//! ```text
//!                             Add API key
//!   Shelbi needs access to the Linear API. Add a personal API key below.
//!
//!   Linear account
//!   [ lin_api…                                        ] [Verify]
//!   Create one in Linear: Settings → Account → Security & access →
//!   Personal API keys.  Open Linear
//!                                                [Back] [Continue]
//! ```
//!
//! GitHub's (Sign in to GitHub) shows how to sign the GitHub CLI in (the
//! command, with Copy) and Retry. Continue waits for a verified key or a
//! signed-in `gh`.
//!
//! Choose your GitHub project / Choose your Linear project (03-github-choose,
//! 03a-linear-choose), the pickers 624pt wide in the 704pt column:
//!
//! ```text
//!   ✓ Account connected · Change account
//!   Organization                      Workspace
//!   [ octocat                    ⌄ ]  [ Example workspace          ⌄ ]
//!   Repository                        Team
//!   [ octocat/shelbi             ⌄ ]  [ Engineering · ENG          ⌄ ]
//!                                     Project
//!                                     [ Any project                ⌄ ]
//!   This will be the only issue tracker project connected to this Shelbi project.
//! ```
//!
//! GitHub's Change account shows how to sign `gh` in to another account in
//! the account line's place; Linear's opens Add API key. The pickers' reads
//! and Continue's preflight run through the project's [`TrackerService`]
//! (the far daemon's for a project on another machine).
//!
//! How should agents handle issues created in <Tracker>? (04-agent-rules):
//! the two agent rules as setting cards with switches (a click anywhere on a
//! card flips it).
//!
//! Review your first import (06-review-import), from one read of the chosen
//! connection where the project lives ([`TrackerService::preview`]):
//!
//! ```text
//!   Issues to import
//!   [ All open issues                                           ]
//!   ┌──────────────────────────────────────────────────────────┐
//!   │ 64 open issues will be imported                          │
//!   │ 48 assigned to you are eligible for agents.              │
//!   │ 16 others stay visible, but agents will not pick them up.│
//!   └──────────────────────────────────────────────────────────┘
//!   #128     Fix keyboard navigation
//!   #129     Restore the previous view
//!   12 closed issues are excluded. Imported tasks will follow …
//!                                          [Back] [Import 64 issues]
//! ```
//!
//! Importing your issues (07-importing): the connection saved, the status
//! mappings applied (Linear), then the import, its bar sliding until the
//! project's board has read the new tracker ([`ImportWatch`], handed to the
//! project window, which shows the board's toast when it lands).
//!
//! Keys: Return is the step's primary button (on Add API key, Verify until
//! the key verifies, then Continue), Esc closes an open picker, else the
//! window, as ⌘W does (a connection already running carries on in the
//! background).

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, AnyWindowHandle, App, ClickEvent, Context, Div, FocusHandle, Focusable, Global, Role,
    Stateful, Subscription, Window, WindowHandle,
};
use gpui_kit::component::input::InputEvent;
use shelbi_app::settings::tracker::TrackerService;
use shelbi_app::source::ProjectHandle;
use shelbi_app::tracker_connect::flow::{self as copy, Action, Apply, FlowRequest, Move, Preview, Secondary, Step, TrackerFlow};
use shelbi_app::tracker_connect::{import_copy, ImportWatch, StatusSync};
use shelbi_state::keymap::{Key, KeyChord, Mods};

use super::tracker_steps::{
    self as steps, error_line, key_step, labeled, quiet, rule_card, FlowHost, FlowUi, CONTENT_TOP, CONTENT_W,
};
use crate::app::{CloseWindow, ShelbiWindow};
use crate::assets::{heroicon, icons};
use crate::elements::progress_bar::progress_bar;
use crate::elements::spinner;
use crate::keys::chord_from_keystroke;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The window's size (the frames').
pub const SIZE: (f32, f32) = (800.0, 580.0);
/// The window's title (the menu bar's Window list).
pub const TITLE: &str = "Connect to Issue Tracker";
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
    /// The Add API key step's key field, and the open picker.
    ui: FlowUi,
    /// The step the view last showed (to focus the key field on reaching
    /// Add API key).
    shown: Step,
    /// The import's watch, observed until the project's board has it.
    import: Option<(gpui::Entity<ImportWatch>, Subscription)>,
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
        let ui = FlowUi::new(window, cx);
        let key_input = cx.subscribe_in(&ui.linear_ui.key_input, window, |this, input, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                let text = input.read(cx).value().to_string();
                this.flow.linear_mut().set_key_input(text);
                cx.notify();
            }
        });
        let subscriptions = vec![key_input, cx.intercept_keystrokes(move |event, window, cx| {
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
            ui,
            shown: Step::Chooser,
            import: None,
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

    /// A key the flow takes: Return is the primary button (on Add API key,
    /// Verify until the key verifies), Esc closes an open picker, else the
    /// window.
    fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if chord.mods != Mods::NONE {
            return false;
        }
        match chord.code {
            Key::Enter => self.primary(window, cx),
            Key::Esc if self.ui.close_menus() => cx.notify(),
            Key::Esc => self.close(window),
            _ => return false,
        }
        true
    }

    /// After the choice changed: the key field shows the choice's text (and
    /// takes the focus on reaching Add API key), and the reads it asks for
    /// run.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.flow.linear().key_input().to_string();
        self.ui.linear_ui.sync_key(&key, window, cx);
        if self.flow.step() != self.shown {
            self.shown = self.flow.step();
            self.ui.close_menus();
            if key_step(&self.flow) {
                let focus = self.ui.linear_ui.key_input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            } else {
                window.focus(&self.focus, cx);
            }
        }
        self.run_due(window, cx);
        cx.notify();
    }

    /// Flip the move card's switch.
    fn toggle_move(&mut self, cx: &mut Context<Self>) {
        let on = self.flow.move_on();
        self.flow.set_move(!on);
        cx.notify();
    }

    /// The review's Retry after a failed read.
    fn retry_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flow.retry_preview();
        self.run_due(window, cx);
        cx.notify();
    }

    /// The footer's primary button (and Return).
    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.flow.ready() {
            // Continue says why it can't go on (on Add API key, it verifies
            // the key).
            self.flow.advance();
            self.after_change(window, cx);
            return;
        }
        match self.flow.primary() {
            Action::ViewBoard => {
                self.land(true, None, cx);
                self.close(window);
                return;
            }
            Action::None => self.after_change(window, cx),
        }
        cx.notify();
    }

    /// The footer's left button.
    fn secondary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.close_menus();
        match self.flow.secondary() {
            Some(Secondary::Back) => {
                self.flow.back();
                self.after_change(window, cx);
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
                            this.after_change(window, cx);
                        })
                        .ok();
                    })
                    .detach();
                }
                FlowRequest::Github(req) => {
                    let github = self.service.github.clone();
                    let work = cx.background_executor().spawn(async move { github(req) });
                    cx.spawn_in(window, async move |this, cx| {
                        let answer = work.await;
                        this.update_in(cx, |this, window, cx| {
                            this.flow.github_answer(answer);
                            this.after_change(window, cx);
                        })
                        .ok();
                    })
                    .detach();
                }
                FlowRequest::Preview(choice) => {
                    let preview = self.service.preview.clone();
                    let asked = choice.clone();
                    let work = cx.background_executor().spawn(async move { preview(asked) });
                    cx.spawn_in(window, async move |this, cx| {
                        let result = work.await;
                        this.update_in(cx, |this, _, cx| {
                            this.flow.preview_answer(&choice, result);
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                FlowRequest::LocalIssues => {
                    let count = self.service.local_issues.clone();
                    let work = cx.background_executor().spawn(async move { count() });
                    cx.spawn_in(window, async move |this, cx| {
                        let result = work.await;
                        this.update_in(cx, |this, _, cx| {
                            this.flow.local_answer(result);
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                FlowRequest::Move(choice) => {
                    let move_issues = self.service.move_issues.clone();
                    let work = cx.background_executor().spawn(async move { move_issues(choice) });
                    cx.spawn_in(window, async move |this, cx| {
                        let result = work.await;
                        this.update_in(cx, |this, window, cx| {
                            // Moved: the switch is due next.
                            this.flow.moved(result);
                            this.after_change(window, cx);
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
                                this.start_import(cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            }
        }
        // The Connected chooser's beat, then Choose project.
        if let Some(token) = self.flow.beat_due() {
            let timer = cx.background_executor().timer(Duration::from_millis(copy::CONNECTED_BEAT_MS));
            cx.spawn_in(window, async move |this, cx| {
                timer.await;
                this.update_in(cx, |this, window, cx| {
                    this.flow.beat_done(token);
                    this.after_change(window, cx);
                })
                .ok();
            })
            .detach();
        }
    }

    /// The connection was written: watch for its import, and hand the
    /// watch to the project window, whose board reads the new tracker and
    /// says when the issues have landed (here, and in its toast).
    fn start_import(&mut self, cx: &mut Context<Self>) {
        let Some(done) = self.flow.finished() else { return };
        let watch = cx.new(|_| ImportWatch::new(done.backend, self.flow.source_name()));
        let observe = cx.observe(&watch, |this, watch, cx| {
            if let Some(n) = watch.read(cx).landed() {
                this.flow.imported(n);
                cx.notify();
            }
        });
        self.import = Some((watch.clone(), observe));
        self.land(false, Some(watch), cx);
    }

    /// The import's watch (tests stand in for the project's board).
    #[cfg(test)]
    pub fn import_watch(&self) -> Option<gpui::Entity<ImportWatch>> {
        self.import.as_ref().map(|(w, _)| w.clone())
    }

    /// Tell the project window: a connection was written (it reads its
    /// board again, watching for the import), and on View board, show the
    /// board.
    fn land(&self, view_board: bool, watch: Option<gpui::Entity<ImportWatch>>, cx: &mut App) {
        let Some(from) = self.from else { return };
        from.update(cx, |w, window, cx| w.tracker_connected(view_board, watch, window, cx)).ok();
    }
}

impl Focusable for TrackerConnectView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FlowHost for TrackerConnectView {
    fn flow(&self) -> &TrackerFlow {
        &self.flow
    }

    fn flow_mut(&mut self) -> &mut TrackerFlow {
        &mut self.flow
    }

    fn flow_ui(&self) -> &FlowUi {
        &self.ui
    }

    fn flow_ui_mut(&mut self) -> &mut FlowUi {
        &mut self.ui
    }

    fn flow_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.after_change(window, cx);
    }
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
    /// Review your first import: what to import, the counts, the first few
    /// issues and the closed ones left out (or the read's loading or error).
    /// The move card (the agent rules' setting-card style): move the local
    /// board's issues to the tracker first, on by default.
    fn move_card(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.flow.offers_move() {
            return None;
        }
        Some(
            rule_card("tracker-move", self.flow.move_title(), self.flow.move_help(), self.flow.move_on(), theme)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_move(cx))),
        )
    }

    fn review_step(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let field = div()
            .debug_selector(|| "tracker-import-scope".into())
            .flex()
            .items_center()
            .w_full()
            .h(px(32.0))
            .px(px(12.0))
            .rounded(px(6.0))
            .bg(theme.toolbar)
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(import_copy::ALL_OPEN);
        let col = div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .w_full()
            .child(labeled(import_copy::ISSUES_TO_IMPORT, field, theme));
        match self.flow.preview() {
            Preview::Loaded(p) => {
                let rules = self.flow.rules();
                let summary = div()
                    .debug_selector(|| "tracker-import-summary".into())
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .w_full()
                    .px(px(12.0))
                    .py(px(12.0))
                    .rounded(px(8.0))
                    .bg(theme.toolbar)
                    .child(
                        div()
                            .debug_selector(|| "tracker-import-count".into())
                            .type_style(Type::BaseMedium)
                            .text_color(theme.text_loud)
                            .child(import_copy::will_import(p.open)),
                    )
                    .child(
                        div()
                            .debug_selector(|| "tracker-import-eligible".into())
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .type_style(Type::Small)
                            .text_color(theme.muted)
                            .children(import_copy::eligibility(p, rules)),
                    );
                let rows = p.first.iter().enumerate().map(|(i, issue)| {
                    div()
                        .debug_selector(move || format!("tracker-import-issue-{i}"))
                        .flex()
                        .flex_row()
                        .gap(px(12.0))
                        .type_style(Type::Base)
                        .child(div().flex_none().w(px(64.0)).text_color(theme.muted).child(issue.key.clone()))
                        .child(div().flex_1().min_w_0().truncate().text_color(theme.foreground).child(issue.title.clone()))
                });
                // With the move card the first issues make room for it (the
                // frame's summary sits right over the card).
                let card = self.move_card(theme, cx);
                let rows = card.is_none().then(|| div().flex().flex_col().gap(px(10.0)).children(rows));
                col.child(summary)
                    .children(card)
                    .children(rows)
                    .child(quiet("tracker-import-closed", import_copy::closed_note(p), theme))
            }
            Preview::Failed(why) => col.children(self.move_card(theme, cx)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(error_line("tracker-import-error", format!("Couldn't read the issues to import: {why}"), theme))
                    .child(
                        div().flex().child(
                            style::button("tracker-import-retry", copy::RETRY, ButtonKind::Secondary, ButtonSize::Small, false, theme)
                                .role(Role::Button)
                                .aria_label(copy::RETRY)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.retry_preview(window, cx))),
                        ),
                    ),
            ),
            Preview::Idle | Preview::Wanted | Preview::Loading => col.children(self.move_card(theme, cx)).child(
                div()
                    .debug_selector(|| "tracker-import-loading".into())
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(spinner::spinner_sized("tracker-import-spinner", theme.muted, 14.0))
                    .child(div().type_style(Type::Small).text_color(theme.muted).child(import_copy::LOADING)),
            ),
        }
    }

    fn importing(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut lines = div().flex().flex_col().gap(px(16.0)).w_full();
        // The move first, when it's on: "Moving 3 local issues…", then
        // "Moved 3 local issues to Linear".
        if let Some((done, text)) = self.flow.move_line() {
            lines = lines.child(progress_line("tracker-progress-move", done, text, theme));
        }
        let moving = matches!(self.flow.moving(), Move::Wanted | Move::Running | Move::Failed(_));
        match self.flow.apply() {
            // The connection waits for the move.
            Apply::Idle if moving => {}
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
                // The import: the board's first read of the new tracker. It
                // can't count issue by issue, so the bar slides until then.
                let landed = self.flow.landed().is_some();
                let color = if landed { theme.green } else { theme.cyan };
                lines = lines.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(16.0))
                        .child(progress_line("tracker-progress-import", landed, self.flow.import_line(), theme))
                        .child(progress_bar(
                            "tracker-progress-bar",
                            landed.then_some(1.0),
                            CONTENT_W,
                            color,
                            theme.toolbar,
                        )),
                );
            }
            // The refusal shows as the step's error; the local board still
            // has issues, the move card offers moving them again.
            Apply::Refused(_) => lines = lines.children(self.move_card(theme, cx)),
        }
        // Closing leaves the import running, but not the move.
        let running = !matches!(self.flow.apply(), Apply::Refused(_)) && !moving;
        lines.when(running, |d| {
            d.child(div().type_style(Type::Small).text_color(theme.muted).child(copy::BACKGROUND_NOTE))
        })
    }

    fn content(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        if let Some(shared) = steps::content(self, theme, window, cx) {
            return shared;
        }
        match self.flow.step() {
            Step::ReviewImport => self.review_step(theme, cx),
            Step::Importing => self.importing(theme, cx),
            Step::Chooser | Step::Authorize | Step::ChooseProject | Step::AgentRules => div(),
        }
    }

    /// The left button and the primary one, right-aligned (every step after
    /// the chooser, and the Connected chooser after Back).
    fn footer(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        let label = gpui::SharedString::from(self.flow.primary_label()?);
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
            style::button("tracker-primary", label.clone(), ButtonKind::Primary, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.primary(window, cx)))
        } else {
            style::disabled_button("tracker-primary", label.clone(), ButtonKind::Primary, ButtonSize::Base, theme)
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
                .child(div().flex_1())
                .children(secondary)
                .child(primary),
        )
    }
}

impl Render for TrackerConnectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let error = self.load_error.clone().or_else(|| self.flow.error());
        let content = self.content(&theme, window, cx);
        let footer = self.footer(&theme, cx);
        // The frames hang each step's content a fixed way under the heading;
        // only Add API key's field and Importing's progress sit in the
        // middle.
        let centered = steps::centered(self.flow.step());
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
    use shelbi_app::new_project::Tracker;
    use shelbi_app::source::ProjectKey;
    use shelbi_app::tracker_connect::flow::CardState;
    use shelbi_app::tracker_connect::{ConnectTracker, Connected, TrackerChoice};
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
            github: Arc::new(shelbi_app::github_setup::fixture::answer),
            sync_statuses: Arc::new(|| StatusSync::NotNeeded),
            preview: Arc::new(shelbi_app::tracker_connect::fixture::answer),
            local_issues: Arc::new(|| Ok(0)),
            move_issues: Arc::new(shelbi_app::tracker_connect::fixture::moved),
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

        /// Run out the Connected chooser's beat.
        fn beat(&mut self) {
            self.cx.executor().advance_clock(Duration::from_millis(copy::CONNECTED_BEAT_MS + 100));
            self.cx.run_until_parked();
        }

        /// A card's Connect with its tracker authorized: the Connected
        /// chooser, then Choose project after the beat.
        fn connect(&mut self, card: &'static str) {
            self.click(card);
            assert_eq!(self.flow(|f| f.step()), Step::Chooser, "the Connected chooser first");
            assert!(self.flow(|f| f.authorized().is_some()));
            self.beat();
            assert_eq!(self.flow(|f| f.step()), Step::ChooseProject);
        }

        /// Pick the acme organization, then its acme/app, from the menus.
        fn pick_repo(&mut self) {
            self.click("tracker-github-owner");
            self.click("tracker-github-owner-1");
            assert_eq!(self.flow(|f| f.github().owner().to_string()), "acme");
            self.click("tracker-github-repo");
            self.click("tracker-github-repo-0");
            assert_eq!(self.flow(|f| f.repo().to_string()), "acme/app");
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
        assert!(!r.drawn("tracker-primary") && !r.drawn("tracker-back"), "the chooser has no footer");
        assert_eq!(r.flow(|f| f.heading()), copy::CHOOSER_HEADING);

        // Connect: gh is signed in, so the chooser shows GitHub Connected
        // and Linear disabled, then moves on by itself.
        r.click("tracker-connect-github");
        assert_eq!(r.flow(|f| f.step()), Step::Chooser);
        assert!(r.drawn("tracker-card-github-connected") && r.drawn("tracker-connect-linear"));
        assert!(!r.drawn("tracker-primary"), "no footer during the beat");
        r.click("tracker-connect-linear");
        assert_eq!(r.flow(|f| f.tracker()), Some(Tracker::Github), "Linear's Connect is disabled");
        r.beat();
        assert_eq!(r.flow(|f| f.step()), Step::ChooseProject);
        assert!(r.drawn("tracker-back") && r.drawn("tracker-primary"));
        assert!(r.drawn("tracker-github-account") && r.drawn("tracker-only-project"));
        // Continue waits for a repository, and says why.
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::ChooseProject);
        assert!(r.drawn("tracker-error"));
        r.pick_repo();
        assert!(!r.drawn("tracker-error"));
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::AgentRules);
        r.click("tracker-back");
        assert_eq!(r.flow(|f| f.step()), Step::ChooseProject);
        // Back again: the chooser, GitHub still Connected, with Continue.
        r.click("tracker-back");
        assert_eq!(r.flow(|f| f.step()), Step::Chooser);
        assert!(r.drawn("tracker-card-github-connected") && r.drawn("tracker-primary") && !r.drawn("tracker-back"));
        assert_eq!(r.flow(|f| f.cards()[1].state), CardState::Connect, "Linear can switch now");
        r.beat();
        assert_eq!(r.flow(|f| f.step()), Step::Chooser, "no beat after Back");
        r.click("tracker-primary");
        assert_eq!(r.flow(|f| (f.step(), f.repo().to_string())), (Step::ChooseProject, "acme/app".to_string()));
        r.keys("enter");
        r.keys("enter");
        assert_eq!(r.flow(|f| (f.step(), f.primary_label())), (Step::ReviewImport, Some("Import 64 issues".to_string())));
        // Import writes the connection, through the service, once.
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::Importing);
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            [ConnectTracker::new(TrackerChoice::Github { repo: "acme/app".into() }, false).with_rules(shelbi_app::tracker_connect::flow::DEFAULT_RULES)]
        );
        assert!(r.flow(|f| f.finished().is_some()));
        assert!(r.drawn("tracker-progress-connection") && r.drawn("tracker-close"));
        assert_eq!(r.flow(|f| f.primary_label()).as_deref(), Some(copy::VIEW_BOARD));
    }

    #[gpui::test]
    fn github_signed_out_shows_how_to_sign_in_and_retry_loads_the_pickers(cx: &mut TestAppContext) {
        let mut service = recording(Arc::default());
        service.github = Arc::new(shelbi_app::github_setup::fixture::signed_out);
        let mut r = local(cx, service, IssueTrackerConfig::default());
        r.click("tracker-connect-github");
        assert_eq!(r.flow(|f| (f.step(), f.heading())), (Step::Authorize, copy::GITHUB_AUTH_HEADING.to_string()));
        assert!(r.drawn("tracker-github-auth") && r.drawn("tracker-github-auth-command"));
        assert!(!r.drawn("tracker-github-owner"), "no pickers until gh is signed in");
        assert!(!r.drawn("tracker-error"));
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::Authorize, "Continue waits");

        // Signed in now: Retry reads the account again, then Continue shows
        // GitHub Connected and moves on.
        r.update(|v, _, _| v.service.github = Arc::new(shelbi_app::github_setup::fixture::answer));
        r.click("tracker-github-retry");
        assert!(!r.drawn("tracker-github-auth") && r.drawn("tracker-github-signed-in"));
        r.click("tracker-primary");
        assert!(r.drawn("tracker-card-github-connected"));
        r.beat();
        assert!(r.drawn("tracker-github-account") && r.drawn("tracker-github-owner"));
        assert_eq!(r.flow(|f| f.github().owner().to_string()), "octocat", "the login first");

        // Change account explains the same, and Cancel keeps the account.
        r.click("tracker-github-change-account");
        assert!(r.drawn("tracker-github-auth") && r.drawn("tracker-github-cancel-change"));
        r.click("tracker-github-cancel-change");
        assert!(r.drawn("tracker-github-owner"));
    }

    #[gpui::test]
    fn github_continue_runs_the_preflight_and_shows_a_failure(cx: &mut TestAppContext) {
        use shelbi_app::github_setup::{GithubAnswer, GithubRequest};
        let mut service = recording(Arc::default());
        service.github = Arc::new(|req| match req {
            GithubRequest::Preflight { repo } => GithubAnswer::Preflight { repo, result: Err("Issues are off".into()) },
            other => shelbi_app::github_setup::fixture::answer(other),
        });
        let mut r = local(cx, service, IssueTrackerConfig::default());
        r.connect("tracker-connect-github");
        r.pick_repo();
        r.keys("enter");
        assert_eq!(r.flow(|f| (f.step(), f.error())), (Step::ChooseProject, Some("Issues are off".to_string())));
        assert!(r.drawn("tracker-error"));
    }

    #[gpui::test]
    fn linear_without_a_key_asks_for_one_then_shows_workspace_team_and_project(cx: &mut TestAppContext) {
        use shelbi_app::linear_setup::{LinearAnswer, LinearFailure, LinearRequest};
        super::super::about::tests::OPENED.with(|o| o.borrow_mut().clear());
        let mut service = recording(Arc::default());
        service.linear = Arc::new(shelbi_app::linear_setup::fixture::no_key);
        let mut r = local(cx, service, IssueTrackerConfig::default());
        r.click("tracker-connect-linear");
        assert_eq!(r.flow(|f| (f.step(), f.heading())), (Step::Authorize, copy::API_KEY_HEADING.to_string()));
        assert_eq!(r.flow(|f| f.subheading()), copy::API_KEY_SUBHEADING);
        assert!(r.drawn("tracker-key-input") && r.drawn("tracker-key-verify") && r.drawn("tracker-key-help"));
        assert!(r.drawn("tracker-back") && r.drawn("tracker-primary"));
        assert!(!r.flow(|f| f.ready()), "Continue waits for a verified key");
        r.click("tracker-linear-keys-link");
        assert_eq!(super::super::about::tests::OPENED.with(|o| o.borrow().clone()), vec![copy::LINEAR_KEYS_URL.to_string()]);

        // A key Linear refuses: why, under the field.
        r.update(|v, window, cx| {
            v.service.linear = Arc::new(|req| match req {
                LinearRequest::Login(_) => LinearAnswer::Login(Err(LinearFailure::Rejected("Linear refused this key.".into()))),
                other => shelbi_app::linear_setup::fixture::no_key(other),
            });
            v.edit_flow(|f| f.linear_mut().set_key_input("lin_api_bad"), window, cx);
        });
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::Authorize);
        assert!(r.drawn("tracker-key-error"));
        assert_eq!(r.flow(|f| f.key_error()).as_deref(), Some("Linear refused this key."));

        // A good one verifies (Return), and the next Return is Continue: the
        // chooser shows Linear Connected, then Choose your Linear project.
        r.update(|v, window, cx| {
            v.service.linear = Arc::new(shelbi_app::linear_setup::fixture::answer);
            v.edit_flow(|f| f.linear_mut().set_key_input("lin_api_x"), window, cx);
        });
        r.click("tracker-key-verify");
        assert!(!r.drawn("tracker-key-error") && r.drawn("tracker-key-verified"));
        assert_eq!(r.flow(|f| f.step()), Step::Authorize, "Continue is the user's");
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::Chooser);
        assert!(r.drawn("tracker-card-linear-connected"));
        r.beat();
        assert_eq!(r.flow(|f| f.heading()), "Choose your Linear project");
        assert!(r.drawn("tracker-linear-account") && r.drawn("tracker-connect-linear-workspace"));
        assert!(!r.drawn("tracker-key-input"), "no key field on Choose project");
        assert_eq!(
            r.flow(|f| f.linear().workspace().loaded().cloned()),
            Some(shelbi_app::linear_setup::fixture::WORKSPACE.to_string())
        );

        // Team is required; Project defaults to Any project.
        r.keys("enter");
        assert_eq!(r.flow(|f| (f.step(), f.error())), (Step::ChooseProject, Some(shelbi_app::linear_setup::TEAM_REQUIRED.into())));
        r.click("tracker-connect-linear-team");
        assert!(r.drawn("tracker-connect-linear-team-menu"));
        r.click("tracker-connect-linear-team-1");
        assert_eq!(r.flow(|f| f.linear().team().to_string()), "DES");
        assert!(r.drawn("tracker-connect-linear-project"));
        assert_eq!(r.flow(|f| f.linear().filter(shelbi_app::linear_setup::Filter::Project).map(str::to_string)), None);
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::AgentRules, "the preflight passed");

        // Back keeps the choice; Change account opens Add API key, and its
        // Back returns to Choose project.
        r.click("tracker-back");
        assert_eq!(r.flow(|f| f.linear().team().to_string()), "DES");
        r.click("tracker-linear-change-account");
        assert_eq!(r.flow(|f| f.step()), Step::Authorize);
        assert!(r.drawn("tracker-key-input"));
        r.click("tracker-back");
        assert_eq!(r.flow(|f| (f.step(), f.linear().team().to_string())), (Step::ChooseProject, "DES".to_string()));
    }

    #[gpui::test]
    fn linear_with_a_working_key_skips_add_api_key(cx: &mut TestAppContext) {
        let mut r = local(cx, recording(Arc::default()), IssueTrackerConfig::default());
        r.click("tracker-connect-linear");
        assert_eq!(r.flow(|f| (f.step(), f.authorized())), (Step::Chooser, Some(Tracker::Linear)));
        assert!(r.drawn("tracker-card-linear-connected") && r.drawn("tracker-connect-github"));
        r.beat();
        assert_eq!(r.flow(|f| f.step()), Step::ChooseProject);
        assert!(r.drawn("tracker-linear-account"));
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
    fn a_failed_preview_shows_why_and_retry_reads_it_again(cx: &mut TestAppContext) {
        let reads = Arc::new(Mutex::new(0));
        let mut service = recording(Arc::default());
        let counter = reads.clone();
        service.preview = Arc::new(move |choice| {
            let mut n = counter.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Err("gh: API rate limit exceeded".into())
            } else {
                shelbi_app::tracker_connect::fixture::answer(choice)
            }
        });
        let mut r = local(cx, service, IssueTrackerConfig::default());
        r.connect("tracker-connect-github");
        r.pick_repo();
        r.keys("enter");
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::ReviewImport);
        assert!(r.drawn("tracker-import-error") && r.drawn("tracker-import-retry"));
        assert!(!r.drawn("tracker-import-summary"));
        assert_eq!(r.flow(|f| f.primary_label()).as_deref(), Some(copy::IMPORT), "no count to put in the button");
        r.click("tracker-import-retry");
        assert_eq!(*reads.lock().unwrap(), 2);
        assert!(!r.drawn("tracker-import-error") && r.drawn("tracker-import-summary"));
        assert_eq!(r.flow(|f| f.primary_label()).as_deref(), Some("Import 64 issues"));
    }

    /// On to Review your first import with GitHub's acme/app.
    fn to_review(r: &mut Rig) {
        r.connect("tracker-connect-github");
        r.pick_repo();
        r.keys("enter");
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::ReviewImport);
    }

    #[gpui::test]
    fn local_issues_show_the_move_card_and_import_moves_them_first(cx: &mut TestAppContext) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let moves = Arc::new(Mutex::new(Vec::new()));
        let mut service = recording(asked.clone());
        service.local_issues = Arc::new(|| Ok(3));
        let record = moves.clone();
        service.move_issues = Arc::new(move |choice| {
            record.lock().unwrap().push(choice);
            Ok(shelbi_app::tracker_connect::Moved { moved: Vec::new(), already: 3, comments: 0 })
        });
        let mut r = local(cx, service, IssueTrackerConfig::default());
        to_review(&mut r);
        assert!(r.drawn("tracker-move-on") && r.drawn("tracker-import-summary"));
        assert!(!r.drawn("tracker-import-issue-0"), "the card takes the first issues' room");
        assert_eq!(r.flow(|f| f.subheading()), "Import from acme/app.");
        r.keys("enter");
        assert_eq!(*moves.lock().unwrap(), vec![TrackerChoice::Github { repo: "acme/app".into() }]);
        let reqs = asked.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        assert!(!reqs[0].force, "after the move nothing is left behind");
        assert!(r.drawn("tracker-progress-move") && r.drawn("tracker-progress-connection"));
        assert!(!r.drawn("tracker-error"));
    }

    #[gpui::test]
    fn the_move_off_forces_the_switch_and_no_card_without_local_issues(cx: &mut TestAppContext) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut service = recording(asked.clone());
        service.local_issues = Arc::new(|| Ok(1));
        service.move_issues = Arc::new(|_| panic!("the move is off"));
        let mut r = local(cx, service, IssueTrackerConfig::default());
        to_review(&mut r);
        r.click("tracker-move");
        assert!(r.drawn("tracker-move-off"));
        assert!(!r.flow(|f| f.move_on()));
        r.keys("enter");
        assert!(asked.lock().unwrap()[0].force, "a forced switch, no refusal round-trip");
        assert!(!r.drawn("tracker-progress-move") && !r.drawn("tracker-error"));

        // An empty local board: no card, the flow as before.
        let mut r = local(cx, recording(Arc::default()), IssueTrackerConfig::default());
        to_review(&mut r);
        assert!(!r.drawn("tracker-move"));
        assert!(r.drawn("tracker-import-issue-0"));
    }

    #[gpui::test]
    fn a_move_that_stops_shows_why_with_try_again_and_back(cx: &mut TestAppContext) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut service = recording(asked.clone());
        service.local_issues = Arc::new(|| Ok(2));
        let tries = Arc::new(Mutex::new(0));
        let counter = tries.clone();
        service.move_issues = Arc::new(move |_| {
            let mut n = counter.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Err(shelbi_app::tracker_connect::MoveFailed {
                    moved: vec![shelbi_app::tracker_connect::MovedIssue { id: "a".into(), title: "Fix login".into() }],
                    already: 0,
                    left: vec![shelbi_app::tracker_connect::MovedIssue { id: "b".into(), title: "Add tests".into() }],
                    reason: "GitHub refused the write".into(),
                })
            } else {
                Ok(shelbi_app::tracker_connect::Moved { moved: Vec::new(), already: 2, comments: 0 })
            }
        });
        let mut r = local(cx, service, IssueTrackerConfig::default());
        to_review(&mut r);
        r.keys("enter");
        assert!(r.drawn("tracker-error") && r.drawn("tracker-back"));
        let error = r.flow(|f| f.error()).unwrap();
        assert!(error.starts_with("Moved 1 of 2 local issues to GitHub, then it stopped."), "{error}");
        assert!(!error.contains("shelbi issue-store"), "{error}");
        assert_eq!(r.flow(|f| f.primary_label()).as_deref(), Some(copy::TRY_AGAIN));
        assert!(asked.lock().unwrap().is_empty(), "nothing was switched");
        r.keys("enter");
        assert_eq!(*tries.lock().unwrap(), 2);
        assert_eq!(asked.lock().unwrap().len(), 1, "moved, then switched");
        assert!(!r.drawn("tracker-error"));
    }

    #[gpui::test]
    fn esc_closes_the_window(cx: &mut TestAppContext) {
        let mut r = local(cx, recording(Arc::default()), IssueTrackerConfig::default());
        let windows = r.cx.cx.update(|cx| cx.windows().len());
        r.keys("escape");
        assert_eq!(r.cx.cx.update(|cx| cx.windows().len()), windows - 1);
    }

    /// A far project (on another machine) reads its pickers, its preview
    /// and its local board's count over the host's link, and Import moves
    /// the local issues and connects on the far daemon
    /// (`far::Request::MoveIssues`, then `far::Request::ConnectTracker`).
    /// (The far fixture refuses a switch that wasn't moved first.)
    #[gpui::test]
    fn a_far_project_reads_moves_and_connects_through_its_daemon(cx: &mut TestAppContext) {
        let far = crate::remote_fixture::FarHost::new("tracker-flow");
        let mut remote = crate::app::RemoteWindow::new(far.session(), far.listing(), far.link.clone(), far.remote.clone());
        let handle = remote.handle_for_test(crate::remote_fixture::PROJECT);
        assert!(handle.link.is_remote());
        let service = super::super::properties::tracker_service(&handle);
        let mut r = rig(cx, handle, service);
        assert!(r.flow(|f| f.loaded()), "the far project's connection was read");
        assert!(r.drawn("tracker-connect-github"), "the far project is on its local board");

        r.connect("tracker-connect-github");
        r.pick_repo();
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::AgentRules, "the far daemon's preflight passed");
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::ReviewImport);
        // The far board's two issues: the move card, on.
        assert!(r.drawn("tracker-move-on"), "the move card, on by default");
        assert_eq!(r.flow(|f| f.move_title()), "Move your 2 local issues to GitHub");
        r.keys("enter");
        let asked: Vec<_> = far.asked.lock().unwrap().requests.clone();
        use shelbi_app::github_setup::GithubRequest;
        for want in [
            GithubRequest::Account,
            GithubRequest::Repos { owner: "acme".into() },
            GithubRequest::Preflight { repo: "acme/app".into() },
        ] {
            assert!(asked.contains(&shelbi_app::far::Request::Github(want.clone())), "{want:?} went to the far daemon: {asked:?}");
        }
        let choice = TrackerChoice::Github { repo: "acme/app".into() };
        for want in [
            shelbi_app::far::Request::ImportPreview(choice.clone()),
            shelbi_app::far::Request::LocalIssues,
            shelbi_app::far::Request::MoveIssues(choice.clone()),
        ] {
            assert!(asked.contains(&want), "{want:?} went to the far daemon: {asked:?}");
        }
        let connect = asked.iter().position(|q| matches!(q, shelbi_app::far::Request::ConnectTracker(c)
            if c.choice == choice && !c.force));
        let moved = asked.iter().position(|q| matches!(q, shelbi_app::far::Request::MoveIssues(_)));
        assert!(moved < connect && connect.is_some(), "moved, then an unforced switch: {asked:?}");
        assert!(r.flow(|f| f.finished().is_some()), "{:?}", r.flow(|f| (f.apply().clone(), f.moving().clone())));
        assert!(r.drawn("tracker-progress-move"));
        assert_eq!(r.flow(|f| f.move_line()), Some((true, "Moved 2 local issues to GitHub".to_string())));
        assert!(!r.drawn("tracker-error"));
    }
}
