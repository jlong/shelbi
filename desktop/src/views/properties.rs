//! Settings ▸ Properties (`settings-properties-page`): the project YAML's own
//! settings, over the shared
//! [`PropertiesForm`](shelbi_app::settings::properties::PropertiesForm) the
//! TUI's page draws too.
//!
//! ```text
//!  Properties
//!  Settings for this project, stored in ~/.shelbi/projects/shelbi.yaml.
//!
//!  Name               [ Shelbi                          ]
//!  Repository         [ git@github.com:jlong/shelbi.git ]
//!  Default branch     [ main                            ]
//!  Default workflow   [ app                           ⌄ ]
//!  Editor             [ Default ($EDITOR or vim)      ⌄ ]
//!  Issue tracker      GitHub · jlong/shelbi
//!  ▸ Advanced
//!     Workspace poll interval    [ 5   ] seconds
//!     GitHub reconcile interval  [ 900 ] seconds
//! ```
//!
//! The project's config is read off the UI thread through the window's
//! source (`workflow_config`, which carries the project YAML), and read
//! again each time the page is shown. A field saves when it's committed
//! (Return, or focus leaving it) and a picker when a choice is made, through
//! the window's config store (the project's daemon, here or on the far
//! machine), as the Orchestrator and General pages' controls do. A refused
//! value shows its error under the field and isn't written.

use std::collections::HashMap;
use std::path::Path;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, App, ClickEvent, Context, Entity, Focusable, FontWeight, MouseDownEvent, Role,
    SharedString, Subscription, Window,
};
use gpui_kit::component::input::{InputEvent, InputState};
use shelbi_app::settings::general::{PickerEntry, ToolSetting};
use shelbi_app::settings::orchestrator::SaveStatus;
use shelbi_app::settings::properties::{
    self as props, PropertiesForm, Text, ADVANCED_LABEL, DEFAULT_WORKFLOW_HELP, DEFAULT_WORKFLOW_LABEL,
    ISSUE_TRACKER_LABEL, ISSUE_TRACKER_NOTE, SECONDS, TITLE,
};
use shelbi_app::source::ProjectHandle;
use shelbi_state::tools::DetectedTools;
use shelbi_state::workflow_config::WorkflowConfig;

use super::tool_select::{AppIcons, ToolSelect, ToolSelectEvent};
use super::workflow_editor::text_field;
use crate::assets::{glyph, glyphs, heroicon, icons};
use crate::elements::controls;
use crate::elements::placed::{placed, MenuScroll};
use crate::theme::style::{self, Anchor, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The page title's size (Desktop/Heading, as the Workspaces page's).
const TITLE_SIZE: f32 = 18.0;
/// An interval field's width.
const NUMBER_W: f32 = 96.0;
/// The editor picker's element ids' prefix.
const EDITOR_PREFIX: &str = "properties-editor";

/// A text field's selector.
pub fn text_selector(t: Text) -> &'static str {
    match t {
        Text::Label => "properties-name",
        Text::Repository => "properties-repository",
        Text::DefaultBranch => "properties-default-branch",
        Text::Editor => "properties-editor-command",
        Text::PollInterval => "properties-poll-interval",
        Text::ReconcileInterval => "properties-reconcile-interval",
    }
}

/// The Properties page.
pub struct PropertiesPage {
    form: PropertiesForm,
    handle: ProjectHandle,
    inputs: HashMap<Text, Entity<InputState>>,
    editor_select: Entity<ToolSelect>,
    /// Other command… is being typed.
    typing_editor: bool,
    /// Why Browse's pick can't be the editor.
    browse_error: Option<String>,
    detected: DetectedTools,
    /// The Default workflow menu's highlighted row, while it's open.
    workflow_menu: Option<usize>,
    workflow_anchor: Anchor,
    workflow_scroll: MenuScroll,
    /// Re-read the config on the next draw (the page was shown again).
    reload: bool,
    _subscriptions: Vec<Subscription>,
}

impl PropertiesPage {
    pub fn new(handle: &ProjectHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let form = PropertiesForm::default();
        let mut inputs = HashMap::new();
        let mut subscriptions = Vec::new();
        for t in props::TEXTS {
            let input = cx.new(|cx| InputState::new(window, cx));
            subscriptions.push(cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => this.typed(t, cx),
                    InputEvent::PressEnter { .. } => this.commit(window, cx),
                    // The editor's command saves on Return (or Save); a click
                    // elsewhere leaves it unsaved, as General's.
                    InputEvent::Blur if t != Text::Editor => this.commit(window, cx),
                    _ => {}
                }
            }));
            inputs.insert(t, input);
        }
        let editor_select = cx.new(|cx| {
            ToolSelect::new(ToolSetting::Editor, form.editor_picker(&DetectedTools::default()), cx)
                .with_prefix(EDITOR_PREFIX)
        });
        subscriptions.push(cx.subscribe_in(&editor_select, window, |this, _, event: &ToolSelectEvent, window, cx| {
            match event {
                ToolSelectEvent::Picked(entry) => this.pick_editor(entry, window, cx),
            }
        }));
        let mut page = PropertiesPage {
            form,
            handle: handle.clone(),
            inputs,
            editor_select,
            typing_editor: false,
            browse_error: None,
            detected: DetectedTools::default(),
            workflow_menu: None,
            workflow_anchor: Anchor::default(),
            workflow_scroll: MenuScroll::default(),
            reload: false,
            _subscriptions: subscriptions,
        };
        // A test's page never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.detect(cx);
            page.load(window, cx);
        }
        page
    }

    /// Find this machine's editors for the picker (none for a project on
    /// another machine: its editor runs there).
    fn detect(&mut self, cx: &mut Context<Self>) {
        if self.handle.link.is_remote() {
            return;
        }
        let detect = cx.background_executor().spawn(async { shelbi_state::tools::detected_tools() });
        cx.spawn(async move |this, cx| {
            let detected = detect.await;
            this.update(cx, |this, cx| {
                this.detected = detected;
                this.sync(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Read the project's config off the UI thread and show it.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.handle.source.clone();
        let read = cx.background_executor().spawn(async move { source.workflow_config() });
        cx.spawn_in(window, async move |this, cx| {
            let config = read.await;
            this.update_in(cx, |this, window, cx| this.loaded(config, window, cx)).ok();
        })
        .detach();
    }

    /// The config was read (a test hands it in).
    pub fn loaded(&mut self, config: Result<WorkflowConfig, String>, window: &mut Window, cx: &mut Context<Self>) {
        self.form.load(config);
        self.fill(window, cx);
        self.sync(cx);
    }

    /// Show the form's text in every field that doesn't hold it already.
    fn fill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (t, input) in &self.inputs {
            let text = self.form.text(*t).to_string();
            if input.read(cx).value() != text.as_str() {
                input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
        }
    }

    /// Show the form's editor in its picker.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let picker = self.form.editor_picker(&self.detected);
        let enabled = self.form.editor_editable() && !self.form.saving();
        self.editor_select.update(cx, |s, cx| s.set(picker, &AppIcons::new(), enabled, cx));
        cx.notify();
    }

    /// Re-read the config when next drawn (the page was shown again), and
    /// forget the last visit's save line. A test's page reads no disk.
    pub fn reload_when_drawn(&mut self) {
        self.form.clear_status();
        self.reload = !cfg!(test);
    }

    #[cfg(any(test, feature = "visual-check"))]
    pub fn form(&self) -> &PropertiesForm {
        &self.form
    }

    /// `t`'s field.
    #[cfg(test)]
    pub fn input(&self, t: Text) -> Entity<InputState> {
        self.inputs[&t].clone()
    }

    /// Open Advanced (a visual state).
    #[cfg(feature = "visual-check")]
    pub(crate) fn offscreen_open_advanced(&mut self, cx: &mut Context<Self>) {
        if !self.form.advanced_open() {
            self.form.toggle_advanced();
        }
        cx.notify();
    }

    /// A field's text changed.
    fn typed(&mut self, t: Text, cx: &mut Context<Self>) {
        let text = self.inputs[&t].read(cx).value().to_string();
        if text != self.form.text(t) {
            self.form.set_text(t, &text);
            cx.notify();
        }
    }

    /// Check the fields and save what changed.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // What each field holds now (a paste or a programmatic set sends no
        // Change). The editor's command counts only while it's typed.
        for t in props::TEXTS {
            if t == Text::Editor && !self.typing_editor {
                continue;
            }
            let text = self.inputs[&t].read(cx).value().to_string();
            if text != self.form.text(t) {
                self.form.set_text(t, &text);
            }
        }
        self.typing_editor = false;
        let next = self.form.commit();
        self.sync(cx);
        if let Some(config) = next {
            self.save(config, window, cx);
        }
    }

    /// Write `config` off the UI thread; a commit queued behind it follows.
    fn save(&mut self, config: WorkflowConfig, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.handle.clone();
        let label_before = self.form.config().and_then(|c| c.properties().label.clone());
        let save = cx.background_executor().spawn(async move { props::save(&store, &config) });
        cx.spawn_in(window, async move |this, cx| {
            let done = save.await;
            this.update_in(cx, |this, window, cx| {
                let next = this.form.saved_result(done);
                this.fill(window, cx);
                this.sync(cx);
                // A new name shows on the tab and the sidebar header: the
                // catalog reads it again (the shown project's model follows
                // on its next refresh).
                let label_after = this.form.config().and_then(|c| c.properties().label.clone());
                if label_after != label_before {
                    if let Some(catalog) = cx.try_global::<crate::projects::Catalog>().cloned() {
                        catalog.refresh(|| {});
                    }
                }
                if let Some(next) = next {
                    this.save(next, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// An entry was picked from the editor's picker.
    fn pick_editor(&mut self, entry: &PickerEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_error = None;
        match entry {
            PickerEntry::OtherCommand => {
                self.typing_editor = true;
                let text = self.form.text(Text::Editor).to_string();
                let focus = self.inputs[&Text::Editor].update(cx, |s, cx| {
                    s.set_value(text, window, cx);
                    s.focus_handle(cx)
                });
                window.focus(&focus, cx);
                cx.notify();
            }
            PickerEntry::Browse => self.browse_editor(window, cx),
            PickerEntry::Default | PickerEntry::Tool(_) => {
                self.typing_editor = false;
                if let Some(config) = self.form.choose_editor_entry(entry) {
                    self.fill(window, cx);
                    self.sync(cx);
                    self.save(config, window, cx);
                }
            }
        }
    }

    /// Choose a program for the editor in a native open panel.
    fn browse_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked =
            crate::platform::open_panel::choose_file(Path::new("/usr/local/bin"), "Choose the editor to run in a pane.");
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(path)) = picked.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.browse_picked(&path, window, cx)).ok();
        })
        .detach();
    }

    /// Browse picked `path`: a program is saved; an app says why it can't be.
    pub(crate) fn browse_picked(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        match self.form.browse_editor(path) {
            Ok(Some(config)) => {
                self.fill(window, cx);
                self.sync(cx);
                self.save(config, window, cx);
            }
            Ok(None) => {}
            Err(e) => {
                self.browse_error = Some(e);
                cx.notify();
            }
        }
    }

    /// Pick `name` as the default workflow.
    pub fn choose_workflow(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.workflow_menu = None;
        if let Some(config) = self.form.choose_workflow(name) {
            self.save(config, window, cx);
        }
        cx.notify();
    }

    fn toggle_workflow_menu(&mut self, cx: &mut Context<Self>) {
        self.workflow_menu = match self.workflow_menu {
            Some(_) => None,
            None if self.form.editable() => {
                let all = self.form.workflows();
                Some(all.iter().position(|w| w == self.form.default_workflow()).unwrap_or(0))
            }
            None => None,
        };
        cx.notify();
    }
}

/// A label over its control, the help and the field's error under it.
fn row(
    selector: &'static str,
    label: &'static str,
    control: impl IntoElement,
    help: String,
    error: Option<String>,
    theme: &Theme,
) -> AnyElement {
    controls::labeled_control(label, control, Some(help.into()), theme)
        .id(selector)
        .debug_selector(move || format!("{selector}-row"))
        .role(Role::Group)
        .aria_label(label)
        .children(error.map(|e| {
            div()
                .debug_selector(move || format!("{selector}-error"))
                .type_style(Type::XSmall)
                .text_color(theme.error)
                .child(e)
        }))
        .into_any_element()
}

impl PropertiesPage {
    /// A text field's row.
    fn text_row(&self, t: Text, theme: &Theme, window: &Window, cx: &App) -> AnyElement {
        let selector = text_selector(t);
        let input = &self.inputs[&t];
        let placeholder = Some(SharedString::from(self.form.placeholder(t)));
        let invalid = self.form.error(t).is_some();
        let field = text_field(input, selector, placeholder, invalid, window, cx);
        let control = if t.numeric() {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(controls::CONTROL_GAP))
                .child(field.w(px(NUMBER_W)).flex_none())
                .child(div().type_style(Type::Base).text_color(theme.form_hint).child(SECONDS))
        } else {
            div().w(px(controls::CONTROL_W)).max_w_full().child(field)
        };
        row(selector, t.label(), control, self.form.help(t), self.form.error(t).map(str::to_string), theme)
    }

    /// The Default workflow picker: the button with the workflow and a
    /// chevron, the workflows floating under it while open.
    fn workflow_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.workflow_menu;
        let current = self.form.default_workflow().to_string();
        let chevron = style::button_colors(ButtonKind::Secondary, theme).icon;
        let button = style::button_base("properties-default-workflow-button", ButtonKind::Secondary, ButtonSize::Base, false, theme)
            .w_full()
            .justify_between()
            .role(Role::ComboBox)
            .aria_label(format!("{DEFAULT_WORKFLOW_LABEL}: {current}"))
            .aria_expanded(open.is_some())
            .when(!self.form.editable(), |b| b.opacity(0.6))
            .child(div().flex_1().min_w_0().truncate().child(current.clone()))
            .child(style::dropdown_slot(ButtonSize::Base, chevron))
            .child(style::anchor_probe(&self.workflow_anchor))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_workflow_menu(cx)));
        self.workflow_scroll.follow(open);
        let menu = open.map(|highlight| {
            let items: Vec<AnyElement> = self
                .form
                .workflows()
                .into_iter()
                .enumerate()
                .map(|(i, name)| {
                    let id = SharedString::from(format!("properties-workflow-{name}"));
                    let selected = name == current;
                    let pick = name.clone();
                    style::menu_row(id, i == highlight, theme)
                        .role(Role::ListBoxOption)
                        .aria_label(name.clone())
                        .aria_selected(selected)
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .children(selected.then(|| heroicon(icons::CHECK).text_color(theme.form_hint)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.choose_workflow(&pick, window, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            let list = style::popover(
                div()
                    .id("properties-workflows")
                    .debug_selector(|| "properties-workflows".into())
                    .role(Role::ListBox)
                    .aria_label("Workflows")
                    .flex()
                    .flex_col()
                    .p_1()
                    .overflow_y_scroll()
                    .track_scroll(self.workflow_scroll.handle())
                    .occlude(),
                theme,
            )
            .children(items);
            let list = placed(&self.workflow_anchor, list).match_width().on_dismiss(cx.listener(
                |this, _: &MouseDownEvent, _, cx| {
                    this.workflow_menu = None;
                    cx.notify();
                },
            ));
            deferred(list).with_priority(2)
        });
        let control = div().relative().w(px(controls::CONTROL_W)).max_w_full().child(button).children(menu);
        row("properties-default-workflow", DEFAULT_WORKFLOW_LABEL, control, DEFAULT_WORKFLOW_HELP.into(), None, theme)
    }

    /// The Editor: its picker, and the command field under it while Other
    /// command… is being typed.
    fn editor_row(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let field = self.typing_editor.then(|| {
            let input = &self.inputs[&Text::Editor];
            controls::picker(
                text_field(input, text_selector(Text::Editor), None, self.form.error(Text::Editor).is_some(), window, cx)
                    .flex_1()
                    .min_w_0(),
                controls::form_button("properties-editor-command-save", "Save", theme)
                    .on_click(cx.listener(|this, _, window, cx| this.commit(window, cx))),
            )
        });
        let help = if self.typing_editor {
            "A command line, with any flags (code --wait). Return saves it; a blank command uses the default.".to_string()
        } else {
            self.form.help(Text::Editor)
        };
        let error = self.browse_error.clone().or_else(|| self.form.error(Text::Editor).map(str::to_string));
        let control = div()
            .flex()
            .flex_col()
            .gap(px(controls::CONTROL_GAP))
            .w(px(controls::CONTROL_W))
            .max_w_full()
            .child(self.editor_select.clone())
            .children(field);
        row("properties-editor", Text::Editor.label(), control, help, error, theme)
    }

    /// Advanced's disclosure: a chevron and the label.
    fn advanced_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.form.advanced_open();
        div()
            .id("properties-advanced")
            .debug_selector(|| "properties-advanced".into())
            .role(Role::Button)
            .aria_label(ADVANCED_LABEL)
            .aria_expanded(open)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .text_color(theme.form_hint)
            .hover(|d| d.text_color(theme.text_loud))
            // The sidebar sections' chevron, turned as theirs is.
            .child(
                glyph(glyphs::CHEVRON_DOWN)
                    .text_color(theme.form_hint)
                    .with_transformation(crate::theme::fold::chevron(if open { 1.0 } else { 0.0 })),
            )
            .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(ADVANCED_LABEL))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.form.toggle_advanced();
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for PropertiesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if std::mem::take(&mut self.reload) {
            self.load(window, cx);
        }
        let line = |selector: &'static str, text: String, color| {
            div()
                .debug_selector(move || selector.into())
                .type_style(Type::XSmall)
                .text_color(color)
                .child(text)
        };
        let unreadable = self.form.unreadable().map(|e| line("properties-unreadable", e.to_string(), theme.error));
        let status = self.form.status_message().map(|text| {
            let color = if matches!(self.form.status(), SaveStatus::Failed(_)) { theme.error } else { theme.form_hint };
            line("properties-status", text, color)
        });
        let main = [Text::Label, Text::Repository, Text::DefaultBranch].map(|t| self.text_row(t, &theme, window, cx));
        let tracker = self.form.issue_tracker().map(|summary| {
            row(
                "properties-issue-tracker",
                ISSUE_TRACKER_LABEL,
                div()
                    .debug_selector(|| "properties-issue-tracker-summary".into())
                    .type_style(Type::Base)
                    .text_color(theme.form_text)
                    .child(summary),
                ISSUE_TRACKER_NOTE.into(),
                None,
                &theme,
            )
        });
        let advanced = self.form.advanced_open().then(|| {
            div()
                .flex()
                .flex_col()
                .gap_6()
                .pl(px(26.0))
                .child(self.text_row(Text::PollInterval, &theme, window, cx))
                .child(self.text_row(Text::ReconcileInterval, &theme, window, cx))
        });
        div()
            .id("settings-properties")
            .debug_selector(|| "settings-properties".into())
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
                    .child(
                        div()
                            .debug_selector(|| "properties-subtitle".into())
                            .type_style(Type::Small)
                            .text_color(theme.form_hint)
                            .child(self.form.subtitle()),
                    ),
            )
            .children(unreadable)
            .children(main)
            .child(self.workflow_row(&theme, cx))
            .child(self.editor_row(&theme, window, cx))
            .children(tracker)
            .child(self.advanced_toggle(&theme, cx))
            .children(advanced)
            .children(status)
    }
}
