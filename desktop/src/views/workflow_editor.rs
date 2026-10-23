//! The workflow editor (`dt-wf-editor-form`): one workflow, full window,
//! without the sidebar, as the Workflow Builder (2305:10965) and Workflow
//! Form (2252:29281) frames draw it. Settings ▸ Workflows opens it (a row's
//! click, its edit button, a workflow just created); the window hides the
//! settings sidebar while it shows.
//!
//! **The titlebar row** (the frames' Header): room for the traffic lights,
//! Back, the workflow's name with its Default badge, the Builder | Form
//! segmented control in the middle, then Discard changes (only with unsaved
//! edits), Save changes (disabled while there are none) and a ⋮ menu with
//! Edit YAML, Duplicate, Set as default and Delete.
//!
//! **The Form** (`4-workflow-form.png`, scrolling): Name, Description and
//! Initial status; the Statuses table (a drag handle, the status, its Owner
//! and Agent pickers, its Workspace tags and delete) with Manage project
//! statuses and Add status; the Transitions (From, To, the actions as chips
//! with a menu each and a + to add one, Details, delete) with Add
//! transition; Git (Base branch, Branch name, Merge strategy); Review (the
//! review mode, and its recipe's fields when it runs the branch);
//! Automation (Zen settings, Required task fields); and Delete workflow.
//! **The Builder** (`workflow_builder`, `dt-wf-editor-builder`) draws the
//! same draft as a graph; the editor opens on it.
//!
//! Every field edits a draft ([`EditorState`]) through
//! `shelbi_state::workflow_config`; nothing is written until Save. Pickers
//! apply when picked, text fields as typed (Name at Save, since a rename
//! reads the board). A change the model refuses shows its message under the
//! field or row it concerns. Discard reverts; Back, Esc, Manage project
//! statuses, Duplicate and Edit YAML ask to save or discard unsaved edits
//! first.
//!
//! **Edit YAML** opens the workflow's file in the user's editor. When the
//! window comes back to the front, the file is read again into a clean
//! draft; a draft edited meanwhile is kept, and its save is refused should
//! it overwrite the hand edit (the model's check for files changed on disk).
//!
//! A transition's **Details** open under its row on every row, empty or
//! not: the keys the row can't show (`target`, `run`, `ready`,
//! `ready_timeout`, `url`). The button takes the Select style when the
//! transition sets any of them.

use std::collections::HashMap;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    MouseDownEvent, Pixels, Point, ScrollHandle, SharedString, Stateful, Subscription, Window,
};
use gpui_kit::base::input::{Textarea, TextareaState};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use shelbi_core::{Owner, StatusCategory, TransitionAction};
use shelbi_state::workflow_config::WorkflowConfig;

use super::workflow_editor_state::{
    action_label, owner_label, run_delete, run_duplicate, run_save, strategy_label, Change, DetailsForm, EditorState,
    Field, GitForm, Leave, Mode, ProjectGit, ReviewForm, ReviewMode, SaveJob, SaveOutcome, ACTIONS, STRATEGIES,
};
use super::sidebar;
use super::workflow_builder_state::BuilderState;
use super::workflow_graph::Pt;
use super::workflows_state::category_label;
use crate::assets::{glyph, glyphs, heroicon, icons, Icon};
use crate::elements::controls;
use crate::theme::motion::fill_layer;
use crate::theme::style::{self, ButtonContent, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The titlebar row (the Header component): 52pt, its content starting
/// beside the traffic lights ([`sidebar::BESIDE_TRAFFIC_LIGHTS`]).
pub(super) const TITLEBAR_H: f32 = 52.0;
/// The Header's toolbar buttons (Back, ⋮).
const TOOLBAR_BUTTON: f32 = 32.0;
/// The form's inset in the window and the gap between its sections.
const INSET: f32 = 32.0;
const SECTION_GAP: f32 = 24.0;
/// The identity and Git rows' narrow fields (Name, Initial status, Base
/// branch, Merge strategy), and the gap between a row's fields.
const FIELD_W: f32 = 200.0;
const FIELD_GAP: f32 = 16.0;
/// Review mode and Zen settings.
const WIDE_PICKER_W: f32 = 320.0;
/// The status table's columns (`Workflow Status Row`, 2276:1492).
const HANDLE_W: f32 = 24.0;
const STATUS_W: f32 = 264.0;
const OWNER_W: f32 = 160.0;
const AGENT_W: f32 = 224.0;
/// A transition row's From and To pickers, its arrow, and its Details
/// button's column (`Workflow Transition Row`, 2277:1519).
const ENDPOINT_W: f32 = 200.0;
const ARROW_W: f32 = 16.0;
const DETAILS_W: f32 = 67.0;
/// A table row's buttons (delete, add an action).
const ROW_BUTTON: f32 = 24.0;
/// A menu's width, and its gap under its button.
pub(super) const MENU_W: f32 = 200.0;
pub(super) const MENU_GAP: f32 = 6.0;
/// The leave prompt's width.
pub(super) const PROMPT_W: f32 = 400.0;

/// What the editor tells the settings view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorEvent {
    /// Back to Settings ▸ Workflows.
    Close,
    /// Back to the Workflows page's Statuses.
    ManageStatuses,
}

/// The menu that's open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Menu {
    More,
    InitialStatus,
    AddStatus,
    Owner(String),
    Agent(String),
    From(usize),
    To(usize),
    /// Transition `.0`'s action chip at `.1`.
    Action(usize, usize),
    AddAction(usize),
    MergeStrategy,
    ReviewMode,
    Zen,
    // The Builder's (`workflow_builder`).
    BuilderAddStatus,
    Zoom,
    /// The status inspector's Status picker.
    InspectStatus,
    DrawFrom,
    NewFrom,
    NewTo,
    /// New transition's action at this slot.
    NewAction(usize),
    NewAddAction,
    /// The transition inspector's action at `.1` of transition `.0`.
    BuilderAction(usize, usize),
}

/// A status row being dragged by its handle.
#[derive(Debug, Clone)]
pub struct WorkflowStatusDrag {
    pub id: String,
    name: SharedString,
}

/// The row that follows the pointer during a drag.
struct DragGhost {
    name: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        style::popover(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .h(px(32.0))
                .type_style(Type::Base)
                .text_color(theme.text_loud),
            &theme,
        )
        .child(heroicon(icons::BARS_2).text_color(theme.icon))
        .child(self.name.clone())
    }
}

/// The open Details panel's fields.
struct Details {
    index: usize,
    target: Entity<InputState>,
    run: Entity<TextareaState>,
    ready: Entity<InputState>,
    timeout: Entity<InputState>,
    url: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// The Run the branch fields, in [`ReviewForm`]'s order.
struct ReviewInputs {
    serve: Entity<InputState>,
    setup: Entity<InputState>,
    ready: Entity<InputState>,
    url: Entity<InputState>,
    workdir: Entity<InputState>,
}

/// The workflow editor.
pub struct WorkflowEditor {
    project: String,
    /// `None` until the workflow config has loaded.
    pub(super) state: Option<EditorState>,
    load_error: Option<String>,
    name: Entity<InputState>,
    description: Entity<InputState>,
    base_branch: Entity<InputState>,
    branch: Entity<InputState>,
    params: Entity<InputState>,
    review: ReviewInputs,
    /// Each status's Workspace tags field, by id (the Form's row and the
    /// Builder's inspector share it).
    pub(super) tags: HashMap<String, (Entity<InputState>, Subscription)>,
    details: Option<Details>,
    pub(super) menu: Option<Menu>,
    /// Where the open menu's button last painted (its own click toggles it).
    menu_anchor: style::Anchor,
    scroll: ScrollHandle,
    /// The Builder's selection, drawing, focus and viewport.
    pub(super) builder: BuilderState,
    /// The Builder's inspector, and the Status panel's offset kept while a
    /// transition panel that goes back to it shows.
    pub(super) inspector_scroll: ScrollHandle,
    pub(super) back_scroll: Option<Point<Pixels>>,
    /// Where the Builder's canvas and its graph frame last painted.
    pub(super) canvas_anchor: style::Anchor,
    pub(super) frame_anchor: style::Anchor,
    pub(super) canvas_focus: FocusHandle,
    /// The Builder's action rows' drag handles, by slot (a list holds each
    /// action at most once).
    pub(super) action_focus: Vec<FocusHandle>,
    /// Where a press on the canvas's background went down (a click there
    /// deselects).
    pub(super) pressed_at: Option<Pt>,
    /// Read the file again when the window is next active (Edit YAML sent
    /// the user to their editor).
    reload_on_return: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for WorkflowEditor {}

impl WorkflowEditor {
    /// Edit workflow `name` of `project`, from `config` when the caller has
    /// it loaded (else it is read off the UI thread; a test's editor reads
    /// no disk).
    pub fn new(
        project: &str,
        name: &str,
        config: Option<WorkflowConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut field = |cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx));
        let (name_input, description, base_branch, branch, params) =
            (field(cx), field(cx), field(cx), field(cx), field(cx));
        let review = ReviewInputs {
            serve: field(cx),
            setup: field(cx),
            ready: field(cx),
            url: field(cx),
            workdir: field(cx),
        };
        let mut subscriptions = vec![cx.subscribe_in(&name_input, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                if let Some(state) = this.state.as_mut() {
                    state.type_name(&text);
                }
                cx.notify();
            }
        })];
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() && std::mem::take(&mut this.reload_on_return) {
                this.read_again(window, cx);
            }
        }));
        let texts = [&description, &base_branch, &branch, &params, &review.serve, &review.setup, &review.ready, &review.url, &review.workdir];
        for input in texts {
            subscriptions.push(cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.typed(cx);
                }
            }));
        }
        let mut editor = WorkflowEditor {
            project: project.to_string(),
            state: None,
            load_error: None,
            name: name_input,
            description,
            base_branch,
            branch,
            params,
            review,
            tags: HashMap::new(),
            details: None,
            menu: None,
            menu_anchor: Default::default(),
            scroll: ScrollHandle::new(),
            builder: BuilderState::default(),
            inspector_scroll: ScrollHandle::new(),
            back_scroll: None,
            canvas_anchor: Default::default(),
            frame_anchor: Default::default(),
            canvas_focus: cx.focus_handle(),
            action_focus: (0..ACTIONS.len()).map(|_| cx.focus_handle()).collect(),
            pressed_at: None,
            reload_on_return: false,
            _subscriptions: subscriptions,
        };
        match config {
            Some(config) if config.workflow(name).is_some() => editor.show(EditorState::new(config, name), window, cx),
            Some(_) => editor.load_error = Some(format!("There is no workflow named {name}.")),
            None if cfg!(test) => editor.load_error = Some("The workflows haven’t loaded.".into()),
            None => editor.load(name.to_string(), window, cx),
        }
        // The project's own git values, the Git fields' placeholders (a
        // test's editor keeps the defaults, reading no disk).
        if !cfg!(test) {
            let project = project.to_string();
            let read = cx.background_executor().spawn(async move { ProjectGit::load(&project) });
            cx.spawn(async move |this, cx| {
                let git = read.await;
                this.update(cx, |this, cx| {
                    if let Some(state) = this.state.as_mut() {
                        state.set_project_git(git);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        editor
    }

    /// Read the workflow config off the UI thread and edit `name`.
    fn load(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project.clone();
        let read = cx.background_executor().spawn(async move { WorkflowConfig::load(&project) });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update_in(cx, |this, window, cx| {
                match config {
                    Ok(config) if config.workflow(&name).is_some() => this.show(EditorState::new(config, &name), window, cx),
                    Ok(_) => this.load_error = Some(format!("There is no workflow named {name}.")),
                    Err(e) => this.load_error = Some(format!("Couldn’t read the workflows: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Edit `state` from here on: every field shows it.
    fn show(&mut self, state: EditorState, window: &mut Window, cx: &mut Context<Self>) {
        let git = self.state.as_ref().map(|s| s.project_git().clone());
        self.state = Some(state);
        if let (Some(state), Some(git)) = (self.state.as_mut(), git) {
            state.set_project_git(git);
        }
        self.details = None;
        self.menu = None;
        self.tags.clear();
        self.builder = BuilderState::default();
        self.sync(true, window, cx);
    }

    /// Put the draft's values in the fields: all of them (`force`), or
    /// those not being typed in. Gives new statuses their tags field.
    fn sync(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let w = state.workflow();
        let git = state.git_form();
        let review = state.review_form().clone();
        let mut values = vec![
            (self.name.clone(), state.typed_name().to_string()),
            (self.description.clone(), w.description.clone().unwrap_or_default()),
            (self.base_branch.clone(), git.base_branch),
            (self.branch.clone(), git.branch),
            (self.params.clone(), state.params_text()),
            (self.review.serve.clone(), review.serve),
            (self.review.setup.clone(), review.setup),
            (self.review.ready.clone(), review.ready),
            (self.review.url.clone(), review.url),
            (self.review.workdir.clone(), review.workdir),
        ];
        let ids: Vec<String> = w.statuses.iter().map(|s| s.id.clone()).collect();
        self.tags.retain(|id, _| ids.contains(id));
        for id in &ids {
            let text = state.tags_text(id);
            if let Some((input, _)) = self.tags.get(id) {
                values.push((input.clone(), text));
                continue;
            }
            let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
            let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.typed(cx);
                }
            });
            self.tags.insert(id.clone(), (input, sub));
        }
        for (input, value) in values {
            let typing = input.read(cx).focus_handle(cx).is_focused(window);
            if (force || !typing) && input.read(cx).value() != value.as_str() {
                input.update(cx, |s, cx| s.set_value(value, window, cx));
            }
        }
        if let Some(details) = &self.details {
            let form = state.details_form(details.index);
            let fields = [
                (details.target.clone(), form.target),
                (details.ready.clone(), form.ready),
                (details.timeout.clone(), form.ready_timeout),
                (details.url.clone(), form.url),
            ];
            for (input, value) in fields {
                if input.read(cx).value() != value.as_str() {
                    input.update(cx, |s, cx| s.set_value(value, window, cx));
                }
            }
            if details.run.read(cx).value() != form.run.as_str() {
                details.run.update(cx, |s, cx| s.set_value(form.run, window, cx));
            }
        }
    }

    /// What the text fields hold, as changes (Save applies those that
    /// differ from the draft).
    fn texts(&self, cx: &App) -> Vec<Change> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let mut texts = vec![
            Change::Description(value(&self.description)),
            Change::Git(GitForm {
                base_branch: value(&self.base_branch),
                branch: value(&self.branch),
                merge_strategy: state.git_form().merge_strategy,
            }),
            Change::RequiredParams(value(&self.params)),
            Change::Review(self.review_form(cx)),
        ];
        for (id, (input, _)) in &self.tags {
            texts.push(Change::Tags {
                id: id.clone(),
                text: value(input),
            });
        }
        if let Some(details) = &self.details {
            texts.push(Change::Details {
                index: details.index,
                form: self.details_form(details, cx),
            });
        }
        texts
    }

    fn review_form(&self, cx: &App) -> ReviewForm {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        ReviewForm {
            serve: value(&self.review.serve),
            setup: value(&self.review.setup),
            ready: value(&self.review.ready),
            url: value(&self.review.url),
            workdir: value(&self.review.workdir),
        }
    }

    fn details_form(&self, details: &Details, cx: &App) -> DetailsForm {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        DetailsForm {
            target: value(&details.target),
            run: details.run.read(cx).value().to_string(),
            ready: value(&details.ready),
            ready_timeout: value(&details.timeout),
            url: value(&details.url),
        }
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

    /// Make `change` (a picker's, a button's).
    pub(super) fn change(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        // Removing a transition (or a status, with its transitions)
        // renumbers the rest: the open Details would be another's.
        if matches!(change, Change::RemoveTransition(_) | Change::RemoveStatus(_)) {
            self.details = None;
        }
        let structural = matches!(change, Change::AddStatus(_) | Change::RemoveStatus(_));
        if let Some(state) = self.state.as_mut() {
            state.change(change);
        }
        if structural {
            self.sync(false, window, cx);
        }
        cx.notify();
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = if self.menu.as_ref() == Some(&menu) { None } else { Some(menu) };
        cx.notify();
    }

    /// Builder or Form: both show the one draft, so the Form's fields take
    /// whatever the Builder changed.
    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.state.as_mut() {
            state.mode = mode;
        }
        self.menu = None;
        self.sync(false, window, cx);
        cx.notify();
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
        let reload = self.state.as_mut().is_some_and(EditorState::discard);
        self.sync(true, window, cx);
        if reload && !cfg!(test) {
            self.read_again(window, cx);
        }
        cx.notify();
    }

    /// Read the files again into the draft, unless it holds edits (or a
    /// save is running) by the time they're read. Closes when the workflow
    /// is gone from them.
    fn read_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project.clone();
        let read = cx.background_executor().spawn(async move { WorkflowConfig::load(&project) });
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
                            cx.emit(EditorEvent::Close);
                        }
                    }
                    Err(e) => this.load_error = Some(format!("Couldn’t read the workflows: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open the workflow's file (as saved) in the user's editor; the draft
    /// is read again when the window comes back to the front.
    fn edit_yaml(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let path = state.file();
        self.reload_on_return = true;
        let open = cx.background_executor().spawn(async move {
            let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            if !path.exists() {
                return Err(format!("Couldn’t find {file}."));
            }
            super::external_edit::open_file(&path).map_err(|e| format!("Couldn’t open {file}: {e}"))
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

    /// Back (or Esc): close, asking first about unsaved edits. Esc with the
    /// prompt up or a menu open closes that instead, and in the Builder
    /// steps back out of a drawing or a selection first.
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
        }
        if self.state.as_ref().is_some_and(|s| s.mode == Mode::Builder) && self.step_back() {
            cx.notify();
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
            (Some(Leave::Back), None) => cx.emit(EditorEvent::Close),
            (Some(Leave::ManageStatuses), None) => cx.emit(EditorEvent::ManageStatuses),
            (Some(Leave::Duplicate | Leave::EditYaml), None) | (None, _) => {}
        }
        cx.notify();
    }

    fn go(&mut self, then: Leave, window: &mut Window, cx: &mut Context<Self>) {
        match then {
            Leave::Back => cx.emit(EditorEvent::Close),
            Leave::ManageStatuses => cx.emit(EditorEvent::ManageStatuses),
            Leave::Duplicate => self.duplicate(window, cx),
            Leave::EditYaml => self.edit_yaml(window, cx),
        }
    }

    /// Copy the workflow as saved and edit the copy.
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
                    Ok(config) => this.show(EditorState::new(config, &copy), window, cx),
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

    /// Delete the workflow (refused while open issues use it) and close.
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
                    Ok(()) => cx.emit(EditorEvent::Close),
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

    /// Open (or close) transition `index`'s Details.
    fn toggle_details(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if self.details.as_ref().is_some_and(|d| d.index == index) {
            self.details = None;
            cx.notify();
            return;
        }
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let form = state.details_form(index);
        let mut input = |value: String, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).default_value(value));
        let target = input(form.target, cx);
        let ready = input(form.ready, cx);
        let timeout = input(form.ready_timeout, cx);
        let url = input(form.url, cx);
        let run = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 8).default_value(form.run));
        let mut subscriptions = Vec::new();
        for field in [&target, &ready, &timeout, &url] {
            subscriptions.push(cx.subscribe_in(field, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.typed(cx);
                }
            }));
        }
        subscriptions.push(cx.subscribe_in(&run, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.typed(cx);
            }
        }));
        self.details = Some(Details {
            index,
            target,
            run,
            ready,
            timeout,
            url,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Drop status `dragged` on status `onto`'s row: it takes that row's
    /// place.
    fn drop_status(&mut self, dragged: &str, onto: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.state.as_ref().and_then(|s| s.workflow().statuses.iter().position(|st| st.id == onto)) else {
            return;
        };
        if dragged != onto {
            self.change(Change::MoveStatus { id: dragged.to_string(), index }, window, cx);
        }
    }
}

// ---------------------------------------------------------------------------
// Test hooks

#[cfg(any(test, feature = "visual-check"))]
impl WorkflowEditor {
    /// Make `change` as its control would.
    pub fn make(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        self.change(change, window, cx);
    }

    /// Open transition `index`'s Details.
    #[cfg(feature = "visual-check")]
    pub fn offscreen_open_details(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_details(index, window, cx);
    }

    /// Scroll the form to its foot.
    #[cfg(feature = "visual-check")]
    pub fn offscreen_scroll_to_bottom(&mut self) {
        self.scroll.scroll_to_bottom();
    }

    /// Show Builder or Form, as the segmented control does.
    pub fn show_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(mode, window, cx);
    }

    /// The Builder's state, to put it in a frame's state.
    #[cfg(feature = "visual-check")]
    pub fn builder_mut(&mut self) -> &mut BuilderState {
        &mut self.builder
    }

    /// New transition's Add transition.
    #[cfg(feature = "visual-check")]
    pub fn add_drawn_transition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_new_transition(window, cx);
    }

    /// Fit, as its button does.
    #[cfg(feature = "visual-check")]
    pub fn fit_canvas(&mut self, cx: &mut Context<Self>) {
        self.fit(cx);
    }

    /// A zoom picker level.
    #[cfg(feature = "visual-check")]
    pub fn zoom_canvas(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.zoom_to(zoom, cx);
    }

    /// Give the Builder's canvas keyboard focus.
    #[cfg(feature = "visual-check")]
    pub fn focus_canvas(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.canvas_focus, cx);
        cx.notify();
    }

    pub fn state(&self) -> Option<&EditorState> {
        self.state.as_ref()
    }

    /// Where the Builder's graph frame last painted, and the zoom.
    #[cfg(feature = "visual-check")]
    pub fn bench_frame(&self) -> (gpui::Point<gpui::Pixels>, f32) {
        let origin = self.frame_anchor.get().map(|b| b.origin).unwrap_or_default();
        (origin, self.builder.viewport.zoom)
    }
}

#[cfg(test)]
impl WorkflowEditor {

    /// Status `id`'s Workspace tags field.
    pub fn tags_field(&self, id: &str) -> Option<Entity<InputState>> {
        self.tags.get(id).map(|(input, _)| input.clone())
    }

    pub fn description_field(&self) -> Entity<InputState> {
        self.description.clone()
    }

    pub fn name_field(&self) -> Entity<InputState> {
        self.name.clone()
    }

    pub fn base_branch_field(&self) -> Entity<InputState> {
        self.base_branch.clone()
    }

    /// The open Details' ready timeout field.
    pub fn details_timeout_field(&self) -> Option<Entity<InputState>> {
        self.details.as_ref().map(|d| d.timeout.clone())
    }

    pub fn drop_on(&mut self, dragged: &str, onto: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_status(dragged, onto, window, cx);
    }

    /// The Builder's graph of the draft, as the canvas draws it.
    #[cfg(test)]
    pub fn builder_graph(&self) -> Option<super::workflow_graph::Graph> {
        self.graph()
    }

    pub fn builder_selection(&self) -> Option<super::workflow_builder_state::Selection> {
        self.builder.selection.clone()
    }

    pub fn builder_focus(&self) -> Option<String> {
        self.builder.focus.clone()
    }

    /// The Builder inspector's scroll offset.
    pub fn inspector_offset(&self) -> gpui::Point<Pixels> {
        self.inspector_scroll.offset()
    }

}

// ---------------------------------------------------------------------------
// Rendering helpers

/// A section's title: Base semibold, loud.
pub(super) fn section_title(text: &'static str, theme: &Theme) -> Div {
    div()
        .flex_none()
        .type_style(Type::BaseSemibold)
        .text_color(theme.text_loud)
        .child(text)
}

/// A section's header: its title and count, then its buttons at the right.
fn section_header(title: &'static str, count: usize, buttons: Vec<AnyElement>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .w_full()
        .child(section_title(title, theme))
        .child(
            div()
                .flex_none()
                .type_style(Type::Small)
                .text_color(theme.form_hint)
                .child(count.to_string()),
        )
        .child(div().flex_1())
        .children(buttons)
}

/// A field's label over its control, the helper under it (the Labeled
/// Control component, without its 640pt cap: the form's fields span the
/// row).
fn labeled(label: &'static str, control: impl IntoElement, help: Option<SharedString>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(div().type_style(Type::Base).text_color(theme.text_loud).child(label))
        .child(control)
        .children(help.map(|h| controls::helper(h, theme)))
}

/// A small helper or footnote line: Small in `form_hint`.
pub(super) fn footnote(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div().type_style(Type::Small).text_color(theme.form_hint).child(text.into())
}

/// An error under the field or row it's about.
pub(super) fn error_line(selector: String, text: &str, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .type_style(Type::XSmall)
        .text_color(theme.error)
        .child(text.to_string())
}

/// A header's button: the Button at Small, with an optional left icon.
pub(super) fn header_button(id: impl Into<SharedString>, icon: Option<Icon>, label: &'static str, theme: &Theme) -> Stateful<Div> {
    style::content_button(id, ButtonContent::label(label).left(icon), ButtonKind::Secondary, ButtonSize::Small, false, theme)
}

/// A row's 24pt icon button (delete, add an action): `icon` (18pt) on the
/// overlay fill.
pub(super) fn row_button(id: impl Into<SharedString>, icon: Icon, inert: bool, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(ROW_BUTTON))
        .rounded(px(6.0));
    if inert {
        d.bg(theme.background).opacity(0.4).child(heroicon(icon).text_color(theme.icon))
    } else {
        fill_layer(d.cursor_pointer(), id, px(6.0), [theme.background, theme.background_hover, theme.background_active])
            .child(heroicon(icon).text_color(theme.icon))
    }
}

/// A toolbar button (Back, ⋮): 32pt, 8pt corners, the icon brightening
/// on hover.
pub(super) fn toolbar_button(id: &'static str, icon: Icon, theme: &Theme) -> Stateful<Div> {
    let radius = px(8.0);
    let loud = theme.text_loud;
    let d = div()
        .id(id)
        .debug_selector(move || id.into())
        .group(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(TOOLBAR_BUTTON))
        .rounded(radius)
        .cursor_pointer();
    fill_layer(d, id, radius, [theme.background.opacity(0.0), theme.background_hover, theme.background_active])
        .child(heroicon(icon).text_color(theme.icon).group_hover(id, move |s| s.text_color(loud)))
}

/// A picker's button: the Button (Small in a row, Base in the form), an
/// optional status icon, the choice, and a chevron.
pub(super) fn picker_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    icon: Option<StatusCategory>,
    small: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let size = if small { ButtonSize::Small } else { ButtonSize::Base };
    let chevron = style::button_colors(ButtonKind::Secondary, theme).icon;
    style::button_base(id, ButtonKind::Secondary, size, false, theme)
        .children(icon.map(|c| style::left_slot(size, style::status_icon_large(c, theme))))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(style::right_icon_slot(icons::CHEVRON_DOWN, size, chevron))
}

/// A popup menu's panel.
pub(super) fn popup(id: &'static str, width: f32, theme: &Theme) -> Stateful<Div> {
    style::popover(
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .flex_col()
            .p_1()
            .w(px(width))
            .occlude(),
        theme,
    )
}

/// A menu's item.
pub(super) fn menu_item(id: impl Into<SharedString>, label: impl Into<SharedString>, selected: bool, theme: &Theme) -> Stateful<Div> {
    style::menu_row(id, selected, theme).child(label.into())
}

/// A menu's rule between groups.
pub(super) fn menu_rule(theme: &Theme) -> Div {
    div().my_1().mx(px(-4.0)).h(px(1.0)).bg(theme.divider)
}

/// `button` with a menu's `items` floating under it while `open`: the menu
/// hangs left from the button's right edge when `right` (a button at the
/// window's right). `toggle` runs on the button's click, `close` on a press
/// outside the menu (one on its button toggles it closed instead).
#[allow(clippy::too_many_arguments)]
pub(super) fn dropdown<V: 'static>(
    open: bool,
    anchor: &style::Anchor,
    id: &'static str,
    button: Stateful<Div>,
    items: impl FnOnce(&mut Context<V>) -> Vec<AnyElement>,
    width: f32,
    right: bool,
    toggle: impl Fn(&mut V, &mut Context<V>) + 'static,
    close: impl Fn(&mut V, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> Div {
    let button = button
        .when(open, |d| d.child(style::anchor_probe(anchor)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            toggle(this, cx)
        }));
    let theme = *cx.global::<Theme>();
    let panel = open.then(|| {
        let anchor = anchor.clone();
        let items = items(cx);
        let top = anchor.get().map_or(px(style::BUTTON_H), |b| b.size.height) + px(MENU_GAP);
        let at = div().absolute().top(top);
        let at = if right { at.right_0() } else { at.left_0() };
        deferred(
            at.child(
                popup(id, width, &theme)
                    .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        // A press on its button toggles it closed through its click.
                        if anchor.get().is_some_and(|b| b.contains(&event.position)) {
                            return;
                        }
                        close(this, cx);
                        cx.notify();
                    }))
                    .children(items),
            ),
        )
        .with_priority(1)
    });
    div().relative().flex().flex_none().child(button).children(panel)
}

/// A single-line field: `input` in the Textbox, `placeholder` (Base,
/// `form_hint`) while it's empty, the edge in `error` when `invalid`.
pub(super) fn text_field(
    input: &Entity<InputState>,
    selector: impl Into<SharedString>,
    placeholder: Option<SharedString>,
    invalid: bool,
    window: &Window,
    cx: &App,
) -> Div {
    field(input, selector, placeholder, invalid, false, window, cx)
}

/// [`text_field`] whose text can be selected and copied but not changed.
pub(super) fn readonly_field(input: &Entity<InputState>, selector: impl Into<SharedString>, window: &Window, cx: &App) -> Div {
    field(input, selector, None, false, true, window, cx)
}

fn field(
    input: &Entity<InputState>,
    selector: impl Into<SharedString>,
    placeholder: Option<SharedString>,
    invalid: bool,
    readonly: bool,
    window: &Window,
    cx: &App,
) -> Div {
    let theme = cx.global::<Theme>();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    let empty = input.read(cx).value().is_empty();
    let content = div()
        .relative()
        .flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .items_center()
        .child(
            Input::new(input)
                .appearance(false)
                .readonly(readonly)
                .h_full()
                .px_0()
                .type_style(Type::Base)
                .text_color(theme.form_text),
        )
        .when_some(placeholder.filter(|_| empty), |d, text| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .type_style(Type::Base)
                    .text_color(theme.form_hint)
                    .child(text),
            )
        });
    let selector: SharedString = selector.into();
    let field = if invalid {
        controls::invalid_textbox(content, focused, theme)
    } else {
        controls::textbox(content, focused, theme)
    };
    field.debug_selector(move || selector.to_string()).w_full().min_w_0()
}

/// The titlebar's Discard changes: a quiet link with the discard glyph,
/// brightening on hover.
pub(super) fn discard_link(id: &'static str, theme: &Theme) -> Stateful<Div> {
    let hover = theme.text_loud;
    div()
        .id(id)
        .debug_selector(move || id.into())
        .group(id)
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .px(px(4.0))
        .h(px(18.0))
        .cursor_pointer()
        .type_style(Type::Small)
        .text_color(theme.form_hint)
        .hover(move |s| s.text_color(hover))
        .child(glyph(glyphs::DISCARD).text_color(theme.form_hint).group_hover(id, move |s| s.text_color(hover)))
        .child("Discard changes")
}

/// A dialog over the editor (the leave prompt, a confirm): `title`, `body`
/// and its `buttons` at the right, on the scrim.
pub(super) fn dialog_overlay(
    id: &'static str,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    buttons: Vec<AnyElement>,
    theme: &Theme,
) -> Div {
    let panel = style::dialog(
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w(px(PROMPT_W))
            .p(px(16.0))
            .occlude(),
        theme,
    )
    .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(title.into()))
    .child(footnote(body, theme))
    .child(div().flex().flex_row().justify_end().gap(px(8.0)).pt(px(8.0)).children(buttons));
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.scrim)
        .child(panel)
}

impl WorkflowEditor {
    /// `button` with `menu`'s items floating under it while it's open.
    pub(super) fn dropdown(
        &self,
        menu: Menu,
        id: &'static str,
        button: Stateful<Div>,
        items: impl FnOnce(&mut Context<Self>) -> Vec<AnyElement>,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.menu.as_ref() == Some(&menu);
        // Menus whose buttons sit at the window's right hang left.
        let right = matches!(menu, Menu::More | Menu::AddStatus | Menu::Zoom);
        dropdown(
            open,
            &self.menu_anchor,
            id,
            button,
            items,
            width,
            right,
            move |this: &mut Self, cx| this.toggle_menu(menu.clone(), cx),
            |this: &mut Self, _| this.menu = None,
            cx,
        )
    }

    /// [`text_field`].
    pub(super) fn text_field(
        &self,
        input: &Entity<InputState>,
        selector: impl Into<SharedString>,
        placeholder: Option<SharedString>,
        invalid: bool,
        window: &Window,
        cx: &App,
    ) -> Div {
        text_field(input, selector, placeholder, invalid, window, cx)
    }
}

impl Render for WorkflowEditor {
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
            Some(state) if state.mode == Mode::Builder => self.render_builder(&theme, window, cx).into_any_element(),
            Some(_) => self.render_form(&theme, window, cx).into_any_element(),
        };
        let prompt = self.state.as_ref().and_then(EditorState::prompt).map(|_| self.render_prompt(&theme, cx));
        div()
            .id("workflow-editor")
            .debug_selector(|| "workflow-editor".into())
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
            .children(prompt)
    }
}

impl WorkflowEditor {
    /// The titlebar row: the traffic lights' room, Back, the name and its
    /// badge; Builder | Form; Discard, Save and ⋮.
    fn render_titlebar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref();
        let name = state.map(|s| s.name().to_string()).unwrap_or_default();
        let dirty = state.is_some_and(EditorState::is_dirty);
        let busy = state.is_some_and(EditorState::busy);
        let badge = state.filter(|s| s.is_default()).map(|_| {
            div()
                .debug_selector(|| "workflow-editor-default-badge".into())
                .flex_none()
                .px(px(4.0))
                .rounded(px(2.0))
                .bg(theme.workflow_badge)
                .type_style(Type::XSmall)
                .text_color(theme.workflow_badge_text)
                .child("Default")
        });
        let back = toolbar_button("workflow-editor-back", icons::ARROW_LEFT, theme)
            .tooltip(style::tooltip("Back to Workflows"))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.leave(Leave::Back, Some(window), cx)));
        let mode = state.map_or(Mode::Form, |s| s.mode);
        let segment = |label: &'static str, value: Mode, cx: &mut Context<Self>| {
            controls::segment(format!("workflow-editor-mode-{}", label.to_lowercase()), label, mode == value, true, theme)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_mode(value, window, cx)))
        };
        let discard = dirty.then(|| {
            discard_link("workflow-editor-discard", theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.discard(window, cx)))
        });
        let save = if dirty && !busy {
            style::button("workflow-editor-save", "Save changes", ButtonKind::Primary, ButtonSize::Small, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)))
        } else {
            style::disabled_button(
                "workflow-editor-save",
                if busy { "Saving…" } else { "Save changes" },
                ButtonKind::Primary,
                ButtonSize::Small,
                theme,
            )
        };
        let more = self.dropdown(
            Menu::More,
            "workflow-editor-menu",
            toolbar_button("workflow-editor-more", icons::ELLIPSIS_VERTICAL, theme),
            |cx| self.more_items(cx),
            MENU_W,
            cx,
        );
        div()
            .debug_selector(|| "workflow-editor-titlebar".into())
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
                            .debug_selector(|| "workflow-editor-title".into())
                            .flex_none()
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud)
                            .child(name),
                    )
                    .children(badge),
            )
            .child(
                controls::segmented_track(theme)
                    .child(segment("Builder", Mode::Builder, cx))
                    .child(segment("Form", Mode::Form, cx)),
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

    /// The ⋮ menu: Edit YAML, Duplicate, Set as default, Delete (disabled,
    /// with the reason, for the default and the only workflow).
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
        let edit_yaml = menu_item("workflow-editor-edit-yaml", "Edit YAML", false, &theme).on_click(cx.listener(
            |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.leave(Leave::EditYaml, Some(window), cx)
            },
        ));
        let duplicate = menu_item("workflow-editor-duplicate", "Duplicate", false, &theme).on_click(cx.listener(
            |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.leave(Leave::Duplicate, Some(window), cx)
            },
        ));
        let default = if state.is_default() {
            inert(
                menu_item("workflow-editor-set-default", "Set as default", false, &theme),
                Some("Already the default".into()),
            )
        } else {
            menu_item("workflow-editor-set-default", "Set as default", false, &theme).on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.change(Change::SetDefault, window, cx)
                },
            ))
        };
        let blocked = state.delete_blocked();
        let delete = menu_item("workflow-editor-delete", "Delete", false, &theme).text_color(theme.error);
        let delete = if blocked.is_some() {
            inert(delete, blocked)
        } else {
            delete.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.delete(window, cx)
            }))
        };
        vec![
            edit_yaml.into_any_element(),
            duplicate.into_any_element(),
            default.into_any_element(),
            menu_rule(&theme).into_any_element(),
            delete.into_any_element(),
        ]
    }

    /// The Form, scrolling under the titlebar.
    fn render_form(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.as_ref().expect("the form draws a loaded workflow");
        let banner = state
            .error_at(&Field::Save)
            .map(|e| error_line("workflow-editor-error".into(), e, theme).into_any_element())
            .or_else(|| {
                state.notice().map(|n| {
                    div()
                        .debug_selector(|| "workflow-editor-notice".into())
                        .type_style(Type::XSmall)
                        .text_color(theme.form_hint)
                        .child(n.to_string())
                        .into_any_element()
                })
            });
        div()
            .id("workflow-form")
            .debug_selector(|| "workflow-form".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SECTION_GAP))
                    .p(px(INSET))
                    .children(banner)
                    .child(self.render_identity(theme, window, cx))
                    .child(self.render_statuses(theme, window, cx))
                    .child(self.render_transitions(theme, window, cx))
                    .child(div().h(px(1.0)).w_full().bg(theme.divider_strong))
                    .child(self.render_git(theme, window, cx))
                    .child(self.render_review(theme, window, cx))
                    .child(self.render_automation(theme, window, cx))
                    .child(self.render_management(theme, cx)),
            )
    }

    /// Name, Description and Initial status.
    fn render_identity(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let initial = state.initial_status().cloned();
        let statuses = state.workflow().statuses.clone();
        let picker = self.dropdown(
            Menu::InitialStatus,
            "workflow-initial-status-menu",
            picker_button(
                "workflow-initial-status",
                initial.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
                initial.as_ref().map(|s| s.category),
                false,
                theme,
            )
            .w(px(FIELD_W)),
            |cx| {
                let theme = *cx.global::<Theme>();
                statuses
                    .iter()
                    .map(|s| {
                        let id = s.id.clone();
                        style::menu_row(format!("workflow-initial-status-{id}"), initial.as_ref().is_some_and(|i| i.id == id), &theme)
                            .child(status_icon(s.category, &theme))
                            .child(s.name.clone())
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.change(Change::InitialStatus(id.clone()), window, cx)
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            FIELD_W,
            cx,
        );
        let name_error = state.error_at(&Field::Name);
        let field_error = |field: Field, selector: &'static str| {
            state.error_at(&field).map(|e| error_line(selector.into(), e, theme))
        };
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(FIELD_GAP))
            .w_full()
            .child(
                labeled(
                    "Name",
                    self.text_field(&self.name, "workflow-name", None, name_error.is_some(), window, cx),
                    None,
                    theme,
                )
                .w(px(FIELD_W))
                .flex_none()
                .children(name_error.map(|e| error_line("workflow-name-error".into(), e, theme))),
            )
            .child(
                labeled(
                    "Description",
                    self.text_field(&self.description, "workflow-description", Some("What this workflow is for".into()), false, window, cx),
                    None,
                    theme,
                )
                .flex_1()
                .min_w_0()
                .children(field_error(Field::Description, "workflow-description-error")),
            )
            .child(
                labeled("Initial status", picker, None, theme)
                    .w(px(FIELD_W))
                    .flex_none()
                    .children(field_error(Field::InitialStatus, "workflow-initial-status-error")),
            )
    }

    /// The Statuses section: its header, the table, the footnote.
    fn render_statuses(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let statuses = state.workflow().statuses.clone();
        let manage = header_button("workflow-manage-statuses", None, "Manage project statuses", theme).on_click(
            cx.listener(|this, _: &ClickEvent, window, cx| this.leave(Leave::ManageStatuses, Some(window), cx)),
        );
        let addable = state.addable_statuses();
        let add = self
            .dropdown(
                Menu::AddStatus,
                "workflow-add-status-menu",
                header_button("workflow-add-status", Some(icons::PLUS), "Add status", theme),
                |cx| {
                    let theme = *cx.global::<Theme>();
                    if addable.is_empty() {
                        // Nothing left to add: say so, and where new ones come from.
                        return vec![div()
                            .debug_selector(|| "workflow-add-status-none".into())
                            .px_2()
                            .py(px(6.0))
                            .type_style(Type::Small)
                            .text_color(theme.form_hint)
                            .child("Every project status is in this workflow. Manage project statuses adds more.")
                            .into_any_element()];
                    }
                    addable
                        .iter()
                        .map(|s| {
                            let id = s.id.clone();
                            style::menu_row(format!("workflow-add-status-{id}"), false, &theme)
                                .child(status_icon(s.category, &theme))
                                .child(s.name.clone())
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.change(Change::AddStatus(id.clone()), window, cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                },
                MENU_W + 40.0,
                cx,
            )
            .into_any_element();
        let headings = div()
            .flex()
            .flex_row()
            .gap(px(12.0))
            .px(px(8.0))
            .py(px(4.0))
            .type_style(Type::XSmall)
            .text_color(theme.form_hint)
            .child(div().w(px(HANDLE_W)).flex_none())
            .child(div().w(px(STATUS_W)).flex_none().child("Status"))
            .child(div().w(px(OWNER_W)).flex_none().child("Owner"))
            .child(div().w(px(AGENT_W)).flex_none().child("Agent"))
            .child(div().flex_1().min_w_0().child("Workspace tags"))
            .child(div().w(px(ROW_BUTTON)).flex_none());
        let rows: Vec<AnyElement> = statuses
            .iter()
            .map(|s| self.render_status_row(s, statuses.len(), theme, window, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(section_header(
                "Statuses",
                statuses.len(),
                vec![manage.into_any_element(), add],
                theme,
            ))
            .children(state.error_at(&Field::Statuses).map(|e| error_line("workflow-statuses-error".into(), e, theme)))
            .child(
                div()
                    .debug_selector(|| "workflow-statuses-table".into())
                    .flex()
                    .flex_col()
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme.input_border)
                    .child(headings)
                    .child(div().h(px(1.0)).bg(theme.divider_strong))
                    .children(rows),
            )
            .child(footnote(
                "Owner applies when Zen is off. Agent selects automation; tags limit eligible workspaces.",
                theme,
            ))
    }

    fn render_status_row(
        &self,
        s: &shelbi_core::WorkflowStatus,
        count: usize,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.as_ref().expect("loaded");
        let id = s.id.clone();
        let handle = {
            let drag = WorkflowStatusDrag {
                id: id.clone(),
                name: s.name.clone().into(),
            };
            div()
                .id(SharedString::from(format!("workflow-status-handle-{id}")))
                .debug_selector({
                    let id = id.clone();
                    move || format!("workflow-status-handle-{id}")
                })
                .flex()
                .flex_none()
                .justify_center()
                .w(px(HANDLE_W - 4.0))
                .cursor_grab()
                .child(heroicon(icons::BARS_2).text_color(theme.icon))
                .on_drag(drag, |drag: &WorkflowStatusDrag, _offset, _window, cx: &mut App| {
                    let name = drag.name.clone();
                    cx.new(|_| DragGhost { name })
                })
        };
        let identity = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .w(px(STATUS_W))
            .child(status_icon(s.category, theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(div().truncate().type_style(Type::BaseMedium).text_color(theme.text_loud).child(s.name.clone()))
                    .child(div().truncate().type_style(Type::XSmall).text_color(theme.form_hint).child(category_label(s.category))),
            );
        let owner = {
            let current = s.owner;
            let row = id.clone();
            self.dropdown(
                Menu::Owner(id.clone()),
                "workflow-owner-menu",
                picker_button(format!("workflow-owner-{id}"), owner_label(current), None, true, theme).w(px(OWNER_W)),
                move |cx| {
                    let theme = *cx.global::<Theme>();
                    [Owner::User, Owner::Agent]
                        .into_iter()
                        .map(|o| {
                            let row = row.clone();
                            menu_item(format!("workflow-owner-{row}-{}", owner_label(o).to_lowercase()), owner_label(o), o == current, &theme)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.change(Change::Owner { id: row.clone(), owner: o }, window, cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                },
                OWNER_W,
                cx,
            )
        };
        let agent = {
            let current = s.agent.clone();
            let row = id.clone();
            let agents = state.agents();
            self.dropdown(
                Menu::Agent(id.clone()),
                "workflow-agent-menu",
                picker_button(format!("workflow-agent-{id}"), current.clone().unwrap_or_else(|| "None".into()), None, true, theme)
                    .w(px(AGENT_W)),
                move |cx| {
                    let theme = *cx.global::<Theme>();
                    agents
                        .into_iter()
                        .map(Some)
                        .chain([None])
                        .map(|a| {
                            let row = row.clone();
                            let key = a.clone().unwrap_or_else(|| "none".into());
                            let label = a.clone().unwrap_or_else(|| "None".into());
                            menu_item(format!("workflow-agent-{row}-{key}"), label, a == current, &theme)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.change(Change::Agent { id: row.clone(), agent: a.clone() }, window, cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                },
                AGENT_W,
                cx,
            )
        };
        let tags = self
            .tags
            .get(&id)
            .map(|(input, _)| {
                self.text_field(input, format!("workflow-tags-{id}"), Some("Any workspace".into()), false, window, cx)
                    .flex_1()
            });
        let delete = {
            let only = count <= 1;
            let button = row_button(format!("workflow-status-delete-{id}"), icons::TRASH_BUTTON, only, theme);
            if only {
                button.tooltip(style::tooltip("A workflow needs at least one status"))
            } else {
                let id = id.clone();
                button
                    .tooltip(style::tooltip("Remove from this workflow"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.change(Change::RemoveStatus(id.clone()), window, cx)
                    }))
            }
        };
        let onto = id.clone();
        let hover = *theme;
        let row = div()
            .id(SharedString::from(format!("workflow-status-row-{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("workflow-status-row-{id}")
            })
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .p(px(12.0))
            .drag_over::<WorkflowStatusDrag>(move |st, _, _, _| st.bg(hover.background))
            .on_drop(cx.listener(move |this, drag: &WorkflowStatusDrag, window, cx| {
                this.drop_status(&drag.id, &onto, window, cx)
            }))
            .child(handle)
            .child(identity)
            .child(owner)
            .child(agent)
            .children(tags)
            .child(delete);
        let error = state
            .error_at(&Field::Status(id.clone()))
            .map(|e| error_line(format!("workflow-status-error-{id}"), e, theme).px(px(12.0)).pb(px(10.0)));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    /// The Transitions section.
    fn render_transitions(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let count = state.transitions().len();
        let add = header_button("workflow-add-transition", Some(icons::PLUS), "Add transition", theme).on_click(
            cx.listener(|this, _: &ClickEvent, window, cx| this.change(Change::AddTransition, window, cx)),
        );
        let headings = div()
            .flex()
            .flex_row()
            .gap(px(12.0))
            .px(px(8.0))
            .type_style(Type::XSmall)
            .text_color(theme.form_hint)
            .child(div().w(px(ENDPOINT_W)).flex_none().child("From"))
            .child(div().w(px(ARROW_W)).flex_none())
            .child(div().w(px(ENDPOINT_W)).flex_none().child("To"))
            .child(div().flex_1().min_w_0().child("Actions"))
            .child(div().w(px(DETAILS_W)).flex_none())
            .child(div().w(px(ROW_BUTTON)).flex_none());
        let rows: Vec<AnyElement> = (0..count).map(|i| self.render_transition_row(i, theme, window, cx)).collect();
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(section_header("Transitions", count, vec![add.into_any_element()], theme))
            .child(footnote(
                "Run these actions in order when a task moves between statuses. Other moves have no actions.",
                theme,
            ))
            .children(state.error_at(&Field::Transitions).map(|e| error_line("workflow-transitions-error".into(), e, theme)))
            .when(count > 0, |d| d.child(headings))
            .child(div().debug_selector(|| "workflow-transitions".into()).flex().flex_col().w_full().children(rows))
    }

    fn render_transition_row(&self, index: usize, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.as_ref().expect("loaded");
        let t = state.transitions()[index].clone();
        let statuses = state.workflow().statuses.clone();
        let endpoint = |menu: Menu, which: &'static str, current: String, cx: &mut Context<Self>| {
            let status = statuses.iter().find(|s| s.id == current).cloned();
            let list = statuses.clone();
            let make = menu.clone();
            self.dropdown(
                menu,
                "workflow-transition-status-menu",
                picker_button(
                    format!("workflow-transition-{which}-{index}"),
                    status.as_ref().map(|s| s.name.clone()).unwrap_or(current.clone()),
                    status.as_ref().map(|s| s.category),
                    true,
                    theme,
                )
                .w(px(ENDPOINT_W)),
                move |cx| {
                    let theme = *cx.global::<Theme>();
                    list.iter()
                        .map(|s| {
                            let id = s.id.clone();
                            let make = make.clone();
                            style::menu_row(format!("workflow-transition-{which}-{index}-{id}"), s.id == current, &theme)
                                .child(status_icon(s.category, &theme))
                                .child(s.name.clone())
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    let change = match make {
                                        Menu::From(index) => Change::From { index, status: id.clone() },
                                        _ => Change::To { index, status: id.clone() },
                                    };
                                    this.change(change, window, cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                },
                ENDPOINT_W,
                cx,
            )
        };
        let from = endpoint(Menu::From(index), "from", t.from.clone(), cx);
        let to = endpoint(Menu::To(index), "to", t.to.clone(), cx);
        let chips: Vec<AnyElement> = t
            .actions
            .iter()
            .enumerate()
            .map(|(slot, &action)| self.action_chip(index, slot, action, &t.actions, theme, cx).into_any_element())
            .collect();
        let missing: Vec<TransitionAction> = ACTIONS.into_iter().filter(|a| !t.actions.contains(a)).collect();
        let add_action = (!missing.is_empty()).then(|| {
            self.dropdown(
                Menu::AddAction(index),
                "workflow-add-action-menu",
                row_button(format!("workflow-add-action-{index}"), icons::PLUS_BUTTON, false, theme)
                    .tooltip(style::tooltip("Add an action")),
                move |cx| {
                    let theme = *cx.global::<Theme>();
                    missing
                        .iter()
                        .map(|&action| {
                            menu_item(format!("workflow-add-action-{index}-{action}"), action_label(action), false, &theme)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.change(Change::AddAction { index, action }, window, cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                },
                MENU_W,
                cx,
            )
        });
        let open = self.details.as_ref().is_some_and(|d| d.index == index);
        let kind = if open || state.has_details(index) { ButtonKind::Select } else { ButtonKind::Secondary };
        let details = style::button(format!("workflow-transition-details-{index}"), "Details", kind, ButtonSize::Small, false, theme)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.toggle_details(index, window, cx)));
        let delete = row_button(format!("workflow-transition-delete-{index}"), icons::TRASH_BUTTON, false, theme)
            .tooltip(style::tooltip("Delete transition"))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.change(Change::RemoveTransition(index), window, cx)));
        let row = div()
            .debug_selector(move || format!("workflow-transition-row-{index}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .p(px(12.0))
            .child(from)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(ARROW_W))
                    .child(heroicon(icons::ARROW_RIGHT_BUTTON).text_color(theme.icon)),
            )
            .child(to)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.0))
                    .children(chips)
                    .children(add_action),
            )
            .child(details)
            .child(delete);
        let error = state
            .error_at(&Field::Transition(index))
            .map(|e| error_line(format!("workflow-transition-error-{index}"), e, theme).px(px(12.0)).pb(px(10.0)));
        let panel = self
            .details
            .as_ref()
            .filter(|d| d.index == index)
            .map(|d| self.render_details(d, theme, window, cx));
        div().flex().flex_col().child(row).children(error).children(panel).into_any_element()
    }

    /// An action's chip: its name over a chevron, its menu swapping it for
    /// another action, moving it, or removing it.
    fn action_chip(
        &self,
        index: usize,
        slot: usize,
        action: TransitionAction,
        actions: &[TransitionAction],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let others: Vec<TransitionAction> = ACTIONS.into_iter().filter(|a| *a == action || !actions.contains(a)).collect();
        let last = actions.len() - 1;
        let button = style::content_button(
            format!("workflow-action-{index}-{slot}"),
            ButtonContent::label(action_label(action)).right(icons::CHEVRON_DOWN),
            ButtonKind::Secondary,
            ButtonSize::Small,
            false,
            theme,
        )
        .whitespace_nowrap();
        self.dropdown(
            Menu::Action(index, slot),
            "workflow-action-menu",
            button,
            move |cx| {
                let theme = *cx.global::<Theme>();
                let mut items: Vec<AnyElement> = others
                    .iter()
                    .map(|&a| {
                        menu_item(format!("workflow-action-{index}-{slot}-{a}"), action_label(a), a == action, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.change(Change::SetAction { index, slot, action: a }, window, cx)
                            }))
                            .into_any_element()
                    })
                    .collect();
                items.push(menu_rule(&theme).into_any_element());
                if slot > 0 {
                    items.push(
                        menu_item(format!("workflow-action-{index}-{slot}-earlier"), "Run earlier", false, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.change(Change::MoveAction { index, slot, to: slot - 1 }, window, cx)
                            }))
                            .into_any_element(),
                    );
                }
                if slot < last {
                    items.push(
                        menu_item(format!("workflow-action-{index}-{slot}-later"), "Run later", false, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.change(Change::MoveAction { index, slot, to: slot + 1 }, window, cx)
                            }))
                            .into_any_element(),
                    );
                }
                items.push(
                    menu_item(format!("workflow-action-{index}-{slot}-remove"), "Remove", false, &theme)
                        .text_color(theme.error)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.change(Change::RemoveAction { index, slot }, window, cx)
                        }))
                        .into_any_element(),
                );
                items
            },
            MENU_W,
            cx,
        )
    }

    /// A transition's Details, under its row: its target branch, the
    /// commands it runs, and its readiness check.
    fn render_details(&self, d: &Details, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let run_focused = d.run.read(cx).focus_handle(cx).is_focused(window);
        let run_empty = d.run.read(cx).value().is_empty();
        let run = div()
            .debug_selector(|| "workflow-details-run".into())
            .relative()
            .flex()
            .w_full()
            .min_h(px(56.0))
            .p(px(1.0))
            .rounded(px(6.0))
            .bg(if run_focused { theme.focus_ring } else { theme.input_border })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(5.0))
                    .bg(theme.textbox)
                    .type_style(Type::Base)
                    .text_color(theme.form_text)
                    .child(Textarea::new(&d.run))
                    .when(run_empty, |el| {
                        el.child(
                            div()
                                .absolute()
                                .left(px(8.0))
                                .top(px(6.0))
                                .text_color(theme.form_hint)
                                .child("One command per line"),
                        )
                    }),
            );
        let field = |label: &'static str, input: &Entity<InputState>, selector: &'static str, placeholder: &'static str, cx: &App| {
            labeled(label, self.text_field(input, selector, Some(placeholder.into()), false, window, cx), None, theme)
        };
        div()
            .debug_selector(|| "workflow-details".into())
            .flex()
            .flex_col()
            .gap(px(12.0))
            .mx(px(12.0))
            .mb(px(12.0))
            .p(px(16.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.input_border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(FIELD_GAP))
                    .child(field("Target branch", &d.target, "workflow-details-target", "The workflow’s base branch", cx).w(px(WIDE_PICKER_W)))
                    .child(field("URL", &d.url, "workflow-details-url", "http://localhost:$PORT", cx).flex_1().min_w_0()),
            )
            .child(labeled(
                "Run",
                run,
                Some("Commands run in order, in the task’s worktree, after the actions.".into()),
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(FIELD_GAP))
                    .child(field("Ready check", &d.ready, "workflow-details-ready", "Polled until it exits 0", cx).flex_1().min_w_0())
                    .child(field("Ready timeout (seconds)", &d.timeout, "workflow-details-timeout", "90", cx).w(px(FIELD_W))),
            )
    }

    /// Git: Base branch, Branch name, Merge strategy, over the project's
    /// values.
    fn render_git(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let project = state.project_git().clone();
        let current = state.git_form().merge_strategy;
        let strategy = self.dropdown(
            Menu::MergeStrategy,
            "workflow-merge-strategy-menu",
            picker_button("workflow-merge-strategy", strategy_label(current), None, false, theme).w(px(FIELD_W)),
            move |cx| {
                let theme = *cx.global::<Theme>();
                STRATEGIES
                    .into_iter()
                    .map(|s| {
                        menu_item(format!("workflow-merge-strategy-{}", s.as_str()), strategy_label(s), s == current, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                let form = GitForm {
                                    base_branch: this.base_branch.read(cx).value().to_string(),
                                    branch: this.branch.read(cx).value().to_string(),
                                    merge_strategy: s,
                                };
                                this.change(Change::Git(form), window, cx)
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            FIELD_W,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(section_title("Git", theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(FIELD_GAP))
                    .w_full()
                    .child(
                        labeled(
                            "Base branch",
                            self.text_field(&self.base_branch, "workflow-base-branch", Some(project.base_branch.into()), false, window, cx),
                            None,
                            theme,
                        )
                        .w(px(FIELD_W))
                        .flex_none(),
                    )
                    .child(
                        labeled(
                            "Branch name",
                            self.text_field(&self.branch, "workflow-branch-name", Some(project.branch.into()), false, window, cx),
                            None,
                            theme,
                        )
                        .flex_1()
                        .min_w_0(),
                    )
                    .child(labeled("Merge strategy", strategy, None, theme).w(px(FIELD_W)).flex_none()),
            )
            .children(state.error_at(&Field::Git).map(|e| error_line("workflow-git-error".into(), e, theme)))
            .child(footnote(
                "Use these settings when creating and merging branches for this workflow.",
                theme,
            ))
    }

    /// Review: the mode, and the recipe's fields when it runs the branch.
    fn render_review(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let mode = state.review_mode();
        let picker = self.dropdown(
            Menu::ReviewMode,
            "workflow-review-mode-menu",
            picker_button("workflow-review-mode", mode.label(), None, false, theme).w(px(WIDE_PICKER_W)),
            move |cx| {
                let theme = *cx.global::<Theme>();
                [ReviewMode::DiffOnly, ReviewMode::Serve]
                    .into_iter()
                    .map(|m| {
                        let key = if m == ReviewMode::Serve { "serve" } else { "diff" };
                        menu_item(format!("workflow-review-mode-{key}"), m.label(), m == mode, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.change(Change::ReviewMode(m), window, cx)
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            WIDE_PICKER_W,
            cx,
        );
        let field = |label: &'static str, input: &Entity<InputState>, selector: &'static str, placeholder: &'static str, cx: &App| {
            labeled(label, self.text_field(input, selector, Some(placeholder.into()), false, window, cx), None, theme)
                .flex_1()
                .min_w_0()
        };
        let recipe = (mode == ReviewMode::Serve).then(|| {
            div()
                .debug_selector(|| "workflow-review-recipe".into())
                .flex()
                .flex_col()
                .gap(px(12.0))
                .w_full()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(FIELD_GAP))
                        .child(field("Serve command", &self.review.serve, "workflow-review-serve", "npm run dev -- -p $SLOT", cx))
                        .child(field("URL", &self.review.url, "workflow-review-url", "http://localhost:$SLOT", cx)),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(FIELD_GAP))
                        .child(field("Setup", &self.review.setup, "workflow-review-setup", "npm install", cx))
                        .child(field("Ready check", &self.review.ready, "workflow-review-ready", "curl -sf http://localhost:$SLOT", cx))
                        .child(field("Working directory", &self.review.workdir, "workflow-review-workdir", "The worktree’s root", cx)),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(section_title("Review", theme))
            .child(labeled("Review mode", picker, Some(mode.help().into()), theme).w(px(WIDE_PICKER_W)))
            .children(recipe)
            .children(state.error_at(&Field::Review).map(|e| error_line("workflow-review-error".into(), e, theme)))
    }

    /// Automation: Zen settings and Required task fields.
    fn render_automation(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let custom = state.zen_override();
        let (label, help): (SharedString, SharedString) = match &custom {
            None => (
                "Use project settings".into(),
                "Inherit checks, CI timeout, and protected paths from this project.".into(),
            ),
            Some(what) => (
                "Custom".into(),
                format!("This workflow sets its own {what} in its YAML file.").into(),
            ),
        };
        let has_custom = custom.is_some();
        let zen = self.dropdown(
            Menu::Zen,
            "workflow-zen-menu",
            picker_button("workflow-zen", label, None, false, theme).w(px(WIDE_PICKER_W)),
            move |cx| {
                let theme = *cx.global::<Theme>();
                let mut items = vec![menu_item("workflow-zen-project", "Use project settings", !has_custom, &theme)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.change(Change::InheritZen, window, cx)))
                    .into_any_element()];
                if has_custom {
                    items.push(
                        menu_item("workflow-zen-custom", "Custom", true, &theme)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.menu = None;
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
                items
            },
            WIDE_PICKER_W,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(section_title("Automation", theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(FIELD_GAP))
                    .w_full()
                    .child(
                        labeled("Zen settings", zen, Some(help), theme)
                            .w(px(WIDE_PICKER_W))
                            .flex_none()
                            .children(state.error_at(&Field::Zen).map(|e| error_line("workflow-zen-error".into(), e, theme))),
                    )
                    .child(
                        labeled(
                            "Required task fields",
                            self.text_field(&self.params, "workflow-required-params", Some("None".into()), false, window, cx),
                            Some("Require these fields before a task can use this workflow.".into()),
                            theme,
                        )
                        .flex_1()
                        .min_w_0()
                        .children(
                            state
                                .error_at(&Field::RequiredParams)
                                .map(|e| error_line("workflow-required-params-error".into(), e, theme)),
                        ),
                    ),
            )
    }

    /// Delete workflow, at the form's foot.
    fn render_management(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let blocked = state.delete_blocked();
        let button = match blocked {
            Some(why) => style::disabled_button("workflow-delete", "Delete workflow", ButtonKind::Danger, ButtonSize::Base, theme)
                .tooltip(style::tooltip(why)),
            None => style::button("workflow-delete", "Delete workflow", ButtonKind::Danger, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.delete(window, cx))),
        };
        div().flex().flex_row().w_full().child(button)
    }

    /// The leave prompt: save or discard the unsaved edits, or stay.
    fn render_prompt(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.state.as_ref().map(|s| s.name().to_string()).unwrap_or_default();
        let buttons = vec![
            style::button("workflow-leave-cancel", "Cancel", ButtonKind::Quiet, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(state) = this.state.as_mut() {
                        state.cancel_prompt();
                    }
                    cx.notify();
                }))
                .into_any_element(),
            style::button("workflow-leave-discard", "Discard", ButtonKind::Secondary, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let then = this.state.as_mut().and_then(EditorState::discard_and_leave);
                    this.sync(true, window, cx);
                    if let Some(then) = then {
                        this.go(then, window, cx);
                    }
                    cx.notify();
                }))
                .into_any_element(),
            style::button("workflow-leave-save", "Save", ButtonKind::Primary, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let texts = this.texts(cx);
                    let job = this.state.as_mut().and_then(|s| s.save_and_leave(texts));
                    this.run_save(job, window, cx);
                }))
                .into_any_element(),
        ];
        dialog_overlay(
            "workflow-leave-prompt",
            format!("Save changes to {name}?"),
            "Your edits to this workflow haven’t been saved.",
            buttons,
            theme,
        )
    }
}

/// A status's board icon in the rows' 20pt frame.
pub(super) fn status_icon(category: StatusCategory, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(20.0))
        .child(style::status_icon_large(category, theme))
}
