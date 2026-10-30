//! The agent details editor (`dt-agent-details`): one agent, full window,
//! without the sidebar, as the Settings · Agent Details frame (2405:6965)
//! draws it. Settings ▸ Agents opens it (a row's click, its edit button, an
//! agent just created); the window hides the settings sidebar while it
//! shows, as it does for the workflow editor, whose shell, controls and
//! save machinery this reuses.
//!
//! **The titlebar row**: room for the traffic lights, Back, the agent's name
//! with its badge (Default for a shipped agent as shipped, Customized once
//! edited, none for the user's own, as on the Agents page), then Discard
//! changes (only with unsaved edits), Save changes (disabled while there are
//! none) and a ⋮ menu with Edit YAML, Duplicate, Reset to default and Delete.
//!
//! **The page** (scrolling): Name and Description; then two columns. Left:
//! Instructions (`instructions.md` in a large monospace text area, with the
//! footnote linking back to the shared instructions) and Skills (each
//! skill's name, description and remove; Add skill opens a form under the
//! header: a name, a description and the SKILL.md body typed in, or an
//! existing skill folder or `.md` file chosen with Import). Right: Runner
//! preferences (Preferred runner, Model, Reasoning effort, Permissions, each
//! with its "default" choice first) and, under a rule, Workflow assignments:
//! one link row per workflow status the agent runs on, opening that
//! workflow's editor. A rule and the note on when changes apply close it.
//!
//! Every field edits a draft ([`AgentEditorState`]) through
//! `shelbi_state::agents_config`; nothing is written until Save. Pickers
//! apply when picked, text fields as typed (Name at Save, since a rename
//! moves the agent's directory). A change the model refuses shows its
//! message under the field it concerns. Discard reverts; Back, Esc, Edit
//! YAML, Duplicate, an assignment row and the footnotes' links ask to save
//! or discard unsaved edits first. Delete asks to confirm.
//!
//! **Edit YAML** opens the agent's `agent.yaml` in the user's editor; when
//! the window comes back to the front, the files are read again into a
//! clean draft (a draft edited meanwhile is kept, and its save is refused
//! should it overwrite the hand edit).

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, Focusable,
    PathPromptOptions, ScrollHandle, SharedString, Stateful, Subscription, Window,
};
use gpui_kit::component::input::{Escape, InputEvent, InputState, Textarea, TextareaState};
use shelbi_core::StatusCategory;
use shelbi_state::agents_config::{AgentsConfig, Provenance};

use super::agent_editor_state::{
    run_delete, run_duplicate, run_save, AgentEditorState, AgentFile, Change, Field, Leave, SaveJob, SaveOutcome,
};
use super::agents_state::runner_label;
use super::sidebar;
use super::workflow_editor::{
    dialog_overlay, discard_link, dropdown, error_line, footnote, header_button, menu_item, menu_rule, readonly_field,
    row_button, section_title, text_field, toolbar_button, MENU_W, TITLEBAR_H,
};
use crate::assets::{icons, Icon};
use crate::elements::terminal::metrics::PRIMARY_FAMILY;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The page's inset in the window, and the gap between its rows, its
/// columns and the left column's sections (`32`).
const INSET: f32 = 32.0;
const GAP: f32 = 32.0;
/// The Name field's width (`Agent name`), and the gap to Description.
const NAME_W: f32 = 240.0;
const FIELD_GAP: f32 = 16.0;
/// The right column (`Agent configuration`).
const SIDE_W: f32 = 456.0;
/// The instructions' text area (`# Developer`, 2405:9748).
const INSTRUCTIONS_H: f32 = 410.0;
/// The Add skill form's SKILL.md body.
const SKILL_BODY_H: f32 = 160.0;
/// The skills list's corners (`radius/default`).
const SKILLS_RADIUS: f32 = 12.0;

/// What the editor tells the settings view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEditorEvent {
    /// Back to Settings ▸ Agents.
    Close,
    /// Open workflow `.0` in the workflow editor.
    OpenWorkflow(String),
    /// Settings ▸ Workflows.
    ShowWorkflows,
}

/// The menu that's open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    More,
    Runner,
    Model,
    Effort,
    Permissions,
}

/// The Add skill form's fields.
struct SkillForm {
    name: Entity<InputState>,
    description: Entity<InputState>,
    body: Entity<TextareaState>,
}

/// The agent details editor.
pub struct AgentEditor {
    project: String,
    /// `None` until the agents have loaded.
    state: Option<AgentEditorState>,
    load_error: Option<String>,
    name: Entity<InputState>,
    description: Entity<InputState>,
    instructions: Entity<TextareaState>,
    skill_form: Option<SkillForm>,
    menu: Option<Menu>,
    /// Where the open menu's button last painted (its own click toggles it).
    menu_anchor: style::Anchor,
    scroll: ScrollHandle,
    /// Read the files again when the window is next active (Edit YAML sent
    /// the user to their editor).
    reload_on_return: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AgentEditorEvent> for AgentEditor {}

impl AgentEditor {
    /// Edit agent `name` of `project`, from `config` when the caller has it
    /// loaded (else it is read off the UI thread; a test's editor reads no
    /// disk).
    pub fn new(
        project: &str,
        name: &str,
        config: Option<AgentsConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let description = cx.new(|cx| InputState::new(window, cx));
        let instructions = cx.new(|cx| TextareaState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&name_input, window, |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if let Some(state) = this.state.as_mut() {
                        state.type_name(&text);
                    }
                    cx.notify();
                }
            }),
            cx.subscribe_in(&description, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.typed(cx);
                }
            }),
            cx.subscribe_in(&instructions, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.typed(cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() && std::mem::take(&mut this.reload_on_return) {
                    this.read_again(window, cx);
                }
            }),
        ];
        let mut editor = AgentEditor {
            project: project.to_string(),
            state: None,
            load_error: None,
            name: name_input,
            description,
            instructions,
            skill_form: None,
            menu: None,
            menu_anchor: Default::default(),
            scroll: ScrollHandle::new(),
            reload_on_return: false,
            _subscriptions: subscriptions,
        };
        match config {
            Some(config) if config.agent(name).is_some() => editor.show(AgentEditorState::new(config, name), window, cx),
            Some(_) => editor.load_error = Some(format!("There is no agent named {name}.")),
            None if cfg!(test) => editor.load_error = Some("The agents haven’t loaded.".into()),
            None => editor.load(name.to_string(), window, cx),
        }
        editor
    }

    /// Read the agents off the UI thread and edit `name`.
    fn load(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project.clone();
        let read = cx.background_executor().spawn(async move { AgentsConfig::load(&project) });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update_in(cx, |this, window, cx| {
                match config {
                    Ok(config) if config.agent(&name).is_some() => {
                        this.show(AgentEditorState::new(config, &name), window, cx)
                    }
                    Ok(_) => this.load_error = Some(format!("There is no agent named {name}.")),
                    Err(e) => this.load_error = Some(format!("Couldn’t read the agents: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Edit `state` from here on: every field shows it.
    fn show(&mut self, state: AgentEditorState, window: &mut Window, cx: &mut Context<Self>) {
        self.state = Some(state);
        self.menu = None;
        self.skill_form = None;
        self.sync(true, window, cx);
    }

    /// Put the draft's values in the fields: all of them (`force`), or
    /// those not being typed in.
    fn sync(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let a = state.agent();
        let fields = [
            (self.name.clone(), state.typed_name().to_string()),
            (self.description.clone(), a.description().unwrap_or_default().to_string()),
        ];
        for (input, value) in fields {
            let typing = input.read(cx).focus_handle(cx).is_focused(window);
            if (force || !typing) && input.read(cx).value() != value.as_str() {
                input.update(cx, |s, cx| s.set_value(value, window, cx));
            }
        }
        let text = a.instructions().to_string();
        let typing = self.instructions.read(cx).focus_handle(cx).is_focused(window);
        if (force || !typing) && self.instructions.read(cx).value() != text.as_str() {
            self.instructions.update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    /// What the text fields hold, as changes (Save applies those that
    /// differ from the draft).
    fn texts(&self, cx: &App) -> Vec<Change> {
        vec![
            Change::Description(self.description.read(cx).value().to_string()),
            Change::Instructions(self.instructions.read(cx).value().to_string()),
        ]
    }

    /// A text field changed: make each text change that differs from the
    /// draft (none does when the view itself filled the field).
    fn typed(&mut self, cx: &mut Context<Self>) {
        let texts = self.texts(cx);
        let Some(state) = self.state.as_mut() else {
            return;
        };
        for change in texts {
            if state.differs(&change) {
                state.change(change);
            }
        }
        cx.notify();
    }

    /// Make `change` (a picker's, a button's). Returns whether it was made.
    fn change(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.menu = None;
        let reset = change == Change::Reset;
        let made = self.state.as_mut().is_some_and(|s| s.change(change));
        if reset && made {
            self.sync(true, window, cx);
        }
        cx.notify();
        made
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = if self.menu == Some(menu) { None } else { Some(menu) };
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Skills

    /// Add skill: the form under the header, its name focused.
    fn open_skill_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if self.skill_form.is_none() {
            let name = cx.new(|cx| InputState::new(window, cx));
            let description = cx.new(|cx| InputState::new(window, cx));
            let body = cx.new(|cx| TextareaState::new(window, cx));
            name.update(cx, |s, cx| s.focus(window, cx));
            self.skill_form = Some(SkillForm { name, description, body });
        }
        cx.notify();
    }

    fn close_skill_form(&mut self, cx: &mut Context<Self>) {
        self.skill_form = None;
        if let Some(state) = self.state.as_mut() {
            state.clear_error_at(&Field::NewSkill);
        }
        cx.notify();
    }

    /// The form's Add: the skill goes into the draft and the form closes,
    /// or the model's refusal shows in it.
    fn add_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.skill_form else {
            return;
        };
        let change = Change::AddSkill {
            name: form.name.read(cx).value().to_string(),
            description: form.description.read(cx).value().to_string(),
            body: form.body.read(cx).value().to_string(),
        };
        if self.change(change, window, cx) {
            self.skill_form = None;
        }
    }

    /// The form's Import: choose a skill folder (or a `.md` file) and copy
    /// it into the draft.
    fn import_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if this.change(Change::ImportSkill(path), window, cx) {
                    this.skill_form = None;
                }
            })
            .ok();
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // Save, Discard, leaving

    /// Save changes.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let texts = self.texts(cx);
        let job = self.state.as_mut().and_then(|s| s.begin_save(texts));
        self.run_save(job, window, cx);
    }

    fn run_save(&mut self, job: Option<SaveJob>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(job) = job else {
            cx.notify();
            return;
        };
        let save = cx.background_executor().spawn(async move { run_save(job) });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = save.await;
            this.update_in(cx, |this, window, cx| this.saved(outcome, window, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn saved(&mut self, outcome: SaveOutcome, window: &mut Window, cx: &mut Context<Self>) {
        let ok = outcome.is_ok();
        let then = self.state.as_mut().and_then(|s| s.saved(outcome));
        if ok {
            self.sync(true, window, cx);
        }
        if let Some(then) = then {
            self.go(then, window, cx);
        }
        cx.notify();
    }

    /// Discard changes: the draft goes back to the files (read again when
    /// they changed under the last save).
    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.skill_form = None;
        let reload = self.state.as_mut().is_some_and(AgentEditorState::discard);
        self.sync(true, window, cx);
        if reload && !cfg!(test) {
            self.read_again(window, cx);
        }
        cx.notify();
    }

    /// Read the files again into the draft, unless it holds edits (or a
    /// save is running) by the time they're read. Closes when the agent is
    /// gone from them.
    fn read_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project.clone();
        let read = cx.background_executor().spawn(async move { AgentsConfig::load(&project) });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update_in(cx, |this, window, cx| {
                let Some(state) = this.state.as_mut() else {
                    return;
                };
                if state.is_dirty() || state.busy() {
                    return;
                }
                match config {
                    Ok(config) => {
                        if state.reloaded(config) {
                            this.sync(true, window, cx);
                        } else {
                            cx.emit(AgentEditorEvent::Close);
                        }
                    }
                    Err(e) => this.load_error = Some(format!("Couldn’t read the agents: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open one of the agent's files (as saved: `agent.yaml`,
    /// `instructions.md`, a skill's `SKILL.md`) in the user's editor; the
    /// draft is read again when the window comes back to the front.
    fn edit_file(&mut self, file: AgentFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let path = state.file(&file);
        let label = file.label();
        self.reload_on_return = true;
        let open = cx.background_executor().spawn(async move {
            if !path.exists() {
                return Err(format!("Couldn’t find {label}."));
            }
            super::external_edit::open_file(&path).map_err(|e| format!("Couldn’t open {label}: {e}"))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = open.await;
            this.update(cx, |this, cx| {
                if let (Err(e), Some(state)) = (result, this.state.as_mut()) {
                    state.open_failed(e);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Back (or Esc): close, asking first about unsaved edits. Esc with a
    /// dialog up, a menu or the Add skill form open closes that instead.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
            return;
        }
        if let Some(state) = self.state.as_mut() {
            if state.prompt().is_some() {
                state.cancel_prompt();
                cx.notify();
                return;
            }
            if state.confirming_delete() {
                state.cancel_delete();
                cx.notify();
                return;
            }
        }
        if self.skill_form.is_some() {
            self.close_skill_form(cx);
            return;
        }
        self.leave(Leave::Back, None, cx);
    }

    /// Leave for `then`, or put the prompt up first.
    fn leave(&mut self, then: Leave, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.menu = None;
        let go = match self.state.as_mut() {
            Some(state) => state.leave(then),
            None => Some(then),
        };
        match (go, window) {
            (Some(then), Some(window)) => self.go(then, window, cx),
            (Some(Leave::Back | Leave::SharedInstructions), None) => cx.emit(AgentEditorEvent::Close),
            _ => {}
        }
        cx.notify();
    }

    fn go(&mut self, then: Leave, window: &mut Window, cx: &mut Context<Self>) {
        match then {
            Leave::Back | Leave::SharedInstructions => cx.emit(AgentEditorEvent::Close),
            Leave::Workflow { name, .. } => cx.emit(AgentEditorEvent::OpenWorkflow(name)),
            Leave::Workflows => cx.emit(AgentEditorEvent::ShowWorkflows),
            Leave::Duplicate => self.duplicate(window, cx),
            Leave::Open(file) => self.edit_file(file, window, cx),
        }
    }

    /// Copy the agent as saved and edit the copy.
    fn duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let (name, copy) = (state.name().to_string(), state.duplicate_name());
        let Some(base) = state.begin_job() else {
            return;
        };
        let job = cx.background_executor().spawn({
            let copy = copy.clone();
            async move { run_duplicate(base, &name, &copy) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let done = job.await;
            this.update_in(cx, |this, window, cx| {
                match done {
                    Ok(config) => this.show(AgentEditorState::new(config, &copy), window, cx),
                    Err(e) => {
                        if let Some(state) = this.state.as_mut() {
                            state.job_failed(e);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The confirm's Delete: delete the agent (as saved) and close.
    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.delete_blocked().is_some() {
            return;
        }
        let name = state.name().to_string();
        let Some(base) = state.begin_job() else {
            return;
        };
        let job = cx.background_executor().spawn(async move { run_delete(base, &name) });
        cx.spawn_in(window, async move |this, cx| {
            let done = job.await;
            this.update(cx, |this, cx| {
                match done {
                    Ok(()) => cx.emit(AgentEditorEvent::Close),
                    Err(e) => {
                        if let Some(state) = this.state.as_mut() {
                            state.job_failed(e);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

#[cfg(any(test, feature = "visual-check"))]
impl AgentEditor {
    /// Make `change` as its control would.
    pub fn make(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        self.change(change, window, cx);
    }

    pub fn state(&self) -> Option<&AgentEditorState> {
        self.state.as_ref()
    }
}

#[cfg(feature = "visual-check")]
impl AgentEditor {
    /// Open `menu` (the offscreen renders' menus).
    pub fn offscreen_open_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = Some(menu);
        cx.notify();
    }

    /// Open the Add skill form, filled in.
    pub fn offscreen_add_skill(&mut self, name: &str, description: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_skill_form(window, cx);
        if let Some(form) = &self.skill_form {
            form.name.update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
            form.description.update(cx, |s, cx| s.set_value(description.to_string(), window, cx));
        }
    }

    /// Ask to confirm Delete (as its menu item does).
    pub fn offscreen_ask_delete(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.state.as_mut() {
            state.ask_delete();
        }
        cx.notify();
    }
}

#[cfg(test)]
impl AgentEditor {
    pub fn name_field(&self) -> Entity<InputState> {
        self.name.clone()
    }

    pub fn description_field(&self) -> Entity<InputState> {
        self.description.clone()
    }

    pub fn instructions_field(&self) -> Entity<TextareaState> {
        self.instructions.clone()
    }

    /// The Add skill form's name, description and body, while it's open.
    pub fn skill_fields(&self) -> Option<(Entity<InputState>, Entity<InputState>, Entity<TextareaState>)> {
        self.skill_form.as_ref().map(|f| (f.name.clone(), f.description.clone(), f.body.clone()))
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers

/// A field's label over its control (`Agent name`: Base, 8pt above).
fn labeled(label: &'static str, control: impl IntoElement, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(div().type_style(Type::Base).text_color(theme.foreground).child(label))
        .child(control)
}

/// A picker's button at the column's width (the Button at Small, 32pt tall
/// as the frame stretches it): an optional status icon, the choice, and
/// `trailing` (the chevron) in the right icon slot.
fn select_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    icon: Option<StatusCategory>,
    trailing: Icon,
    theme: &Theme,
) -> Stateful<Div> {
    // The frame's instances are the Small size stretched to the form's
    // 32pt rows.
    style::button_base(id, ButtonKind::Secondary, ButtonSize::Small, false, theme)
        .h(px(style::BUTTON_H))
        .w_full()
        .children(icon.map(|c| style::left_slot(ButtonSize::Small, style::status_icon_large(c, theme))))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(style::right_icon_slot(trailing, ButtonSize::Small, theme.button_icon))
}

/// A link in a footnote: the footnote's color, underlined on hover.
fn link(id: &'static str, text: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .cursor_pointer()
        .hover(|s| s.underline())
        .child(text)
}

/// A 1pt rule (`Divider`: `theme.divider` as the frame sets it).
fn rule(theme: &Theme) -> Div {
    div().flex_none().h(px(1.0)).w_full().bg(theme.input_border)
}

/// A monospace text area (`Desktop/Terminal`) in the text box's edge and
/// well, `height` tall.
fn markdown_area(
    input: &Entity<TextareaState>,
    selector: &'static str,
    height: f32,
    window: &Window,
    cx: &App,
) -> Div {
    let theme = cx.global::<Theme>();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_none()
        .w_full()
        .h(px(height))
        .p(px(1.0))
        .rounded(px(6.0))
        .bg(if focused { theme.focus_ring } else { theme.input_border })
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                // The text area's own inset (about 9pt across, 8pt down)
                // makes up the rest of the frame's 16pt.
                .px(px(7.0))
                .py(px(8.0))
                .rounded(px(5.0))
                .bg(theme.textbox)
                .type_style(Type::Base)
                .font_family(PRIMARY_FAMILY)
                .text_color(theme.form_text)
                // The component's own 1.25rem line would set 20pt lines.
                .child(
                    Textarea::new(input)
                        .appearance(false)
                        .h_full()
                        .font_family(PRIMARY_FAMILY)
                        .text_size(px(14.0))
                        .line_height(px(18.0)),
                ),
        )
}

impl Render for AgentEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let body = match self.state.as_ref() {
            None => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .type_style(Type::Small)
                .text_color(theme.form_hint)
                .child(self.load_error.clone().unwrap_or_else(|| "Loading…".into()))
                .into_any_element(),
            Some(_) => self.render_page(&theme, window, cx).into_any_element(),
        };
        let dialog = match self.state.as_ref() {
            Some(s) if s.prompt().is_some() => Some(self.render_prompt(&theme, cx)),
            Some(s) if s.confirming_delete() => Some(self.render_confirm_delete(&theme, cx)),
            _ => None,
        };
        div()
            .id("agent-editor")
            .debug_selector(|| "agent-editor".into())
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.nested(theme.canvas))
            .type_style(Type::Base)
            .text_color(theme.foreground)
            // Esc anywhere in the editor, a field's included, goes Back.
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.back(cx);
                cx.stop_propagation();
            }))
            .child(self.render_titlebar(&theme, cx))
            .child(body)
            .children(dialog)
    }
}

impl AgentEditor {
    /// The titlebar row: the traffic lights' room, Back, the name and its
    /// badge; Discard, Save and ⋮.
    fn render_titlebar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref();
        let name = state.map(|s| s.name().to_string()).unwrap_or_default();
        let dirty = state.is_some_and(AgentEditorState::is_dirty);
        let busy = state.is_some_and(AgentEditorState::busy);
        let badge = state
            .and_then(|s| match s.provenance() {
                Provenance::Shipped => Some("Default"),
                Provenance::Customized => Some("Customized"),
                Provenance::UserCreated => None,
            })
            .map(|label| {
                div()
                    .debug_selector(move || format!("agent-editor-badge-{}", label.to_lowercase()))
                    .flex_none()
                    .px(px(4.0))
                    .rounded(px(2.0))
                    .bg(theme.workflow_badge)
                    .type_style(Type::XSmall)
                    .text_color(theme.workflow_badge_text)
                    .child(label)
            });
        let back = toolbar_button("agent-editor-back", icons::ARROW_LEFT, theme)
            .tooltip(style::tooltip("Back to Agents"))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.leave(Leave::Back, Some(window), cx)));
        let discard = dirty.then(|| {
            discard_link("agent-editor-discard", theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.discard(window, cx)))
        });
        let save = if dirty && !busy {
            style::button("agent-editor-save", "Save changes", ButtonKind::Primary, ButtonSize::Small, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)))
        } else {
            style::disabled_button(
                "agent-editor-save",
                if busy { "Saving…" } else { "Save changes" },
                ButtonKind::Primary,
                ButtonSize::Small,
                theme,
            )
        };
        let more = dropdown(
            self.menu == Some(Menu::More),
            &self.menu_anchor,
            "agent-editor-menu",
            toolbar_button("agent-editor-more", icons::ELLIPSIS_VERTICAL, theme),
            |cx| self.more_items(cx),
            MENU_W,
            true,
            |this: &mut Self, cx| this.toggle_menu(Menu::More, cx),
            |this: &mut Self, _| this.menu = None,
            cx,
        );
        div()
            .debug_selector(|| "agent-editor-titlebar".into())
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .pl(px(sidebar::BESIDE_TRAFFIC_LIGHTS))
            .pr(px(12.0))
            .h(px(TITLEBAR_H))
            .bg(theme.toolbar)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.0))
                    .child(back)
                    .child(
                        div()
                            .debug_selector(|| "agent-editor-title".into())
                            .flex_none()
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud)
                            .child(name),
                    )
                    .children(badge),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .children(discard)
                    .child(save)
                    // The frame's Ellipsis Button: 30pt across, the 32pt
                    // button flush left in it.
                    .child(div().flex_none().w(px(30.0)).child(more)),
            )
    }

    /// The ⋮ menu: Edit YAML, Duplicate, Reset to default, Delete (the last
    /// two disabled, with the reason, where the model refuses them).
    fn more_items(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = *cx.global::<Theme>();
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let inert = |row: Stateful<Div>, why: Option<String>| match why {
            Some(why) => row
                .opacity(0.4)
                .cursor_default()
                .tooltip(style::tooltip(why))
                .on_click(|_, _, cx: &mut App| cx.stop_propagation()),
            None => row,
        };
        let edit_yaml = menu_item("agent-editor-edit-yaml", "Edit YAML", false, &theme).on_click(cx.listener(
            |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.leave(Leave::Open(AgentFile::Manifest), Some(window), cx)
            },
        ));
        let edit_instructions = menu_item("agent-editor-edit-instructions", "Edit instructions.md", false, &theme)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.leave(Leave::Open(AgentFile::Instructions), Some(window), cx)
            }));
        let duplicate = menu_item("agent-editor-duplicate", "Duplicate", false, &theme).on_click(cx.listener(
            |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.leave(Leave::Duplicate, Some(window), cx)
            },
        ));
        let reset_blocked = state.reset_blocked();
        let reset = menu_item("agent-editor-reset", "Reset to default", false, &theme);
        let reset = if reset_blocked.is_some() {
            inert(reset, reset_blocked)
        } else {
            reset.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.change(Change::Reset, window, cx);
            }))
        };
        let delete_blocked = state.delete_blocked();
        let delete = menu_item("agent-editor-delete", "Delete", false, &theme).text_color(theme.error);
        let delete = if delete_blocked.is_some() {
            inert(delete, delete_blocked)
        } else {
            delete.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.menu = None;
                if let Some(state) = this.state.as_mut() {
                    state.ask_delete();
                }
                cx.notify();
            }))
        };
        vec![
            edit_yaml.into_any_element(),
            edit_instructions.into_any_element(),
            duplicate.into_any_element(),
            reset.into_any_element(),
            menu_rule(&theme).into_any_element(),
            delete.into_any_element(),
        ]
    }

    /// The page, scrolling under the titlebar (`Agent details content`).
    fn render_page(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.as_ref().expect("the page draws a loaded agent");
        let banner = state
            .error_at(&Field::Save)
            .map(|e| error_line("agent-editor-error".into(), e, theme).into_any_element())
            .or_else(|| {
                state.notice().map(|n| {
                    div()
                        .debug_selector(|| "agent-editor-notice".into())
                        .type_style(Type::XSmall)
                        .text_color(theme.form_hint)
                        .child(n.to_string())
                        .into_any_element()
                })
            });
        div()
            .id("agent-details")
            .debug_selector(|| "agent-details".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(GAP))
                    .p(px(INSET))
                    .children(banner)
                    .child(self.render_identity(theme, window, cx))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(px(GAP))
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(GAP))
                                    .child(self.render_instructions(theme, window, cx))
                                    .child(self.render_skills(theme, window, cx)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_none()
                                    .w(px(SIDE_W))
                                    .gap(px(GAP))
                                    .child(self.render_runner(theme, cx))
                                    .child(rule(theme))
                                    .child(self.render_assignments(theme, cx)),
                            ),
                    )
                    .child(rule(theme))
                    .child(
                        footnote(
                            "Changes apply to new agent sessions. Existing sessions keep their current instructions and settings.",
                            theme,
                        )
                        .debug_selector(|| "agent-editor-footer".into()),
                    ),
            )
    }

    /// Name and Description.
    fn render_identity(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let name_error = state.error_at(&Field::Name);
        // A name that can't change (a shipped agent, one workflows name) is
        // read-only and says why.
        let name = match state.rename_blocked() {
            Some(why) => div()
                .id("agent-name-locked")
                .tooltip(style::tooltip(why))
                .child(readonly_field(&self.name, "agent-name", cx.global::<Theme>(), window, cx))
                .into_any_element(),
            None => text_field(&self.name, "agent-name", None, name_error.is_some(), window, cx).into_any_element(),
        };
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(FIELD_GAP))
            .w_full()
            .child(
                labeled("Name", name, theme)
                    .w(px(NAME_W))
                    .flex_none()
                    .children(name_error.map(|e| error_line("agent-name-error".into(), e, theme))),
            )
            .child(
                labeled(
                    "Description",
                    text_field(&self.description, "agent-description", Some("What this agent does".into()), false, window, cx),
                    theme,
                )
                .flex_1()
                .min_w_0()
                .children(state.error_at(&Field::Description).map(|e| error_line("agent-description-error".into(), e, theme))),
            )
    }

    /// Instructions: the heading with its Markdown tag, the helper, the
    /// text area and the footnote.
    fn render_instructions(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let shared = link("agent-shared-instructions-link", "Shared project instructions").on_click(cx.listener(
            |this, _: &ClickEvent, window, cx| this.leave(Leave::SharedInstructions, Some(window), cx),
        ));
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(section_title("Instructions", theme))
                    .child(div().type_style(Type::Small).text_color(theme.form_hint).child("Markdown")),
            )
            .child(footnote("Define how this agent approaches a task and when its work is complete.", theme))
            .child(markdown_area(&self.instructions, "agent-instructions", INSTRUCTIONS_H, window, cx))
            .children(state.error_at(&Field::Instructions).map(|e| error_line("agent-instructions-error".into(), e, theme)))
            .child(
                footnote("", theme)
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .child(shared)
                    .child(" are included before these instructions."),
            )
    }

    /// Skills: the header (count, Add skill), the Add skill form while it's
    /// open, and the list or its empty state.
    fn render_skills(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let skills = state.skills();
        let add = header_button("agent-add-skill", Some(icons::PLUS), "Add skill", theme)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_skill_form(window, cx)));
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .w_full()
            .child(section_title("Skills", theme))
            .child(
                div()
                    .debug_selector(|| "agent-skills-count".into())
                    .flex_none()
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child(skills.len().to_string()),
            )
            .child(div().flex_1())
            .child(add);
        let frame = || {
            div()
                .flex()
                .flex_col()
                .w_full()
                .rounded(px(SKILLS_RADIUS))
                .border_1()
                .border_color(theme.input_border)
        };
        let list = if skills.is_empty() {
            frame()
                .debug_selector(|| "agent-skills-empty".into())
                .gap(px(8.0))
                .p(px(16.0))
                .child(div().type_style(Type::BaseMedium).text_color(theme.foreground).child("No skills added"))
                .child(footnote("Add reusable instructions for tasks this agent performs often.", theme))
        } else {
            let count = skills.len();
            frame().debug_selector(|| "agent-skills".into()).children(skills.into_iter().enumerate().map(|(i, skill)| {
                let name = skill.name.clone();
                let selector = format!("agent-skill-{name}");
                let open = {
                    let name = name.clone();
                    row_button(format!("agent-skill-open-{name}"), icons::PENCIL_BUTTON, false, theme)
                        .tooltip(style::tooltip("Open SKILL.md in your editor"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.leave(Leave::Open(AgentFile::Skill(name.clone())), Some(window), cx)
                        }))
                };
                let remove = row_button(format!("agent-skill-remove-{name}"), icons::TRASH_BUTTON, false, theme)
                    .tooltip(style::tooltip("Remove skill"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.change(Change::RemoveSkill(name.clone()), window, cx);
                    }));
                let error = state.error_at(&Field::Skill(skill.name.clone()));
                div()
                    .debug_selector(move || selector.clone())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(16.0))
                    .py(px(12.0))
                    .when(i + 1 < count, |d| d.border_b_1().border_color(theme.input_border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(div().type_style(Type::BaseMedium).text_color(theme.foreground).child(skill.name.clone()))
                            .children(skill.description.clone().map(|d| footnote(d, theme)))
                            .children(error.map(|e| error_line(format!("agent-skill-error-{}", skill.name), e, theme))),
                    )
                    .child(open)
                    .child(remove)
            }))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(header)
            .children(self.skill_form.as_ref().map(|f| self.render_skill_form(f, theme, window, cx)))
            .child(list)
    }

    /// The Add skill form: a name and a description over the SKILL.md body,
    /// Import (an existing skill), Cancel and Add skill.
    fn render_skill_form(&self, form: &SkillForm, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let import = style::button("agent-skill-import", "Import…", ButtonKind::Ghost, ButtonSize::Small, false, theme)
            .tooltip(style::tooltip("Copy a skill folder (with its SKILL.md) or a .md file"))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.import_skill(window, cx)));
        let cancel = style::button("agent-skill-cancel", "Cancel", ButtonKind::Ghost, ButtonSize::Small, false, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_skill_form(cx)));
        let add = style::button("agent-skill-add", "Add skill", ButtonKind::Primary, ButtonSize::Small, false, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.add_skill(window, cx)));
        div()
            .debug_selector(|| "agent-skill-form".into())
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .p(px(16.0))
            .rounded(px(SKILLS_RADIUS))
            .border_1()
            .border_color(theme.input_border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(FIELD_GAP))
                    .child(
                        labeled("Name", text_field(&form.name, "agent-skill-name", Some("release-notes".into()), false, window, cx), theme)
                            .w(px(NAME_W))
                            .flex_none(),
                    )
                    .child(
                        labeled(
                            "Description",
                            text_field(
                                &form.description,
                                "agent-skill-description",
                                Some("When the agent should use this skill".into()),
                                false,
                                window,
                                cx,
                            ),
                            theme,
                        )
                        .flex_1()
                        .min_w_0(),
                    ),
            )
            .child(labeled("Instructions", markdown_area(&form.body, "agent-skill-body", SKILL_BODY_H, window, cx), theme))
            .children(state.error_at(&Field::NewSkill).map(|e| error_line("agent-skill-form-error".into(), e, theme)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(import)
                    .child(div().flex_1())
                    .child(cancel)
                    .child(add),
            )
    }

    /// Runner preferences: the four pickers and the footnote.
    fn render_runner(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let agent = state.agent();
        // The shared choices: (key, label, the change, selected).
        let items = |choices: Vec<shelbi_app::settings::agent_editor::Choice>| -> Vec<(String, String, Change, bool)> {
            choices.into_iter().map(|c| (c.key, c.label, c.change, c.current)).collect()
        };
        let current = |choices: &[shelbi_app::settings::agent_editor::Choice], fallback: String| {
            choices.iter().find(|c| c.current).map_or(fallback, |c| c.label.clone())
        };

        let runners = state.runner_choices();
        let runner_picker = self.picker(
            Menu::Runner,
            "agent-runner",
            current(&runners, agent.preferred_runner().map_or("Project default", runner_label).to_string()),
            items(runners),
            None,
            theme,
            cx,
        );
        let blocked = state.options_blocked();
        let models = state.model_choices();
        let model_picker = self.picker(
            Menu::Model,
            "agent-model",
            current(&models, state.model().unwrap_or("Use runner default").to_string()),
            items(models),
            blocked.clone(),
            theme,
            cx,
        );
        let efforts = state.effort_choices();
        let effort_picker = self.picker(
            Menu::Effort,
            "agent-effort",
            current(&efforts, "Use runner default".into()),
            items(efforts),
            blocked,
            theme,
            cx,
        );
        let modes = state.permission_choices();
        let permissions_picker = self.picker(
            Menu::Permissions,
            "agent-permissions",
            current(&modes, "Use project settings".into()),
            items(modes),
            None,
            theme,
            cx,
        );
        let note = state.runner_footnote();
        let field = |label: &'static str, picker: Div, field: Field, selector: &'static str| {
            labeled(label, picker, theme)
                .w_full()
                .children(state.error_at(&field).map(|e| error_line(selector.into(), e, theme)))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .w_full()
            .child(section_title("Runner preferences", theme))
            .child(footnote("Choose the runner and defaults recommended for this role.", theme))
            .child(field("Preferred runner", runner_picker, Field::Runner, "agent-runner-error"))
            .child(field("Model", model_picker, Field::Model, "agent-model-error"))
            .child(field("Reasoning effort", effort_picker, Field::Effort, "agent-effort-error"))
            .child(field("Permissions", permissions_picker, Field::Permissions, "agent-permissions-error"))
            .child(footnote(note, theme).debug_selector(|| "agent-runner-note".into()))
    }

    /// A Runner preferences picker: its button at the column's width and,
    /// open, its `items` (key, label, the change, selected). `blocked`
    /// dims it with the reason as a tooltip.
    #[allow(clippy::too_many_arguments)]
    fn picker(
        &self,
        menu: Menu,
        id: &'static str,
        label: String,
        items: Vec<(String, String, Change, bool)>,
        blocked: Option<String>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let button = select_button(id, label, None, style::DROPDOWN_CHEVRON, theme);
        if let Some(why) = blocked {
            return div().w_full().child(button.opacity(0.4).cursor_default().tooltip(style::tooltip(why)));
        }
        let menu_id: &'static str = match menu {
            Menu::Runner => "agent-runner-menu",
            Menu::Model => "agent-model-menu",
            Menu::Effort => "agent-effort-menu",
            Menu::Permissions => "agent-permissions-menu",
            Menu::More => "agent-editor-menu",
        };
        dropdown(
            self.menu == Some(menu),
            &self.menu_anchor,
            menu_id,
            button,
            move |cx| {
                let theme = *cx.global::<Theme>();
                items
                    .into_iter()
                    .map(|(key, label, change, selected)| {
                        menu_item(format!("{id}-{key}"), label, selected, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.change(change.clone(), window, cx);
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            SIDE_W,
            false,
            move |this: &mut Self, cx| this.toggle_menu(menu, cx),
            |this: &mut Self, _| this.menu = None,
            cx,
        )
        .w_full()
    }

    /// Workflow assignments: a link row per status that runs the agent
    /// (each opens its workflow's editor), then the footnote.
    fn render_assignments(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let assignments = state.assignments();
        let rows: Vec<AnyElement> = if assignments.is_empty() {
            vec![footnote("Not assigned to any workflow status.", theme)
                .debug_selector(|| "agent-assignments-none".into())
                .into_any_element()]
        } else {
            assignments
                .into_iter()
                .map(|a| {
                    let leave = Leave::Workflow { name: a.workflow.clone(), status: Some(a.status.clone()) };
                    select_button(
                        format!("agent-assignment-{}-{}", a.workflow, a.status),
                        format!("{} / {}", a.workflow, a.status_name),
                        a.category,
                        icons::CHEVRON_RIGHT,
                        theme,
                    )
                    .tooltip(style::tooltip(format!("Open the {} workflow", a.workflow)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.leave(leave.clone(), Some(window), cx)
                    }))
                    .into_any_element()
                })
                .collect()
        };
        let workflows = link("agent-workflows-link", "Workflows")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.leave(Leave::Workflows, Some(window), cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(section_title("Workflow assignments", theme))
            .child(footnote("This agent runs when a task enters the assigned status.", theme))
            .children(rows)
            .child(
                footnote("Manage assignments in ", theme)
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .child(workflows)
                    .child("."),
            )
    }

    /// The leave prompt: save or discard the unsaved edits, or stay.
    fn render_prompt(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = self.state.as_ref().map(|s| s.name().to_string()).unwrap_or_default();
        let buttons = vec![
            style::button("agent-leave-cancel", "Cancel", ButtonKind::Ghost, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(state) = this.state.as_mut() {
                        state.cancel_prompt();
                    }
                    cx.notify();
                }))
                .into_any_element(),
            style::button("agent-leave-discard", "Discard", ButtonKind::Secondary, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let then = this.state.as_mut().and_then(AgentEditorState::discard_and_leave);
                    this.skill_form = None;
                    this.sync(true, window, cx);
                    if let Some(then) = then {
                        this.go(then, window, cx);
                    }
                    cx.notify();
                }))
                .into_any_element(),
            style::button("agent-leave-save", "Save", ButtonKind::Primary, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let texts = this.texts(cx);
                    let job = this.state.as_mut().and_then(|s| s.save_and_leave(texts));
                    this.run_save(job, window, cx);
                }))
                .into_any_element(),
        ];
        dialog_overlay(
            "agent-leave-prompt",
            format!("Save changes to {name}?"),
            "Your edits to this agent haven’t been saved.",
            buttons,
            theme,
        )
        .into_any_element()
    }

    /// Delete's confirm: what's lost, Cancel and Delete.
    fn render_confirm_delete(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = self.state.as_ref().map(|s| s.name().to_string()).unwrap_or_default();
        let buttons = vec![
            style::button("agent-delete-cancel", "Cancel", ButtonKind::Ghost, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(state) = this.state.as_mut() {
                        state.cancel_delete();
                    }
                    cx.notify();
                }))
                .into_any_element(),
            style::button("agent-delete-confirm", "Delete", ButtonKind::Danger, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.delete(window, cx)))
                .into_any_element(),
        ];
        dialog_overlay(
            "agent-delete-prompt",
            format!("Delete {name}?"),
            "Its instructions and skills are deleted with it. This can’t be undone.",
            buttons,
            theme,
        )
        .into_any_element()
    }
}
