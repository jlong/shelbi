//! New task (`new-task-dialog`, Figma "New task · Dark" `2730:10845`): the
//! dialog the board's New issue and `n`, File ▸ New Issue… and the
//! palette's New Issue open over the window. From the frame, top to bottom
//! on the canvas panel (680pt, 32pt in, 24pt between its parts, 12pt
//! corners, the divider's edge) over the scrim:
//!
//! - "New task" (Title, 24 / 32 semibold, loud) and "Add work for your
//!   Orchestrator and agents." (Base) 8pt under it;
//! - Title: its label (Base Medium, loud) over a 40pt Textbox;
//! - Workflow, Status and Priority, three Base Default buttons with the
//!   dropdown chevron across the row (12pt apart), Status with its status
//!   glyph in the Left Icon slot; each opens a menu under it;
//! - Description: a 152pt multi-line box (12pt in, the divider's edge);
//! - the write-up card (the Default fill, 16pt in, 12pt corners): "Let
//!   Orchestrator write this up" (Base Medium, loud), its help (Small,
//!   muted), and the Switch (36 × 22) at its right;
//! - the footer: a divider, then "⌘ Return to create" (X-Small, muted),
//!   Cancel (Large Default) and Create Task (Large Primary, disabled while
//!   the title is blank, "Creating…" while it's out).
//!
//! [`NewTaskForm`] (shared with the TUI's dialog, in `shelbi_app::new_task`)
//! holds the draft and decides everything, the words included; this view
//! paints its model, keeps the two text fields in step with it, and runs the
//! create through [`NewTaskRunner`] on the background executor. A create
//! that worked closes the dialog and the window flashes its outcome
//! ([`NewTaskEvent::Created`]); a failure shows over the footer, the draft
//! kept.
//!
//! Keys, taken ahead of the text fields: Tab / ⇧Tab walk the controls in
//! visual order and never leave the dialog; ⌘Return creates from anywhere;
//! Return creates from Title, opens a picker, toggles the switch or presses
//! the focused button, and in Description is a new line; Esc closes an open
//! menu, else cancels; ↑ / ↓ and Return pick in an open menu; Space opens a
//! picker, toggles the switch or presses a button. The focus ring shows once
//! the keyboard is used.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    Role, SharedString, Stateful, Subscription, Toggled, Window,
};
use gpui_kit::component::input::{InputEvent, InputState, Textarea, TextareaState};
use shelbi_app::new_task::{self as model, Action, CreateRequest, Created, Field, Key, Menu, NewTaskForm};
use shelbi_app::source::ProjectLink;
use shelbi_app::view::{NewTaskModel, NewTaskOption};
use shelbi_core::Workflow;
use shelbi_state::keymap::{Key as Code, KeyChord, Mods};

use super::workflow_editor::{dialog_text_field, dropdown, error_line};
use crate::elements::{controls, spinner};
use crate::keys::chord_from_keystroke;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The panel's width, inset and corners (the frame's).
pub const DIALOG_W: f32 = 680.0;
const PADDING: f32 = 32.0;
const RADIUS: f32 = 12.0;
/// The Title field's height (the frame's 40pt Textbox).
const TITLE_H: f32 = 40.0;
/// The Description box's height.
const DESCRIPTION_H: f32 = 152.0;
/// A picker menu's width: its button's (the row's three share 616pt, 12pt
/// apart).
const MENU_W: f32 = (DIALOG_W - 2.0 * PADDING - 2.0 * 12.0) / 3.0;

/// Runs a create through the window's link (the project's daemon's add,
/// then the hand-off to its orchestrator, local or over the host's relay),
/// blocking: [`model::create_linked`], or a test's recorder.
pub type NewTaskRunner = Arc<dyn Fn(&ProjectLink, &CreateRequest) -> Result<Created, String> + Send + Sync>;

/// The [`NewTaskRunner`] the dialog creates through, as a gpui global when a
/// test sets one; otherwise the live one.
#[derive(Clone)]
pub struct NewTaskGlobal(pub NewTaskRunner);

impl gpui::Global for NewTaskGlobal {}

/// What the dialog tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewTaskEvent {
    /// Cancelled: nothing was created.
    Closed,
    /// Created: the line the window flashes ([`Created::message`]).
    Created(String),
}

/// The open dialog.
pub struct NewTaskDialog {
    link: ProjectLink,
    form: NewTaskForm,
    /// The dialog's own focus: the pickers, the switch and the buttons
    /// paint their ring from it.
    focus: FocusHandle,
    title_input: Entity<InputState>,
    description_input: Entity<TextareaState>,
    /// Where an open menu hangs from (its button's bounds, probed at paint).
    menu_anchor: style::Anchor,
    /// The keyboard has moved focus: rings show from then on.
    keyboard: bool,
    /// The text field the window's focus was in at the last render, so a
    /// field newly focused (a click into it) takes the form's focus, and one
    /// left focused under a picker's click doesn't take it back.
    text_focus: Option<Field>,
    runner: NewTaskRunner,
    _subscriptions: Vec<Subscription>,
}

impl NewTaskDialog {
    /// A blank draft for the project `link` reaches, over its `workflows`, on
    /// `default_workflow`, created through the [`NewTaskGlobal`] runner when
    /// one is set (a test's), else the live one.
    pub fn new(
        link: ProjectLink,
        workflows: &[Workflow],
        default_workflow: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let runner = cx.try_global::<NewTaskGlobal>().map(|g| g.0.clone()).unwrap_or_else(fallback_runner);
        let form = NewTaskForm::new(workflows, default_workflow);
        let focus = cx.focus_handle();
        let title_input = cx.new(|cx| InputState::new(window, cx));
        let description_input = cx.new(|cx| TextareaState::new(window, cx));
        let this_window = window.window_handle();
        let me = cx.weak_entity();
        let mut subscriptions = vec![
            // Ahead of the text fields' own bindings: Tab, Return and Esc
            // are the dialog's everywhere in it.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != this_window {
                    return;
                }
                let Some(chord) = chord_from_keystroke(&event.keystroke) else {
                    return;
                };
                if me.update(cx, |this, cx| this.key(chord, window, cx)).unwrap_or(false) {
                    cx.stop_propagation();
                }
            }),
        ];
        subscriptions.push(cx.subscribe_in(&title_input, window, |this, input, event: &InputEvent, _, cx| {
            match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.form.set_title(&text);
                }
                InputEvent::Focus => this.form.set_focus(Field::Title),
                _ => return,
            }
            cx.notify();
        }));
        subscriptions.push(cx.subscribe_in(&description_input, window, |this, input, event: &InputEvent, _, cx| {
            match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.form.set_description(&text);
                }
                InputEvent::Focus => this.form.set_focus(Field::Description),
                _ => return,
            }
            cx.notify();
        }));
        let mut dialog = NewTaskDialog {
            link,
            form,
            focus,
            title_input,
            description_input,
            menu_anchor: Default::default(),
            keyboard: false,
            text_focus: None,
            runner,
            _subscriptions: subscriptions,
        };
        dialog.sync_focus(window, cx);
        dialog
    }

    #[cfg(test)]
    pub fn form(&self) -> &NewTaskForm {
        &self.form
    }

    /// The text fields (tests type into them as the user would).
    #[cfg(test)]
    pub fn inputs(&self) -> (Entity<InputState>, Entity<TextareaState>) {
        (self.title_input.clone(), self.description_input.clone())
    }

    /// Fill the draft as the frame shows it (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_fill(&mut self, title: &str, description: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.title_input.update(cx, |s, cx| s.set_value(title.to_string(), window, cx));
        self.description_input.update(cx, |s, cx| s.set_value(description.to_string(), window, cx));
        self.form.set_title(title);
        self.form.set_description(description);
        self.form.set_focus(Field::Title);
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// Switch the write-up on (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_write_up(&mut self, cx: &mut Context<Self>) {
        self.form.toggle_write_up();
        cx.notify();
    }

    /// Close without creating (the window's ⌘W), unless a create is out.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if !self.form.creating() {
            cx.emit(NewTaskEvent::Closed);
        }
    }

    /// A key the dialog takes. `false` lets it through to the focused text
    /// field.
    pub(crate) fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let bare = chord.mods == Mods::NONE;
        let focus = self.form.focus();
        let menu_open = self.form.menu().is_some();
        let in_text = !menu_open && focus.is_text();
        let key = match chord.code {
            Code::Tab if bare => Key::Tab,
            Code::Tab | Code::BackTab if chord.mods == Mods::SHIFT || chord.code == Code::BackTab => Key::BackTab,
            Code::Enter if chord.mods == Mods::SUPER => Key::CmdEnter,
            // Return in the Description is its new line.
            Code::Enter if bare && in_text && focus == Field::Description => return false,
            Code::Enter if bare => Key::Enter,
            Code::Esc if bare => Key::Esc,
            Code::Char(' ') if bare && !in_text => Key::Space,
            Code::Up if bare && !in_text => Key::Up,
            Code::Down if bare && !in_text => Key::Down,
            _ => return false,
        };
        if matches!(key, Key::Tab | Key::BackTab | Key::Space | Key::Up | Key::Down) {
            self.keyboard = true;
        }
        let action = self.form.key(key);
        self.act(action, window, cx);
        true
    }

    fn press(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let action = self.form.press(field);
        self.act(action, window, cx);
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Stay => {}
            Action::Create(request) => self.create(request, window, cx),
            Action::Close => {
                cx.emit(NewTaskEvent::Closed);
                return;
            }
        }
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// Run `request` off the UI thread; the answer comes back to the form.
    /// A dialog closed meanwhile (its window gone or switched to another
    /// project) drops it.
    fn create(&mut self, request: CreateRequest, window: &mut Window, cx: &mut Context<Self>) {
        let runner = self.runner.clone();
        let link = self.link.clone();
        let work = cx.background_executor().spawn(async move { runner(&link, &request) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| this.created(result, window, cx)).ok();
        })
        .detach();
    }

    fn created(&mut self, result: Result<Created, String>, window: &mut Window, cx: &mut Context<Self>) {
        let message = result.as_ref().ok().map(Created::message);
        if let Err(e) = &result {
            tracing::warn!("new task wasn't created: {e}");
        }
        match self.form.finished(result.map(|_| ())) {
            Action::Close => cx.emit(NewTaskEvent::Created(message.unwrap_or_default())),
            _ => {
                self.sync_focus(window, cx);
                cx.notify();
            }
        }
    }

    /// Give the keyboard to the focused control: a text field's input, else
    /// the dialog (which paints the ring on the rest).
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.form.focus() {
            _ if self.form.menu().is_some() => self.focus.clone(),
            Field::Title => self.title_input.read(cx).focus_handle(cx),
            Field::Description => self.description_input.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    /// Follow the keyboard into a text field however it got there (a
    /// click, the window handing focus back): the form's focus is where
    /// Tab goes on from.
    fn follow_focus(&mut self, window: &mut Window, cx: &mut App) {
        let fields = [
            (Field::Title, self.title_input.read(cx).focus_handle(cx)),
            (Field::Description, self.description_input.read(cx).focus_handle(cx)),
        ];
        let now = fields.iter().find(|(_, h)| h.is_focused(window)).map(|(f, _)| *f);
        if now != self.text_focus {
            self.text_focus = now;
            if let Some(field) = now.filter(|f| *f != self.form.focus()) {
                self.form.set_focus(field);
            }
        }
        // An open menu has the keyboard (its ↑ ↓ Return), not a text field
        // left focused under the click that opened it.
        if self.form.menu().is_some() && !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
            self.text_focus = None;
        }
    }

    /// Whether `field`'s ring shows.
    fn ring(&self, field: Field, window: &Window) -> bool {
        self.keyboard && self.focus.is_focused(window) && self.form.focus() == field && self.form.menu().is_none()
    }
}

impl EventEmitter<NewTaskEvent> for NewTaskDialog {}

/// The live runner, or in a test that set none, one that never reaches the
/// daemon.
fn fallback_runner() -> NewTaskRunner {
    #[cfg(not(test))]
    {
        Arc::new(|link: &ProjectLink, request: &CreateRequest| model::create_linked(link, request))
    }
    #[cfg(test)]
    {
        Arc::new(|_: &ProjectLink, _: &CreateRequest| Err("no daemon in tests".to_string()))
    }
}

/// A field's label (Base Medium, loud).
fn label(text: &'static str, theme: &Theme) -> Div {
    div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(text)
}

/// A field: its label over its control, 8pt apart.
fn field(text: &'static str, control: impl IntoElement, theme: &Theme) -> Div {
    div().flex().flex_col().gap(px(8.0)).w_full().min_w_0().child(label(text, theme)).child(control)
}

impl NewTaskDialog {
    fn render_heading(&self, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(
                div()
                    .debug_selector(|| "new-task-title".into())
                    .type_style(Type::Title)
                    .text_color(theme.text_loud)
                    .child(model::TITLE),
            )
            .child(div().type_style(Type::Base).text_color(theme.foreground).child(model::SUBTITLE))
    }

    /// Workflow, Status and Priority across the row.
    fn render_pickers(&self, m: &NewTaskModel, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        let picker = |menu: Menu, id: &'static str, value: &str, options: &[NewTaskOption], this: &Self, cx: &mut Context<Self>| {
            let field = menu.field();
            let focused = this.ring(field, window);
            let base = if m.creating {
                style::disabled_button_base(id, ButtonKind::Secondary, ButtonSize::Base, theme)
            } else {
                style::button_base(id, ButtonKind::Secondary, ButtonSize::Base, focused, theme)
            };
            let name = match menu {
                Menu::Workflow => model::WORKFLOW_LABEL,
                Menu::Status => model::STATUS_LABEL,
                Menu::Priority => model::PRIORITY_LABEL,
            };
            // Status's glyph sits in the Left Icon slot, ahead of the label;
            // the chevron in the Right Icon slot at the far side.
            let glyph = (menu == Menu::Status)
                .then(|| style::left_slot(ButtonSize::Base, style::status_icon_large(m.status_category, theme)));
            let value = value.to_string();
            let button = base
                .w_full()
                .role(Role::ComboBox)
                .aria_label(SharedString::from(format!("{name}: {value}")))
                .children(glyph)
                .child(
                    div()
                        .debug_selector(move || format!("{id}-value"))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(value),
                )
                .child(style::dropdown_slot(ButtonSize::Base, theme.button_icon));
            if m.creating {
                return div().w_full().child(button);
            }
            let open = m.menu.is_some_and(|(o, _)| o == menu);
            let cursor = m.menu.map(|(_, c)| c);
            let rows: Vec<(usize, NewTaskOption)> = options.iter().cloned().enumerate().collect();
            dropdown(
                open,
                &this.menu_anchor,
                match menu {
                    Menu::Workflow => "new-task-workflow-menu",
                    Menu::Status => "new-task-status-menu",
                    Menu::Priority => "new-task-priority-menu",
                },
                button,
                move |cx| {
                    let theme = cx.global::<Theme>().opaque();
                    rows.into_iter()
                        .map(|(i, o)| menu_row(menu, i, &o, cursor == Some(i), &theme, cx))
                        .collect()
                },
                MENU_W,
                false,
                move |this: &mut Self, cx| {
                    this.form.toggle_menu(menu);
                    cx.notify();
                },
                |this: &mut Self, _| this.form.close_menu(),
                cx,
            )
            .w_full()
        };
        let workflow = picker(Menu::Workflow, "new-task-workflow", &m.workflow, &m.workflows, self, cx);
        let status = picker(Menu::Status, "new-task-status", &m.status, &m.statuses, self, cx);
        let priority = picker(Menu::Priority, "new-task-priority", &m.priority, &m.priorities, self, cx);
        div()
            .flex()
            .flex_row()
            .gap(px(12.0))
            .w_full()
            .child(div().flex_1().min_w_0().child(field(model::WORKFLOW_LABEL, workflow, theme)))
            .child(div().flex_1().min_w_0().child(field(model::STATUS_LABEL, status, theme)))
            .child(div().flex_1().min_w_0().child(field(model::PRIORITY_LABEL, priority, theme)))
    }

    /// The Description: the multi-line box, the divider's edge at rest and
    /// the focus ring with its glow while typed in, the placeholder while
    /// it's empty.
    fn render_description(&self, m: &NewTaskModel, theme: &Theme, window: &Window, cx: &App) -> Div {
        let focused = self.description_input.read(cx).focus_handle(cx).is_focused(window);
        let edge = if focused { theme.focus_ring } else { theme.divider };
        div()
            .debug_selector(|| "new-task-description".into())
            .relative()
            .flex()
            .flex_none()
            .w_full()
            .h(px(DESCRIPTION_H))
            .p(px(1.0))
            .rounded(px(6.0))
            .bg(edge)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    // The text area's own inset (about 9pt across, 8pt down)
                    // makes up the rest of the frame's 12pt.
                    .px(px(3.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .bg(theme.textbox)
                    .type_style(Type::Base)
                    .text_color(theme.form_text)
                    .child(
                        Textarea::new(&self.description_input)
                            .appearance(false)
                            .h_full()
                            .text_size(px(14.0))
                            .line_height(px(18.0)),
                    )
                    .when(m.description.is_empty(), |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(12.0))
                                .left(px(12.0))
                                .type_style(Type::Base)
                                .text_color(theme.form_hint)
                                .child(model::DESCRIPTION_PLACEHOLDER),
                        )
                    }),
            )
            .when(focused, |d| d.children(style::focus_bands(6.0, theme)))
    }

    /// "Let Orchestrator write this up": the card, its label and help, and
    /// the Switch. A click anywhere on it toggles.
    fn render_write_up(&self, m: &NewTaskModel, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = m.write_up;
        div()
            .id("new-task-write-up")
            .debug_selector(|| "new-task-write-up".into())
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .w_full()
            .p(px(16.0))
            .rounded(px(RADIUS))
            .bg(theme.background)
            .cursor_pointer()
            .role(Role::Switch)
            .aria_label(model::WRITE_UP)
            .aria_toggled(if on { Toggled::True } else { Toggled::False })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(model::WRITE_UP))
                    .child(div().type_style(Type::Small).text_color(theme.muted).child(model::WRITE_UP_HELP)),
            )
            .child(controls::switch(on, theme))
            .when(self.ring(Field::WriteUp, window), |d| d.children(style::focus_bands(RADIUS, theme)))
            .when(!m.creating, |d| {
                d.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.form.toggle_write_up();
                    this.act(Action::Stay, window, cx);
                }))
            })
    }

    fn render_footer(&self, m: &NewTaskModel, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        let cancel = if m.creating {
            style::disabled_button("new-task-cancel", model::CANCEL, ButtonKind::Secondary, ButtonSize::Large, theme)
        } else {
            style::button(
                "new-task-cancel",
                model::CANCEL,
                ButtonKind::Secondary,
                ButtonSize::Large,
                self.ring(Field::Cancel, window),
                theme,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.press(Field::Cancel, window, cx)))
        }
        .role(Role::Button)
        .aria_label(model::CANCEL)
        .aria_keyshortcuts("Escape");
        let create = if m.creating {
            style::pending_button(
                "new-task-create",
                model::CREATING,
                spinner::spinner_sized("new-task-creating", theme.primary_label, 16.0),
                ButtonKind::Primary,
                ButtonSize::Large,
                theme,
            )
        } else if !m.can_create {
            style::disabled_button("new-task-create", model::CREATE, ButtonKind::Primary, ButtonSize::Large, theme)
        } else {
            style::button(
                "new-task-create",
                model::CREATE,
                ButtonKind::Primary,
                ButtonSize::Large,
                self.ring(Field::Create, window),
                theme,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.press(Field::Create, window, cx)))
        }
        .role(Role::Button)
        .aria_label(model::CREATE)
        .aria_keyshortcuts("Meta+Enter");
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .w_full()
            .child(div().flex_none().h(px(1.0)).w_full().bg(theme.divider))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .w_full()
                    .child(
                        div()
                            .debug_selector(|| "new-task-hint".into())
                            .flex_1()
                            .min_w_0()
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .child(model::CREATE_HINT),
                    )
                    .child(cancel)
                    .child(create),
            )
    }
}

/// A picker menu's row: a status's glyph before its label, the chosen
/// row's selection fill on the keyboard's row.
fn menu_row(menu: Menu, i: usize, option: &NewTaskOption, lit: bool, theme: &Theme, cx: &mut Context<NewTaskDialog>) -> AnyElement {
    let id = format!(
        "new-task-{}-{i}",
        match menu {
            Menu::Workflow => "workflow",
            Menu::Status => "status",
            Menu::Priority => "priority",
        }
    );
    style::menu_row(id, lit, theme)
        .role(Role::MenuItemRadio)
        .aria_label(SharedString::from(option.label.clone()))
        .aria_selected(option.selected)
        .children(option.category.map(|c| style::status_icon(c, theme)))
        .child(div().flex_1().min_w_0().truncate().child(option.label.clone()))
        .when(option.selected, |d| d.child(crate::assets::check_mark().text_color(theme.text_loud)))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            cx.stop_propagation();
            this.form.choose(menu, i);
            this.act(Action::Stay, window, cx);
        }))
        .into_any_element()
}

impl Focusable for NewTaskDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for NewTaskDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A dialog: opaque over a translucent window, its fields too.
        let theme = cx.global::<Theme>().opaque();
        self.follow_focus(window, cx);
        let m = self.form.model();
        let title = dialog_text_field(
            &self.title_input,
            "new-task-title-field",
            Some(model::TITLE_PLACEHOLDER.into()),
            false,
            window,
            cx,
        )
        .h(px(TITLE_H));
        let details = div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .w_full()
            .child(field(model::TITLE_LABEL, title, &theme))
            .child(self.render_pickers(&m, &theme, window, cx))
            .child(field(model::DESCRIPTION_LABEL, self.render_description(&m, &theme, window, cx), &theme));
        let panel = div()
            .id("new-task-dialog")
            .debug_selector(|| "new-task-dialog".into())
            .role(Role::Dialog)
            .aria_label(model::TITLE)
            .flex()
            .flex_col()
            .gap(px(24.0))
            .w(px(DIALOG_W))
            .p(px(PADDING))
            .rounded(px(RADIUS))
            .border_1()
            .border_color(theme.divider)
            .bg(theme.canvas)
            .text_color(theme.form_text)
            .occlude()
            .child(self.render_heading(&theme))
            .child(details)
            .child(self.render_write_up(&m, &theme, window, cx))
            .children(m.error.as_deref().map(|e| error_line("new-task-error".into(), e, &theme)))
            .child(self.render_footer(&m, &theme, window, cx));
        div()
            .id("new-task-layer")
            .track_focus(&self.focus)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .font_family("Geist")
            .type_style(Type::Base)
            .child(
                div()
                    .id("new-task-scrim")
                    .debug_selector(|| "new-task-scrim".into())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.scrim)
                    .occlude()
                    .child(panel),
            )
    }
}

