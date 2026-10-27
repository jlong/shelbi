//! The Add Project wizard window (`dt-add-project-wizard`, Figma "01 ·
//! Project · Choose folder" 2481:8574, "01 · Project · Folder selected"
//! 2481:8810, "02 · Settings · In the project" 2483:8653 and "Outside the
//! project" 2483:8897).
//!
//! Every Add Project entry point (the sidebar's switcher, the palette and
//! File menu, the empty-project chooser) and the first launch with no
//! project open this one window, fixed at 800 × 580 with only the traffic
//! lights over its content. [`shelbi_app::add_project_wizard`] holds the
//! steps, the values, the Tab order and what each key does; this view paints
//! them and does the toolkit's part:
//!
//! - **01** picks the folder: Browse opens the native folder picker (a
//!   cancelled picker keeps the folder there was), and the path field takes
//!   a typed path (`~/…`, spaces and all). Each change is checked off the UI
//!   thread ([`Engine::check`]); an answer for an older path is dropped.
//!   Continue waits for a usable answer. A folder whose name is already a
//!   project's gets a name field (only then).
//! - **02** picks where settings live: two cards, one selected, with the
//!   real paths for the chosen folder.
//! - **Create Project** runs [`Engine::create`] (the shared
//!   `project_create::add_project`) once, off the UI thread; success opens
//!   the project ([`crate::app::open_added_project`]) and closes the
//!   wizard, a failure returns to 02 with the reason.
//!
//! Keys: Tab / ⇧Tab move focus (the focus ring shows once the keyboard is
//! used), Return is the enabled primary action, Space activates the focused
//! button or card, the arrows move between the cards, Esc and ⌘W cancel.
//! Cancelling writes nothing.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, Global, Hsla,
    PathPromptOptions, SharedString, Stateful, Subscription, Window, WindowHandle,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use shelbi_app::add_project_wizard::{
    self as wiz, Action, CreateRequest, FolderCheck, Focus, Origin, Screen, Tone, Wizard,
};
use shelbi_app::exec::Mutation;
use shelbi_core::ConfigMode;
use shelbi_orchestrator::project_create::AddProjectError;
use shelbi_state::keymap::{Key, KeyChord, Mods};

use crate::app::{CloseWindow, ShelbiWindow};
use crate::commands;
use crate::elements::controls;
use crate::keys::chord_from_keystroke;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::{self, Theme};

/// The window's size (the frames').
pub const SIZE: (f32, f32) = (800.0, 580.0);
/// The intro line's width.
const INTRO_W: f32 = 600.0;

/// [`Engine::check`]'s signature: the launch directory, the path field's
/// text, the typed name.
pub type CheckFn = dyn Fn(&Path, &str, Option<&str>) -> FolderCheck + Send + Sync;
/// [`Engine::create`]'s signature: the request and the launch directory.
pub type CreateFn = dyn Fn(&CreateRequest, &Path) -> Result<String, AddProjectError> + Send + Sync;

/// The blocking work the wizard runs off the UI thread. A test swaps in its
/// own, so nothing reads or writes a real Shelbi home.
#[derive(Clone)]
pub struct Engine {
    /// What the path field's folder is (`project_create::check_folder`).
    pub check: Arc<CheckFn>,
    /// Create the project; its id on success.
    pub create: Arc<CreateFn>,
}

impl Engine {
    /// The shared engine `shelbi init` and the TUI's form use.
    pub fn real() -> Self {
        Engine {
            check: Arc::new(shelbi_orchestrator::project_create::check_folder),
            create: Arc::new(|request, cwd| {
                let mutation = Mutation::AddProject {
                    name: request.name.clone(),
                    root: request.root.clone(),
                    mode: request.mode,
                };
                commands::run_add_project(&mutation, cwd)
            }),
        }
    }
}

/// What to do with a created project: open it (the window the wizard came
/// from switches to it, else a new window opens on it).
pub type OnCreated = Rc<dyn Fn(String, Option<WindowHandle<ShelbiWindow>>, &mut App)>;

/// The open wizard, so asking again brings it to the front.
#[derive(Default)]
struct OpenWizard(Option<WindowHandle<AddProjectView>>);

impl Global for OpenWizard {}

/// Open the wizard, or bring the open one to the front. `from` is the
/// project window it was asked from (`None` on the first launch).
pub fn show(origin: Origin, from: Option<WindowHandle<ShelbiWindow>>, cx: &mut App) {
    if let Some(open) = cx.try_global::<OpenWizard>().and_then(|o| o.0) {
        if open.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let cwd = launch_dir();
    let opened = cx.open_window(super::about::options("Add Project", SIZE, false, cx), |window, cx| {
        cx.new(|cx| {
            AddProjectView::new(
                origin,
                from,
                cwd,
                Engine::real(),
                Rc::new(crate::app::open_added_project),
                window,
                cx,
            )
        })
    });
    match opened {
        Ok(handle) => {
            cx.set_global(OpenWizard(Some(handle)));
            cx.activate(true);
        }
        Err(e) => tracing::error!(error = %e, "could not open Add Project"),
    }
}

/// What a typed relative path resolves against: the working directory, or
/// the home folder when the app was opened from Finder (its cwd is `/`).
fn launch_dir() -> PathBuf {
    std::env::current_dir()
        .ok()
        .filter(|d| d.parent().is_some())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The Add Project wizard.
pub struct AddProjectView {
    wizard: Wizard,
    focus: FocusHandle,
    path_input: Entity<InputState>,
    name_input: Entity<InputState>,
    /// The keyboard has moved focus: focus rings show from then on.
    keyboard: bool,
    from: Option<WindowHandle<ShelbiWindow>>,
    cwd: PathBuf,
    engine: Engine,
    on_created: OnCreated,
    _subscriptions: Vec<Subscription>,
}

impl AddProjectView {
    pub fn new(
        origin: Origin,
        from: Option<WindowHandle<ShelbiWindow>>,
        cwd: PathBuf,
        engine: Engine,
        on_created: OnCreated,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        theme::window::apply_to(window, cx);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let path_input = cx.new(|cx| InputState::new(window, cx).placeholder("~/Projects/my-project"));
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        let this_window = window.window_handle();
        let me = cx.weak_entity();
        let subscriptions = vec![
            theme::follow_system(window, cx),
            cx.subscribe_in(&path_input, window, |this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        this.wizard.set_path(text);
                        this.after_change(window, cx);
                    }
                    InputEvent::Focus => {
                        this.wizard.set_focus(Focus::Path);
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(&name_input, window, |this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        this.wizard.set_name(text);
                        this.after_change(window, cx);
                    }
                    InputEvent::Focus => {
                        this.wizard.set_focus(Focus::Name);
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            // Ahead of the text fields' own bindings (Tab indents, Return
            // and Esc are theirs otherwise): the wizard's keys are its own
            // everywhere in the window.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != this_window {
                    return;
                }
                let Some(chord) = chord_from_keystroke(&event.keystroke) else {
                    return;
                };
                let taken = me
                    .update(cx, |this, cx| this.key(chord, window, cx))
                    .unwrap_or(false);
                if taken {
                    cx.stop_propagation();
                }
            }),
        ];
        AddProjectView {
            wizard: Wizard::new(origin, home),
            focus,
            path_input,
            name_input,
            keyboard: false,
            from,
            cwd,
            engine,
            on_created,
            _subscriptions: subscriptions,
        }
    }

    /// The wizard's state (tests).
    #[cfg(test)]
    pub fn wizard(&self) -> &Wizard {
        &self.wizard
    }

    /// A key the wizard takes: Tab and ⇧Tab, Return and Esc anywhere;
    /// Space and the arrows off the text fields. `false` lets it through.
    fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let in_text = matches!(self.wizard.focus(), Focus::Path | Focus::Name);
        let bare = chord.mods == Mods::NONE;
        let taken = match chord.code {
            Key::Tab | Key::BackTab => bare || chord.mods == Mods::SHIFT,
            Key::Enter | Key::Esc => bare,
            Key::Char(' ') | Key::Up | Key::Down | Key::Left | Key::Right => bare && !in_text,
            _ => false,
        };
        if !taken {
            return false;
        }
        if chord.code != Key::Enter && chord.code != Key::Esc {
            self.keyboard = true;
        }
        let action = self.wizard.handle_key(chord, in_text);
        self.act(action, window, cx);
        true
    }

    /// A click (or Space) on `focus`.
    pub(crate) fn click(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        let action = self.wizard.activate(focus);
        self.act(action, window, cx);
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Stay => {}
            Action::Browse => self.browse(window, cx),
            Action::Close => {
                window.remove_window();
                return;
            }
            Action::Create(request) => self.create(request, window, cx),
        }
        self.after_change(window, cx);
    }

    /// After any change: keep focus on a control the screen offers, put the
    /// keyboard there, start a due check, repaint.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.wizard.settle_focus();
        self.sync_focus(window, cx);
        self.run_checks(window, cx);
        cx.notify();
    }

    /// Give the keyboard to the wizard's focused control: a text field's own
    /// input, else the window's root (which paints the ring on a button or
    /// card).
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.wizard.focus() {
            Focus::Path => self.path_input.read(cx).focus_handle(cx),
            Focus::Name => self.name_input.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    /// Check the current text off the UI thread, if a check is due. A
    /// newer text starts its own check; an answer for older text is dropped
    /// when it lands.
    fn run_checks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.wizard.check_due() else {
            return;
        };
        self.wizard.begin_check(key.clone());
        let (check, cwd) = (self.engine.check.clone(), self.cwd.clone());
        let task = cx.background_executor().spawn({
            let key = key.clone();
            async move { check(&cwd, &key.path, key.name.as_deref()) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.wizard.finish_check(key, result);
                // A clash opened the name field with the folder's name.
                if let Some(name) = this.wizard.name().map(str::to_string) {
                    if this.name_input.read(cx).value() != name.as_str() {
                        this.name_input.update(cx, |s, cx| s.set_value(name, window, cx));
                    }
                }
                this.after_change(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Browse: the native folder picker. A cancel keeps the folder there
    /// was.
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            this.update_in(cx, |this, window, cx| this.folder_picked(picked.as_deref(), window, cx))
                .ok();
        })
        .detach();
    }

    /// The folder picker closed with `path` (`None`: cancelled).
    pub fn folder_picked(&mut self, path: Option<&Path>, window: &mut Window, cx: &mut Context<Self>) {
        self.wizard.folder_picked(path);
        if path.is_some() {
            let text = self.wizard.path().to_string();
            self.path_input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        self.after_change(window, cx);
    }

    /// Create Project: the engine, once, off the UI thread. Success opens
    /// the project, then closes the wizard (in that order, so the first
    /// launch's app never runs out of windows); a failure goes back on 02.
    /// Should the wizard close mid-creation, a project that got created
    /// still opens.
    fn create(&mut self, request: CreateRequest, window: &mut Window, cx: &mut Context<Self>) {
        let (create, cwd) = (self.engine.create.clone(), self.cwd.clone());
        let work = cx.background_executor().spawn(async move { create(&request, &cwd) });
        let (from, on_created) = (self.from, self.on_created.clone());
        let me = window.window_handle().downcast::<Self>();
        cx.spawn(async move |_, cx| {
            let result = work.await;
            cx.update(|cx| match result {
                Ok(slug) => {
                    on_created(slug, from, cx);
                    if let Some(me) = me {
                        me.update(cx, |_, window, _| window.remove_window()).ok();
                    }
                }
                Err(AddProjectError::Invalid(msg) | AddProjectError::Failed(msg)) => {
                    if let Some(me) = me {
                        me.update(cx, |this, window, cx| {
                            this.wizard.creation_failed(msg);
                            this.after_change(window, cx);
                        })
                        .ok();
                    }
                }
            });
        })
        .detach();
    }

    /// Whether `focus` shows the keyboard's ring.
    fn ring(&self, focus: Focus, window: &Window) -> bool {
        self.keyboard && self.wizard.focus() == focus && self.focus.is_focused(window)
    }

    /// Back and Continue / Create Project, under the step count.
    fn footer(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let screen = self.wizard.screen();
        let back = if self.wizard.back_enabled() {
            style::button("add-project-back", "Back", ButtonKind::Secondary, ButtonSize::Base, self.ring(Focus::Back, window), theme)
                .on_click(cx.listener(|this, _, window, cx| this.click(Focus::Back, window, cx)))
        } else {
            style::disabled_button("add-project-back", "Back", ButtonKind::Secondary, ButtonSize::Base, theme)
        };
        let label = match screen {
            Screen::Folder => "Continue",
            Screen::Settings | Screen::Creating => "Create Project",
        };
        let primary = if self.wizard.primary_enabled() {
            style::button("add-project-primary", label, ButtonKind::Primary, ButtonSize::Base, self.ring(Focus::Primary, window), theme)
                .on_click(cx.listener(|this, _, window, cx| this.click(Focus::Primary, window, cx)))
        } else {
            style::disabled_button("add-project-primary", label, ButtonKind::Primary, ButtonSize::Base, theme)
        };
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .w_full()
            .child(
                div()
                    .debug_selector(|| "add-project-step".into())
                    .type_style(Type::SmallMedium)
                    .text_color(theme.muted)
                    .child(wiz::step_label(screen)),
            )
            .child(div().flex_1())
            .child(back)
            .child(primary)
    }

    fn browse_button(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        style::button("add-project-browse", "Browse", ButtonKind::Secondary, ButtonSize::Base, self.ring(Focus::Browse, window), theme)
            .on_click(cx.listener(|this, _, window, cx| this.click(Focus::Browse, window, cx)))
    }

    /// Screen 01's box: the prompt and Browse, or (once a folder is chosen)
    /// its name, what it is, and the path field.
    fn folder_box(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let shell = div()
            .debug_selector(|| "add-project-box".into())
            .flex()
            .w_full()
            .gap(px(16.0))
            .p(px(24.0))
            .rounded(px(12.0))
            // `surface/depth-10`, as the frames fill their boxes.
            .bg(theme.toolbar);
        if !self.wizard.chosen() {
            return shell
                .flex_row()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .type_style(Type::Base)
                        .text_color(theme.foreground)
                        .child(wiz::CHOOSE_PROMPT),
                )
                .child(self.browse_button(window, theme, cx));
        }
        let check = self.wizard.check();
        let title = check
            .and_then(|c| c.name.clone())
            .or_else(|| wiz::folder_name(Path::new(self.wizard.path().trim())))
            .unwrap_or_else(|| "Project folder".to_string());
        let (status, color): (SharedString, Hsla) = match check {
            Some(c) => (c.message().into(), tone_color(c.tone(), theme)),
            None => ("Checking this folder…".into(), theme.muted),
        };
        let field = |input: &Entity<InputState>, selector: &'static str, cx: &mut Context<Self>| {
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
            .debug_selector(move || selector.into())
            .flex_1()
        };
        let path_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .w_full()
            .child(field(&self.path_input, "add-project-path", cx))
            .child(self.browse_button(window, theme, cx));
        let name_row = self.wizard.name().is_some().then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .w_full()
                .child(
                    div()
                        .type_style(Type::BaseMedium)
                        .text_color(theme.text_loud)
                        .child("Project name"),
                )
                .child(div().flex().w_full().child(field(&self.name_input, "add-project-name", cx)))
        });
        shell
            .flex_col()
            .items_start()
            .child(
                div()
                    .debug_selector(|| "add-project-folder-name".into())
                    .type_style(Type::BaseSemibold)
                    .text_color(theme.text_loud)
                    .child(title),
            )
            .child(
                div()
                    .debug_selector(|| "add-project-status".into())
                    .w_full()
                    .type_style(Type::Base)
                    .text_color(color)
                    .child(status),
            )
            .child(path_row)
            .children(name_row)
    }

    /// One of screen 02's cards.
    fn card(&self, mode: ConfigMode, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let (id, title, body) = match mode {
            ConfigMode::InRepo => ("add-project-card-in-repo", wiz::IN_REPO_TITLE, wiz::IN_REPO_BODY),
            ConfigMode::Global => ("add-project-card-global", wiz::GLOBAL_TITLE, wiz::GLOBAL_BODY),
        };
        let selected = self.wizard.mode() == mode;
        let path = self.wizard.storage_path(mode).unwrap_or_default();
        div()
            .id(id)
            .debug_selector(move || id.into())
            .relative()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(16.0))
            .w_full()
            .p(px(20.0))
            .rounded(px(12.0))
            .border_1()
            .cursor_pointer()
            .when(selected, |d| d.bg(theme.primary_fill).border_color(theme.focus_ring))
            .when(!selected, |d| {
                d.bg(theme.toolbar)
                    .border_color(theme.divider)
                    .hover(|s| s.bg(theme.background_hover))
            })
            .when(self.ring(Focus::Card(mode), window), |d| {
                d.children(style::focus_bands_past(12.0, 1.0, theme))
            })
            .child(radio(selected, theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.0))
                    .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(title))
                    .child(div().type_style(Type::Base).text_color(theme.foreground).child(body))
                    .child(
                        div()
                            .debug_selector(move || format!("{id}-path"))
                            .type_style(Type::Small)
                            .text_color(theme.muted)
                            .truncate()
                            .child(path),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.click(Focus::Card(mode), window, cx)))
    }
}

/// A status line's color.
fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Normal => theme.foreground,
        Tone::Warning => theme.warning,
        Tone::Error => theme.error,
    }
}

/// A card's radio: on, a ring with a dot inside in the Primary label's
/// color; off, a heavier ring in the gray (the frames' ◉ and ○, 14 × 18).
fn radio(on: bool, theme: &Theme) -> Div {
    let ring = div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(14.0))
        .rounded_full();
    let ring = if on {
        ring.border(px(1.25))
            .border_color(theme.primary_label)
            .child(div().size(px(10.0)).rounded_full().bg(theme.primary_label))
    } else {
        ring.border(px(1.75)).border_color(theme.gray)
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(14.0))
        .h(px(18.0))
        .child(ring)
}

impl Render for AddProjectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let screen = self.wizard.screen();
        let intro = match screen {
            Screen::Folder => wiz::FOLDER_INTRO.to_string(),
            Screen::Settings => wiz::SETTINGS_INTRO.to_string(),
            Screen::Creating => {
                let name = self.wizard.check().and_then(|c| c.name.clone()).unwrap_or_default();
                format!("Setting up {name}…")
            }
        };
        let details = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .justify_center()
            .gap(px(16.0))
            .w_full();
        let details = match screen {
            Screen::Folder => details.child(self.folder_box(window, &theme, cx)),
            Screen::Settings => details
                .child(self.card(ConfigMode::InRepo, window, &theme, cx))
                .child(self.card(ConfigMode::Global, window, &theme, cx))
                .children(self.wizard.error().map(|msg| {
                    div()
                        .debug_selector(|| "add-project-error".into())
                        .type_style(Type::Small)
                        .text_color(theme.error)
                        .child(msg.to_string())
                })),
            Screen::Creating => details,
        };
        let root: AnyElement = div()
            .id("add-project")
            .debug_selector(|| "add-project".into())
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(24.0))
            .p(px(48.0))
            .bg(theme.canvas)
            .font_family(style::UI_FAMILY)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .w_full()
                    .text_center()
                    .child(
                        div()
                            .debug_selector(|| "add-project-title".into())
                            .type_style(Type::Title)
                            .text_color(theme.text_loud)
                            .child(screen.title()),
                    )
                    .child(
                        div()
                            .w(px(INTRO_W))
                            .type_style(Type::Base)
                            .text_color(theme.foreground)
                            .child(intro),
                    ),
            )
            .child(details)
            .child(self.footer(window, &theme, cx))
            .into_any_element();
        root
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Mutex;

    use gpui::{TestAppContext, VisualTestContext};
    use shelbi_app::add_project_wizard::{Problem, Repo};

    use super::*;

    /// A folder check that knows `/repo`, `/repo two` (spaces), `/wt` (a
    /// worktree), `/plain` (no Git) and `/taken` (an id already
    /// registered); anything else is missing. `/slow…` paths wait for the
    /// test to release them.
    fn fake_check(cwd: &Path, root: &str, name: Option<&str>) -> FolderCheck {
        let path = shelbi_orchestrator::project_create::absolutize(cwd, Path::new(root.trim()));
        let folder = wiz::folder_name(&path).unwrap_or_default();
        let name = name.map(str::to_string).unwrap_or(folder.clone());
        let slug = shelbi_core::normalize_project_name(&name).ok();
        let repo = match folder.as_str() {
            "repo" | "repo two" | "taken" | "slow" => Repo::Git,
            "wt" => Repo::Worktree,
            "plain" => Repo::NotGit,
            _ => return FolderCheck::failed(path, Problem::NotFound),
        };
        let problem = match slug.as_deref() {
            None => Some(Problem::NoName),
            Some("taken") => Some(Problem::Taken { slug: "taken".into() }),
            _ => None,
        };
        FolderCheck {
            in_repo_dir: path.join(".shelbi"),
            global_dir: slug.as_ref().map(|s| PathBuf::from("/home/.shelbi/projects").join(s)),
            path,
            name: Some(name),
            slug,
            repo: Some(repo),
            problem,
        }
    }

    struct Rig {
        cx: VisualTestContext,
        view: Entity<AddProjectView>,
        created: Arc<Mutex<Vec<CreateRequest>>>,
        opened: Rc<RefCell<Vec<String>>>,
        windows: usize,
    }

    fn rig(cx: &mut TestAppContext, origin: Origin, result: Result<String, AddProjectError>) -> Rig {
        super::super::about::tests::init(cx);
        let created = Arc::new(Mutex::new(Vec::new()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let engine = Engine {
            check: Arc::new(fake_check),
            create: Arc::new({
                let created = created.clone();
                move |request, _| {
                    created.lock().unwrap().push(request.clone());
                    result.clone()
                }
            }),
        };
        let on_created: OnCreated = Rc::new({
            let opened = opened.clone();
            move |slug, _, _| opened.borrow_mut().push(slug)
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            AddProjectView::new(origin, None, PathBuf::from("/"), engine, on_created, window, cx)
        });
        let windows = cx.update(|cx| cx.windows().len());
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        window
            .update(&mut vcx, |_, _, cx| view = Some(cx.entity()))
            .unwrap();
        Rig {
            cx: vcx,
            view: view.unwrap(),
            created,
            opened,
            windows,
        }
    }

    impl Rig {
        fn wizard<R>(&mut self, f: impl FnOnce(&Wizard) -> R) -> R {
            self.view.read_with(&self.cx, |v, _| f(v.wizard()))
        }

        fn pick(&mut self, path: Option<&str>) {
            let view = self.view.clone();
            self.cx.update(|window, cx| {
                view.update(cx, |v, cx| v.folder_picked(path.map(Path::new), window, cx))
            });
            self.cx.run_until_parked();
        }

        fn type_path(&mut self, text: &str) {
            let input = self.view.read_with(&self.cx, |v, _| v.path_input.clone());
            self.cx.update(|window, cx| input.update(cx, |s, cx| s.set_value(text.to_string(), window, cx)));
            let view = self.view.clone();
            self.cx.update(|window, cx| {
                view.update(cx, |v, cx| {
                    v.wizard.set_path(text.to_string());
                    v.after_change(window, cx)
                })
            });
            self.cx.run_until_parked();
        }

        fn click(&mut self, selector: &'static str) {
            let bounds = self.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} isn't drawn"));
            self.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
            self.cx.run_until_parked();
        }

        fn keys(&mut self, keys: &str) {
            self.cx.simulate_keystrokes(keys);
            self.cx.run_until_parked();
        }

        fn drawn(&mut self, selector: &'static str) -> bool {
            self.cx.debug_bounds(selector).is_some()
        }

        fn window_open(&mut self) -> bool {
            // The app's, not the window's: it may have closed.
            self.cx.cx.update(|cx| cx.windows().len()) == self.windows
        }
    }

    #[gpui::test]
    fn screen_one_starts_empty_and_a_cancelled_picker_keeps_the_choice(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::Workspace, Ok("repo".into()));
        assert!(r.drawn("add-project-browse"));
        assert!(!r.drawn("add-project-path"), "no path field before a folder is chosen");
        r.click("add-project-primary");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Folder, "Continue starts disabled");

        r.pick(None);
        assert!(!r.wizard(|w| w.chosen()), "a cancelled picker chooses nothing");
        r.pick(Some("/repo two"));
        assert_eq!(r.wizard(|w| w.path().to_string()), "/repo two");
        assert!(r.drawn("add-project-path"));
        assert!(r.wizard(|w| w.primary_enabled()), "a Git folder can continue");
        r.pick(None);
        assert_eq!(r.wizard(|w| w.path().to_string()), "/repo two", "cancel keeps the folder");
        assert_eq!(r.wizard(|w| w.check().and_then(|c| c.name.clone())).as_deref(), Some("repo two"));
    }

    #[gpui::test]
    fn invalid_folders_keep_continue_disabled_and_a_plain_folder_warns(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::Workspace, Ok("x".into()));
        r.type_path("/nowhere");
        assert!(!r.wizard(|w| w.primary_enabled()));
        assert_eq!(r.wizard(|w| w.check().map(|c| c.tone())), Some(Tone::Error));
        r.type_path("/plain");
        assert!(r.wizard(|w| w.primary_enabled()), "a non-Git folder is allowed");
        assert_eq!(r.wizard(|w| w.check().map(|c| c.tone())), Some(Tone::Warning));
        r.type_path("/wt");
        assert_eq!(r.wizard(|w| w.check().and_then(|c| c.repo)), Some(Repo::Worktree));
        assert!(r.wizard(|w| w.primary_enabled()));
    }

    #[gpui::test]
    fn a_clash_shows_the_name_field_seeded_and_a_new_name_continues(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::Workspace, Ok("taken-2".into()));
        r.type_path("/taken");
        assert!(r.drawn("add-project-name"), "the name field shows only on a clash");
        assert!(!r.wizard(|w| w.primary_enabled()));
        let seeded = r.view.read_with(&r.cx, |v, cx| v.name_input.read(cx).value().to_string());
        assert_eq!(seeded, "taken");
        let view = r.view.clone();
        r.cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.wizard.set_name("Taken 2");
                v.after_change(window, cx)
            })
        });
        r.cx.run_until_parked();
        assert!(r.wizard(|w| w.primary_enabled()));
    }

    #[gpui::test]
    fn the_flow_keeps_its_values_and_creates_once(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::Workspace, Ok("repo".into()));
        r.pick(Some("/repo"));
        r.click("add-project-primary");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Settings);
        let path = |r: &mut Rig, id: &'static str| {
            let _ = r.cx.debug_bounds(id).expect("drawn");
            r.wizard(|w| (w.storage_path(ConfigMode::InRepo), w.storage_path(ConfigMode::Global)))
        };
        assert_eq!(
            path(&mut r, "add-project-card-in-repo-path"),
            (Some("repo/.shelbi".into()), Some("/home/.shelbi/projects/repo".into()))
        );

        // Pick a card, go Back and forward: the folder and the card hold.
        r.click("add-project-card-in-repo");
        assert_eq!(r.wizard(|w| w.mode()), ConfigMode::InRepo);
        r.click("add-project-back");
        assert_eq!(r.wizard(|w| (w.screen(), w.path().to_string())), (Screen::Folder, "/repo".into()));
        r.click("add-project-primary");
        assert_eq!(r.wizard(|w| w.mode()), ConfigMode::InRepo);

        // Return creates; a second Return, while it runs, doesn't.
        r.keys("enter enter");
        assert_eq!(
            *r.created.lock().unwrap(),
            [CreateRequest { name: "repo".into(), root: "/repo".into(), mode: ConfigMode::InRepo }]
        );
        assert_eq!(*r.opened.borrow(), ["repo"]);
        assert!(!r.window_open(), "success closes the wizard");
    }

    #[gpui::test]
    fn a_failed_create_returns_to_settings_with_the_reason(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::Workspace, Err(AddProjectError::Invalid("a project already lives at `repo`".into())));
        r.pick(Some("/repo"));
        r.keys("enter");
        r.keys("enter");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Settings);
        assert!(r.drawn("add-project-error"));
        assert!(r.opened.borrow().is_empty());
        assert!(r.window_open());
    }

    #[gpui::test]
    fn keyboard_tab_space_arrows_and_escape(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::Workspace, Ok("repo".into()));
        r.pick(Some("/repo"));
        // Browse has focus; Tab to Back, then Continue, then the path field.
        r.keys("tab");
        assert_eq!(r.wizard(|w| w.focus()), Focus::Back);
        r.keys("tab");
        assert_eq!(r.wizard(|w| w.focus()), Focus::Primary);
        r.keys("space");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Settings, "Space presses Continue");
        assert_eq!(r.wizard(|w| w.focus()), Focus::Card(ConfigMode::Global));
        r.keys("up");
        assert_eq!(r.wizard(|w| w.mode()), ConfigMode::InRepo, "an arrow moves the choice");
        r.keys("shift-tab");
        assert_eq!(r.wizard(|w| w.focus()), Focus::Primary, "⇧Tab wraps backward");
        r.keys("escape");
        assert!(!r.window_open(), "Esc cancels");
        assert!(r.created.lock().unwrap().is_empty(), "cancelling creates nothing");
    }

    #[gpui::test]
    fn first_launch_disables_screen_one_back(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::FirstLaunch, Ok("repo".into()));
        r.click("add-project-back");
        assert!(r.window_open(), "a disabled Back does nothing");
        assert!(!r.wizard(|w| w.back_enabled()));
    }

    #[gpui::test]
    fn a_check_for_an_older_path_never_lands(cx: &mut TestAppContext) {
        // Two picks before either check runs: both checks run, and whichever
        // lands last, the shown answer is the current path's.
        let mut r = rig(cx, Origin::Workspace, Ok("repo".into()));
        let view = r.view.clone();
        r.cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.folder_picked(Some(Path::new("/repo")), window, cx);
                v.folder_picked(Some(Path::new("/nowhere")), window, cx);
            })
        });
        r.cx.run_until_parked();
        assert_eq!(r.wizard(|w| w.path().to_string()), "/nowhere");
        assert_eq!(
            r.wizard(|w| w.check().and_then(|c| c.problem.clone())),
            Some(Problem::NotFound),
            "the /repo answer was dropped"
        );
        assert!(!r.wizard(|w| w.primary_enabled()));
    }
}
