//! Settings ▸ Wiki (`wiki-settings`): where the project's wiki lives and
//! where its sync stands, over the shared
//! [`WikiForm`](shelbi_app::settings::wiki::WikiForm) the TUI's Wiki page
//! draws too.
//!
//! ```text
//!  Wiki
//!  Where this project's wiki lives, and whether Shelbi syncs it on its own.
//!
//!  Source     (•) Orphan branch  ( ) Repo folder
//!  Branch     [ docs/planning              ]
//!  Auto-sync  [x]
//!  Status     ● Synced · 2m ago   [ Sync now ]
//! ```
//!
//! The project's config is read off the UI thread through the window's
//! config store (the project's daemon, here or on the far machine), and the
//! status from that machine's hub wiki service through the window's link
//! ([`shelbi_app::wiki::ask`]); both are read again each time the page is
//! shown. Once the project has a wiki the radio and the checkbox save as
//! they're picked and the field when it's committed (Return, or focus
//! leaving it), as the Properties page's do; before, they're Set up wiki's
//! choices, and nothing is written until it's pressed.

use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Context, Entity, FontWeight, Hsla, Role, SharedString, Subscription, Toggled, Window};
use gpui_kit::component::input::{InputEvent, InputState};
use shelbi_app::settings::orchestrator::SaveStatus;
use shelbi_app::settings::store::Store;
use shelbi_app::settings::wiki::{
    self as wk, Ask, Tone, WikiDone, WikiForm, AUTO_SYNC_HELP, AUTO_SYNC_LABEL, HELP, SOURCES, SOURCE_LABEL,
    STATUS_LABEL, TITLE,
};
use shelbi_app::source::ProjectHandle;
use shelbi_core::WikiSource;

use super::workflow_editor::text_field;
use crate::elements::controls;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The page title's size (Desktop/Heading, as the Properties page's).
const TITLE_SIZE: f32 = 18.0;

/// A source's radio row's selector.
pub fn source_selector(source: WikiSource) -> &'static str {
    match source {
        WikiSource::Branch => "wiki-source-branch",
        WikiSource::Folder => "wiki-source-folder",
    }
}

/// The Wiki page.
pub struct WikiSettingsPage {
    form: WikiForm,
    store: Store,
    ask: Ask,
    input: Entity<InputState>,
    /// Re-read the page on the next draw (it was shown again).
    reload: bool,
    _subscriptions: Vec<Subscription>,
}

impl WikiSettingsPage {
    pub fn new(handle: &ProjectHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => this.typed(cx),
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit(window, cx),
                _ => {}
            }
        })];
        let link = handle.link.clone();
        let mut page = WikiSettingsPage {
            form: WikiForm::default(),
            store: std::sync::Arc::new(handle.clone()),
            ask: std::sync::Arc::new(move |req| shelbi_app::wiki::ask(&link, req)),
            input,
            reload: false,
            _subscriptions: subscriptions,
        };
        // A test's page never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.form.visit();
            page.run(window, cx);
        }
        page
    }

    /// Read the page again when next drawn (it was shown again). A test's
    /// page reads nothing unless told to.
    pub fn reload_when_drawn(&mut self) {
        self.reload = !cfg!(test);
    }

    /// Read through `store` and ask `ask` instead (a test's).
    #[cfg(test)]
    pub fn set_ports(&mut self, store: Store, ask: Ask) {
        self.store = store;
        self.ask = ask;
    }

    /// Read the config and the hub's status now (a test's first visit).
    #[cfg(test)]
    pub fn visit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form.visit();
        self.run(window, cx);
    }

    #[cfg(test)]
    pub fn form(&self) -> &WikiForm {
        &self.form
    }

    #[cfg(test)]
    pub fn input(&self) -> Entity<InputState> {
        self.input.clone()
    }

    /// Run the form's waiting work off the UI thread, in order, handing each
    /// result back as it lands.
    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.form.work_due() {
            return;
        }
        let generation = self.form.generation();
        let work = self.form.take_work();
        let (store, ask) = (self.store.clone(), self.ask.clone());
        cx.spawn_in(window, async move |this, cx| {
            for item in work {
                let (store, ask) = (store.clone(), ask.clone());
                let done: WikiDone = cx
                    .background_executor()
                    .spawn(async move { item.run(store.as_ref(), &ask) })
                    .await;
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        this.form.done(generation, done);
                        this.fill(window, cx);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
            // A result may queue more (a save reads the status again).
            this.update_in(cx, |this, window, cx| this.run(window, cx)).ok();
        })
        .detach();
    }

    /// Show the form's text in the field when it doesn't hold it already.
    fn fill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.form.text();
        if self.input.read(cx).value() != text.as_str() {
            self.input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    /// The field's text changed.
    fn typed(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text != self.form.text() {
            self.form.set_text(&text);
            cx.notify();
        }
    }

    /// Check the field and save it when it changed.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // What the field holds now (a paste or a programmatic set sends no
        // Change).
        self.typed(cx);
        self.form.commit_text();
        self.after(window, cx);
    }

    fn after(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fill(window, cx);
        self.run(window, cx);
        cx.notify();
    }

    pub fn choose_source(&mut self, source: WikiSource, window: &mut Window, cx: &mut Context<Self>) {
        self.form.choose_source(source);
        self.after(window, cx);
    }

    pub fn toggle_auto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form.toggle_auto();
        self.after(window, cx);
    }

    /// Sync now, or Set up wiki.
    pub fn press(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // What's typed goes with it.
        self.typed(cx);
        self.form.press();
        self.after(window, cx);
    }
}

/// A radio row: the radio, the source's label over its description.
fn radio_row(source: WikiSource, on: bool, enabled: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let id = source_selector(source);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(Role::RadioButton)
        .aria_label(wk::source_label(source))
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
                .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(wk::source_label(source)))
                .child(div().type_style(Type::Small).text_color(theme.form_hint).child(wk::source_help(source))),
        )
}

/// A tone's color.
fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Good => theme.icon_green,
        Tone::Busy => theme.icon_yellow,
        Tone::Muted => theme.form_hint,
        Tone::Bad => theme.error,
    }
}

impl Render for WikiSettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if std::mem::take(&mut self.reload) {
            self.form.visit();
            self.run(window, cx);
        }
        let f = &self.form;
        let editable = f.editable();
        let line = |selector: &'static str, text: String, color: Hsla| {
            div()
                .debug_selector(move || selector.into())
                .type_style(Type::XSmall)
                .text_color(color)
                .child(text)
        };
        let unreadable = f
            .unreadable()
            .map(|e| line("wiki-unreadable", format!("The project's settings don't read: {e}"), theme.error));
        let block_unreadable = f.block_unreadable().map(|e| {
            line(
                "wiki-block-unreadable",
                format!("The project's wiki: block doesn't read ({e}). A change here replaces it."),
                theme.error,
            )
        });

        // Source.
        let current = f.source();
        let radios = SOURCES.map(|source| {
            radio_row(source, source == current, editable, &theme).when(editable, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose_source(source, window, cx)))
            })
        });
        let source_group = div()
            .id("wiki-source")
            .debug_selector(|| "wiki-source".into())
            .role(Role::RadioGroup)
            .aria_label(SOURCE_LABEL)
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(SharedString::from(SOURCE_LABEL)))
            .children(radios);

        // The branch or the folder.
        let placeholder = Some(SharedString::from(wk::placeholder(current)));
        let field = text_field(&self.input, "wiki-location", placeholder, f.error().is_some(), window, cx);
        let help: SharedString = wk::location_help(current).into();
        let location: AnyElement = controls::labeled_control(
            wk::location_label(current),
            div().w(px(controls::CONTROL_W)).max_w_full().when(!editable, |d| d.opacity(0.6)).child(field),
            Some(help),
            &theme,
        )
        .id("wiki-location-row")
        .debug_selector(|| "wiki-location-row".into())
        .role(Role::Group)
        .aria_label(wk::location_label(current))
        .children(f.error().map(|e| line("wiki-location-error", e.to_string(), theme.error)))
        .into_any_element();

        // Auto-sync.
        let auto = controls::checkbox("wiki-auto-sync", AUTO_SYNC_LABEL, f.auto(), false, &theme)
            .role(Role::CheckBox)
            .aria_label(AUTO_SYNC_LABEL)
            .aria_toggled(if f.auto() { Toggled::True } else { Toggled::False })
            .when(!editable, |d| d.opacity(0.6))
            .when(editable, |d| {
                d.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_auto(window, cx)))
            });
        let auto_group = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(auto)
            .child(controls::helper(AUTO_SYNC_HELP, &theme).pl(px(26.0)));

        // Status, and its button.
        let status = f.status_line(chrono::Utc::now());
        let color = tone_color(status.tone, &theme);
        let (button, enabled) = f.button();
        let action = style::button("wiki-sync-button", button.label(), ButtonKind::Secondary, ButtonSize::Small, false, &theme)
            .role(Role::Button)
            .aria_label(button.label())
            .when(!enabled, |d| d.opacity(0.6))
            .when(enabled, |d| d.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.press(window, cx))));
        let status_row = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(div().type_style(Type::Base).text_color(theme.text_loud).child(STATUS_LABEL))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .w_full()
                    .child(
                        div()
                            .id("wiki-status")
                            .debug_selector(|| "wiki-status".into())
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .min_w_0()
                            .child(div().flex_none().size(px(8.0)).rounded_full().bg(color))
                            .child(div().type_style(Type::Base).text_color(theme.form_text).truncate().child(status.text)),
                    )
                    .child(action),
            )
            .children(f.root().map(|r| line("wiki-root", format!("At {r}"), theme.form_hint)))
            .children(f.pull_request().map(|pr| line("wiki-pull-request", format!("Pull request: {pr}"), theme.form_hint)))
            .children(f.sync_error().map(|e| line("wiki-sync-error", e.to_string(), theme.error)));

        let save_line = f.status_message().map(|text| {
            let color = if matches!(f.status(), SaveStatus::Failed(_)) { theme.error } else { theme.form_hint };
            line("wiki-save-status", text, color)
        });
        let loaded = f.config().is_some();
        div()
            .id("settings-wiki")
            .debug_selector(|| "settings-wiki".into())
            .flex()
            .flex_col()
            .w_full()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .type_style(Type::Heading)
                            .text_size(px(TITLE_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_loud)
                            .child(TITLE),
                    )
                    .child(div().type_style(Type::Small).text_color(theme.form_hint).child(HELP)),
            )
            .children(unreadable)
            .children(block_unreadable)
            .when(loaded, |d| {
                d.child(source_group)
                    .child(location)
                    .child(auto_group)
                    .child(status_row)
                    .children(save_line)
            })
    }
}
