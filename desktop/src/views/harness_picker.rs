//! The Harness and Default model fields as the desktop draws them, over
//! `shelbi_app::harness_picker`: each a label, a 36pt picker button across
//! the column (the choice, its provider muted, the dropdown chevron) whose
//! menu lists the choices, and the helper under it. One control for every
//! place that asks for a runner and model: the orchestrator onboarding's
//! "Choose your Orchestrator" (`views/onboarding.rs`, Figma 2709:11068) and
//! the new-project wizard's "Agent defaults" (`views/add_project.rs`, Figma
//! 2720:33054). The view that hosts them keeps which menu is open and hands
//! in what a choice does.

use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Context, Div, Entity, SharedString, Stateful, Subscription, Window};
use gpui_kit::component::input::{InputEvent, InputState};
use shelbi_app::harness_picker::CUSTOM_MODEL;
use shelbi_app::onboarding::LOADING;
use shelbi_app::view::{HarnessOption, ModelOption};
use shelbi_core::RunnerKind;

use super::workflow_editor::{dropdown, footnote, menu_item, text_field};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// A picker's height (the frames' 36pt Claude Code / Opus buttons).
pub const PICKER_H: f32 = 36.0;

/// A Model picker's "Custom model…": a field under the picker, focused,
/// holding the name being typed; Return hands it to `commit` (blank is the
/// runner's default). Its host keeps it while it's open, drops it to close.
pub struct CustomModel {
    input: Entity<InputState>,
    _sub: Subscription,
}

impl CustomModel {
    pub fn open<V: 'static>(
        current: Option<&str>,
        commit: fn(&mut V, Option<String>, &mut Window, &mut Context<V>),
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let current = current.unwrap_or("").to_string();
        input.update(cx, |s, cx| {
            s.set_value(current, window, cx);
            s.focus(window, cx);
        });
        let sub = cx.subscribe_in(&input, window, move |this: &mut V, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let text = input.read(cx).value().trim().to_string();
                commit(this, (!text.is_empty()).then_some(text), window, cx);
            }
        });
        CustomModel { input, _sub: sub }
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// The field, `selector` its test selector.
    pub fn render(&self, selector: &'static str, window: &Window, cx: &gpui::App) -> Div {
        text_field(&self.input, selector, Some("A model name; Return sets it".into()), false, window, cx)
    }
}

/// While OpenCode's model list is still being read (`on_opencode`, and the
/// probe hasn't answered), wait for it off the UI thread and redraw the
/// view when it does. `watching` (the view's flag, through `flag`) keeps it
/// to one wait.
pub fn watch_opencode_models<V: 'static>(on_opencode: bool, flag: fn(&mut V) -> &mut bool, view: &mut V, cx: &mut Context<V>) {
    if !on_opencode || *flag(view) || shelbi_app::opencode_models::catalog().settled() {
        return;
    }
    *flag(view) = true;
    let wait = cx
        .background_executor()
        .spawn(async { shelbi_app::opencode_models::wait(shelbi_app::opencode_models::DEADLINE * 2) });
    cx.spawn(async move |this, cx| {
        wait.await;
        this.update(cx, |view, cx| {
            *flag(view) = false;
            cx.notify();
        })
        .ok();
    })
    .detach();
}

/// A picker that is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    Harness,
    Model,
}

/// Whose fields these are: their element ids (and test selectors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Onboarding,
    AddProject,
}

impl Host {
    fn harness(self) -> &'static str {
        match self {
            Host::Onboarding => "onboarding-harness",
            Host::AddProject => "add-project-harness",
        }
    }

    fn harness_menu(self) -> &'static str {
        match self {
            Host::Onboarding => "onboarding-harness-menu",
            Host::AddProject => "add-project-harness-menu",
        }
    }

    fn model(self) -> &'static str {
        match self {
            Host::Onboarding => "onboarding-model",
            Host::AddProject => "add-project-model",
        }
    }

    fn custom_model(self) -> &'static str {
        match self {
            Host::Onboarding => "onboarding-custom-model",
            Host::AddProject => "add-project-custom-model",
        }
    }

    fn model_menu(self) -> &'static str {
        match self {
            Host::Onboarding => "onboarding-model-menu",
            Host::AddProject => "add-project-model-menu",
        }
    }
}

/// What the two fields show.
pub struct Fields<'a> {
    pub host: Host,
    pub harness_label: &'static str,
    pub model_label: &'static str,
    /// The helper under each picker.
    pub help: &'static str,
    pub harnesses: &'a [HarnessOption],
    pub models: &'a [ModelOption],
    /// What the Default model button shows.
    pub model_choice: String,
    /// Still reading what the pickers start on: both show "Loading…".
    pub loading: bool,
    /// The pickers can be used (not loading, no save running).
    pub enabled: bool,
    /// The runner takes a model.
    pub model_enabled: bool,
    /// In place of the helper under Default model.
    pub model_note: Option<String>,
    /// The column's width (the menus are as wide).
    pub width: f32,
    /// The keyboard's focus ring on each button.
    pub ring: (bool, bool),
    /// Default model ▸ Custom model…, while it's open: drawn under the
    /// picker.
    pub custom: Option<&'a CustomModel>,
}

/// What the host does when a picker is used.
pub struct Handlers<V> {
    pub select_runner: fn(&mut V, RunnerKind, &mut Context<V>),
    pub select_model: fn(&mut V, Option<String>, &mut Context<V>),
    /// Default model ▸ Custom model…: open the typed field.
    pub custom_model: fn(&mut V, &mut Window, &mut Context<V>),
    pub toggle: fn(&mut V, Menu, &mut Context<V>),
    pub close: fn(&mut V, &mut Context<V>),
}

/// A picker's button: `label` (Base Medium) and `secondary` (Small, muted)
/// at the left, the dropdown chevron at the right, [`PICKER_H`] tall across
/// the column.
pub fn picker_button(
    id: &'static str,
    label: String,
    secondary: Option<String>,
    ring: bool,
    theme: &Theme,
) -> Stateful<Div> {
    style::button_base(id, ButtonKind::Secondary, ButtonSize::Base, ring, theme)
        .h(px(PICKER_H))
        .w_full()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .gap(px(8.0))
                .min_w_0()
                .child(div().debug_selector(move || format!("{id}-label")).truncate().child(label))
                .children(secondary.map(|s| {
                    div()
                        .debug_selector(move || format!("{id}-provider"))
                        .type_style(Type::Small)
                        .text_color(theme.muted)
                        .child(s)
                })),
        )
        .child(style::dropdown_slot(ButtonSize::Base, theme.button_icon))
}

/// A field's label (Base Semibold, loud).
fn label(text: &'static str, theme: &Theme) -> Div {
    div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(text)
}

/// Harness over Default model, 24pt apart, each its label, picker and
/// helper 8pt apart.
pub fn fields<V: 'static>(
    f: Fields<'_>,
    open: Option<Menu>,
    anchor: &style::Anchor,
    on: Handlers<V>,
    theme: &Theme,
    window: &Window,
    cx: &mut Context<V>,
) -> Div {
    let Handlers { select_runner, select_model, custom_model, toggle, close } = on;
    let harness = f.harnesses.iter().find(|h| h.selected);
    let (shown, provider) = match harness {
        Some(h) if !f.loading => (h.label.clone(), h.provider.clone()),
        _ => (LOADING.to_string(), None),
    };
    let harness_button = picker_button(f.host.harness(), shown, provider, f.ring.0, theme);
    let harness_picker = if !f.enabled {
        div().w_full().child(harness_button.opacity(0.4).cursor_default())
    } else {
        let items = f.harnesses.to_vec();
        let host = f.host;
        dropdown(
            open == Some(Menu::Harness),
            anchor,
            host.harness_menu(),
            harness_button,
            move |cx| {
                let theme = *cx.global::<Theme>();
                items
                    .into_iter()
                    .map(|h| {
                        let kind = h.kind;
                        style::menu_row(format!("{}-{}", host.harness(), kind.as_str()), h.selected, &theme)
                            .child(h.label)
                            .children(h.provider.map(|p| div().type_style(Type::Small).text_color(theme.muted).child(p)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                select_runner(this, kind, cx);
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            f.width,
            false,
            move |this: &mut V, cx| toggle(this, Menu::Harness, cx),
            move |this: &mut V, cx| close(this, cx),
            cx,
        )
        .w_full()
    };

    let shown = if f.loading { LOADING.to_string() } else { f.model_choice.clone() };
    let model_button = picker_button(f.host.model(), shown, None, f.ring.1, theme);
    let model_picker = if !f.model_enabled || !f.enabled {
        div().w_full().child(model_button.opacity(0.4).cursor_default())
    } else {
        let items = f.models.to_vec();
        let host = f.host;
        dropdown(
            open == Some(Menu::Model),
            anchor,
            host.model_menu(),
            model_button,
            move |cx| {
                let theme = *cx.global::<Theme>();
                let mut rows: Vec<AnyElement> = items
                    .into_iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let model = o.model.clone();
                        menu_item(format!("{}-{i}", host.model()), o.label, o.selected, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                select_model(this, model.clone(), cx);
                            }))
                            .into_any_element()
                    })
                    .collect();
                rows.push(
                    menu_item(format!("{}-custom", host.model()), CUSTOM_MODEL, false, &theme)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            custom_model(this, window, cx);
                        }))
                        .into_any_element(),
                );
                rows
            },
            f.width,
            false,
            move |this: &mut V, cx| toggle(this, Menu::Model, cx),
            move |this: &mut V, cx| close(this, cx),
            cx,
        )
        .w_full()
    };
    let model_help: SharedString = f.model_note.clone().unwrap_or_else(|| f.help.to_string()).into();
    let model_picker = match f.custom {
        Some(custom) => div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(model_picker)
            .child(custom.render(f.host.custom_model(), window, cx)),
        None => model_picker,
    };

    let field = |text: &'static str, picker: Div, help: SharedString| {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(label(text, theme))
            .child(picker)
            .child(footnote(help, theme))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(24.0))
        .w_full()
        .child(field(f.harness_label, harness_picker, f.help.into()))
        .child(field(f.model_label, model_picker, model_help))
}
