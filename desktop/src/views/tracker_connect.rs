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
//! Without a signed-in `gh` the GitHub step shows how to sign in (the
//! command, with Copy) and Retry in the account line's place; without a
//! working Linear key the Linear step shows the key field and a link to
//! Linear's API key settings. Change account opens the same. The pickers'
//! reads and Continue's preflight run through the project's
//! [`TrackerService`] (the far daemon's for a project on another machine).
//!
//! Keys: Return is the step's primary button (Verify while the Linear key
//! field shows), Esc closes an open picker, else the window, as ⌘W does (a
//! connection already running carries on in the background). The Agent
//! rules and Review import steps draw placeholders until `tc-import` builds
//! them.

use std::collections::HashMap;

use gpui::{
    div, prelude::*, px, AnyElement, AnyWindowHandle, App, ClickEvent, ClipboardItem, Context, Div, FocusHandle,
    Focusable, Global, Role, Stateful, Subscription, Window, WindowHandle,
};
use gpui_kit::component::input::InputEvent;
use shelbi_app::github_setup::{self as gh, GithubAuth};
use shelbi_app::linear_setup::{KeyState, Loadable, LOADING};
use shelbi_app::new_project::Tracker;
use shelbi_app::settings::tracker::TrackerService;
use shelbi_app::source::ProjectHandle;
use shelbi_app::tracker_connect::flow::{
    self as copy, Action, Apply, Card, CardState, FlowRequest, Secondary, Step, TrackerFlow,
};
use shelbi_app::tracker_connect::StatusSync;
use shelbi_state::keymap::{Key, KeyChord, Mods};

use super::harness_picker::picker_button;
use super::linear_fields::{self, LinearUi, Pick};
use super::workflow_editor::dropdown;
use crate::app::{CloseWindow, ShelbiWindow};
use crate::assets::{heroicon, icons};
use crate::elements::terminal::metrics::PRIMARY_FAMILY as MONO_FAMILY;
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
/// The Choose project step's pickers' width (the frames' 624pt, centered in
/// the 704pt column).
const FIELDS_W: f32 = 624.0;

/// A GitHub picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GithubPick {
    Owner,
    Repo,
}

impl GithubPick {
    fn id(self) -> &'static str {
        match self {
            GithubPick::Owner => "tracker-github-owner",
            GithubPick::Repo => "tracker-github-repo",
        }
    }

    fn menu_id(self) -> &'static str {
        match self {
            GithubPick::Owner => "tracker-github-owner-menu",
            GithubPick::Repo => "tracker-github-repo-menu",
        }
    }

    fn label(self) -> &'static str {
        match self {
            GithubPick::Owner => gh::OWNER_LABEL,
            GithubPick::Repo => gh::REPO_LABEL,
        }
    }
}

/// What a click in the GitHub step asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GithubMsg {
    Owner(String),
    Repo(String),
    ChangeAccount,
    CancelChangeAccount,
    Retry,
}

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
    /// The Linear step's key field and open picker.
    linear_ui: LinearUi,
    /// The GitHub step's open picker, and where the open one hangs.
    github_menu: Option<GithubPick>,
    github_anchor: style::Anchor,
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
        let linear_ui = LinearUi::new(window, cx);
        let key_input = cx.subscribe_in(&linear_ui.key_input, window, |this, input, event: &InputEvent, _, cx| {
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
            linear_ui,
            github_menu: None,
            github_anchor: Default::default(),
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

    /// A key the flow takes: Return is the primary button (Verify while the
    /// Linear key field shows), Esc closes an open picker, else the window.
    fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if chord.mods != Mods::NONE {
            return false;
        }
        match chord.code {
            Key::Enter if self.key_field_shows() => self.linear_msg(linear_fields::Msg::Verify, window, cx),
            Key::Enter => self.primary(window, cx),
            Key::Esc if self.linear_ui.menu.is_some() || self.github_menu.is_some() => {
                self.linear_ui.menu = None;
                self.github_menu = None;
                cx.notify();
            }
            Key::Esc => self.close(window),
            _ => return false,
        }
        true
    }

    /// The Linear step's key field shows (Return verifies the key).
    fn key_field_shows(&self) -> bool {
        self.flow.step() == Step::ChooseProject
            && self.flow.tracker() == Some(Tracker::Linear)
            && self.flow.linear().needs_key()
    }

    /// After the choice changed: the key field shows the choice's text, and
    /// the reads it asks for run.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.flow.linear().key_input().to_string();
        self.linear_ui.sync_key(&key, window, cx);
        self.run_due(window, cx);
        cx.notify();
    }

    /// A click in the Linear step's fields.
    fn linear_msg(&mut self, msg: linear_fields::Msg, window: &mut Window, cx: &mut Context<Self>) {
        use linear_fields::Msg;
        let linear = self.flow.linear_mut();
        match msg {
            Msg::Verify => {
                linear.submit_key();
            }
            Msg::ChangeKey => {
                linear.change_key();
                let focus = self.linear_ui.key_input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            Msg::CancelChangeKey => linear.cancel_change_key(),
            Msg::Retry => linear.retry(),
            Msg::Team(team) => linear.select_team(&team),
            Msg::Filter(filter, value) => linear.select_filter(filter, Some(&value)),
        }
        self.after_change(window, cx);
    }

    fn linear_menu(&mut self, pick: Option<Pick>, cx: &mut Context<Self>) {
        match pick {
            Some(p) => self.linear_ui.toggle(p),
            None => self.linear_ui.menu = None,
        }
        cx.notify();
    }

    /// A click in the GitHub step.
    fn github_msg(&mut self, msg: GithubMsg, window: &mut Window, cx: &mut Context<Self>) {
        self.github_menu = None;
        let github = self.flow.github_mut();
        match msg {
            GithubMsg::Owner(owner) => github.select_owner(&owner),
            GithubMsg::Repo(repo) => github.select_repo(&repo),
            GithubMsg::ChangeAccount => github.change_account(),
            GithubMsg::CancelChangeAccount => github.cancel_change_account(),
            GithubMsg::Retry => github.retry(),
        }
        self.after_change(window, cx);
    }

    /// A card's Connect.
    fn connect(&mut self, tracker: Tracker, window: &mut Window, cx: &mut Context<Self>) {
        if self.flow.connect(tracker) {
            self.after_change(window, cx);
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
        self.linear_ui.menu = None;
        self.github_menu = None;
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

/// The account line (`✓ Account connected · Change account`, in the accent).
fn account_line(id: &'static str, theme: &Theme, cx: &mut Context<TrackerConnectView>, on_change: fn(&mut TrackerConnectView, &mut Window, &mut Context<TrackerConnectView>)) -> Div {
    div()
        .debug_selector(move || format!("{id}-account"))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .type_style(Type::Small)
        .text_color(theme.link)
        .child(format!("✓ {} ·", copy::ACCOUNT_CONNECTED))
        .child(
            super::about::link(format!("{id}-change-account"), copy::CHANGE_ACCOUNT, theme.link, theme)
                .role(Role::Button)
                .aria_label(copy::CHANGE_ACCOUNT)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| on_change(this, window, cx))),
        )
}

/// A quiet line (an account being checked).
fn quiet(selector: &'static str, text: impl Into<gpui::SharedString>, theme: &Theme) -> Div {
    div().debug_selector(move || selector.into()).type_style(Type::Small).text_color(theme.muted).child(text.into())
}

fn error_line(selector: &'static str, text: impl Into<gpui::SharedString>, theme: &Theme) -> Div {
    div().debug_selector(move || selector.into()).type_style(Type::Small).text_color(theme.error).child(text.into())
}

/// A field: its label over its control, 8pt apart.
fn labeled(label: &'static str, control: impl IntoElement, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w_full()
        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(label))
        .child(control)
}

impl TrackerConnectView {
    /// The Choose your GitHub project step: the account line (or how to
    /// sign `gh` in), then Organization and Repository.
    fn github_step(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let choice = self.flow.github();
        let mut col = div().flex().flex_col().gap(px(16.0)).w(px(FIELDS_W));
        if choice.needs_auth() {
            return col.child(self.github_auth(theme, cx));
        }
        col = col.child(match (choice.auth(), choice.account()) {
            (GithubAuth::Valid, Loadable::Loaded(_)) => account_line("tracker-github", theme, cx, |this, window, cx| {
                this.github_msg(GithubMsg::ChangeAccount, window, cx)
            }),
            (_, Loadable::Failed(why)) => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(error_line("tracker-github-account-error", format!("Couldn't read the GitHub account: {why}"), theme))
                .child(self.github_retry(theme, cx)),
            _ => quiet("tracker-github-checking", "Checking the GitHub account…", theme),
        });

        let owner_label = match choice.owners() {
            Some(_) => choice.owner().to_string(),
            None => LOADING.to_string(),
        };
        let owner = self.github_picker(GithubPick::Owner, choice.owners(), choice.owner(), owner_label, theme, cx);
        let repos = choice.repos();
        let repo_label = match (&repos, choice.repo()) {
            (_, repo) if !repo.is_empty() => repo.to_string(),
            (Loadable::Loaded(rows), _) if rows.is_empty() => gh::NO_REPOS.to_string(),
            (Loadable::Loaded(_), _) => gh::CHOOSE_REPO.to_string(),
            (Loadable::Failed(_), _) => "Couldn't load".to_string(),
            _ => LOADING.to_string(),
        };
        let repo = self.github_picker(GithubPick::Repo, repos.loaded().cloned(), choice.repo(), repo_label, theme, cx);
        col.child(labeled(gh::OWNER_LABEL, owner, theme))
            .child(labeled(gh::REPO_LABEL, repo, theme).children(repos.error().map(|e| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(error_line("tracker-github-repo-error", e.to_string(), theme))
                    .child(self.github_retry(theme, cx))
            })))
    }

    fn github_retry(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        style::button("tracker-github-retry", copy::RETRY, ButtonKind::Secondary, ButtonSize::Small, false, theme)
            .role(Role::Button)
            .aria_label(copy::RETRY)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.github_msg(GithubMsg::Retry, window, cx)))
    }

    /// How to sign the project's machine's `gh` in (or in to another
    /// account): why, the command with Copy, and Retry.
    fn github_auth(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let choice = self.flow.github();
        let message = choice.auth_message().unwrap_or(gh::NOT_SIGNED_IN);
        let command = div()
            .debug_selector(|| "tracker-github-auth-command".into())
            .flex()
            .flex_row()
            .items_center()
            .h(px(36.0))
            .gap(px(8.0))
            .pl(px(12.0))
            .pr(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.divider)
            .bg(theme.code_bg)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(MONO_FAMILY)
                    .text_size(px(14.0))
                    .line_height(px(18.0))
                    .text_color(theme.code_fg)
                    .child(gh::AUTH_COMMAND),
            )
            .child(
                style::button("tracker-github-copy", "Copy", ButtonKind::Secondary, ButtonSize::Small, false, theme)
                    .role(Role::Button)
                    .aria_label("Copy the command")
                    .on_click(|_: &ClickEvent, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(gh::AUTH_COMMAND.into()))),
            );
        let cancel = choice.changing_account().then(|| {
            style::button("tracker-github-cancel-change", "Cancel", ButtonKind::Secondary, ButtonSize::Small, false, theme)
                .role(Role::Button)
                .aria_label("Cancel")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.github_msg(GithubMsg::CancelChangeAccount, window, cx)
                }))
        });
        div()
            .debug_selector(|| "tracker-github-auth".into())
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(div().type_style(Type::Base).text_color(theme.foreground).child(message))
            .child(command)
            .child(div().flex().flex_row().gap(px(8.0)).child(self.github_retry(theme, cx)).children(cancel))
    }

    /// A GitHub picker: its button, and `rows` in a menu under it while
    /// open. Greyed while there are no rows to pick.
    fn github_picker(
        &self,
        pick: GithubPick,
        rows: Option<Vec<String>>,
        selected: &str,
        label: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = pick.id();
        let button = picker_button(id, label, None, false, theme).aria_label(pick.label());
        let Some(rows) = rows.filter(|r| !r.is_empty()) else {
            return div().w_full().child(button.opacity(0.4).cursor_default()).into_any_element();
        };
        let selected = selected.to_string();
        dropdown(
            self.github_menu == Some(pick),
            &self.github_anchor,
            pick.menu_id(),
            button,
            move |cx| {
                let theme = *cx.global::<Theme>();
                rows.into_iter()
                    .enumerate()
                    .map(|(i, row)| {
                        let on_row = row == selected;
                        let value = row.clone();
                        style::menu_row(format!("{id}-{i}"), on_row, &theme)
                            .child(div().flex_1().min_w_0().truncate().child(row))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                let msg = match pick {
                                    GithubPick::Owner => GithubMsg::Owner(value.clone()),
                                    GithubPick::Repo => GithubMsg::Repo(value.clone()),
                                };
                                this.github_msg(msg, window, cx);
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            FIELDS_W,
            false,
            move |this: &mut Self, cx| {
                this.github_menu = if this.github_menu == Some(pick) { None } else { Some(pick) };
                cx.notify();
            },
            |this: &mut Self, cx| {
                this.github_menu = None;
                cx.notify();
            },
            cx,
        )
        .w_full()
        .into_any_element()
    }

    /// The Choose your Linear project step: the account line (or the key
    /// field, with a link to Linear's key settings), then Workspace, Team
    /// and Project.
    fn linear_step(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        let choice = self.flow.linear();
        let col = div().flex().flex_col().gap(px(16.0)).w(px(FIELDS_W));
        let on = linear_fields::Handlers {
            msg: |this: &mut Self, msg, window, cx| this.linear_msg(msg, window, cx),
            menu: |this: &mut Self, pick, cx| this.linear_menu(pick, cx),
        };
        let enabled = !self.flow.checking();
        if choice.needs_key() {
            let link = super::about::link("tracker-linear-keys-link", copy::LINEAR_KEYS_LINK, theme.link, theme)
                .type_style(Type::Small)
                .role(Role::Link)
                .aria_label(copy::LINEAR_KEYS_LINK)
                .on_click(|_: &ClickEvent, _, cx| super::about::open_url(copy::LINEAR_KEYS_URL, cx));
            return col
                .child(linear_fields::key_entry(linear_fields::Host::Connect, choice, &self.linear_ui, true, on, theme, window, cx))
                .child(div().flex().child(link));
        }
        let account = match choice.key_state() {
            KeyState::Valid { .. } => account_line("tracker-linear", theme, cx, |this, window, cx| {
                this.linear_msg(linear_fields::Msg::ChangeKey, window, cx)
            }),
            _ => quiet("tracker-linear-checking", "Checking the Linear API key…", theme),
        };
        col.child(account)
            .child(linear_fields::connect_fields(choice, &self.linear_ui, enabled, on, FIELDS_W, theme, cx))
    }

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

    fn content(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        let tracker = self.flow.tracker();
        match self.flow.step() {
            Step::Chooser => self.chooser(theme, cx),
            Step::ChooseProject => {
                let body = match tracker {
                    Some(Tracker::Linear) => self.linear_step(theme, window, cx),
                    _ => self.github_step(theme, cx),
                };
                // The frames hang the account line 78pt under the subheading,
                // 16pt above where the chooser's cards start.
                div().flex().flex_col().items_center().w_full().mt(px(-16.0)).child(
                    body.child(
                        div()
                            .debug_selector(|| "tracker-only-project".into())
                            .type_style(Type::Small)
                            .text_color(theme.muted)
                            .child(copy::ONLY_PROJECT),
                    ),
                )
            }
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let error = self.load_error.clone().or_else(|| self.flow.error());
        let content = self.content(&theme, window, cx);
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
            github: Arc::new(shelbi_app::github_setup::fixture::answer),
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
        assert!(r.drawn("tracker-one-at-a-time"));
        assert!(!r.drawn("tracker-step") && !r.drawn("tracker-primary"), "the chooser has no footer");
        assert_eq!(r.flow(|f| f.heading()), copy::CHOOSER_HEADING);

        r.click("tracker-connect-github");
        assert_eq!(r.flow(|f| (f.step(), f.counter())), (Step::ChooseProject, Some("Step 1 of 4".to_string())));
        assert!(r.drawn("tracker-step") && r.drawn("tracker-back") && r.drawn("tracker-primary"));
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
    fn github_signed_out_shows_how_to_sign_in_and_retry_loads_the_pickers(cx: &mut TestAppContext) {
        let mut service = recording(Arc::default());
        service.github = Arc::new(shelbi_app::github_setup::fixture::signed_out);
        let mut r = local(cx, service, IssueTrackerConfig::default());
        r.click("tracker-connect-github");
        assert!(r.drawn("tracker-github-auth") && r.drawn("tracker-github-auth-command"));
        assert!(!r.drawn("tracker-github-owner"), "no pickers until gh is signed in");
        assert!(!r.drawn("tracker-error"));
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::ChooseProject, "Continue waits");

        // Signed in now: Retry reads the account again.
        r.update(|v, _, _| v.service.github = Arc::new(shelbi_app::github_setup::fixture::answer));
        r.click("tracker-github-retry");
        assert!(!r.drawn("tracker-github-auth"));
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
        r.click("tracker-connect-github");
        r.pick_repo();
        r.keys("enter");
        assert_eq!(r.flow(|f| (f.step(), f.error())), (Step::ChooseProject, Some("Issues are off".to_string())));
        assert!(r.drawn("tracker-error"));
    }

    #[gpui::test]
    fn linear_without_a_key_shows_key_entry_then_workspace_team_and_project(cx: &mut TestAppContext) {
        super::super::about::tests::OPENED.with(|o| o.borrow_mut().clear());
        let mut service = recording(Arc::default());
        service.linear = Arc::new(shelbi_app::linear_setup::fixture::no_key);
        let mut r = local(cx, service, IssueTrackerConfig::default());
        r.click("tracker-connect-linear");
        assert_eq!(r.flow(|f| f.heading()), "Choose your Linear project");
        assert!(r.drawn("tracker-connect-linear-key") && r.drawn("tracker-connect-linear-verify"));
        assert!(!r.drawn("tracker-connect-linear-team"), "no pickers without a key");
        r.click("tracker-linear-keys-link");
        assert_eq!(super::super::about::tests::OPENED.with(|o| o.borrow().clone()), vec![copy::LINEAR_KEYS_URL.to_string()]);

        // A pasted key verifies, and Linear answers with it from then on.
        r.update(|v, window, cx| {
            v.service.linear = Arc::new(shelbi_app::linear_setup::fixture::answer);
            v.edit_flow(|f| f.linear_mut().set_key_input("lin_api_x"), window, cx);
        });
        r.click("tracker-connect-linear-verify");
        assert!(r.drawn("tracker-linear-account") && r.drawn("tracker-connect-linear-workspace"));
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

        // Back keeps the choice; Change account goes back to key entry.
        r.click("tracker-back");
        assert_eq!(r.flow(|f| f.linear().team().to_string()), "DES");
        r.click("tracker-linear-change-account");
        assert!(r.drawn("tracker-connect-linear-key") && r.drawn("tracker-connect-linear-cancel-key"));
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
        r.pick_repo();
        r.keys("enter");
        assert_eq!(r.flow(|f| f.step()), Step::AgentRules, "the far daemon's preflight passed");
        r.keys("enter");
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
