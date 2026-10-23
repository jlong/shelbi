//! Settings ▸ Workflows (`dt-wf-settings-index`, Figma frame 2301:5824):
//! the project's workflows and its status catalog, edited in place.
//!
//! Top to bottom: the Workflows header (New workflow) over a table of the
//! project's workflows (the name with a Default badge and the description,
//! the status and transition counts, an edit button and a ⋮ menu with
//! Edit YAML, Duplicate, Set as default and Delete; a file that can't be
//! read shows its error, and a ⋮ menu with Edit YAML only); a rule; the Statuses header (Edit
//! YAML, New status) over the catalog (a drag handle, the status's board
//! icon, its name in a textbox, its id, a Category picker, the workflows it
//! is used in, and delete); the footnote; then the six categories' legend.
//!
//! Every change goes through `shelbi_state::workflow_config` and saves as
//! it is made ([`WorkflowsState`]): a name on Enter or when its field loses
//! focus, a category when picked, an order when a row is dropped. Errors
//! show under the row they're about. Delete is disabled up front for the
//! default and the only workflow, and for a status a workflow uses (its
//! tooltip names them); deleting a workflow that open issues use is refused
//! when chosen (that needs the board).
//!
//! New workflow and Duplicate type the name into a new row at the bottom of
//! the table; so does New status (with its category), which shows the id it
//! will get. A new workflow opens in its editor once created. The edit
//! button and a row click ask the window to open a workflow's editor
//! ([`WorkflowsEvent::Edit`]).
//!
//! Both Edit YAML items open their file in the user's editor; when the
//! window comes back to the front after one, the files are read again to
//! show the hand edit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    deferred, div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, Focusable, FontWeight,
    SharedString, Stateful, Subscription, Window,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use shelbi_core::StatusCategory;
use shelbi_state::workflow_config::{StatusSummary, WorkflowConfig, WorkflowSummary};

use super::workflows_state::{
    category_help, category_label, commit, Committed, Draft, Edit, Target, WorkflowsState, CATEGORIES,
};
use crate::assets::{heroicon, icons, Icon};
use crate::elements::controls;
use crate::elements::terminal::metrics::PRIMARY_FAMILY;
use crate::theme::motion::fill_layer;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The page's titles: `Desktop/Heading`, 18 / 28 semibold.
const TITLE_SIZE: f32 = 18.0;
/// The workflow table's count columns and its buttons' column.
const COUNT_W: f32 = 96.0;
const ACTIONS_W: f32 = 188.0;
/// The status table's columns (`Status Row`, 2285:7121): the name column
/// (the handle, the icon and the textbox), the id and the category.
const NAME_COL_W: f32 = 316.0;
const IDENTITY_W: f32 = 280.0;
const ID_W: f32 = 200.0;
const CATEGORY_W: f32 = 200.0;
/// A row's 24pt icon buttons (edit, ⋮, delete).
pub(super) const ROW_BUTTON: f32 = 24.0;
/// The status icon's frame.
const STATUS_ICON_BOX: f32 = 20.0;
/// The ⋮ menu's and the category picker's widths.
const MENU_W: f32 = 200.0;
/// A menu's gap under its button.
pub(super) const MENU_GAP: f32 = 6.0;

/// What the page tells the settings view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowsEvent {
    /// Open workflow `name`'s editor.
    Edit(String),
}

/// The menu that's open.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Menu {
    /// A status's category picker; `None` is the new status row's.
    Category(Option<String>),
    /// A workflow's ⋮ menu.
    Workflow(String),
}

/// A status row being dragged by its handle.
#[derive(Debug, Clone)]
pub struct StatusDrag {
    pub id: String,
    name: SharedString,
}

/// The row that follows the pointer during a drag.
struct StatusGhost {
    name: SharedString,
}

impl Render for StatusGhost {
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

/// The Workflows page.
pub struct WorkflowsPage {
    state: WorkflowsState,
    /// Each status's name field, by id.
    names: HashMap<String, (Entity<InputState>, Subscription)>,
    /// The new row's name field (a workflow's or a status's).
    draft_input: Entity<InputState>,
    menu: Option<Menu>,
    /// Where the open menu's button last painted (its own click toggles it).
    menu_anchor: style::Anchor,
    /// Re-read the files on the next draw (the page was shown again).
    reload: bool,
    /// Re-read the files when the window is next active (an Edit YAML
    /// sent the user to their editor).
    reload_on_return: bool,
    /// Where the Statuses section last painted (the editor's Manage project
    /// statuses scrolls it into view).
    statuses_anchor: style::Anchor,
    _subscriptions: Vec<Subscription>,
}

impl WorkflowsPage {
    pub fn new(project: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let draft_input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&draft_input, window, |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.submit_draft(window, cx),
                    // The new status row shows the id its name would get.
                    InputEvent::Change => cx.notify(),
                    _ => {}
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() && std::mem::take(&mut this.reload_on_return) {
                    this.load(window, cx);
                }
            }),
        ];
        let mut page = WorkflowsPage {
            state: WorkflowsState::new(project),
            names: HashMap::new(),
            draft_input,
            menu: None,
            menu_anchor: Default::default(),
            reload: false,
            reload_on_return: false,
            statuses_anchor: Default::default(),
            _subscriptions: subscriptions,
        };
        // A test's page never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.load(window, cx);
        }
        page
    }

    /// Read the project's workflow config off the UI thread (again, to pick
    /// up hand edits) and show it.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.state.project().to_string();
        let read = cx
            .background_executor()
            .spawn(async move { WorkflowConfig::load(&project).map_err(|e| e.to_string()) });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update_in(cx, |this, window, cx| {
                if this.state.saving() {
                    // A save in flight lands its own fresh model.
                    return;
                }
                this.state.loaded(config);
                this.sync_names(None, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the files when next drawn, to pick up hand edits (the page
    /// was shown again). A test's page reads no disk unless told to.
    pub fn reload_when_drawn(&mut self) {
        self.reload = !cfg!(test);
    }

    /// The page's state.
    pub fn state(&self) -> &WorkflowsState {
        &self.state
    }

    /// Where the Statuses section painted, since
    /// [`forget_statuses_bounds`](Self::forget_statuses_bounds).
    pub fn statuses_bounds(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.statuses_anchor.get()
    }

    /// Wait for the Statuses section's next paint.
    pub fn forget_statuses_bounds(&mut self) {
        self.statuses_anchor.set(None);
    }

    /// Status `id`'s name field.
    #[cfg(test)]
    pub fn name_field(&self, id: &str) -> Option<Entity<InputState>> {
        self.names.get(id).map(|(input, _)| input.clone())
    }

    /// The new row's name field.
    #[cfg(test)]
    pub fn draft_input(&self) -> Entity<InputState> {
        self.draft_input.clone()
    }

    /// Drop status `dragged` on status `onto`'s row.
    #[cfg(test)]
    pub fn drop_on(&mut self, dragged: &str, onto: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_status(dragged, onto, window, cx);
    }

    /// Give each status a name field showing its name: new ones for new
    /// statuses, the name put back in any that isn't being typed in (or in
    /// `reset`'s, whose edit was refused).
    fn sync_names(&mut self, reset: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let statuses = self.state.statuses();
        self.names.retain(|id, _| statuses.iter().any(|s| &s.id == id));
        for status in statuses {
            if !self.names.contains_key(&status.id) {
                let input = cx.new(|cx| InputState::new(window, cx).default_value(status.name.clone()));
                let id = status.id.clone();
                let sub = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.rename(&id, window, cx);
                    }
                });
                self.names.insert(status.id.clone(), (input, sub));
                continue;
            }
            let input = self.names[&status.id].0.clone();
            let typing = input.read(cx).focus_handle(cx).is_focused(window);
            let forced = reset == Some(status.id.as_str());
            if (forced || !typing) && input.read(cx).value() != status.name.as_str() {
                input.update(cx, |s, cx| s.set_value(status.name.clone(), window, cx));
            }
        }
    }

    /// Make `edit` and save it (after the save in flight, if any).
    fn request(&mut self, edit: Edit, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(job) = self.state.request(edit) {
            self.start(job, window, cx);
        }
        cx.notify();
    }

    /// Save one edit off the UI thread; when it lands, start the next.
    fn start(&mut self, (config, edit): (WorkflowConfig, Edit), window: &mut Window, cx: &mut Context<Self>) {
        let project = self.state.project().to_string();
        let save = cx
            .background_executor()
            .spawn(async move { commit(&project, &config, edit) });
        cx.spawn_in(window, async move |this, cx| {
            let done = save.await;
            this.update_in(cx, |this, window, cx| this.committed(done, window, cx)).ok();
        })
        .detach();
    }

    fn committed(&mut self, done: Committed, window: &mut Window, cx: &mut Context<Self>) {
        let created = match (&done.edit, &done.error) {
            (Edit::CreateWorkflow { name }, None) => Some(name.clone()),
            _ => None,
        };
        let refused = match (&done.edit, &done.error) {
            (Edit::RenameStatus { id, .. }, Some(_)) => Some(id.clone()),
            _ => None,
        };
        let next = self.state.committed(done);
        self.sync_names(refused.as_deref(), window, cx);
        if let Some(job) = next {
            self.start(job, window, cx);
        }
        if let Some(name) = created {
            cx.emit(WorkflowsEvent::Edit(name));
        }
        cx.notify();
    }

    /// A name field was left (or Enter pressed in it): save a changed name.
    fn rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((input, _)) = self.names.get(id) else {
            return;
        };
        let typed = input.read(cx).value().trim().to_string();
        let current = self.state.statuses().into_iter().find(|s| s.id == id).map(|s| s.name);
        if current.as_deref().is_some_and(|name| name != typed) {
            self.request(
                Edit::RenameStatus {
                    id: id.to_string(),
                    name: typed,
                },
                window,
                cx,
            );
        }
    }

    /// Open the new row: `draft`, its field holding `text`, focused.
    fn open_draft(&mut self, draft: Draft, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.state.draft = Some(draft);
        self.menu = None;
        self.draft_input.update(cx, |s, cx| s.set_value(text, window, cx));
        let focus = self.draft_input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn close_draft(&mut self, cx: &mut Context<Self>) {
        self.state.draft = None;
        if matches!(self.menu, Some(Menu::Category(None))) {
            self.menu = None;
        }
        cx.notify();
    }

    /// Enter in the new row: create it.
    fn submit_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.draft_input.read(cx).value().trim().to_string();
        let edit = match self.state.draft.clone() {
            None => return,
            Some(_) if name.is_empty() => return,
            Some(Draft::Workflow { from: None }) => Edit::CreateWorkflow { name },
            Some(Draft::Workflow { from: Some(from) }) => Edit::DuplicateWorkflow { from, name },
            Some(Draft::Status { category }) => Edit::AddStatus { name, category },
        };
        self.request(edit, window, cx);
    }

    fn drop_status(&mut self, dragged: &str, onto: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = self.state.drop_status(dragged, onto) {
            self.request(edit, window, cx);
        }
    }

    /// Pick `category` in the open category picker.
    fn pick_category(&mut self, target: Option<String>, category: StatusCategory, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match target {
            Some(id) => self.request(Edit::SetCategory { id, category }, window, cx),
            None => {
                self.state.draft = Some(Draft::Status { category });
                cx.notify();
            }
        }
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = if self.menu.as_ref() == Some(&menu) { None } else { Some(menu) };
        cx.notify();
    }

    /// Open `statuses.yaml` in the user's editor (writing the shipped
    /// catalog first, should the project have none yet).
    fn edit_yaml(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.state.config().map(|c| c.statuses_path().to_path_buf()) else {
            return;
        };
        let project = self.state.project().to_string();
        let prepare = move |path: &Path| {
            if !path.exists() {
                shelbi_state::scaffold_project_statuses(&project)
                    .map_err(|e| format!("Couldn't write statuses.yaml: {e}"))?;
            }
            Ok(())
        };
        self.open_yaml(path, Target::Page, prepare, window, cx);
    }

    /// Open workflow `name`'s file in the user's editor (its ⋮ menu's Edit
    /// YAML), one that can't be read too: that's how it gets fixed.
    fn edit_workflow_yaml(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(path) = self.state.config().map(|c| c.workflow_path(name)) else {
            return;
        };
        let file = format!("{name}.yaml");
        let prepare = move |path: &Path| {
            if path.exists() {
                Ok(())
            } else {
                Err(format!("Couldn't find {file}."))
            }
        };
        self.open_yaml(path, Target::Workflow(name.to_string()), prepare, window, cx);
        cx.notify();
    }

    /// Open `path` in the user's editor off the UI thread, once `prepare`
    /// has it ready; an error shows at `target`. The files are read again
    /// when the window comes back to the front.
    fn open_yaml(
        &mut self,
        path: PathBuf,
        target: Target,
        prepare: impl FnOnce(&Path) -> Result<(), String> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reload_on_return = true;
        let open = cx.background_executor().spawn(async move {
            prepare(&path)?;
            let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            super::external_edit::open_file(&path).map_err(|e| format!("Couldn't open {file}: {e}"))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = open.await;
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.state.set_error(target, e);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

impl EventEmitter<WorkflowsEvent> for WorkflowsPage {}

/// A section's header (`Header`, 2301:5824): the title and its buttons, the
/// helper under them.
pub(super) fn section_header(title: &'static str, help: &'static str, buttons: Vec<AnyElement>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap(px(6.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .type_style(Type::Heading)
                        .text_size(px(TITLE_SIZE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_loud)
                        .child(title),
                )
                .children(buttons),
        )
        .child(div().type_style(Type::Small).text_color(theme.form_hint).child(help))
}

/// A bordered table (`Workflow index`, `Table`): its column headings, a
/// rule, then its rows. The Settings frames' edge is the form controls'
/// (`input_border`); the rule under the headings is the stronger divider.
pub(super) fn table(headings: Div, rows: Vec<AnyElement>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.input_border)
        .child(headings.type_style(Type::SmallMedium).text_color(theme.form_hint))
        .child(div().h(px(1.0)).bg(theme.divider_strong))
        .children(rows)
}

/// A header's button: the Button component at Small with a left icon.
pub(super) fn header_button(id: &'static str, icon: Icon, label: &'static str, theme: &Theme) -> Stateful<Div> {
    style::small_icon_button(id, icon, label, ButtonKind::Secondary, false, theme)
}

/// A row's 24pt icon button (`Delete status`): `icon` (18pt) on the
/// overlay fill, 6pt corners; `inert` dims it and drops the pointer.
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
        d.bg(theme.background)
            .opacity(0.4)
            .child(heroicon(icon).text_color(theme.icon))
    } else {
        fill_layer(
            d.cursor_pointer(),
            id,
            px(6.0),
            [theme.background, theme.background_hover, theme.background_active],
        )
        .child(heroicon(icon).text_color(theme.icon))
    }
}

/// An error line under the row it's about.
pub(super) fn row_error(selector: String, text: &str, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .px(px(12.0))
        .pb(px(10.0))
        .type_style(Type::XSmall)
        .text_color(theme.error)
        .child(text.to_string())
}

/// A popup menu's panel (the caller floats it under its button).
pub(super) fn popup(id: &'static str, theme: &Theme) -> Stateful<Div> {
    style::popover(
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .flex_col()
            .p_1()
            .w(px(MENU_W))
            .occlude(),
        theme,
    )
}

impl WorkflowsPage {
    fn render_workflows(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let new_workflow = header_button("workflows-new", icons::PLUS, "New workflow", theme)
        .on_click(cx.listener(|this, _, window, cx| {
            this.open_draft(Draft::Workflow { from: None }, String::new(), window, cx)
        }));
        let headings = div()
            .flex()
            .flex_row()
            .gap(px(24.0))
            .px(px(8.0))
            .py(px(6.0))
            .child(div().flex_1().min_w_0().child("Workflow"))
            .child(div().w(px(COUNT_W)).flex_none().child("Statuses"))
            .child(div().w(px(COUNT_W)).flex_none().child("Transitions"))
            .child(div().w(px(ACTIONS_W)).flex_none());
        let mut rows: Vec<AnyElement> = self
            .state
            .workflows()
            .into_iter()
            .map(|w| self.render_workflow_row(w, theme, cx))
            .collect();
        if let Some(config) = self.state.config() {
            rows.extend(config.unreadable().iter().map(|u| self.render_unreadable_row(&u.name, &u.error, theme, cx)));
        }
        if let Some(Draft::Workflow { from }) = &self.state.draft {
            rows.push(self.render_workflow_draft(from.clone(), theme, window, cx));
        }
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.0))
            .child(section_header(
                "Workflows",
                "Configure the statuses, agents, and transitions for each kind of work.",
                vec![new_workflow.into_any_element()],
                theme,
            ))
            .child(table(headings, rows, theme))
    }

    fn render_workflow_row(&self, w: WorkflowSummary, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = w.name.clone();
        let menu_open = self.menu == Some(Menu::Workflow(name.clone()));
        let edit = {
            let name = name.clone();
            row_button(format!("workflow-edit-{name}"), icons::PENCIL_BUTTON, false, theme)
                .tooltip(style::tooltip("Edit workflow"))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    cx.emit(WorkflowsEvent::Edit(name.clone()));
                }))
        };
        let more = self.more_button(&name, theme, cx);
        let menu = menu_open.then(|| self.render_workflow_menu(&name, Some(&w), theme, cx));
        let badge = w.is_default.then(|| {
            div()
                .debug_selector(|| "workflow-default-badge".into())
                .flex_none()
                .px(px(4.0))
                .rounded(px(2.0))
                .bg(theme.workflow_badge)
                .type_style(Type::XSmall)
                .text_color(theme.workflow_badge_text)
                .child("Default")
        });
        let row_id = SharedString::from(format!("workflow-row-{name}"));
        let selector = row_id.clone();
        let open = name.clone();
        let row = div()
            .id(row_id)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(24.0))
            .p(px(12.0))
            .cursor_pointer()
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(WorkflowsEvent::Edit(open.clone()))))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .flex_none()
                                    .type_style(Type::BaseSemibold)
                                    .text_color(theme.text_loud)
                                    .child(w.name.clone()),
                            )
                            .children(badge),
                    )
                    .children(w.description.clone().map(|d| {
                        div().type_style(Type::Small).text_color(theme.form_hint).child(d)
                    })),
            )
            .child(count(w.statuses, theme))
            .child(count(w.transitions, theme))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .justify_end()
                    .gap(px(8.0))
                    .w(px(ACTIONS_W))
                    .child(edit)
                    .child(more)
                    .children(menu),
            );
        let error = self
            .state
            .error_at(&Target::Workflow(name.clone()))
            .map(|e| row_error(format!("workflow-error-{name}"), e, theme));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    /// A workflow file that can't be read: its name and error, and a ⋮
    /// menu with Edit YAML to fix it.
    fn render_unreadable_row(&self, name: &str, error: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let menu_open = self.menu == Some(Menu::Workflow(name.to_string()));
        let menu = menu_open.then(|| self.render_workflow_menu(name, None, theme, cx));
        let row_id = SharedString::from(format!("workflow-row-{name}"));
        let selector = row_id.clone();
        let row = div()
            .id(row_id)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(24.0))
            .p(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud)
                            .child(name.to_string()),
                    )
                    .child(
                        div()
                            .type_style(Type::Small)
                            .text_color(theme.error)
                            .child(format!("Can’t be edited here: {error}")),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .justify_end()
                    .w(px(ACTIONS_W))
                    .child(self.more_button(name, theme, cx))
                    .children(menu),
            );
        let error = self
            .state
            .error_at(&Target::Workflow(name.to_string()))
            .map(|e| row_error(format!("workflow-error-{name}"), e, theme));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    /// Workflow `name`'s ⋮ button, which opens (and closes) its menu.
    fn more_button(&self, name: &str, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let menu = Menu::Workflow(name.to_string());
        let open = self.menu.as_ref() == Some(&menu);
        row_button(format!("workflow-more-{name}"), icons::ELLIPSIS_VERTICAL_BUTTON, false, theme)
            .when(open, |d| d.child(style::anchor_probe(&self.menu_anchor)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.toggle_menu(menu.clone(), cx);
            }))
    }

    /// A workflow's ⋮ menu: Edit YAML, Duplicate, Set as default, Delete
    /// (disabled, with the reason, for the default and the only workflow).
    /// A file that can't be read (`w` is `None`) has Edit YAML only.
    fn render_workflow_menu(
        &self,
        name: &str,
        w: Option<&WorkflowSummary>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = name.to_string();
        let item = |id: String, label: &'static str, inert: Option<String>, danger: bool| {
            let row = style::menu_row(SharedString::from(id), false, theme).child(label);
            let row = if danger { row.text_color(theme.error) } else { row };
            match inert {
                Some(why) => row.opacity(0.4).cursor_default().tooltip(style::tooltip(why)),
                None => row,
            }
        };
        let edit_yaml = {
            let name = name.clone();
            item(format!("workflow-edit-yaml-{name}"), "Edit YAML", None, false).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.edit_workflow_yaml(&name, window, cx);
                },
            ))
        };
        let anchor = self.menu_anchor.clone();
        let popup = popup("workflow-menu", theme)
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                // A press on its button toggles it closed through its click.
                if anchor.get().is_some_and(|b| b.contains(&event.position)) {
                    return;
                }
                this.menu = None;
                cx.notify();
            }))
            .child(edit_yaml);
        let Some(w) = w else {
            return deferred(div().absolute().top(px(ROW_BUTTON + MENU_GAP)).right_0().child(popup)).with_priority(1);
        };
        let duplicate = {
            let name = name.clone();
            item(format!("workflow-duplicate-{name}"), "Duplicate", None, false).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    let suggested = this.state.duplicate_name(&name);
                    this.open_draft(Draft::Workflow { from: Some(name.clone()) }, suggested, window, cx);
                },
            ))
        };
        let default = {
            let name = name.clone();
            let inert = w.is_default.then(|| "Already the default".to_string());
            let row = item(format!("workflow-set-default-{name}"), "Set as default", inert.clone(), false);
            if inert.is_some() {
                row.on_click(|_, _, cx: &mut App| cx.stop_propagation())
            } else {
                row.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.request(Edit::SetDefault { name: name.clone() }, window, cx);
                }))
            }
        };
        let delete = {
            let name = name.clone();
            let inert = self.state.delete_workflow_blocked(&name);
            let row = item(format!("workflow-delete-{name}"), "Delete", inert.clone(), true);
            if inert.is_some() {
                row.on_click(|_, _, cx: &mut App| cx.stop_propagation())
            } else {
                row.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.request(Edit::DeleteWorkflow { name: name.clone() }, window, cx);
                }))
            }
        };
        deferred(
            div().absolute().top(px(ROW_BUTTON + MENU_GAP)).right_0().child(
                popup
                    .child(duplicate)
                    .child(default)
                    .child(div().my_1().mx(px(-4.0)).h(px(1.0)).bg(theme.divider))
                    .child(delete),
            ),
        )
        .with_priority(1)
    }

    /// The workflow table's new row: the name field, and Enter or Esc.
    fn render_workflow_draft(&self, from: Option<String>, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let hint = match &from {
            Some(from) => format!("Duplicate of {from}. Enter to create, Esc to cancel."),
            None => "Enter to create, Esc to cancel.".to_string(),
        };
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(24.0))
            .p(px(12.0))
            .child(
                div()
                    .flex_none()
                    .w(px(IDENTITY_W))
                    .child(self.draft_field("workflow-new-name", "Workflow name", theme, window, cx)),
            )
            .child(div().flex_1().min_w_0().type_style(Type::Small).text_color(theme.form_hint).child(hint));
        let error = self
            .state
            .error_at(&Target::NewWorkflow)
            .map(|e| row_error("workflow-new-error".into(), e, theme));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    /// The new row's name field; Esc closes the row.
    fn draft_field(
        &self,
        selector: &'static str,
        placeholder: &'static str,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.draft_input.read(cx).focus_handle(cx).is_focused(window);
        let empty = self.draft_input.read(cx).value().is_empty();
        let field = div()
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .child(
                Input::new(&self.draft_input)
                    .appearance(false)
                    .h_full()
                    .px_0()
                    .type_style(Type::Base)
                    .text_color(theme.form_text),
            )
            .when(empty, |d| {
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
                        .child(placeholder),
                )
            });
        controls::textbox(field, focused, theme)
            .debug_selector(move || selector.into())
            .w_full()
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.close_draft(cx);
                cx.stop_propagation();
            }))
    }

    fn render_statuses(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let edit_yaml = header_button("statuses-edit-yaml", icons::DOCUMENT_TEXT, "Edit YAML", theme)
        .on_click(cx.listener(|this, _, window, cx| this.edit_yaml(window, cx)));
        let new_status = header_button("statuses-new", icons::PLUS, "New status", theme)
        .on_click(cx.listener(|this, _, window, cx| {
            this.open_draft(
                Draft::Status {
                    category: StatusCategory::Backlog,
                },
                String::new(),
                window,
                cx,
            )
        }));
        let headings = div()
            .flex()
            .flex_row()
            .gap(px(12.0))
            .px(px(8.0))
            .py(px(6.0))
            .child(div().w(px(NAME_COL_W)).flex_none().child("Name"))
            .child(div().w(px(ID_W)).flex_none().child("ID"))
            .child(div().w(px(CATEGORY_W)).flex_none().child("Category"))
            .child(div().flex_1().min_w_0().child("Used in"))
            .child(div().w(px(ROW_BUTTON)).flex_none());
        let mut rows: Vec<AnyElement> = self
            .state
            .statuses()
            .into_iter()
            .map(|s| self.render_status_row(s, theme, window, cx))
            .collect();
        if let Some(Draft::Status { category }) = self.state.draft {
            rows.push(self.render_status_draft(category, theme, window, cx));
        }
        let legend = |cats: &[StatusCategory]| {
            div().flex().flex_col().flex_1().min_w_0().gap(px(16.0)).children(cats.iter().map(|c| {
                div()
                    .flex()
                    .flex_col()
                    .type_style(Type::Small)
                    .child(div().type_style(Type::SmallMedium).text_color(theme.form_text).child(category_label(*c)))
                    .child(div().text_color(theme.form_hint).child(category_help(*c)))
            }))
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.0))
            .child(style::anchor_probe(&self.statuses_anchor))
            .child(section_header(
                "Statuses",
                "Work transitions through statuses in workflows. Configure the statuses, agents, and \
                 transitions for each kind of work.",
                vec![edit_yaml.into_any_element(), new_status.into_any_element()],
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(table(headings, rows, theme).debug_selector(|| "statuses-table".into()))
                    .child(
                        div()
                            .type_style(Type::Small)
                            .text_color(theme.form_hint)
                            .child("Renaming a status keeps its ID, so existing workflow references stay intact."),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "status-categories".into())
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        div()
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud)
                            .child("Status categories"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(32.0))
                            .child(legend(&CATEGORIES[..3]))
                            .child(legend(&CATEGORIES[3..])),
                    ),
            )
    }

    fn render_status_row(&self, s: StatusSummary, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let id = s.id.clone();
        let handle = {
            let drag = StatusDrag {
                id: id.clone(),
                name: s.name.clone().into(),
            };
            div()
                .id(SharedString::from(format!("status-handle-{id}")))
                .debug_selector({
                    let id = id.clone();
                    move || format!("status-handle-{id}")
                })
                .flex_none()
                .cursor_grab()
                .child(heroicon(icons::BARS_2).text_color(theme.icon))
                .on_drag(drag, |drag: &StatusDrag, _offset, _window, cx: &mut App| {
                    let name = drag.name.clone();
                    cx.new(|_| StatusGhost { name })
                })
        };
        let name_field = self.names.get(&id).map(|(input, _)| {
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            controls::textbox(
                div().flex_1().min_w_0().child(
                    Input::new(input)
                        .appearance(false)
                        .h_full()
                        .px_0()
                        .type_style(Type::Base)
                        .text_color(theme.form_text),
                ),
                focused,
                theme,
            )
            .debug_selector({
                let id = id.clone();
                move || format!("status-name-{id}")
            })
            .flex_1()
            .min_w_0()
        });
        let identity = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .w(px(IDENTITY_W))
            .child(status_icon_box(s.category, theme))
            .children(name_field);
        let blocked = self.state.delete_status_blocked(&id);
        let delete = {
            let button = row_button(format!("status-delete-{id}"), icons::TRASH_BUTTON, blocked.is_some(), theme);
            match blocked {
                Some(why) => button.tooltip(style::tooltip(why)),
                None => {
                    let id = id.clone();
                    button.tooltip(style::tooltip("Delete status")).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.request(Edit::DeleteStatus { id: id.clone() }, window, cx)
                        },
                    ))
                }
            }
        };
        let onto = id.clone();
        let hover = *theme;
        let row = div()
            .id(SharedString::from(format!("status-row-{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("status-row-{id}")
            })
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .p(px(12.0))
            .drag_over::<StatusDrag>(move |st, _, _, _| st.bg(hover.background))
            .on_drop(cx.listener(move |this, drag: &StatusDrag, window, cx| {
                this.drop_status(&drag.id, &onto, window, cx)
            }))
            .child(handle)
            .child(identity)
            .child(
                div()
                    .w(px(ID_W))
                    .flex_none()
                    .truncate()
                    .font_family(PRIMARY_FAMILY)
                    .text_size(px(14.0))
                    .line_height(px(18.0))
                    .text_color(theme.form_hint)
                    .child(s.id.clone()),
            )
            .child(self.category_picker(Some(id.clone()), s.category, theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_style(Type::Small)
                    .text_color(theme.form_text)
                    .child(s.used_in.join(", ")),
            )
            .child(delete);
        let error = self
            .state
            .error_at(&Target::Status(id.clone()))
            .map(|e| row_error(format!("status-error-{id}"), e, theme));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    /// The status table's new row: its icon and name field, the id it will
    /// get, and its category.
    fn render_status_draft(&self, category: StatusCategory, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let typed = self.draft_input.read(cx).value().trim().to_string();
        let id = self
            .state
            .config()
            .filter(|_| !typed.is_empty())
            .map(|c| c.status_id_for(&typed))
            .unwrap_or_default();
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .p(px(12.0))
            .child(div().w(px(20.0)).flex_none())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .w(px(IDENTITY_W))
                    .child(status_icon_box(category, theme))
                    .child(div().flex_1().min_w_0().child(self.draft_field(
                        "status-new-name",
                        "Status name",
                        theme,
                        window,
                        cx,
                    ))),
            )
            .child(
                div()
                    .debug_selector(|| "status-new-id".into())
                    .w(px(ID_W))
                    .flex_none()
                    .truncate()
                    .font_family(PRIMARY_FAMILY)
                    .text_size(px(14.0))
                    .line_height(px(18.0))
                    .text_color(theme.form_hint)
                    .child(id),
            )
            .child(self.category_picker(None, category, theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child("Enter to add, Esc to cancel."),
            );
        let error = self
            .state
            .error_at(&Target::NewStatus)
            .map(|e| row_error("status-new-error".into(), e, theme));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    /// The Category picker (`Category`, 2285:7131): the category over a
    /// chevron, the six categories in a menu under it. `target` is the
    /// status's id, `None` for the new row's.
    fn category_picker(&self, target: Option<String>, category: StatusCategory, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let key = target.clone().unwrap_or_else(|| "new".into());
        let open = self.menu == Some(Menu::Category(target.clone()));
        let button = {
            let target = target.clone();
            style::button_base(format!("status-category-{key}"), ButtonKind::Secondary, ButtonSize::Base, false, theme)
                .w(px(CATEGORY_W))
                .child(div().flex_1().min_w_0().truncate().child(category_label(category)))
                .child(style::right_icon_slot(icons::CHEVRON_DOWN, ButtonSize::Base, theme.button_icon))
                .when(open, |d| d.child(style::anchor_probe(&self.menu_anchor)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_menu(Menu::Category(target.clone()), cx)
                }))
        };
        let menu = open.then(|| {
            let anchor = self.menu_anchor.clone();
            let items = CATEGORIES.iter().map(|&c| {
                let target = target.clone();
                style::menu_row(format!("status-category-{key}-{}", c.as_str()), c == category, theme)
                    .child(status_icon_box(c, theme))
                    .child(category_label(c))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.pick_category(target.clone(), c, window, cx)
                    }))
            });
            deferred(
                div().absolute().top(px(style::BUTTON_H + MENU_GAP)).left_0().child(
                    popup("status-category-menu", theme)
                        .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            if anchor.get().is_some_and(|b| b.contains(&event.position)) {
                                return;
                            }
                            this.menu = None;
                            cx.notify();
                        }))
                        .children(items),
                ),
            )
            .with_priority(1)
        });
        div().relative().flex_none().child(button).children(menu)
    }
}

/// A count column's cell.
fn count(n: usize, theme: &Theme) -> Div {
    div()
        .w(px(COUNT_W))
        .flex_none()
        .type_style(Type::Base)
        .text_color(theme.form_text)
        .child(n.to_string())
}

/// The status's board icon, filling the row's 20pt frame.
fn status_icon_box(category: StatusCategory, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(STATUS_ICON_BOX))
        .child(style::status_icon_large(category, theme))
}

impl Render for WorkflowsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if std::mem::take(&mut self.reload) {
            self.load(window, cx);
        }
        let page_error = self.state.error_at(&Target::Page).map(|e| {
            div()
                .debug_selector(|| "workflows-error".into())
                .type_style(Type::XSmall)
                .text_color(theme.error)
                .child(e.to_string())
        });
        let notice = self.state.notice().map(|n| {
            div()
                .debug_selector(|| "workflows-notice".into())
                .type_style(Type::XSmall)
                .text_color(theme.form_hint)
                .child(n.to_string())
        });
        div()
            .id("settings-workflows")
            .debug_selector(|| "settings-workflows".into())
            .flex()
            .flex_col()
            .w_full()
            .gap(px(32.0))
            .children(page_error)
            .children(notice)
            .child(self.render_workflows(&theme, window, cx))
            .child(div().h(px(1.0)).w_full().bg(theme.divider_strong))
            .child(self.render_statuses(&theme, window, cx))
    }
}
