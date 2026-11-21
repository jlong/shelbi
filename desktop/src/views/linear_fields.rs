//! The Linear connection's fields as the desktop draws them, over
//! `shelbi_app::linear_setup::LinearChoice`: the account (whose key, with
//! Change key…, or the paste-and-verify key field while there's no working
//! key), the Team picker (required), then the optional Project, Label and
//! Assignee pickers and the sentence saying what the picks show. One control
//! for Settings ▸ Properties' Issue tracker section
//! (`views/tracker_section.rs`).
//!
//! ```text
//!  Linear account
//!  Ada Lovelace <ada@example.com>                     [Change key…]
//!  Team *
//!  [ ENG  Engineering                               ⌄ ]
//!  Project            Label              Assignee
//!  [ Any          ⌄ ] [ Any          ⌄ ] [ Me          ⌄ ]
//!  Shelbi shows and creates issues in ENG, assigned to you.
//! ```
//!
//! No key yet:
//!
//! ```text
//!  Linear account
//!  Not connected. Paste a personal API key.
//!  [ ••••••••••••••••••••                ] [Verify]
//!  Create one in Linear: Settings > Account > Security & access > ...
//! ```
//!
//! Connect to an Issue tracker's "Choose your Linear project" step
//! (`views/tracker_steps.rs`, also the new-project wizard's) lays the same pickers out its own way
//! ([`connect_fields`]): the key's Workspace (read-only), Team as
//! `Name · KEY`, and Project from "Any project", stacked at the frame's
//! width, with no label or assignee filter. Its key entry is its own Add API
//! key step.
//!
//! The host keeps the choice, the key field and which picker is open
//! ([`LinearUi`]), and hands in what a click does ([`Handlers`]); it runs
//! the reads the choice asks for (`LinearChoice::due`) itself.

use gpui::{div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, Focusable, SharedString, Window};
use gpui_kit::component::input::{Input, InputState};
use shelbi_app::linear_setup::{
    Filter, KeyState, LinearChoice, Loadable, PickOption, ACCOUNT_LABEL, CHANGE_KEY, CHOOSE_TEAM, FILTERS_HELP,
    KEY_HELP, KEY_PLACEHOLDER, LOADING, NOT_CONNECTED, PROJECT_LABEL, TEAM_LABEL, VERIFY,
};
use shelbi_app::tracker_connect::flow::{ANY_PROJECT, WORKSPACE_LABEL};

use super::harness_picker::picker_button;
use super::workflow_editor::{dropdown, footnote};
use crate::elements::controls;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// A picker that can be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Team,
    Filter(Filter),
}

/// What a click asks of the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Verify (or Enter in the key field): check the pasted key and store it.
    Verify,
    ChangeKey,
    CancelChangeKey,
    /// Ask Linear again for the lists whose read failed.
    Retry,
    /// A team row (its value).
    Team(String),
    /// A filter row (its value; empty: no filter).
    Filter(Filter, String),
}

/// Whose fields these are: their element ids and test selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Settings,
    /// Connect to an Issue tracker's Choose your Linear project step.
    Connect,
}

impl Host {
    fn prefix(self) -> &'static str {
        match self {
            Host::Settings => "tracker-linear",
            Host::Connect => "tracker-connect-linear",
        }
    }

    /// A picker's id (and its menu's, with `-menu`).
    pub fn pick_id(self, pick: Pick) -> &'static str {
        match (self, pick) {
            (Host::Settings, Pick::Team) => "tracker-linear-team",
            (Host::Settings, Pick::Filter(Filter::Project)) => "tracker-linear-project",
            (Host::Settings, Pick::Filter(Filter::Label)) => "tracker-linear-label",
            (Host::Settings, Pick::Filter(Filter::Assignee)) => "tracker-linear-assignee",
            (Host::Connect, Pick::Team) => "tracker-connect-linear-team",
            (Host::Connect, Pick::Filter(Filter::Project)) => "tracker-connect-linear-project",
            (Host::Connect, Pick::Filter(Filter::Label)) => "tracker-connect-linear-label",
            (Host::Connect, Pick::Filter(Filter::Assignee)) => "tracker-connect-linear-assignee",
        }
    }

    fn menu_id(self, pick: Pick) -> &'static str {
        match (self, pick) {
            (Host::Settings, Pick::Team) => "tracker-linear-team-menu",
            (Host::Settings, Pick::Filter(Filter::Project)) => "tracker-linear-project-menu",
            (Host::Settings, Pick::Filter(Filter::Label)) => "tracker-linear-label-menu",
            (Host::Settings, Pick::Filter(Filter::Assignee)) => "tracker-linear-assignee-menu",
            (Host::Connect, Pick::Team) => "tracker-connect-linear-team-menu",
            (Host::Connect, Pick::Filter(Filter::Project)) => "tracker-connect-linear-project-menu",
            (Host::Connect, Pick::Filter(Filter::Label)) => "tracker-connect-linear-label-menu",
            (Host::Connect, Pick::Filter(Filter::Assignee)) => "tracker-connect-linear-assignee-menu",
        }
    }

    fn id(self, what: &str) -> SharedString {
        format!("{}-{what}", self.prefix()).into()
    }
}

/// The host's half of the fields: the key field and the open picker.
pub struct LinearUi {
    /// The API key field (masked).
    pub key_input: Entity<InputState>,
    pub menu: Option<Pick>,
    pub anchor: style::Anchor,
}

impl LinearUi {
    pub fn new<V: 'static>(window: &mut Window, cx: &mut Context<V>) -> Self {
        Self::with_placeholder(KEY_PLACEHOLDER, window, cx)
    }

    /// The fields with the key field's own placeholder (Connect to an Issue
    /// tracker's Add API key step: `lin_api…`).
    pub fn with_placeholder<V: 'static>(placeholder: &'static str, window: &mut Window, cx: &mut Context<V>) -> Self {
        LinearUi {
            key_input: cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(placeholder)),
            menu: None,
            anchor: Default::default(),
        }
    }

    /// Show `text` in the key field when it holds something else (the
    /// choice cleared it after a login).
    pub fn sync_key(&self, text: &str, window: &mut Window, cx: &mut App) {
        if self.key_input.read(cx).value() != text {
            let text = text.to_string();
            self.key_input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    /// Open `pick`'s menu, or close it when it's the one open.
    pub fn toggle(&mut self, pick: Pick) {
        self.menu = if self.menu == Some(pick) { None } else { Some(pick) };
    }
}

/// The keyboard's focus ring on each control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ring {
    pub team: bool,
    pub filter: Option<Filter>,
}

/// What the host does when the fields are used.
pub struct Handlers<V> {
    pub msg: fn(&mut V, Msg, &mut Window, &mut Context<V>),
    /// Open a picker's menu (`Some`, from its button) or close it.
    pub menu: fn(&mut V, Option<Pick>, &mut Context<V>),
}

impl<V> Clone for Handlers<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> Copy for Handlers<V> {}

/// A field: its label over the control, then the help.
fn field(label: impl Into<SharedString>, control: impl IntoElement, help: Option<SharedString>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .w_full()
        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(label.into()))
        .child(control)
        .children(help.map(|h| footnote(h, theme)))
}

fn error_text(selector: SharedString, text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.to_string())
        .type_style(Type::Small)
        .text_color(theme.error)
        .child(text.into())
}

/// The Linear fields over `choice`. `enabled` false greys the pickers and
/// buttons (a connection running).
#[allow(clippy::too_many_arguments)]
pub fn fields<V: 'static>(
    host: Host,
    choice: &LinearChoice,
    ui: &LinearUi,
    ring: Ring,
    enabled: bool,
    on: Handlers<V>,
    theme: &Theme,
    window: &Window,
    cx: &mut Context<V>,
) -> Div {
    let mut out = div()
        .debug_selector(move || host.prefix().into())
        .flex()
        .flex_col()
        .gap(px(20.0))
        .w_full()
        .child(account(host, choice, ui, enabled, on, theme, window, cx))
        .child(team(host, choice, ui, ring.team, enabled, on, theme, cx));
    if choice.team_option().is_some() {
        let pickers: Vec<AnyElement> = Filter::ALL
            .into_iter()
            .map(|f| filter(host, choice, ui, f, ring.filter == Some(f), enabled, on, theme, cx).into_any_element())
            .collect();
        let failed = Filter::ALL.iter().any(|f| choice.filter_options(*f).error().is_some());
        out = out.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .w_full()
                .child(div().flex().flex_row().gap(px(12.0)).w_full().children(pickers))
                .child(footnote(FILTERS_HELP, theme))
                .children(failed.then(|| retry(host, enabled, on, theme, cx))),
        );
    }
    if let Some(scope) = choice.scope_line() {
        out = out.child(
            div()
                .debug_selector(move || format!("{}-scope", host.prefix()))
                .type_style(Type::Base)
                .text_color(theme.foreground)
                .child(scope),
        );
    }
    out
}

/// The account row: whose the working key is (and Change key…), or the key
/// field with Verify.
#[allow(clippy::too_many_arguments)]
fn account<V: 'static>(
    host: Host,
    choice: &LinearChoice,
    ui: &LinearUi,
    enabled: bool,
    on: Handlers<V>,
    theme: &Theme,
    window: &Window,
    cx: &mut Context<V>,
) -> Div {
    let msg = on.msg;
    if !choice.needs_key() {
        let (text, known) = match choice.key_state() {
            KeyState::Valid { viewer: Some(v) } => (v.clone(), true),
            KeyState::Valid { viewer: None } => ("Connected".to_string(), true),
            _ => ("Checking the Linear API key…".to_string(), false),
        };
        let change = (known && enabled).then(|| {
            style::button(host.id("change-key"), CHANGE_KEY, ButtonKind::Secondary, ButtonSize::Small, false, theme)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| msg(this, Msg::ChangeKey, window, cx)))
        });
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .w_full()
            .child(
                div()
                    .debug_selector(move || format!("{}-account", host.prefix()))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_style(Type::Base)
                    .text_color(theme.form_text)
                    .child(text),
            )
            .children(change);
        return field(ACCOUNT_LABEL, row, None, theme);
    }
    let focused = ui.key_input.read(cx).focus_handle(cx).is_focused(window);
    let input = controls::textbox(
        div().flex_1().min_w_0().child(
            Input::new(&ui.key_input)
                .appearance(false)
                .h_full()
                .px_0()
                .type_style(Type::Base)
                .text_color(theme.form_text),
        ),
        focused,
        theme,
    )
    .debug_selector(move || format!("{}-key", host.prefix()))
    .flex_1()
    .min_w_0();
    let verify = if choice.logging_in() {
        style::disabled_button(host.id("verify"), "Verifying…", ButtonKind::Primary, ButtonSize::Base, theme)
    } else if enabled {
        style::button(host.id("verify"), VERIFY, ButtonKind::Primary, ButtonSize::Base, false, theme)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| msg(this, Msg::Verify, window, cx)))
    } else {
        style::disabled_button(host.id("verify"), VERIFY, ButtonKind::Primary, ButtonSize::Base, theme)
    };
    let cancel = (choice.changing_key() && !choice.logging_in()).then(|| {
        style::button(host.id("cancel-key"), "Cancel", ButtonKind::Secondary, ButtonSize::Base, false, theme)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| msg(this, Msg::CancelChangeKey, window, cx)))
    });
    let (note, color) = match choice.key_error() {
        Some(why) => (why.to_string(), theme.error),
        None if choice.changing_key() => ("Paste the new key. The current one stays until it's verified.".into(), theme.form_hint),
        None => (NOT_CONNECTED.to_string(), theme.form_hint),
    };
    let control = div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w_full()
        .child(
            div()
                .debug_selector(move || format!("{}-key-note", host.prefix()))
                .type_style(Type::Base)
                .text_color(color)
                .child(note),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(controls::CONTROL_GAP))
                .w_full()
                .child(input)
                .child(verify)
                .children(cancel),
        );
    field(ACCOUNT_LABEL, control, Some(KEY_HELP.into()), theme)
}

/// The Choose your Linear project step's pickers, each `width` wide:
/// Workspace (the key's, read-only), Team (required, `Name · KEY`) and
/// Project ("Any project" first).
#[allow(clippy::too_many_arguments)]
pub fn connect_fields<V: 'static>(
    choice: &LinearChoice,
    ui: &LinearUi,
    enabled: bool,
    on: Handlers<V>,
    width: f32,
    theme: &Theme,
    cx: &mut Context<V>,
) -> Div {
    let host = Host::Connect;
    let (workspace, faded) = match choice.workspace() {
        Loadable::Loaded(name) => (name.clone(), false),
        Loadable::Failed(_) => ("Couldn't load".to_string(), true),
        Loadable::Idle | Loadable::Loading => (LOADING.to_string(), true),
    };
    let workspace = picker_button("tracker-connect-linear-workspace", workspace, None, false, theme)
        .aria_label(WORKSPACE_LABEL)
        .cursor_default()
        .when(faded, |d| d.opacity(0.6));

    // Team: `Name · KEY`.
    let team_row = |row: &PickOption| PickOption {
        label: match row.detail.as_deref().filter(|d| !d.is_empty()) {
            Some(name) => format!("{name} · {}", row.label),
            None => row.label.clone(),
        },
        detail: None,
        ..row.clone()
    };
    let teams = choice.teams();
    let picked = choice.team_option().map(team_row);
    let mut label = shown(teams, picked.as_ref(), CHOOSE_TEAM);
    if choice.needs_key() && teams.loaded().is_none() {
        label = (KEY_FIRST.to_string(), None);
    }
    let rows = teams.loaded().map(|rows| rows.iter().map(team_row).collect());
    let team = picker(host, Pick::Team, rows, picked, label, ui.menu == Some(Pick::Team), false, enabled, ui, on, width, theme, cx);
    let team_error = teams
        .error()
        .filter(|_| !choice.needs_key())
        .map(|e| error_text(host.id("teams-error"), format!("Couldn't load the teams: {e}"), theme));

    // Project: "Any project", then the team's.
    let project_row = |row: PickOption| {
        if row.value.is_empty() {
            PickOption { label: ANY_PROJECT.into(), ..row }
        } else {
            PickOption { detail: None, ..row }
        }
    };
    let list = choice.filter_options(Filter::Project);
    let picked = choice.filter_option(Filter::Project).map(project_row);
    let mut label = (shown(&list, picked.as_ref(), ANY_PROJECT).0, None);
    if picked.is_none() && list.loaded().is_some() {
        label = (choice.filter(Filter::Project).unwrap_or(ANY_PROJECT).to_string(), None);
    }
    let rows = list.loaded().map(|rows| rows.iter().cloned().map(project_row).collect());
    let pick = Pick::Filter(Filter::Project);
    let project = picker(host, pick, rows, picked, label, ui.menu == Some(pick), false, enabled, ui, on, width, theme, cx);
    let project_error =
        list.error().map(|e| error_text(SharedString::from(format!("{}-error", host.pick_id(pick))), e.to_string(), theme));
    let failed = teams.error().is_some() || list.error().is_some() || choice.workspace().error().is_some();

    div()
        .debug_selector(move || host.prefix().into())
        .flex()
        .flex_col()
        .gap(px(16.0))
        .w(px(width))
        .child(field(WORKSPACE_LABEL, workspace, None, theme).gap(px(8.0)))
        .child(field(TEAM_LABEL, team, None, theme).gap(px(8.0)).children(team_error))
        .child(field(PROJECT_LABEL, project, None, theme).gap(px(8.0)).children(project_error))
        .children((failed && !choice.needs_key()).then(|| retry(host, enabled, on, theme, cx)))
}

/// The team picker while there's no working key.
const KEY_FIRST: &str = "Verify an API key first";

/// What a picker's button shows for `list`'s state and the row picked.
fn shown(list: &Loadable<Vec<PickOption>>, picked: Option<&PickOption>, empty: &str) -> (String, Option<String>) {
    match (list, picked) {
        (_, Some(row)) => (row.label.clone(), row.detail.clone()),
        (Loadable::Loading, None) => (LOADING.to_string(), None),
        (Loadable::Failed(_), None) => ("Couldn't load".to_string(), None),
        _ => (empty.to_string(), None),
    }
}

/// A picker: its button, and its rows in a menu under it while open.
#[allow(clippy::too_many_arguments)]
fn picker<V: 'static>(
    host: Host,
    pick: Pick,
    rows: Option<Vec<PickOption>>,
    selected: Option<PickOption>,
    label: (String, Option<String>),
    open: bool,
    ring: bool,
    enabled: bool,
    ui: &LinearUi,
    on: Handlers<V>,
    width: f32,
    theme: &Theme,
    cx: &mut Context<V>,
) -> Div {
    let id = host.pick_id(pick);
    let button = picker_button(id, label.0, label.1, ring, theme).aria_label(match pick {
        Pick::Team => TEAM_LABEL,
        Pick::Filter(f) => f.label(),
    });
    let Some(rows) = rows.filter(|_| enabled) else {
        return div().w_full().child(button.opacity(0.4).cursor_default());
    };
    let (msg, menu) = (on.msg, on.menu);
    dropdown(
        open,
        &ui.anchor,
        host.menu_id(pick),
        button,
        move |cx| {
            let theme = *cx.global::<Theme>();
            rows.into_iter()
                .enumerate()
                .map(|(i, row)| {
                    let on_row = selected.as_ref() == Some(&row);
                    let value = row.value.clone();
                    style::menu_row(format!("{id}-{i}"), on_row, &theme)
                        .child(div().flex_1().min_w_0().truncate().child(row.label))
                        .children(row.detail.map(|d| {
                            div().flex_none().type_style(Type::Small).text_color(theme.muted).child(d)
                        }))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            menu(this, None, cx);
                            let m = match pick {
                                Pick::Team => Msg::Team(value.clone()),
                                Pick::Filter(f) => Msg::Filter(f, value.clone()),
                            };
                            msg(this, m, window, cx);
                        }))
                        .into_any_element()
                })
                .collect()
        },
        width,
        false,
        move |this: &mut V, cx| menu(this, Some(pick), cx),
        move |this: &mut V, cx| menu(this, None, cx),
        cx,
    )
    .w_full()
}

#[allow(clippy::too_many_arguments)]
fn team<V: 'static>(
    host: Host,
    choice: &LinearChoice,
    ui: &LinearUi,
    ring: bool,
    enabled: bool,
    on: Handlers<V>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> Div {
    let teams = choice.teams();
    let picked = choice.team_option().cloned();
    let mut label = shown(teams, picked.as_ref(), CHOOSE_TEAM);
    if picked.is_none() && teams.loaded().is_some() && !choice.team().is_empty() {
        label = (choice.team().to_string(), None);
    }
    if choice.needs_key() && teams.loaded().is_none() {
        label = (KEY_FIRST.to_string(), None);
    }
    let rows = teams.loaded().cloned();
    let open = ui.menu == Some(Pick::Team);
    let control = picker(host, Pick::Team, rows, picked, label, open, ring, enabled, ui, on, 400.0, theme, cx);
    let error = teams
        .error()
        .filter(|_| !choice.needs_key())
        .map(|e| error_text(host.id("teams-error"), format!("Couldn't load the teams: {e}"), theme));
    let retry = teams.error().filter(|_| !choice.needs_key()).map(|_| retry(host, enabled, on, theme, cx));
    field(format!("{TEAM_LABEL} *"), div().w(px(controls::CONTROL_W)).max_w_full().child(control), None, theme)
        .children(error)
        .children(retry)
}

#[allow(clippy::too_many_arguments)]
fn filter<V: 'static>(
    host: Host,
    choice: &LinearChoice,
    ui: &LinearUi,
    f: Filter,
    ring: bool,
    enabled: bool,
    on: Handlers<V>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> Div {
    let list = choice.filter_options(f);
    let picked = choice.filter_option(f);
    // The row's name alone: a member's email (or the key owner's, for Me)
    // shows in the menu, not in a button a third of the row wide.
    let mut label = (shown(&list, picked.as_ref(), shelbi_app::linear_setup::ANY).0, None);
    if picked.is_none() && list.loaded().is_some() {
        // A filter the lists don't name (typed by hand in the YAML).
        label = (choice.filter(f).unwrap_or(shelbi_app::linear_setup::ANY).to_string(), None);
    }
    let rows = list.loaded().cloned();
    let open = ui.menu == Some(Pick::Filter(f));
    let control = picker(host, Pick::Filter(f), rows, picked, label, open, ring, enabled, ui, on, 260.0, theme, cx);
    div()
        .flex_1()
        .min_w_0()
        .child(field(f.label(), control, None, theme))
        .children(list.error().map(|e| error_text(SharedString::from(format!("{}-error", host.pick_id(Pick::Filter(f)))), e.to_string(), theme)))
}

fn retry<V: 'static>(host: Host, enabled: bool, on: Handlers<V>, theme: &Theme, cx: &mut Context<V>) -> AnyElement {
    let msg = on.msg;
    if !enabled {
        return style::disabled_button(host.id("retry"), "Try Again", ButtonKind::Secondary, ButtonSize::Small, theme)
            .into_any_element();
    }
    div()
        .flex()
        .child(
            style::button(host.id("retry"), "Try Again", ButtonKind::Secondary, ButtonSize::Small, false, theme)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| msg(this, Msg::Retry, window, cx))),
        )
        .into_any_element()
}
