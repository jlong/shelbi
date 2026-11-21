//! The screens Connect to an Issue tracker (`views/tracker_connect.rs`) and
//! the Add Project wizard's Issues step (`views/add_project.rs`) share, over
//! one [`TrackerFlow`] each: the chooser (a row per tracker with its
//! Connect, and in the wizard File system's Use), Add API key or Sign in to
//! GitHub, Choose your GitHub / Linear project, and the agent rules. Figma
//! "Issue tracker · Connection flow" 2753:11686.
//!
//! A host view implements [`FlowHost`]: it holds the flow and its [`FlowUi`]
//! (the key field, the open picker), and [`FlowHost::flow_changed`] runs
//! whatever the flow asks for next (its reads, the Connected beat) and
//! repaints. The step bodies here draw from the flow and send clicks back
//! through the handlers below, so both windows show and do the same thing.

use gpui::{div, prelude::*, px, AnyElement, ClickEvent, ClipboardItem, Context, Div, Focusable, Role, Stateful, Window};
use shelbi_app::github_setup::{self as gh, GithubAuth};
use shelbi_app::linear_setup::{KeyState, Loadable, LOADING, VERIFY};
use shelbi_app::new_project::Tracker;
use shelbi_app::tracker_connect::flow::{self as copy, Card, CardState, Step, TrackerFlow};
use shelbi_app::tracker_connect::rules_copy;

use super::harness_picker::picker_button;
use super::linear_fields::{self, LinearUi, Pick};
use super::workflow_editor::dropdown;
use crate::assets::{heroicon, icons};
use crate::elements::controls;
use crate::elements::terminal::metrics::PRIMARY_FAMILY as MONO_FAMILY;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The cards' and the steps' width inside the 48pt inset.
pub(super) const CONTENT_W: f32 = 704.0;
/// The room between the subheading and a step's content (the frames' 76pt,
/// less the column's 24pt gap).
pub(super) const CONTENT_TOP: f32 = 52.0;
/// The Choose project step's pickers' width (the frames' 624pt, centered in
/// the 704pt column), and the Add API key step's field with its Verify.
pub(super) const FIELDS_W: f32 = 624.0;
/// How much lower the chooser's cards sit than the later steps' content
/// (01-select-tracker: the first card at 198pt, without the old note).
const CHOOSER_DROP: f32 = 17.0;
/// The Add API key step's help line's width (its frame's 420pt).
const KEY_HELP_W: f32 = 420.0;

/// A GitHub picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GithubPick {
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
pub(super) enum GithubMsg {
    Owner(String),
    Repo(String),
    ChangeAccount,
    CancelChangeAccount,
    Retry,
}

/// The host's half of the flow's controls: the Add API key field and the
/// Linear step's open picker, and the GitHub step's open picker.
pub struct FlowUi {
    pub linear_ui: LinearUi,
    pub github_menu: Option<GithubPick>,
    pub github_anchor: style::Anchor,
}

impl FlowUi {
    pub fn new<V: 'static>(window: &mut Window, cx: &mut Context<V>) -> Self {
        FlowUi {
            linear_ui: LinearUi::with_placeholder(copy::API_KEY_PLACEHOLDER, window, cx),
            github_menu: None,
            github_anchor: Default::default(),
        }
    }

    /// Close any open picker; `true` when one was open.
    pub fn close_menus(&mut self) -> bool {
        let open = self.linear_ui.menu.is_some() || self.github_menu.is_some();
        self.linear_ui.menu = None;
        self.github_menu = None;
        open
    }
}

/// A view that shows a [`TrackerFlow`].
pub trait FlowHost: Sized + 'static {
    fn flow(&self) -> &TrackerFlow;
    /// The flow, to change as a control does.
    fn flow_mut(&mut self) -> &mut TrackerFlow;
    fn flow_ui(&self) -> &FlowUi;
    fn flow_ui_mut(&mut self) -> &mut FlowUi;
    /// After the flow changed: sync the key field, run what it asks for,
    /// repaint.
    fn flow_changed(&mut self, window: &mut Window, cx: &mut Context<Self>);
    /// The chooser row whose button shows the keyboard's focus ring.
    fn ring(&self, _tracker: Tracker) -> bool {
        false
    }
}

/// The Add API key step shows.
pub(super) fn key_step(flow: &TrackerFlow) -> bool {
    flow.step() == Step::Authorize && flow.tracker() == Some(Tracker::Linear)
}

/// A click in the Linear step's fields, or the Add API key step's.
pub(super) fn linear_msg<V: FlowHost>(this: &mut V, msg: linear_fields::Msg, window: &mut Window, cx: &mut Context<V>) {
    use linear_fields::Msg;
    let flow = this.flow_mut();
    match msg {
        Msg::Verify => {
            flow.verify();
        }
        // Change account: the Add API key step.
        Msg::ChangeKey => flow.change_account(),
        Msg::CancelChangeKey => flow.linear_mut().cancel_change_key(),
        Msg::Retry => flow.linear_mut().retry(),
        Msg::Team(team) => flow.linear_mut().select_team(&team),
        Msg::Filter(filter, value) => flow.linear_mut().select_filter(filter, Some(&value)),
    }
    this.flow_changed(window, cx);
}

/// A Linear picker's button (`Some`) or a press outside its menu.
pub(super) fn linear_menu<V: FlowHost>(this: &mut V, pick: Option<Pick>, cx: &mut Context<V>) {
    let ui = &mut this.flow_ui_mut().linear_ui;
    match pick {
        Some(p) => ui.toggle(p),
        None => ui.menu = None,
    }
    cx.notify();
}

/// A click in the GitHub step.
pub(super) fn github_msg<V: FlowHost>(this: &mut V, msg: GithubMsg, window: &mut Window, cx: &mut Context<V>) {
    this.flow_ui_mut().github_menu = None;
    let github = this.flow_mut().github_mut();
    match msg {
        GithubMsg::Owner(owner) => github.select_owner(&owner),
        GithubMsg::Repo(repo) => github.select_repo(&repo),
        GithubMsg::ChangeAccount => github.change_account(),
        GithubMsg::CancelChangeAccount => github.cancel_change_account(),
        GithubMsg::Retry => github.retry(),
    }
    this.flow_changed(window, cx);
}

/// Flip an agent rule (a click on its card).
pub(super) fn toggle_rule<V: FlowHost>(this: &mut V, assigned_only: bool, cx: &mut Context<V>) {
    let flow = this.flow_mut();
    let mut rules = flow.rules();
    if assigned_only {
        rules.assigned_only = !rules.assigned_only;
    } else {
        rules.require_review = !rules.require_review;
    }
    flow.set_rules(rules);
    cx.notify();
}

/// A card's Connect (or File system's Use).
pub(super) fn connect<V: FlowHost>(this: &mut V, tracker: Tracker, window: &mut Window, cx: &mut Context<V>) {
    if this.flow_mut().connect(tracker) {
        this.flow_changed(window, cx);
    }
    cx.notify();
}

/// A tracker's mark.
pub(super) fn mark(tracker: Tracker) -> crate::assets::Icon {
    match tracker {
        Tracker::Linear => icons::LINEAR,
        Tracker::Github => icons::GITHUB,
        Tracker::FileSystem => icons::FOLDER,
    }
}

/// A card's id (`tracker-card-github`) and its button's.
pub(super) fn card_id(tracker: Tracker) -> &'static str {
    match tracker {
        Tracker::Github => "tracker-card-github",
        Tracker::Linear => "tracker-card-linear",
        Tracker::FileSystem => "tracker-card-local",
    }
}

pub(super) fn button_id(tracker: Tracker) -> &'static str {
    match tracker {
        Tracker::Github => "tracker-connect-github",
        Tracker::Linear => "tracker-connect-linear",
        Tracker::FileSystem => "tracker-connect-local",
    }
}

/// The green "Connected" badge in a card's button slot (02a-connected).
pub(super) fn connected_badge(tracker: Tracker, theme: &Theme) -> Div {
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

/// An agent rule's setting card (04-agent-rules): its title over its help,
/// and its switch at the right.
pub(super) fn rule_card(id: &'static str, title: String, help: impl Into<gpui::SharedString>, on: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(Role::Switch)
        .aria_label(title.clone())
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .flex()
        .flex_row()
        .items_center()
        .gap(px(16.0))
        .w_full()
        .px(px(16.0))
        .py(px(16.0))
        .rounded(px(12.0))
        .bg(theme.toolbar)
        .cursor_pointer()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(8.0))
                .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(title))
                .child(div().debug_selector(move || format!("{id}-help")).type_style(Type::Small).text_color(theme.muted).child(help.into())),
        )
        .child(controls::switch(on, theme).debug_selector(move || format!("{id}-{}", if on { "on" } else { "off" })))
}

/// The account line (`✓ Account connected · Change account`, in the accent).
pub(super) fn account_line<V: FlowHost>(id: &'static str, theme: &Theme, cx: &mut Context<V>, on_change: fn(&mut V, &mut Window, &mut Context<V>)) -> Div {
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
pub(super) fn quiet(selector: &'static str, text: impl Into<gpui::SharedString>, theme: &Theme) -> Div {
    div().debug_selector(move || selector.into()).type_style(Type::Small).text_color(theme.muted).child(text.into())
}

pub(super) fn error_line(selector: &'static str, text: impl Into<gpui::SharedString>, theme: &Theme) -> Div {
    div().debug_selector(move || selector.into()).type_style(Type::Small).text_color(theme.error).child(text.into())
}

/// A field: its label over its control, 8pt apart.
pub(super) fn labeled(label: &'static str, control: impl IntoElement, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w_full()
        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(label))
        .child(control)
}

/// The Choose your GitHub project step: the account line (or how to
/// sign `gh` in), then Organization and Repository.
pub(super) fn github_step<V: FlowHost>(host: &V, theme: &Theme, cx: &mut Context<V>) -> Div {
    let choice = host.flow().github();
    let mut col = div().flex().flex_col().gap(px(16.0)).w(px(FIELDS_W));
    if choice.needs_auth() {
        return col.child(github_auth(host, theme, cx));
    }
    col = col.child(match (choice.auth(), choice.account()) {
        (GithubAuth::Valid, Loadable::Loaded(_)) => account_line("tracker-github", theme, cx, |this, window, cx| {
            github_msg(this, GithubMsg::ChangeAccount, window, cx)
        }),
        (_, Loadable::Failed(why)) => div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(error_line("tracker-github-account-error", format!("Couldn't read the GitHub account: {why}"), theme))
            .child(github_retry::<V>(theme, cx)),
        _ => quiet("tracker-github-checking", "Checking the GitHub account…", theme),
    });

    let owner_label = match choice.owners() {
        Some(_) => choice.owner().to_string(),
        None => LOADING.to_string(),
    };
    let owner = github_picker(host, GithubPick::Owner, choice.owners(), choice.owner(), owner_label, theme, cx);
    let repos = choice.repos();
    let repo_label = match (&repos, choice.repo()) {
        (_, repo) if !repo.is_empty() => repo.to_string(),
        (Loadable::Loaded(rows), _) if rows.is_empty() => gh::NO_REPOS.to_string(),
        (Loadable::Loaded(_), _) => gh::CHOOSE_REPO.to_string(),
        (Loadable::Failed(_), _) => "Couldn't load".to_string(),
        _ => LOADING.to_string(),
    };
    let repo = github_picker(host, GithubPick::Repo, repos.loaded().cloned(), choice.repo(), repo_label, theme, cx);
    col.child(labeled(gh::OWNER_LABEL, owner, theme))
        .child(labeled(gh::REPO_LABEL, repo, theme).children(repos.error().map(|e| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(error_line("tracker-github-repo-error", e.to_string(), theme))
                .child(github_retry::<V>(theme, cx))
        })))
}

pub(super) fn github_retry<V: FlowHost>(theme: &Theme, cx: &mut Context<V>) -> Stateful<Div> {
    style::button("tracker-github-retry", copy::RETRY, ButtonKind::Secondary, ButtonSize::Small, false, theme)
        .role(Role::Button)
        .aria_label(copy::RETRY)
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| github_msg(this, GithubMsg::Retry, window, cx)))
}

/// How to sign the project's machine's `gh` in (or in to another
/// account): why, the command with Copy, and Retry.
pub(super) fn github_auth<V: FlowHost>(host: &V, theme: &Theme, cx: &mut Context<V>) -> Div {
    let choice = host.flow().github();
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
                github_msg(this, GithubMsg::CancelChangeAccount, window, cx)
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
        .child(div().flex().flex_row().gap(px(8.0)).child(github_retry::<V>(theme, cx)).children(cancel))
}

/// A GitHub picker: its button, and `rows` in a menu under it while
/// open. Greyed while there are no rows to pick.
pub(super) fn github_picker<V: FlowHost>(host: &V,
    pick: GithubPick,
    rows: Option<Vec<String>>,
    selected: &str,
    label: String,
    theme: &Theme,
    cx: &mut Context<V>,
) -> AnyElement {
    let id = pick.id();
    let button = picker_button(id, label, None, false, theme).aria_label(pick.label());
    let Some(rows) = rows.filter(|r| !r.is_empty()) else {
        return div().w_full().child(button.opacity(0.4).cursor_default()).into_any_element();
    };
    let selected = selected.to_string();
    dropdown(
        host.flow_ui().github_menu == Some(pick),
        &host.flow_ui().github_anchor,
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
                            github_msg(this, msg, window, cx);
                        }))
                        .into_any_element()
                })
                .collect()
        },
        FIELDS_W,
        false,
        move |this: &mut V, cx| {
            let ui = this.flow_ui_mut();
            ui.github_menu = if ui.github_menu == Some(pick) { None } else { Some(pick) };
            cx.notify();
        },
        |this: &mut V, cx| {
            this.flow_ui_mut().github_menu = None;
            cx.notify();
        },
        cx,
    )
    .w_full()
    .into_any_element()
}

/// The Choose your Linear project step: the account line, then
/// Workspace, Team and Project. (No key field: Change account, or a key
/// that stops working, opens the Add API key step.)
pub(super) fn linear_step<V: FlowHost>(host: &V, theme: &Theme, cx: &mut Context<V>) -> Div {
    let choice = host.flow().linear();
    let col = div().flex().flex_col().gap(px(16.0)).w(px(FIELDS_W));
    let on = linear_fields::Handlers {
        msg: |this: &mut V, msg, window, cx| linear_msg(this, msg, window, cx),
        menu: |this: &mut V, pick, cx| linear_menu(this, pick, cx),
    };
    let enabled = !host.flow().checking();
    let account = match choice.key_state() {
        KeyState::Valid { .. } => account_line("tracker-linear", theme, cx, |this, window, cx| {
            linear_msg(this, linear_fields::Msg::ChangeKey, window, cx)
        }),
        _ => quiet("tracker-linear-checking", "Checking the Linear API key…", theme),
    };
    col.child(account)
        .child(linear_fields::connect_fields(choice, &host.flow_ui().linear_ui, enabled, on, FIELDS_W, theme, cx))
}

/// The Add API key step (02b-linear-api-key): the key field with Verify,
/// 624pt together, why a key was refused under it (or whose it is once
/// verified), then where keys are made, with Open Linear.
pub(super) fn api_key_step<V: FlowHost>(host: &V, theme: &Theme, window: &Window, cx: &mut Context<V>) -> Div {
    let choice = host.flow().linear();
    let focused = host.flow_ui().linear_ui.key_input.read(cx).focus_handle(cx).is_focused(window);
    let error = host.flow().key_error();
    let field = div().flex_1().min_w_0().child(
        gpui_kit::component::input::Input::new(&host.flow_ui().linear_ui.key_input)
            .appearance(false)
            .h_full()
            .px_0()
            .type_style(Type::Base)
            .text_color(theme.form_text),
    );
    let input = if error.is_some() {
        controls::invalid_textbox(field, focused, theme)
    } else {
        controls::textbox(field, focused, theme)
    }
    .debug_selector(|| "tracker-key-input".into())
    .flex_1()
    .min_w_0();
    let verify = if choice.logging_in() {
        style::disabled_button("tracker-key-verify", "Verifying…", ButtonKind::Secondary, ButtonSize::Base, theme)
    } else {
        style::button("tracker-key-verify", VERIFY, ButtonKind::Secondary, ButtonSize::Base, false, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| linear_msg(this, linear_fields::Msg::Verify, window, cx)))
    }
    .role(Role::Button)
    .aria_label(VERIFY);
    let note = match (error, host.flow().verified_line()) {
        (Some(why), _) => Some(error_line("tracker-key-error", why, theme)),
        (None, Some(line)) => Some(
            div()
                .debug_selector(|| "tracker-key-verified".into())
                .flex()
                .items_center()
                .gap(px(4.0))
                .type_style(Type::Small)
                .text_color(theme.green)
                .child(heroicon(icons::CHECK).text_color(theme.green))
                .child(line),
        ),
        (None, None) => None,
    };
    // The help wraps where the frame's 420pt does: the path, then the
    // last step and Open Linear on the next line.
    let (path, last) = copy::API_KEY_HELP.rsplit_once(" → ").unwrap_or((copy::API_KEY_HELP, ""));
    let link = div()
        .id("tracker-linear-keys-link")
        .debug_selector(|| "tracker-linear-keys-link".into())
        .role(Role::Link)
        .aria_label(copy::LINEAR_KEYS_LINK)
        .cursor_pointer()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.foreground)
        .underline()
        .hover(|s| s.text_color(theme.text_loud))
        .child(copy::LINEAR_KEYS_LINK)
        .on_click(|_: &ClickEvent, _, cx| super::about::open_url(copy::LINEAR_KEYS_URL, cx));
    let help = div()
        .debug_selector(|| "tracker-key-help".into())
        .flex()
        .flex_col()
        .w(px(KEY_HELP_W))
        .type_style(Type::Small)
        .text_color(theme.muted)
        .child(format!("{path} →"))
        .child(div().flex().flex_row().gap(px(6.0)).child(last.to_string()).child(link));
    div()
        .debug_selector(|| "tracker-api-key".into())
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w(px(FIELDS_W))
        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(copy::API_KEY_LABEL))
        .child(div().flex().flex_row().items_center().gap(px(8.0)).w_full().child(input).child(verify))
        .children(note)
        .child(help)
}

/// The Sign in to GitHub step: how to sign the GitHub CLI in, with
/// Retry, and who it's signed in as once it is.
pub(super) fn github_auth_step<V: FlowHost>(host: &V, theme: &Theme, cx: &mut Context<V>) -> Div {
    let col = div().flex().flex_col().gap(px(16.0)).w(px(FIELDS_W));
    if let Some(line) = host.flow().verified_line() {
        return col.child(
            div()
                .debug_selector(|| "tracker-github-signed-in".into())
                .flex()
                .items_center()
                .gap(px(4.0))
                .type_style(Type::Base)
                .text_color(theme.green)
                .child(heroicon(icons::CHECK).text_color(theme.green))
                .child(line),
        );
    }
    if host.flow().github().account().is_loading() {
        return col.child(quiet("tracker-github-checking", "Checking the GitHub account…", theme));
    }
    col.child(github_auth(host, theme, cx))
}

pub(super) fn card<V: FlowHost>(host: &V, card: Card, theme: &Theme, cx: &mut Context<V>) -> Stateful<Div> {
    let tracker = card.tracker;
    let ring = host.ring(tracker);
    let action = match card.state {
        CardState::Connected => connected_badge(tracker, theme).into_any_element(),
        CardState::Checking => {
            style::disabled_button(button_id(tracker), copy::CHECKING, ButtonKind::Primary, ButtonSize::Small, theme)
                .role(Role::Button)
                .aria_label(format!("{} {}", copy::CHECKING, card.title))
                .into_any_element()
        }
        CardState::Disabled => {
            let label = if tracker == Tracker::FileSystem { copy::USE } else { copy::CONNECT };
            style::disabled_button(button_id(tracker), label, ButtonKind::Primary, ButtonSize::Small, theme)
                .role(Role::Button)
                .aria_label(format!("{label} {}", card.title))
                .into_any_element()
        }
        CardState::Connect | CardState::Use => {
            let label = if card.state == CardState::Use { copy::USE } else { copy::CONNECT };
            style::button(button_id(tracker), label, ButtonKind::Primary, ButtonSize::Small, ring, theme)
                .role(Role::Button)
                .aria_label(format!("{label} {}", card.title))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| connect(this, tracker, window, cx)))
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

pub(super) fn chooser<V: FlowHost>(host: &V, theme: &Theme, cx: &mut Context<V>) -> Div {
    let cards: Vec<_> = host.flow().cards().into_iter().map(|c| card(host, c, theme, cx)).collect();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(16.0))
        .w_full()
        .mt(px(CHOOSER_DROP))
        .children(cards)
}

/// How should agents handle issues created in <Tracker>? The two rules.
pub(super) fn rules_step<V: FlowHost>(host: &V, theme: &Theme, cx: &mut Context<V>) -> Div {
    let rules = host.flow().rules();
    let card = |id, title, help, on, assigned_only, cx: &mut Context<V>| {
        rule_card(id, title, help, on, theme)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| toggle_rule(this, assigned_only, cx)))
    };
    div()
        .debug_selector(|| "tracker-rules".into())
        .flex()
        .flex_col()
        .gap(px(16.0))
        .w_full()
        // The frame's cards sit 39pt lower than the choose step's fields.
        .mt(px(39.0))
        .child(card(
            "tracker-rule-review",
            rules_copy::REVIEW_TITLE.to_string(),
            rules_copy::REVIEW_HELP,
            rules.require_review,
            false,
            cx,
        ))
        .child(card(
            "tracker-rule-assigned",
            host.flow().assigned_title(),
            rules_copy::ASSIGNED_HELP,
            rules.assigned_only,
            true,
            cx,
        ))
}


/// The step's body for the steps the two windows share (the chooser through
/// the agent rules); `None` for the import's steps, which only Connect to
/// an Issue tracker has.
pub(super) fn content<V: FlowHost>(host: &V, theme: &Theme, window: &Window, cx: &mut Context<V>) -> Option<Div> {
    let tracker = host.flow().tracker();
    Some(match host.flow().step() {
        Step::Chooser => chooser(host, theme, cx),
        // Centered in the room between the heading and the footer.
        Step::Authorize => div().flex().flex_col().items_center().w_full().child(match tracker {
            Some(Tracker::Linear) => api_key_step(host, theme, window, cx),
            _ => github_auth_step(host, theme, cx),
        }),
        Step::ChooseProject => {
            let body = match tracker {
                Some(Tracker::Linear) => linear_step(host, theme, cx),
                _ => github_step(host, theme, cx),
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
        Step::AgentRules => rules_step(host, theme, cx),
        Step::ReviewImport | Step::Importing => return None,
    })
}

/// The step's content sits in the middle of the room under the heading
/// (Add API key's field, Importing's progress), not hung under it.
pub(super) fn centered(step: Step) -> bool {
    matches!(step, Step::Authorize | Step::Importing)
}
