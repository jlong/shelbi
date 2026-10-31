//! The New workspace and Edit workspace dialogs, and the removal
//! confirmation (`dt-workspace-settings-forms`; Figma "Settings · Workspaces
//! · New workspace" 2528:9683 and "Edit workspace" 2526:9184).
//!
//! Settings ▸ Workspaces asks for them ([`Intent`]); the window holds the
//! open one over everything, on the scrim, the 600pt panel centered over the
//! main area as the frames place it. Top to bottom: the title and its line;
//! Name and Machine side by side; Tags (the workspace's own, with the
//! machine's inherited ones said under it); Worktree location, the derived
//! path in the disabled Textbox (read-only, selectable and copyable); Slot
//! (optional), empty for Automatic; a rule; then Remove workspace at the
//! left (Edit only) and Cancel and the default action at the right.
//!
//! [`WorkspaceForm`] holds the draft and decides everything; this view
//! paints it, keeps the text fields and the keyboard in step with it, and
//! runs its jobs through the shared service ([`PoolService`], the
//! `pool_admin` operations) on the background executor. While one runs, the
//! default action shows a spinner beside it and nothing can send another or
//! close the dialog; the result closes it ([`WorkspaceDialogEvent::Closed`]
//! with the outcome) or shows beside the field it concerns, the values kept.
//!
//! Keys, taken ahead of the text fields: Tab / ⇧Tab move through the
//! dialog's controls and never leave it; Return is the default action (on
//! Cancel or Remove workspace, that button); Space presses the focused
//! button or opens the Machine picker, whose ↑ / ↓ and Return pick a
//! machine; Esc closes the picker, else the dialog (discarding the draft).
//! The focus ring shows once the keyboard is used.

use std::path::PathBuf;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, Global, MouseDownEvent, Role, SharedString, Stateful, Subscription, Window,
};
use gpui_kit::component::input::{InputEvent, InputState};
use shelbi_state::keymap::{Key as Code, KeyChord, Mods};

use super::workflow_editor::{dialog_text_field, readonly_field};
use shelbi_app::settings::workspace_form::{
    Action, ConfirmFocus, Focus, Job, Key, MachineChoice, Outcome, PoolService, RemoveConfirm, WorkspaceForm,
    SLOT_HELP, TAGS_HELP,
};
use super::workspaces::WorkspacesPage;
use shelbi_app::settings::workspaces::{Intent, WorkspacesState};
use crate::elements::placed::{placed, MenuScroll};
use crate::elements::spinner;
use crate::keys::chord_from_keystroke;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The form's width (the frames').
pub const DIALOG_W: f32 = 600.0;
/// The confirmation's width.
const CONFIRM_W: f32 = 480.0;
/// The panels' inset and corners (`radius/default`).
const PADDING: f32 = 32.0;
const RADIUS: f32 = 12.0;
/// The dialog title's size (Desktop/Heading, 18 / 28 semibold).
const TITLE_SIZE: f32 = 18.0;

/// The service the dialogs run their jobs through: the live one unless a
/// test (or the visual harness) sets its own.
#[derive(Clone)]
pub struct DialogService(pub PoolService);

impl Global for DialogService {}

/// What the dialog tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDialogEvent {
    /// Close it. `done`: what the job did (then the pool is read again);
    /// `None`: cancelled, the draft discarded.
    Closed { done: Option<Outcome> },
}

/// What the dialog shows.
pub enum Body {
    Form(Box<WorkspaceForm>),
    /// The row menu's Remove workspace…, on its own.
    Remove(RemoveConfirm),
}

/// The open dialog.
pub struct WorkspaceDialog {
    body: Body,
    /// The dialog's own focus: the buttons and the picker paint their ring
    /// from it.
    focus: FocusHandle,
    name_input: Entity<InputState>,
    tags_input: Entity<InputState>,
    location_input: Entity<InputState>,
    slot_input: Entity<InputState>,
    /// The keyboard has moved focus: rings show from then on.
    keyboard: bool,
    /// How far from the window's left edge the main area starts: the panel
    /// centers over it.
    inset: f32,
    service: PoolService,
    /// The job out, if any (a test waits on it).
    running: bool,
    /// The Machine picker's button, measured so its menu opens against it,
    /// and the menu's scroll, following the keyboard.
    machine_anchor: style::Anchor,
    machine_scroll: MenuScroll,
    _subscriptions: Vec<Subscription>,
}

/// The page's machines as the picker lists them.
fn machine_choices(state: &WorkspacesState) -> Vec<MachineChoice> {
    state
        .index()
        .map(|i| i.machines.iter().map(|m| MachineChoice::from_row(m, state.status(m))).collect())
        .unwrap_or_default()
}

impl Body {
    /// What `intent` asks for, over the page's last read. `None` for a
    /// machine intent, before the page has read the pool, or for a workspace
    /// no longer declared.
    pub fn for_intent(intent: &Intent, page: &Entity<WorkspacesPage>, cx: &App) -> Option<Body> {
        let (project, source, machines) = {
            let state = page.read(cx).state();
            let source = state.index()?.source.clone()?.0;
            (state.project().to_string(), source, machine_choices(state))
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Some(match intent {
            Intent::NewWorkspace => Body::Form(Box::new(WorkspaceForm::new_workspace(&project, source, machines, home))),
            Intent::EditWorkspace(name) => {
                Body::Form(Box::new(WorkspaceForm::edit_workspace(&project, source, machines, home, name)?))
            }
            Intent::RemoveWorkspace(name) => {
                Body::Remove(RemoveConfirm::new(&project, &source, name, home.as_deref())?)
            }
            Intent::AddMachine | Intent::ConfigureMachine(_) => return None,
        })
    }
}

impl WorkspaceDialog {
    /// The dialog showing `body` (from [`Body::for_intent`]) over `page`.
    pub fn new(body: Body, page: Entity<WorkspacesPage>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let service = cx
            .try_global::<DialogService>()
            .map(|s| s.0.clone())
            .unwrap_or_else(shelbi_orchestrator::pool_admin::workspace_service);
        let focus = cx.focus_handle();
        let input = |text: &str, window: &mut Window, cx: &mut Context<Self>| {
            let text = text.to_string();
            cx.new(|cx| InputState::new(window, cx).default_value(text))
        };
        let (name, tags, slot, location) = match &body {
            Body::Form(f) => (f.name().to_string(), f.tags().to_string(), f.slot().to_string(), f.location()),
            Body::Remove(_) => Default::default(),
        };
        let name_input = input(&name, window, cx);
        let tags_input = input(&tags, window, cx);
        let slot_input = input(&slot, window, cx);
        let location_input = input(&location, window, cx);
        let this_window = window.window_handle();
        let me = cx.weak_entity();
        let mut subscriptions = vec![
            // A reread of the pool (a machine added meanwhile, a probe's
            // answer): the picker follows.
            cx.observe(&page, |this, page, cx| {
                if let Body::Form(form) = &mut this.body {
                    let state = page.read(cx).state();
                    if let Some(source) = state.index().and_then(|i| i.source.clone()) {
                        form.set_pool(source.0, machine_choices(state));
                    }
                }
                cx.notify();
            }),
            // Ahead of the text fields' own bindings: Tab, Return and Esc
            // are the dialog's everywhere in it.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != this_window {
                    return;
                }
                let Some(chord) = chord_from_keystroke(&event.keystroke) else {
                    return;
                };
                let taken = me.update(cx, |this, cx| this.key(chord, window, cx)).unwrap_or(false);
                if taken {
                    cx.stop_propagation();
                }
            }),
        ];
        for (input, focus) in [
            (&name_input, Focus::Name),
            (&tags_input, Focus::Tags),
            (&location_input, Focus::Location),
            (&slot_input, Focus::Slot),
        ] {
            subscriptions.push(cx.subscribe_in(input, window, move |this, input, event: &InputEvent, window, cx| {
                let Body::Form(form) = &mut this.body else {
                    return;
                };
                match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        match focus {
                            Focus::Name => form.set_name(&text),
                            Focus::Tags => form.set_tags(&text),
                            Focus::Slot => form.set_slot(&text),
                            _ => {}
                        }
                        this.show_location(window, cx);
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        if form.confirm().is_none() {
                            form.set_focus(focus);
                        }
                        cx.notify();
                    }
                    _ => {}
                }
            }));
        }
        let mut dialog = WorkspaceDialog {
            body,
            focus,
            name_input,
            tags_input,
            location_input,
            slot_input,
            keyboard: false,
            inset: 0.0,
            service,
            running: false,
            machine_anchor: Default::default(),
            machine_scroll: Default::default(),
            _subscriptions: subscriptions,
        };
        dialog.sync_focus(window, cx);
        // The suggested name selected, so typing replaces it.
        if matches!(&dialog.body, Body::Form(f) if f.is_new()) {
            dialog.name_input.update(cx, |s, cx| s.select_all(window, cx));
        }
        dialog
    }

    /// The form, when this is one.
    #[cfg(test)]
    pub fn form(&self) -> Option<&WorkspaceForm> {
        match &self.body {
            Body::Form(f) => Some(f),
            Body::Remove(_) => None,
        }
    }

    /// The confirmation shown: the form's, or the dialog's own.
    #[cfg(test)]
    pub fn confirm(&self) -> Option<&RemoveConfirm> {
        match &self.body {
            Body::Form(f) => f.confirm(),
            Body::Remove(c) => Some(c),
        }
    }

    /// Open the Machine picker (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.click(Focus::Machine, window, cx);
    }

    /// Whether a job is out.
    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.running
    }

    /// The text fields (tests type into them as the user would).
    #[cfg(test)]
    pub fn input(&self, focus: Focus) -> Entity<InputState> {
        match focus {
            Focus::Tags => self.tags_input.clone(),
            Focus::Location => self.location_input.clone(),
            Focus::Slot => self.slot_input.clone(),
            _ => self.name_input.clone(),
        }
    }

    /// Where the main area starts, for centering.
    pub fn set_inset(&mut self, inset: f32) {
        self.inset = inset;
    }

    /// Close without saving (the window's ⌘W), unless a job is out.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if !self.running {
            cx.emit(WorkspaceDialogEvent::Closed { done: None });
        }
    }

    /// A key the dialog takes: Tab and ⇧Tab, Return and Esc anywhere;
    /// Space and ↑ / ↓ off the text fields (or with the picker open).
    /// `false` lets it through.
    pub(crate) fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let bare = chord.mods == Mods::NONE;
        let (in_text, menu) = match &self.body {
            Body::Form(f) => (f.confirm().is_none() && f.focus().is_text(), f.menu().is_some()),
            Body::Remove(_) => (false, false),
        };
        let key = match chord.code {
            Code::Tab if bare => Key::Tab,
            Code::Tab | Code::BackTab if chord.mods == Mods::SHIFT || chord.code == Code::BackTab => Key::BackTab,
            Code::Enter if bare => Key::Enter,
            Code::Esc if bare => Key::Esc,
            Code::Char(' ') if bare && (!in_text || menu) => Key::Space,
            Code::Up if bare && (!in_text || menu) => Key::Up,
            Code::Down if bare && (!in_text || menu) => Key::Down,
            _ => return false,
        };
        if matches!(key, Key::Tab | Key::BackTab | Key::Up | Key::Down | Key::Space) {
            self.keyboard = true;
        }
        let action = match &mut self.body {
            Body::Form(f) => f.key(key),
            Body::Remove(c) => c.key(key),
        };
        self.act(action, window, cx);
        true
    }

    /// A click on `focus`.
    fn click(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        let action = match &mut self.body {
            Body::Form(f) => f.activate(focus),
            Body::Remove(_) => Action::Stay,
        };
        self.act(action, window, cx);
    }

    /// A click on one of the confirmation's buttons.
    fn click_confirm(&mut self, focus: ConfirmFocus, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = match &mut self.body {
            Body::Form(f) => f.confirm_mut(),
            Body::Remove(c) => Some(c),
        };
        let Some(confirm) = confirm else {
            return;
        };
        confirm.set_focus(focus);
        let action = match focus {
            ConfirmFocus::Cancel => confirm.cancel(),
            ConfirmFocus::Remove => confirm.remove(),
        };
        // The form's confirmation cancels back onto the form.
        let action = match (&mut self.body, action) {
            (Body::Form(f), Action::Close) => {
                f.dismiss_confirm();
                Action::Stay
            }
            (_, action) => action,
        };
        self.act(action, window, cx);
    }

    /// Pick machine `name` in the picker.
    fn pick(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Body::Form(f) = &mut self.body {
            f.pick_machine(name);
        }
        self.show_location(window, cx);
        self.act(Action::Stay, window, cx);
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Stay => {}
            Action::Run(job) => self.run(job, window, cx),
            Action::Close => {
                cx.emit(WorkspaceDialogEvent::Closed { done: None });
                return;
            }
            Action::Done(outcome) => {
                cx.emit(WorkspaceDialogEvent::Closed { done: Some(outcome) });
                return;
            }
        }
        self.show_location(window, cx);
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// Run `job` off the UI thread; its result comes back to the form. The
    /// job names its project, so a dialog dropped meanwhile (a project
    /// switch) changes nothing but that project.
    fn run(&mut self, job: Job, window: &mut Window, cx: &mut Context<Self>) {
        self.running = true;
        let service = self.service.clone();
        let work = cx.background_executor().spawn(async move { service.run(&job) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| {
                this.running = false;
                let action = match &mut this.body {
                    Body::Form(f) => f.finished(result),
                    Body::Remove(c) => c.finished(result),
                };
                this.act(action, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Put the derived location in its read-only field.
    fn show_location(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Form(form) = &self.body else {
            return;
        };
        let location = form.location();
        if self.location_input.read(cx).value() != location.as_str() {
            self.location_input.update(cx, |s, cx| s.set_value(location, window, cx));
        }
    }

    /// Give the keyboard to the focused control: a text field's input, else
    /// the dialog (which paints the ring on a button or the picker).
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match &self.body {
            Body::Form(f) if f.confirm().is_none() => match f.focus() {
                Focus::Name => self.name_input.read(cx).focus_handle(cx),
                Focus::Tags => self.tags_input.read(cx).focus_handle(cx),
                Focus::Location => self.location_input.read(cx).focus_handle(cx),
                Focus::Slot => self.slot_input.read(cx).focus_handle(cx),
                _ => self.focus.clone(),
            },
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    /// Whether `focus`'s ring shows.
    fn ring(&self, focus: Focus, window: &Window) -> bool {
        self.keyboard
            && self.focus.is_focused(window)
            && matches!(&self.body, Body::Form(f) if f.confirm().is_none() && f.focus() == focus)
    }
}

impl EventEmitter<WorkspaceDialogEvent> for WorkspaceDialog {}

/// A dialog panel: the canvas, its 1pt edge, 32pt in, 24pt between its
/// parts.
fn panel(id: &'static str, title: SharedString, description: SharedString, width: f32, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(Role::Dialog)
        .aria_label(title.clone())
        .aria_description(description.clone())
        .flex()
        .flex_col()
        .gap(px(24.0))
        .w(px(width))
        .p(px(PADDING))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(theme.divider)
        .bg(theme.opaque().canvas)
        .text_color(theme.form_text)
        .occlude()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .type_style(Type::Heading)
                        .text_size(px(TITLE_SIZE))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text_loud)
                        .child(title),
                )
                .child(div().type_style(Type::Small).text_color(theme.form_hint).child(description)),
        )
}

/// A field: its label (Small medium), the control, then its helpers and
/// its error.
fn field(
    selector: &'static str,
    label: &'static str,
    control: impl IntoElement,
    help: Vec<String>,
    error: Option<&str>,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(selector)
        .debug_selector(move || selector.into())
        .role(Role::Group)
        .aria_label(label)
        .flex()
        .flex_col()
        .gap(px(8.0))
        .flex_1()
        .min_w_0()
        .child(div().type_style(Type::SmallMedium).text_color(theme.form_text).child(label))
        .child(control)
        .children(help.into_iter().map(|h| div().type_style(Type::Small).text_color(theme.form_hint).child(h)))
        .children(error.map(|e| {
            let id = format!("{selector}-error");
            div()
                .debug_selector(move || id.clone())
                .type_style(Type::Small)
                .text_color(theme.error)
                .child(e.to_string())
        }))
}

/// A dialog button, with its role, its label as the accessible name, and
/// the keyboard's ring when `focused`.
fn button(id: &'static str, label: &'static str, kind: ButtonKind, enabled: bool, focused: bool, theme: &Theme) -> Stateful<Div> {
    let b = if enabled {
        style::button(id, label, kind, ButtonSize::Small, focused, theme)
    } else {
        style::disabled_button(id, label, kind, ButtonSize::Small, theme)
    };
    b.role(Role::Button).aria_label(label)
}

impl WorkspaceDialog {
    fn render_form(&self, form: &WorkspaceForm, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let errors = form.errors();
        let name = field(
            "workspace-dialog-name",
            "Name",
            dialog_text_field(&self.name_input, "workspace-dialog-name-input", None, errors.name.is_some(), window, cx),
            Vec::new(),
            errors.name.as_deref(),
            theme,
        );
        let machine = field(
            "workspace-dialog-machine",
            "Machine",
            self.render_picker(form, theme, window, cx),
            Vec::new(),
            errors.machine.as_deref(),
            theme,
        );
        let mut tags_help = vec![TAGS_HELP.to_string()];
        tags_help.extend(form.inherited_note());
        let tags = field(
            "workspace-dialog-tags",
            "Tags",
            dialog_text_field(
                &self.tags_input,
                "workspace-dialog-tags-input",
                Some("Optional tags".into()),
                errors.tags.is_some(),
                window,
                cx,
            ),
            tags_help,
            errors.tags.as_deref(),
            theme,
        );
        // The revised disabled Textbox: the Textbox at half opacity. Its text
        // stays selectable and copyable.
        let location = field(
            "workspace-dialog-location",
            "Worktree location",
            div()
                .w_full()
                .opacity(0.5)
                .child(readonly_field(&self.location_input, "workspace-dialog-location-input", &cx.global::<Theme>().opaque(), window, cx)),
            vec![form.location_help()],
            None,
            theme,
        );
        let slot = field(
            "workspace-dialog-slot",
            "Slot (optional)",
            dialog_text_field(
                &self.slot_input,
                "workspace-dialog-slot-input",
                Some(form.slot_placeholder().into()),
                errors.slot.is_some(),
                window,
                cx,
            ),
            vec![SLOT_HELP.to_string()],
            errors.slot.as_deref(),
            theme,
        );
        let general = errors.general.as_ref().map(|e| {
            div()
                .debug_selector(|| "workspace-dialog-error".into())
                .type_style(Type::Small)
                .text_color(theme.error)
                .child(e.clone())
        });
        let busy = form.busy();
        let remove = (!form.is_new()).then(|| {
            button("workspace-dialog-remove", "Remove workspace", ButtonKind::Danger, !busy, self.ring(Focus::Remove, window), theme)
                .when(!busy, |b| b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.click(Focus::Remove, window, cx))))
        });
        let cancel = button("workspace-dialog-cancel", "Cancel", ButtonKind::Secondary, !busy, self.ring(Focus::Cancel, window), theme)
            .when(!busy, |b| b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.click(Focus::Cancel, window, cx))));
        let submit_label = form.submit_label();
        let submit = button(
            "workspace-dialog-submit",
            submit_label,
            ButtonKind::Primary,
            form.can_submit(),
            self.ring(Focus::Submit, window),
            theme,
        )
        .aria_keyshortcuts("Return")
        .when(form.can_submit(), |b| {
            b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.click(Focus::Submit, window, cx)))
        });
        let actions = div()
            .debug_selector(|| "workspace-dialog-actions".into())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .children(remove)
            .child(div().flex_1())
            .when(busy, |d| {
                d.child(
                    div()
                        .debug_selector(|| "workspace-dialog-spinner".into())
                        .child(spinner::spinner("workspace-dialog-spinner", theme.form_hint)),
                )
            })
            .child(cancel)
            .child(submit);
        panel("workspace-dialog", form.title().into(), form.description().into(), DIALOG_W, theme)
            .child(div().flex().flex_row().gap(px(16.0)).child(name).child(machine))
            .child(tags)
            .child(location)
            .child(slot)
            .children(general)
            .child(div().h(px(1.0)).w_full().bg(theme.divider))
            .child(actions)
    }

    /// The Machine picker: the Button at Base with the machine and a
    /// chevron, the machines floating under it while open.
    fn render_picker(&self, form: &WorkspaceForm, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        let open = form.menu();
        let chevron = style::button_colors(ButtonKind::Secondary, theme).icon;
        let label = if form.machine().is_empty() { "Choose a machine".to_string() } else { form.machine().to_string() };
        let button = style::button_base("workspace-dialog-machine-button", ButtonKind::Secondary, ButtonSize::Base, self.ring(Focus::Machine, window), theme)
            .w_full()
            .justify_between()
            .role(Role::ComboBox)
            .aria_label(format!("Machine: {label}"))
            .aria_expanded(open.is_some())
            .when_some(form.machine_choice(), |b, m| b.aria_description(m.detail()))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(style::dropdown_slot(ButtonSize::Base, chevron))
            .child(style::anchor_probe(&self.machine_anchor))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.click(Focus::Machine, window, cx)));
        self.machine_scroll.follow(open);
        let menu = open.map(|highlight| {
            let items: Vec<AnyElement> = form
                .machines()
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let name = m.name.clone();
                    let id = SharedString::from(format!("workspace-dialog-machine-{}", m.name));
                    let selected = m.name == form.machine();
                    style::menu_row(id, i == highlight, theme)
                        .role(Role::ListBoxOption)
                        .aria_label(format!("{}, {}", m.name, m.detail()))
                        .aria_selected(selected)
                        .child(div().flex_1().min_w_0().truncate().child(m.name.clone()))
                        .child(
                            div()
                                .flex_none()
                                .type_style(Type::XSmall)
                                .text_color(match m.status {
                                    shelbi_app::settings::workspaces::MachineStatus::Unreachable(_) => theme.error,
                                    _ => theme.form_hint,
                                })
                                .child(m.detail()),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.pick(&name, window, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            let list = style::popover(
                div()
                    .id("workspace-dialog-machines")
                    .debug_selector(|| "workspace-dialog-machines".into())
                    .role(Role::ListBox)
                    .aria_label("Machines")
                    .flex()
                    .flex_col()
                    .p_1()
                    .overflow_y_scroll()
                    .track_scroll(self.machine_scroll.handle())
                    .occlude(),
                theme,
            )
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if let Body::Form(f) = &mut this.body {
                    f.close_menu();
                }
                cx.notify();
            }))
            .children(items);
            deferred(placed(&self.machine_anchor, list).match_width()).with_priority(2)
        });
        div().relative().w_full().child(button).children(menu)
    }

    /// The removal's confirmation.
    fn render_confirm(&self, confirm: &RemoveConfirm, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let busy = confirm.busy();
        let ring = |f: ConfirmFocus| self.keyboard && self.focus.is_focused(window) && confirm.focus() == f;
        let cancel = button("workspace-remove-cancel", "Cancel", ButtonKind::Secondary, !busy, ring(ConfirmFocus::Cancel), theme)
            .when(!busy, |b| {
                b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.click_confirm(ConfirmFocus::Cancel, window, cx)))
            });
        let remove = button("workspace-remove-confirm", "Remove workspace", ButtonKind::Danger, !busy, ring(ConfirmFocus::Remove), theme)
            .when(!busy, |b| {
                b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.click_confirm(ConfirmFocus::Remove, window, cx)))
            });
        let error = confirm.error().map(|e| {
            div()
                .debug_selector(|| "workspace-remove-error".into())
                .type_style(Type::Small)
                .text_color(theme.error)
                .child(e.to_string())
        });
        panel("workspace-remove-prompt", confirm.title().into(), confirm.body().into(), CONFIRM_W, theme)
            .children(error)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .when(busy, |d| d.child(spinner::spinner("workspace-remove-spinner", theme.form_hint)))
                    .child(cancel)
                    .child(remove),
            )
    }
}

/// The scrim over the window, `panel` centered over the main area.
fn scrim(id: &'static str, inset: f32, panel: impl IntoElement, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .pl(px(inset))
        .bg(theme.scrim)
        .occlude()
        .child(panel)
}

impl Render for WorkspaceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A dialog: opaque over a translucent window, its fields too.
        let theme = cx.global::<Theme>().opaque();
        let root = div()
            .id("workspace-dialog-layer")
            .track_focus(&self.focus)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .font_family("Geist")
            .type_style(Type::Base);
        let inset = self.inset;
        match &self.body {
            Body::Remove(confirm) => {
                let confirm = confirm.clone();
                let panel = self.render_confirm(&confirm, &theme, window, cx);
                root.child(scrim("workspace-dialog-scrim", inset, panel, &theme))
            }
            Body::Form(form) => {
                let form = (**form).clone();
                let panel = self.render_form(&form, &theme, window, cx);
                let confirm = form
                    .confirm()
                    .map(|c| scrim("workspace-remove-scrim", inset, self.render_confirm(c, &theme, window, cx), &theme));
                root.child(scrim("workspace-dialog-scrim", inset, panel, &theme)).children(confirm)
            }
        }
    }
}
