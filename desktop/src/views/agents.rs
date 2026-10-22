//! Settings ▸ Agents (`dt-agents-settings-page`, Figma frame 2377:41040):
//! the project's agents and the instructions they share.
//!
//! Top to bottom: the Agents header (New agent) over a table of the
//! project's agents (the name with a Customized badge and the description;
//! the preferred runner, with Recommended under it when it's the shipped
//! choice; the workflow statuses assigned to the agent, or Not assigned; the
//! skill count; an edit button and a ⋮ menu with Edit YAML, Duplicate, Reset
//! to default and Delete). An agent whose files can't be read shows its
//! error and a ⋮ menu with Edit YAML only. The footnote links to Workflows.
//! Then a rule, and the Shared instructions header (Edit instructions) over
//! a preview of `agents/_shared/preamble.md`'s first lines and the
//! footnote.
//!
//! Every change goes through `shelbi_state::agents_config` and saves as it
//! is made ([`AgentsState`]). New agent and Duplicate type the name into a
//! new row at the bottom of the table; a new agent opens once created.
//! Reset and Delete ask to confirm under the row, and are disabled up front
//! (with the reason as a tooltip) where the model refuses them: Reset for
//! an agent Shelbi doesn't ship or one that is still the shipped default,
//! Delete for a shipped agent or one a workflow status (or the project's
//! runner pin) names. The edit button and a row click ask the window to
//! open the agent ([`AgentsEvent::Edit`]).
//!
//! Edit instructions opens the shared instructions in a text area in place
//! of the preview, with Save and Cancel. Edit YAML opens the agent's
//! `agent.yaml` in the user's editor; when the window comes back to the
//! front after one, the files are read again to show the hand edit, as they
//! are each time the page is shown.

use std::path::PathBuf;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, Focusable,
    FontWeight, SharedString, Stateful, Subscription, Window,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use shelbi_state::agents_config::{AgentSummary, AgentsConfig, Provenance};

use super::agents_state::{
    commit, preview, runner_label, status_chips, Committed, Confirm, Edit, StatusChip, Target, AgentsState,
    PREVIEW_LINES,
};
use super::workflows::{header_button, popup, row_button, row_error, section_header, table, MENU_GAP, ROW_BUTTON};
use crate::assets::icons;
use crate::elements::controls;
use crate::theme::style::{self, ButtonKind, Type, Typography};
use crate::theme::Theme;

/// The table's columns (`Agent Row`, 2397:7514): the runner, the workflow
/// statuses, the skill count and the buttons; the agent takes the rest.
const RUNNER_W: f32 = 140.0;
const STATUSES_W: f32 = 268.0;
const SKILLS_W: f32 = 56.0;
const ACTIONS_W: f32 = 90.0;
/// The new row's name field.
const DRAFT_W: f32 = 280.0;
/// The table's corners (`radius/default`).
const TABLE_RADIUS: f32 = 12.0;
/// The shared instructions' editor's height.
const EDITOR_H: f32 = 360.0;
/// A section header's buttons (the Button component at Small).
const HEADER_BUTTON_H: f32 = 28.0;

/// What the page tells the settings view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsEvent {
    /// Open agent `name`'s details.
    Edit(String),
    /// Show the Workflows page (the footnote's link).
    ShowWorkflows,
}

/// The Agents page.
pub struct AgentsPage {
    state: AgentsState,
    /// The new row's name field.
    draft_input: Entity<InputState>,
    /// The shared instructions' text area, while they're being edited.
    shared_input: Option<Entity<TextareaState>>,
    /// The agent whose ⋮ menu is open.
    menu: Option<String>,
    /// Where the open menu's button last painted (its own click toggles it).
    menu_anchor: style::Anchor,
    /// Re-read the files on the next draw (the page was shown again).
    reload: bool,
    /// Re-read the files when the window is next active (an Edit YAML sent
    /// the user to their editor).
    reload_on_return: bool,
    _subscriptions: Vec<Subscription>,
}

impl AgentsPage {
    pub fn new(project: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let draft_input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&draft_input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_draft(window, cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() && std::mem::take(&mut this.reload_on_return) {
                    this.load(window, cx);
                }
            }),
        ];
        let mut page = AgentsPage {
            state: AgentsState::new(project),
            draft_input,
            shared_input: None,
            menu: None,
            menu_anchor: Default::default(),
            reload: false,
            reload_on_return: false,
            _subscriptions: subscriptions,
        };
        // A test's page never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.load(window, cx);
        }
        page
    }

    /// Read the project's agents off the UI thread (again, to pick up hand
    /// edits) and show them.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.state.project().to_string();
        let read = cx
            .background_executor()
            .spawn(async move { AgentsConfig::load(&project).map_err(|e| e.to_string()) });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update(cx, |this, cx| {
                if this.state.saving() {
                    // A save in flight lands its own fresh model.
                    return;
                }
                this.state.loaded(config);
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
    #[cfg(any(test, feature = "visual-check"))]
    pub fn state(&self) -> &AgentsState {
        &self.state
    }

    /// The new row's name field.
    #[cfg(test)]
    pub fn draft_input(&self) -> Entity<InputState> {
        self.draft_input.clone()
    }

    /// The shared instructions' text area, while it shows.
    #[cfg(test)]
    pub fn shared_input(&self) -> Option<Entity<TextareaState>> {
        self.shared_input.clone()
    }

    /// Open agent `name`'s ⋮ menu (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_open_menu(&mut self, name: &str, cx: &mut Context<Self>) {
        self.menu = Some(name.to_string());
        cx.notify();
    }

    /// Ask to confirm `confirm` under its row (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_ask(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.ask(confirm, cx);
    }

    /// Open the shared instructions' editor (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_edit_shared(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_shared(window, cx);
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
    fn start(&mut self, (config, edit): (AgentsConfig, Edit), window: &mut Window, cx: &mut Context<Self>) {
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
            (Edit::CreateAgent { name } | Edit::DuplicateAgent { name, .. }, None) => Some(name.clone()),
            _ => None,
        };
        let next = self.state.committed(done);
        if !self.state.editing_shared {
            self.shared_input = None;
        }
        if let Some(job) = next {
            self.start(job, window, cx);
        }
        if let Some(name) = created {
            cx.emit(AgentsEvent::Edit(name));
        }
        cx.notify();
    }

    /// Open the new row, its field holding `text`, focused: a new agent, or
    /// (`from`) a duplicate of one.
    fn open_draft(&mut self, from: Option<String>, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.state.draft = Some(from);
        self.state.confirm = None;
        self.menu = None;
        self.draft_input.update(cx, |s, cx| s.set_value(text, window, cx));
        let focus = self.draft_input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn close_draft(&mut self, cx: &mut Context<Self>) {
        self.state.draft = None;
        cx.notify();
    }

    /// Enter in the new row: create it.
    fn submit_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.draft_input.read(cx).value().trim().to_string();
        let edit = match self.state.draft.clone() {
            None => return,
            Some(_) if name.is_empty() => return,
            Some(None) => Edit::CreateAgent { name },
            Some(Some(from)) => Edit::DuplicateAgent { from, name },
        };
        self.request(edit, window, cx);
    }

    fn toggle_menu(&mut self, name: &str, cx: &mut Context<Self>) {
        self.menu = if self.menu.as_deref() == Some(name) { None } else { Some(name.to_string()) };
        cx.notify();
    }

    /// Reset or Delete was chosen: ask to confirm it under the row.
    fn ask(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.menu = None;
        self.state.ask(confirm);
        cx.notify();
    }

    /// Open the shared instructions in the text area, focused.
    fn edit_shared(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.config().is_none() {
            return;
        }
        let text = self.state.shared_instructions().to_string();
        let input = cx.new(|cx| TextareaState::new(window, cx).default_value(text));
        input.update(cx, |s, cx| s.focus(window, cx));
        self.shared_input = Some(input);
        self.state.editing_shared = true;
        cx.notify();
    }

    /// Save the text area's text as the shared instructions.
    fn save_shared(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = &self.shared_input else {
            return;
        };
        let text = input.read(cx).value().to_string();
        self.request(Edit::SetSharedInstructions { text }, window, cx);
    }

    fn cancel_shared(&mut self, cx: &mut Context<Self>) {
        self.shared_input = None;
        self.state.editing_shared = false;
        self.state.clear_error(&Target::Shared);
        cx.notify();
    }

    /// Open agent `name`'s `agent.yaml` in the user's editor (its ⋮ menu's
    /// Edit YAML), one that can't be read too: that's how it gets fixed.
    /// `dir` is the agent's directory.
    fn edit_yaml(&mut self, name: &str, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.reload_on_return = true;
        let target = Target::Agent(name.to_string());
        let open = cx.background_executor().spawn(async move {
            let path = dir.join("agent.yaml");
            if !path.exists() {
                return Err("Couldn't find agent.yaml.".to_string());
            }
            super::external_edit::open_file(&path).map_err(|e| format!("Couldn't open agent.yaml: {e}"))
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
        cx.notify();
    }
}

impl EventEmitter<AgentsEvent> for AgentsPage {}

impl AgentsPage {
    fn render_agents(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let new_agent = header_button("agents-new", icons::PLUS, "New agent", theme)
            .on_click(cx.listener(|this, _, window, cx| this.open_draft(None, String::new(), window, cx)));
        let headings = div()
            .flex()
            .flex_row()
            .gap(px(24.0))
            .px(px(12.0))
            .py(px(6.0))
            .child(div().flex_1().min_w_0().child("Agent"))
            .child(div().w(px(RUNNER_W)).flex_none().child("Preferred runner"))
            .child(div().w(px(STATUSES_W)).flex_none().child("Workflow statuses"))
            .child(div().w(px(SKILLS_W)).flex_none().child("Skills"))
            .child(div().w(px(ACTIONS_W)).flex_none());
        let mut rows: Vec<AnyElement> = self
            .state
            .agents()
            .into_iter()
            .map(|a| self.render_agent_row(a, theme, cx))
            .collect();
        if let Some(config) = self.state.config() {
            rows.extend(
                config
                    .unreadable()
                    .iter()
                    .map(|u| self.render_unreadable_row(&u.name, &u.error, u.path.clone(), theme, cx)),
            );
        }
        if let Some(from) = &self.state.draft {
            rows.push(self.render_draft(from.clone(), theme, window, cx));
        }
        let workflows = div()
            .id("agents-workflows-link")
            .debug_selector(|| "agents-workflows-link".into())
            .cursor_pointer()
            .hover(|s| s.underline())
            .child("Workflows")
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(AgentsEvent::ShowWorkflows)));
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.0))
            .child(section_header(
                "Agents",
                "Configure the roles, instructions, and skills agents use in this project.",
                vec![new_agent.into_any_element()],
                theme,
            ))
            .child(
                table(headings, rows, theme)
                    .rounded(px(TABLE_RADIUS))
                    .debug_selector(|| "agents-table".into()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child("Assign agents to workflow statuses in\u{a0}")
                    .child(workflows)
                    .child(". Runner and model preferences can be changed for each role."),
            )
    }

    fn render_agent_row(&self, a: AgentSummary, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = a.name.clone();
        let edit = {
            let name = name.clone();
            row_button(format!("agent-edit-{name}"), icons::PENCIL_BUTTON, false, theme)
                .tooltip(style::tooltip("Edit agent"))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    cx.emit(AgentsEvent::Edit(name.clone()));
                }))
        };
        let more = self.more_button(&name, theme, cx);
        let menu = (self.menu.as_deref() == Some(name.as_str())).then(|| self.render_menu(&name, theme, cx));
        let badge = (a.provenance == Provenance::Customized).then(|| {
            let selector = format!("agent-customized-{name}");
            div()
                .debug_selector(move || selector.clone())
                .flex_none()
                .px(px(4.0))
                .rounded(px(2.0))
                .bg(theme.workflow_badge)
                .type_style(Type::XSmall)
                .text_color(theme.workflow_badge_text)
                .child("Customized")
        });
        let runner = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(2.0))
            .w(px(RUNNER_W))
            .child(div().truncate().type_style(Type::Base).text_color(theme.form_text).child(match a.preferred_runner {
                Some(runner) => runner_label(runner),
                None => "Project default",
            }))
            .children(a.runner_recommended.then(|| {
                let selector = format!("agent-recommended-{name}");
                div()
                    .debug_selector(move || selector.clone())
                    .type_style(Type::XSmall)
                    .text_color(theme.form_hint)
                    .child("Recommended")
            }));
        let chips = status_chips(&a);
        let statuses = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap_x(px(12.0))
            .gap_y(px(4.0))
            .w(px(STATUSES_W))
            .type_style(Type::Small)
            .when(chips.is_empty(), |d| {
                d.child(div().text_color(theme.form_hint).child("Not assigned"))
            })
            .children(chips.into_iter().map(|chip| status_chip(&name, chip, theme)));
        let row_id = SharedString::from(format!("agent-row-{name}"));
        let selector = row_id.clone();
        let open = name.clone();
        let row = div()
            .id(row_id)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(24.0))
            .px(px(12.0))
            .py(px(16.0))
            .cursor_pointer()
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(AgentsEvent::Edit(open.clone()))))
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
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_none()
                                    .type_style(Type::BaseSemibold)
                                    .text_color(theme.text_loud)
                                    .child(a.name.clone()),
                            )
                            .children(badge),
                    )
                    .children(a.description.clone().map(|d| {
                        div().type_style(Type::Small).text_color(theme.form_hint).child(d)
                    })),
            )
            .child(runner)
            .child(statuses)
            .child(
                div()
                    .w(px(SKILLS_W))
                    .flex_none()
                    .type_style(Type::Base)
                    .text_color(theme.form_hint)
                    .child(a.skills.to_string()),
            )
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
        let confirm = self
            .state
            .confirm
            .as_ref()
            .filter(|c| c.agent() == name)
            .map(|c| self.render_confirm(c.clone(), theme, cx));
        let error = self
            .state
            .error_at(&Target::Agent(name.clone()))
            .map(|e| row_error(format!("agent-error-{name}"), e, theme));
        div().flex().flex_col().child(row).children(confirm).children(error).into_any_element()
    }

    /// An agent whose files can't be read: its name and error, and a ⋮
    /// menu with Edit YAML to fix it.
    fn render_unreadable_row(&self, name: &str, error: &str, dir: PathBuf, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let menu = (self.menu.as_deref() == Some(name)).then(|| self.render_unreadable_menu(name, dir, theme, cx));
        let row_id = SharedString::from(format!("agent-row-{name}"));
        let selector = row_id.clone();
        let row = div()
            .id(row_id)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(24.0))
            .px(px(12.0))
            .py(px(16.0))
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
            .error_at(&Target::Agent(name.to_string()))
            .map(|e| row_error(format!("agent-error-{name}"), e, theme));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    /// Agent `name`'s ⋮ button, which opens (and closes) its menu.
    fn more_button(&self, name: &str, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = self.menu.as_deref() == Some(name);
        let name = name.to_string();
        row_button(format!("agent-more-{name}"), icons::ELLIPSIS_VERTICAL_BUTTON, false, theme)
            .when(open, |d| d.child(style::anchor_probe(&self.menu_anchor)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.toggle_menu(&name, cx);
            }))
    }

    /// The menu's panel, closed by a press outside it (a press on its own
    /// button toggles it through the click).
    fn menu_panel(&self, cx: &mut Context<Self>, theme: &Theme) -> Stateful<Div> {
        let anchor = self.menu_anchor.clone();
        popup("agent-menu", theme).on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
            if anchor.get().is_some_and(|b| b.contains(&event.position)) {
                return;
            }
            this.menu = None;
            cx.notify();
        }))
    }

    /// An agent's ⋮ menu: Edit YAML, Duplicate, Reset to default, Delete,
    /// the last two disabled with the reason where the model refuses them.
    fn render_menu(&self, name: &str, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let name = name.to_string();
        let dir = self.state.config().map(|c| c.agent_dir(&name)).unwrap_or_default();
        let edit_yaml = menu_item(format!("agent-edit-yaml-{name}"), "Edit YAML", None, false, theme).on_click(
            cx.listener({
                let name = name.clone();
                move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.edit_yaml(&name, dir.clone(), window, cx);
                }
            }),
        );
        let duplicate = menu_item(format!("agent-duplicate-{name}"), "Duplicate", None, false, theme).on_click(
            cx.listener({
                let name = name.clone();
                move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    let suggested = this.state.duplicate_name(&name);
                    this.open_draft(Some(name.clone()), suggested, window, cx);
                }
            }),
        );
        let guarded = |id: String, label: &'static str, why: Option<String>, danger: bool, confirm: Confirm| {
            let row = menu_item(id, label, why.clone(), danger, theme);
            if why.is_some() {
                row.on_click(|_, _, cx: &mut App| cx.stop_propagation())
            } else {
                row.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.ask(confirm.clone(), cx);
                }))
            }
        };
        let reset = guarded(
            format!("agent-reset-{name}"),
            "Reset to default",
            self.state.reset_blocked(&name),
            false,
            Confirm::Reset(name.clone()),
        );
        let delete = guarded(
            format!("agent-delete-{name}"),
            "Delete",
            self.state.delete_blocked(&name),
            true,
            Confirm::Delete(name.clone()),
        );
        deferred(
            div().absolute().top(px(ROW_BUTTON + MENU_GAP)).right_0().child(
                self.menu_panel(cx, theme)
                    .child(edit_yaml)
                    .child(duplicate)
                    .child(reset)
                    .child(div().my_1().mx(px(-4.0)).h(px(1.0)).bg(theme.divider))
                    .child(delete),
            ),
        )
        .with_priority(1)
    }

    /// An unreadable agent's ⋮ menu: Edit YAML only (the rest need the
    /// model, which can't load it).
    fn render_unreadable_menu(&self, name: &str, dir: PathBuf, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let name = name.to_string();
        let edit_yaml = menu_item(format!("agent-edit-yaml-{name}"), "Edit YAML", None, false, theme).on_click(
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.edit_yaml(&name, dir.clone(), window, cx);
            }),
        );
        deferred(
            div()
                .absolute()
                .top(px(ROW_BUTTON + MENU_GAP))
                .right_0()
                .child(self.menu_panel(cx, theme).child(edit_yaml)),
        )
        .with_priority(1)
    }

    /// Reset or Delete waiting under its row: what it does, Cancel and the
    /// action.
    fn render_confirm(&self, confirm: Confirm, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = confirm.agent().to_string();
        let (text, label, kind, id) = match &confirm {
            Confirm::Reset(_) => (
                format!(
                    "Reset {name} to the shipped default? Its instructions, agent.yaml and skills go back to \
                     what Shelbi ships, and your changes to them are lost."
                ),
                "Reset",
                ButtonKind::Danger,
                "agent-confirm-reset",
            ),
            Confirm::Delete(_) => (
                format!("Delete {name}? Its directory, with its instructions and skills, is removed."),
                "Delete",
                ButtonKind::Danger,
                "agent-confirm-delete",
            ),
        };
        div()
            .debug_selector(|| "agent-confirm".into())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(12.0))
            .pb(px(16.0))
            .child(div().flex_1().min_w_0().type_style(Type::Small).text_color(theme.form_text).child(text))
            .child(
                style::button("agent-confirm-cancel", "Cancel", ButtonKind::Quiet, false, theme).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.state.confirm = None;
                        cx.notify();
                    },
                )),
            )
            .child(style::button(id, label, kind, false, theme).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| this.request(confirm.edit(), window, cx),
            )))
            .into_any_element()
    }

    /// The table's new row: the name field, and Enter or Esc.
    fn render_draft(&self, from: Option<String>, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let hint = match &from {
            Some(from) => format!("Duplicate of {from}. Enter to create, Esc to cancel."),
            None => "Enter to create, Esc to cancel.".to_string(),
        };
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
                        .child("Agent name"),
                )
            });
        let field = controls::textbox(field, focused, theme)
            .debug_selector(|| "agent-new-name".into())
            .w(px(DRAFT_W))
            .flex_none()
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.close_draft(cx);
                cx.stop_propagation();
            }));
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(24.0))
            .px(px(12.0))
            .py(px(16.0))
            .child(field)
            .child(div().flex_1().min_w_0().type_style(Type::Small).text_color(theme.form_hint).child(hint));
        let error = self
            .state
            .error_at(&Target::NewAgent)
            .map(|e| row_error("agent-new-error".into(), e, theme));
        div().flex().flex_col().child(row).children(error).into_any_element()
    }

    fn render_shared(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let editing = self.shared_input.clone().filter(|_| self.state.editing_shared);
        // While editing, Cancel and Save stand where Edit instructions was,
        // in view however far the text area runs down the page.
        let buttons = if editing.is_some() {
            let save = if self.state.saving() {
                style::disabled_button("agents-shared-save", "Save", ButtonKind::Primary, theme)
            } else {
                style::button("agents-shared-save", "Save", ButtonKind::Primary, false, theme)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_shared(window, cx)))
            };
            vec![
                style::button("agents-shared-cancel", "Cancel", ButtonKind::Quiet, false, theme)
                    .h(px(HEADER_BUTTON_H))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_shared(cx)))
                    .into_any_element(),
                save.h(px(HEADER_BUTTON_H)).into_any_element(),
            ]
        } else {
            vec![header_button("agents-edit-instructions", icons::PENCIL, "Edit instructions", theme)
                .on_click(cx.listener(|this, _, window, cx| this.edit_shared(window, cx)))
                .into_any_element()]
        };
        let body = match editing {
            Some(input) => render_shared_editor(input, theme, window, cx).into_any_element(),
            None => render_preview(self.state.shared_instructions(), theme).into_any_element(),
        };
        let error = self.state.error_at(&Target::Shared).map(|e| {
            div()
                .debug_selector(|| "agents-shared-error".into())
                .type_style(Type::XSmall)
                .text_color(theme.error)
                .child(e.to_string())
        });
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.0))
            .child(section_header(
                "Shared instructions",
                "Project context included before every agent’s own instructions.",
                buttons,
                theme,
            ))
            .child(div().flex().flex_col().gap(px(8.0)).child(body).children(error))
            .child(
                div()
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child("Changes apply to new agent sessions. Existing sessions keep their current instructions."),
            )
    }
}

/// The shared instructions in a text area, in the preview's place: the
/// workflow editor's Run box (a 1pt edge, the focus ring's color while
/// focused, the text box's well), taller.
fn render_shared_editor(
    input: Entity<TextareaState>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<AgentsPage>,
) -> Div {
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    div()
        .debug_selector(|| "agents-shared-editor".into())
        .flex()
        .w_full()
        .h(px(EDITOR_H))
        .p(px(1.0))
        .rounded(px(6.0))
        .bg(if focused { theme.focus_ring } else { theme.input_border })
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                // The text area's own inset (about 9pt across, 4pt down)
                // makes up the rest of the preview's 16pt.
                .px(px(7.0))
                .py(px(12.0))
                .rounded(px(5.0))
                .bg(theme.textbox)
                .type_style(Type::Base)
                .text_color(theme.form_text)
                .child(Textarea::new(&input).appearance(false).h_full()),
        )
}

/// A ⋮ menu's item; `inert` dims it with the reason as a tooltip.
fn menu_item(id: String, label: &'static str, inert: Option<String>, danger: bool, theme: &Theme) -> Stateful<Div> {
    let row = style::menu_row(SharedString::from(id), false, theme).child(label);
    let row = if danger { row.text_color(theme.error) } else { row };
    match inert {
        Some(why) => row.opacity(0.4).cursor_default().tooltip(style::tooltip(why)),
        None => row,
    }
}

/// An assigned status: its board icon (14pt) and name; the workflows that
/// assign it as a tooltip.
fn status_chip(agent: &str, chip: StatusChip, theme: &Theme) -> impl IntoElement {
    let id = SharedString::from(format!("agent-status-{agent}-{}", chip.id));
    let selector = id.clone();
    let workflows = format!("In {}", chip.workflows.join(", "));
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .text_color(theme.form_text)
        .children(chip.category.map(|c| style::status_icon(c, theme).size(px(14.0))))
        .child(chip.name)
        .tooltip(style::tooltip(workflows))
}

/// The shared instructions' preview (`Shared instructions preview`,
/// 2398:7049): the first lines in the text box's well, 12pt between
/// blocks; a quiet line when there are none.
fn render_preview(markdown: &str, theme: &Theme) -> Div {
    let (blocks, cut) = preview(markdown, PREVIEW_LINES);
    let well = div()
        .debug_selector(|| "agents-shared-preview".into())
        .flex()
        .flex_col()
        .w_full()
        .gap(px(12.0))
        .p(px(16.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.input_border)
        .bg(theme.textbox)
        .type_style(Type::Base)
        .text_color(theme.form_text);
    if blocks.is_empty() {
        return well.child(div().text_color(theme.form_hint).child("No shared instructions yet."));
    }
    well.children(blocks.into_iter().map(|b| {
        div()
            .flex()
            .flex_col()
            .when(b.heading, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(theme.text_loud))
            .children(b.lines.into_iter().map(|l| div().child(l)))
    }))
    .when(cut, |d| d.child(div().text_color(theme.form_hint).child("…")))
}

impl Render for AgentsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if std::mem::take(&mut self.reload) {
            self.load(window, cx);
        }
        let page_error = self.state.error_at(&Target::Page).map(|e| {
            div()
                .debug_selector(|| "agents-error".into())
                .type_style(Type::XSmall)
                .text_color(theme.error)
                .child(e.to_string())
        });
        let notice = self.state.notice().map(|n| {
            div()
                .debug_selector(|| "agents-notice".into())
                .type_style(Type::XSmall)
                .text_color(theme.form_hint)
                .child(n.to_string())
        });
        div()
            .id("settings-agents")
            .debug_selector(|| "settings-agents".into())
            .flex()
            .flex_col()
            .w_full()
            .gap(px(32.0))
            .children(page_error)
            .children(notice)
            .child(self.render_agents(&theme, window, cx))
            .child(div().h(px(1.0)).w_full().bg(theme.divider_strong))
            .child(self.render_shared(&theme, window, cx))
    }
}
