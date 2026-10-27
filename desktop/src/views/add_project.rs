//! The Add Project wizard window (`dt-add-project-wizard`, Figma "01 ·
//! Project · Choose folder" 2481:8574, "01 · Project · Folder selected"
//! 2481:8810, "02 · Settings · In the project" 2483:8653 and "Outside the
//! project" 2483:8897).
//!
//! Screen 03 is "03 · Project · Writing config" 2484:8673, "Creating
//! agents" 2514:45915 and "Adding workflows" 2514:45972
//! (`dt-project-creation-checklist`), and screen 04 is "04 · Workflows ·
//! Project ready" 2484:8897 (`dt-wizard-workflow-summary`).
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
//!   `project_create::add_project`) once, off the UI thread, and **03**
//!   shows it as a checklist: the engine reports each stage starting,
//!   finishing or failing through a [`ScaffoldReporter`] whose events cross
//!   to the UI thread over a channel, and the wizard checks a row only when
//!   its stage finishes. There's no footer while it runs. A failure keeps
//!   the checked rows, marks the one that failed with the reason, and
//!   offers Try Again, which runs the same request as a retry (the engine
//!   finishes the partly created project rather than calling its own
//!   registration a clash). Closing the window mid-run drops the run's
//!   events and result; nothing restarts it.
//! - **04**, once the engine has created the project, shows the two
//!   workflows it was created with ([`wiz::created_workflows`]: the shipped
//!   Task and Subtask definitions and statuses) and anything the engine
//!   warned about, and waits for Okay. Okay re-reads the project catalog off
//!   the UI thread ([`Engine::refresh`]), then opens the project on its
//!   Orchestrator ([`crate::app::open_added_project`]) and closes. While
//!   that runs Okay is disabled, with a spinner beside it; if the project
//!   can't be opened, 04 stays with the reason and Okay opens it again (the
//!   project is never created twice). Closing 04 leaves the project created,
//!   in the switcher.
//!
//! Keys: Tab / ⇧Tab move focus (the focus ring shows once the keyboard is
//! used), Return is the enabled primary action, Space activates the focused
//! button or card, the arrows move between the cards, Esc and ⌘W cancel
//! (Esc waits while the engine runs, and while Okay opens the project).
//! Cancelling before Create Project writes nothing.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, Global, Hsla,
    PathPromptOptions, SharedString, Stateful, Subscription, Window, WindowHandle,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use shelbi_app::add_project_wizard::{
    self as wiz, Action, Attempt, CreateRequest, FolderCheck, Focus, Origin, RowState, Screen, Stage,
    Tone, Wizard, WorkflowOverview,
};
use shelbi_app::exec::Mutation;
use shelbi_core::ConfigMode;
use shelbi_orchestrator::project_create::{self as engine, AddProjectError, ScaffoldReporter, ScaffoldStage};
use shelbi_state::keymap::{Key, KeyChord, Mods};

use crate::app::{CloseWindow, ShelbiWindow};
use crate::commands;
use crate::assets::{heroicon, icons};
use crate::elements::{controls, spinner};
use crate::keys::chord_from_keystroke;
use crate::projects::Catalog;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::{self, Theme};

/// The window's size (the frames').
pub const SIZE: (f32, f32) = (800.0, 580.0);
/// The intro line's width.
const INTRO_W: f32 = 600.0;

/// [`Engine::check`]'s signature: the launch directory, the path field's
/// text, the typed name.
pub type CheckFn = dyn Fn(&Path, &str, Option<&str>) -> FolderCheck + Send + Sync;
/// [`Engine::create`]'s signature: the request, the launch directory, and
/// where to report the stages and warnings.
pub type CreateFn =
    dyn Fn(&CreateRequest, &Path, &mut dyn ScaffoldReporter) -> Result<String, AddProjectError> + Send + Sync;

/// [`Engine::refresh`]'s signature.
pub type RefreshFn = dyn Fn() + Send + Sync;

/// The blocking work the wizard runs off the UI thread. A test swaps in its
/// own, so nothing reads or writes a real Shelbi home.
#[derive(Clone)]
pub struct Engine {
    /// What the path field's folder is (`project_create::check_folder`).
    pub check: Arc<CheckFn>,
    /// Create the project, reporting as it goes; its id on success.
    pub create: Arc<CreateFn>,
    /// Re-read the project catalog, so the created project is in it before
    /// it opens (Okay).
    pub refresh: Arc<RefreshFn>,
}

impl Engine {
    /// The shared engine `shelbi init` and the TUI's form use, refreshing
    /// `catalog` (the app's).
    pub fn real(catalog: Option<Catalog>) -> Self {
        Engine {
            refresh: Arc::new(move || {
                if let Some(catalog) = &catalog {
                    catalog.reload();
                }
            }),
            check: Arc::new(shelbi_orchestrator::project_create::check_folder),
            create: Arc::new(|request, cwd, out| {
                let mutation = Mutation::AddProject {
                    name: request.name.clone(),
                    root: request.root.clone(),
                    mode: request.mode,
                };
                let attempt = match request.attempt {
                    Attempt::New => engine::Attempt::New,
                    Attempt::Retry => engine::Attempt::Retry,
                };
                commands::run_add_project(&mutation, attempt, cwd, out)
            }),
        }
    }
}

/// What the engine reported, on its way to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Started(Stage),
    Finished(Stage),
    Failed(Stage, String),
    Warning(String),
}

/// The engine's reporter: stages and warnings go to the wizard over
/// `sender`; the progress lines (the CLI prints them) go to the log. A
/// closed window has dropped the receiver, and the reports go nowhere.
struct Forward(async_channel::Sender<Report>);

impl ScaffoldReporter for Forward {
    fn note(&mut self, line: &str) {
        tracing::debug!(target: "shelbi::add_project", "{line}");
    }
    fn warn(&mut self, line: &str) {
        tracing::warn!(target: "shelbi::add_project", "{line}");
        let _ = self.0.try_send(Report::Warning(line.to_string()));
    }
    fn stage_started(&mut self, stage: ScaffoldStage) {
        let _ = self.0.try_send(Report::Started(stage.into()));
    }
    fn stage_finished(&mut self, stage: ScaffoldStage) {
        let _ = self.0.try_send(Report::Finished(stage.into()));
    }
    fn stage_failed(&mut self, stage: ScaffoldStage, error: &str) {
        tracing::warn!(target: "shelbi::add_project", ?stage, "{error}");
        let _ = self.0.try_send(Report::Failed(stage.into(), error.to_string()));
    }
}

/// What to do with a created project once the catalog has it: open it on
/// its Orchestrator (the window the wizard came from switches to it, else a
/// new window opens on it), or say why it couldn't.
pub type OnCreated = Rc<dyn Fn(&str, Option<WindowHandle<ShelbiWindow>>, &mut App) -> Result<(), String>>;

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
                Engine::real(cx.try_global::<Catalog>().cloned()),
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
    /// The latest creation run; a report or result from an earlier one is
    /// dropped.
    run: u64,
    /// Screen 04's cards.
    workflows: Vec<WorkflowOverview>,
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
            run: 0,
            workflows: wiz::created_workflows(),
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
            Action::Open(created) => self.open(created.slug, window, cx),
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

    /// Create Project or Try Again: the engine, off the UI thread, its
    /// reports crossing back over a channel as they happen, then its result.
    /// Everything lands through a weak handle, so a closed window takes
    /// nothing more; `run` drops anything from an earlier run.
    fn create(&mut self, request: CreateRequest, window: &mut Window, cx: &mut Context<Self>) {
        self.run += 1;
        let run = self.run;
        let (sender, reports) = async_channel::unbounded();
        let (create, cwd) = (self.engine.create.clone(), self.cwd.clone());
        let work = cx
            .background_executor()
            .spawn(async move { create(&request, &cwd, &mut Forward(sender)) });
        cx.spawn_in(window, async move |this, cx| {
            // The sender goes when the engine returns, ending this loop.
            while let Ok(report) = reports.recv().await {
                if this.update_in(cx, |this, window, cx| this.report(run, report, window, cx)).is_err() {
                    return;
                }
            }
            let result = work.await;
            this.update_in(cx, |this, window, cx| this.finished(run, result, window, cx))
                .ok();
        })
        .detach();
    }

    /// Okay: re-read the catalog off the UI thread, then open the project
    /// (at the app's top level, not nested in this window's update) and
    /// close the wizard, its window after the project's so the first
    /// launch's app never runs out of windows. A failure stays on 04 for
    /// Okay to try again. A closed wizard still opens the project it was
    /// asked to (the user pressed Okay).
    fn open(&mut self, slug: String, window: &mut Window, cx: &mut Context<Self>) {
        let refresh = self.engine.refresh.clone();
        let reread = cx.background_executor().spawn(async move { refresh() });
        let (from, on_created) = (self.from, self.on_created.clone());
        let Some(me) = window.window_handle().downcast::<AddProjectView>() else {
            return;
        };
        cx.spawn(async move |_, cx| {
            reread.await;
            let opened = cx.update(|cx| on_created(&slug, from, cx));
            match opened {
                Ok(()) => {
                    me.update(cx, |_, window, _| window.remove_window()).ok();
                }
                Err(why) => {
                    tracing::warn!(target: "shelbi::add_project", project = %slug, "could not open the project: {why}");
                    me.update(cx, |this, window, cx| {
                        this.wizard.open_failed(why);
                        this.after_change(window, cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Screen 03 as the engine's `reports` leave it, and its `result` if
    /// it has ended, without running the engine (the visual harness, tests):
    /// the chosen folder's Continue and Create Project, the request dropped.
    #[cfg(any(test, feature = "visual-check"))]
    pub(crate) fn preview_creating(
        &mut self,
        reports: Vec<Report>,
        result: Option<Result<String, AddProjectError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.wizard.primary();
        let _ = self.wizard.primary();
        for report in reports {
            self.report(self.run, report, window, cx);
        }
        if let Some(result) = result {
            self.finished(self.run, result, window, cx);
        }
        self.after_change(window, cx);
    }

    /// Screen 04 with Okay pressed, without opening anything (the visual
    /// harness): still opening, or failed with `failure`.
    #[cfg(feature = "visual-check")]
    pub(crate) fn preview_open(&mut self, failure: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.wizard.primary();
        if let Some(why) = failure {
            self.wizard.open_failed(why);
        }
        self.after_change(window, cx);
    }

    /// One report from run `run`.
    fn report(&mut self, run: u64, report: Report, window: &mut Window, cx: &mut Context<Self>) {
        if run != self.run {
            return;
        }
        match report {
            Report::Started(stage) => self.wizard.stage_started(stage),
            Report::Finished(stage) => self.wizard.stage_finished(stage),
            Report::Failed(stage, message) => self.wizard.stage_failed(stage, message),
            Report::Warning(line) => self.wizard.warning(&line),
        }
        self.after_change(window, cx);
    }

    /// Run `run` ended: on to the created screen, or the failure on 03.
    fn finished(
        &mut self,
        run: u64,
        result: Result<String, AddProjectError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if run != self.run {
            return;
        }
        match result {
            Ok(slug) => self.wizard.creation_succeeded(slug),
            Err(AddProjectError::Invalid(msg) | AddProjectError::Failed(msg)) => {
                self.wizard.creation_failed(msg)
            }
        }
        self.after_change(window, cx);
    }

    /// Whether `focus` shows the keyboard's ring.
    fn ring(&self, focus: Focus, window: &Window) -> bool {
        self.keyboard && self.wizard.focus() == focus && self.focus.is_focused(window)
    }

    /// Back and Continue / Create Project, under the step count. Screen 03
    /// keeps the row's height (Figma's spacer) with nothing in it while the
    /// engine runs, and Try Again after a failure; 04 has its note and Okay.
    fn footer(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let screen = self.wizard.screen();
        if screen == Screen::Created {
            return self.created_footer(window, theme, cx);
        }
        if screen == Screen::Creating {
            let label = wiz::RETRY_LABEL;
            let button = self.wizard.primary_enabled().then(|| {
                style::button("add-project-primary", label, ButtonKind::Primary, ButtonSize::Base, self.ring(Focus::Primary, window), theme)
                    .on_click(cx.listener(|this, _, window, cx| this.click(Focus::Primary, window, cx)))
            });
            return div()
                .flex()
                .flex_row()
                .flex_none()
                .justify_end()
                .h(px(32.0))
                .w_full()
                .children(button);
        }
        let back = if self.wizard.back_enabled() {
            style::button("add-project-back", "Back", ButtonKind::Secondary, ButtonSize::Base, self.ring(Focus::Back, window), theme)
                .on_click(cx.listener(|this, _, window, cx| this.click(Focus::Back, window, cx)))
        } else {
            style::disabled_button("add-project-back", "Back", ButtonKind::Secondary, ButtonSize::Base, theme)
        };
        let label = match screen {
            Screen::Folder => "Continue",
            _ => "Create Project",
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

    /// Screen 04's footer: the note, and Okay (disabled, a spinner beside
    /// it, while the project opens).
    fn created_footer(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let opening = self.wizard.opening();
        let okay = if self.wizard.primary_enabled() {
            style::button("add-project-primary", wiz::OKAY_LABEL, ButtonKind::Primary, ButtonSize::Base, self.ring(Focus::Primary, window), theme)
                .on_click(cx.listener(|this, _, window, cx| this.click(Focus::Primary, window, cx)))
        } else {
            style::disabled_button("add-project-primary", wiz::OKAY_LABEL, ButtonKind::Primary, ButtonSize::Base, theme)
        };
        let pending = opening.then(|| {
            div()
                .debug_selector(|| "add-project-opening".into())
                .flex_none()
                .child(spinner::spinner_sized("add-project-opening", theme.progress, 16.0))
        });
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(32.0))
            .w_full()
            .child(
                div()
                    .debug_selector(|| "add-project-workflows-note".into())
                    .type_style(Type::SmallMedium)
                    .text_color(theme.muted)
                    .child(wiz::WORKFLOWS_NOTE),
            )
            .child(div().flex_1())
            .children(pending)
            .child(okay)
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

/// One of screen 04's cards: the workflow's name (and Default badge), what
/// it's for, and the statuses a task moves through, each with its status
/// icon.
fn workflow_card(workflow: &WorkflowOverview, theme: &Theme) -> Div {
    let selector = format!("add-project-workflow-{}", workflow.name);
    let badge = workflow.default.then(|| {
        div()
            .debug_selector(|| "add-project-default-badge".into())
            .flex_none()
            .px(px(4.0))
            .rounded(px(2.0))
            .bg(theme.workflow_badge)
            .type_style(Type::XSmall)
            .text_color(theme.workflow_badge_text)
            .child("Default")
    });
    let mut sequence = div().flex().flex_row().flex_wrap().items_center().gap(px(8.0)).w_full();
    for (i, status) in workflow.statuses.iter().enumerate() {
        if i > 0 {
            sequence = sequence.child(
                heroicon(icons::ARROW_RIGHT)
                    .size(px(12.0))
                    .text_color(theme.muted),
            );
        }
        let id = format!("{selector}-status-{}", status.id);
        sequence = sequence.child(
            div()
                .debug_selector(move || id)
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .child(style::status_icon(status.category, theme))
                .child(
                    div()
                        .type_style(Type::Base)
                        .text_color(theme.foreground)
                        .child(status.label.clone()),
                ),
        );
    }
    div()
        .debug_selector(move || selector)
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w_full()
        .p(px(20.0))
        .rounded(px(12.0))
        // `surface/depth-10`, as screen 01's box.
        .bg(theme.toolbar)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .type_style(Type::BaseSemibold)
                        .text_color(theme.text_loud)
                        .child(workflow.title.clone()),
                )
                .children(badge),
        )
        .child(
            div()
                .type_style(Type::Base)
                .text_color(theme.foreground)
                .child(workflow.summary.clone()),
        )
        .child(div().pt(px(4.0)).child(sequence))
}

/// Screen 03's checklist: a row per stage, the failed one with its reason
/// under it.
fn checklist(wizard: &Wizard, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "add-project-checklist".into())
        .flex()
        .flex_col()
        .gap(px(24.0))
        .w_full()
        .children(Stage::ALL.into_iter().map(|stage| {
            let state = wizard.row(stage);
            let reason = wizard.failure().filter(|(s, _)| *s == stage).map(|(_, why)| {
                div()
                    .debug_selector(|| "add-project-failure".into())
                    // Under the label: past the 32pt status and the 12pt gap.
                    .pl(px(44.0))
                    .type_style(Type::Base)
                    .text_color(theme.error)
                    .child(why.to_string())
            });
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .w_full()
                .child(checklist_row(stage, state, theme))
                .children(reason)
        }))
}

/// One checklist row: the Step Status, then the label (muted while pending,
/// loud and medium while running or failed, the body color once done).
fn checklist_row(stage: Stage, state: RowState, theme: &Theme) -> Div {
    let (style, color) = match state {
        RowState::Pending => (Type::Base, theme.muted),
        RowState::Active | RowState::Failed => (Type::BaseMedium, theme.text_loud),
        RowState::Done => (Type::Base, theme.foreground),
    };
    let id = row_selector(stage);
    div()
        .debug_selector(move || format!("{id}-{}", row_state_name(state)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .h(px(32.0))
        .w_full()
        .child(step_status(stage, state, theme))
        .child(div().type_style(style).text_color(color).child(stage.label()))
}

/// The Figma Step Status: Incomplete (an empty circle in the overlay fill),
/// In Progress (the spinner), Complete (a green check on the Success fill).
/// Failed isn't in Figma: an x on the Danger fill, the same way.
fn step_status(stage: Stage, state: RowState, theme: &Theme) -> AnyElement {
    let circle = || {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(spinner::SIZE))
            .rounded_full()
    };
    match state {
        RowState::Pending => circle().bg(theme.background).into_any_element(),
        RowState::Active => {
            spinner::spinner(SharedString::from(row_selector(stage)), theme.progress).into_any_element()
        }
        RowState::Done => circle()
            .bg(theme.success_fill)
            .child(heroicon(icons::CHECK).text_color(theme.icon_green))
            .into_any_element(),
        RowState::Failed => circle()
            .bg(theme.danger_fill)
            .child(heroicon(icons::X_MARK).text_color(theme.error))
            .into_any_element(),
    }
}

/// A stage's row, for tests and the visual harness.
fn row_selector(stage: Stage) -> &'static str {
    match stage {
        Stage::Config => "add-project-row-config",
        Stage::Agents => "add-project-row-agents",
        Stage::Workflows => "add-project-row-workflows",
    }
}

fn row_state_name(state: RowState) -> &'static str {
    match state {
        RowState::Pending => "pending",
        RowState::Active => "active",
        RowState::Done => "done",
        RowState::Failed => "failed",
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
        // Screen 03 has its title alone.
        let intro = match screen {
            Screen::Folder => Some(wiz::FOLDER_INTRO.to_string()),
            Screen::Settings => Some(wiz::SETTINGS_INTRO.to_string()),
            Screen::Creating => None,
            Screen::Created => Some(wiz::CREATED_INTRO.to_string()),
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
                .child(self.card(ConfigMode::Global, window, &theme, cx)),
            Screen::Creating => details.child(checklist(&self.wizard, &theme)),
            // The workflows, then what the engine warned about (the commit
            // guard it installed, a hook it left alone: its disclosure) and
            // why an open failed.
            Screen::Created => details
                .children(self.workflows.iter().map(|w| workflow_card(w, &theme)))
                .children(self.wizard.created().filter(|c| !c.warnings.is_empty()).map(|c| {
                    div()
                        .debug_selector(|| "add-project-warnings".into())
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .children(c.warnings.iter().map(|line| {
                            div()
                                .type_style(Type::Small)
                                .text_color(theme.muted)
                                .child(line.clone())
                        }))
                }))
                .children(self.wizard.open_error().map(|why| {
                    div()
                        .debug_selector(|| "add-project-open-error".into())
                        .type_style(Type::Base)
                        .text_color(theme.error)
                        .child(why.to_string())
                })),
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
                    .children(intro.map(|intro| {
                        div()
                            .w(px(INTRO_W))
                            .type_style(Type::Base)
                            .text_color(theme.foreground)
                            .child(intro)
                    })),
            )
            .child(details)
            .child(self.footer(window, &theme, cx))
            .into_any_element();
        root
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        /// Each open the wizard asked for.
        opened: Rc<RefCell<Vec<String>>>,
        /// How many of the next opens fail.
        open_fails: Rc<Cell<usize>>,
        /// Catalog re-reads, and the opens asked for when each ran.
        refreshed: Arc<AtomicUsize>,
        windows: usize,
    }

    /// A creation engine that reports the stages up to `fail_at` (all of
    /// them when `None`) the way the real one does, warns once, then fails
    /// there or returns `slug`. A retry always gets through.
    fn scripted(fail_at: Option<Stage>, slug: &'static str) -> Arc<CreateFn> {
        Arc::new(move |request, _, out| {
            out.warn("shelbi: installed the commit guard");
            for stage in ScaffoldStage::ALL {
                out.stage_started(stage);
                if fail_at == Some(stage.into()) && request.attempt == Attempt::New {
                    out.stage_failed(stage, "agents is a file");
                    return Err(AddProjectError::Failed("Add project failed: agents is a file".into()));
                }
                out.stage_finished(stage);
            }
            Ok(slug.to_string())
        })
    }

    fn rig(cx: &mut TestAppContext, origin: Origin, result: Result<String, AddProjectError>) -> Rig {
        let create: Arc<CreateFn> = Arc::new(move |_, _, _| result.clone());
        rig_with(cx, origin, create)
    }

    fn rig_with(cx: &mut TestAppContext, origin: Origin, create: Arc<CreateFn>) -> Rig {
        super::super::about::tests::init(cx);
        let created = Arc::new(Mutex::new(Vec::new()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let open_fails = Rc::new(Cell::new(0));
        let refreshed = Arc::new(AtomicUsize::new(0));
        let engine = Engine {
            check: Arc::new(fake_check),
            refresh: Arc::new({
                let refreshed = refreshed.clone();
                move || {
                    refreshed.fetch_add(1, Ordering::SeqCst);
                }
            }),
            create: Arc::new({
                let created = created.clone();
                move |request, cwd, out| {
                    created.lock().unwrap().push(request.clone());
                    create(request, cwd, out)
                }
            }),
        };
        let on_created: OnCreated = Rc::new({
            let (opened, open_fails) = (opened.clone(), open_fails.clone());
            move |slug, _, _| {
                opened.borrow_mut().push(slug.to_string());
                match open_fails.get() {
                    0 => Ok(()),
                    n => {
                        open_fails.set(n - 1);
                        Err("Shelbi couldn’t open a window.".into())
                    }
                }
            }
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
            open_fails,
            refreshed,
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
            [CreateRequest {
                name: "repo".into(),
                root: "/repo".into(),
                mode: ConfigMode::InRepo,
                attempt: Attempt::New,
            }]
        );
        // Created, it waits for the user before opening the project.
        assert_eq!(r.wizard(|w| w.screen()), Screen::Created);
        assert!(r.opened.borrow().is_empty(), "nothing opens before Open Project");
        assert!(r.window_open());
        r.click("add-project-primary");
        assert_eq!(*r.opened.borrow(), ["repo"]);
        assert_eq!(r.refreshed.load(Ordering::SeqCst), 1, "the catalog re-read before it opened");
        assert!(!r.window_open(), "Okay closes the wizard");
        assert_eq!(r.created.lock().unwrap().len(), 1);
    }

    #[gpui::test]
    fn screen_four_shows_the_created_workflows_and_its_okay(cx: &mut TestAppContext) {
        let mut r = rig_with(cx, Origin::Workspace, scripted(None, "repo"));
        r.pick(Some("/repo"));
        r.keys("enter enter");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Created);
        for drawn in [
            "add-project-workflow-task",
            "add-project-workflow-subtask",
            "add-project-default-badge",
            "add-project-workflows-note",
            "add-project-primary",
            "add-project-warnings",
        ] {
            assert!(r.drawn(drawn), "{drawn}");
        }
        for status in [
            "add-project-workflow-task-status-backlog",
            "add-project-workflow-task-status-todo",
            "add-project-workflow-task-status-in-progress",
            "add-project-workflow-task-status-review",
            "add-project-workflow-task-status-done",
            "add-project-workflow-subtask-status-backlog",
            "add-project-workflow-subtask-status-todo",
            "add-project-workflow-subtask-status-in-progress",
            "add-project-workflow-subtask-status-done",
        ] {
            assert!(r.drawn(status), "{status}");
        }
        assert!(!r.drawn("add-project-workflow-subtask-status-review"), "Subtask has no review");
        assert!(!r.drawn("add-project-workflow-task-status-canceled"));
        assert!(!r.drawn("add-project-step"), "no step count on 04");
        assert!(!r.drawn("add-project-back"));
        assert!(!r.drawn("add-project-checklist"));
        assert_eq!(r.wizard(|w| w.focus()), Focus::Primary, "Okay has focus, and Return");
    }

    #[gpui::test]
    fn a_failed_creation_never_reaches_screen_four(cx: &mut TestAppContext) {
        let mut r = rig_with(cx, Origin::Workspace, scripted(Some(Stage::Workflows), "repo"));
        r.pick(Some("/repo"));
        r.keys("enter enter");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Creating);
        assert!(!r.drawn("add-project-workflow-task"));
        assert!(r.drawn("add-project-row-workflows-failed"));
        r.keys("enter");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Created, "the retry got through");
        assert!(r.opened.borrow().is_empty(), "nothing opens before Okay");
    }

    #[gpui::test]
    fn a_failed_open_stays_on_four_and_okay_opens_without_creating_again(cx: &mut TestAppContext) {
        let mut r = rig_with(cx, Origin::FirstLaunch, scripted(None, "repo"));
        r.open_fails.set(1);
        r.pick(Some("/repo"));
        r.keys("enter enter");
        r.keys("enter");
        assert_eq!(*r.opened.borrow(), ["repo"]);
        assert!(r.window_open(), "the wizard stays for another try");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Created);
        assert!(r.drawn("add-project-open-error"));
        assert!(!r.wizard(|w| w.opening()));
        r.click("add-project-primary");
        assert_eq!(*r.opened.borrow(), ["repo", "repo"]);
        assert!(!r.window_open());
        assert_eq!(r.created.lock().unwrap().len(), 1, "the project was created once");
        assert_eq!(r.refreshed.load(Ordering::SeqCst), 2);
    }

    /// The whole flow over the real engine, folder check and catalog, in a
    /// throwaway `SHELBI_HOME` and Git repo, in each storage mode: folder,
    /// settings, the checklist, 04, then Okay re-reads the catalog (which
    /// now lists the project) and asks for it to open. The workflows 04
    /// showed are the ones on disk, and Task is the project's default.
    #[gpui::test]
    fn the_real_engine_creates_what_screen_four_shows_in_both_modes(cx: &mut TestAppContext) {
        let _lock = crate::test_env::lock();
        let base = std::env::temp_dir().join(format!("shb-add-04-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let prev = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", base.join("home"));
        super::super::about::tests::init(cx);
        for (mode, folder) in [(ConfigMode::Global, "outside"), (ConfigMode::InRepo, "inside")] {
            let repo = base.join(folder);
            std::fs::create_dir_all(&repo).unwrap();
            let git = std::process::Command::new("git").arg("init").arg("-q").arg(&repo).status().unwrap();
            assert!(git.success());
            let catalog = Catalog::default();
            let opened = Rc::new(RefCell::new(Vec::new()));
            let on_created: OnCreated = Rc::new({
                let (opened, catalog) = (opened.clone(), catalog.clone());
                move |slug, _, _| {
                    assert!(catalog.lists(slug), "the catalog re-read before the open");
                    opened.borrow_mut().push(slug.to_string());
                    Ok(())
                }
            });
            let window = cx.add_window(|window, cx| {
                AddProjectView::new(Origin::FirstLaunch, None, base.clone(), Engine::real(Some(catalog.clone())), on_created, window, cx)
            });
            let mut vcx = VisualTestContext::from_window(window.into(), cx);
            let view = window.update(&mut vcx, |_, _, cx| cx.entity()).unwrap();
            vcx.update(|window, cx| view.update(cx, |v, cx| v.folder_picked(Some(&repo), window, cx)));
            vcx.run_until_parked();
            let step = |vcx: &mut VisualTestContext, focus: Focus| {
                vcx.update(|window, cx| view.update(cx, |v, cx| v.click(focus, window, cx)));
                vcx.run_until_parked();
            };
            step(&mut vcx, Focus::Primary);
            step(&mut vcx, Focus::Card(mode));
            step(&mut vcx, Focus::Primary);
            let (screen, slug, shown) = view.read_with(&vcx, |v, _| {
                (v.wizard().screen(), v.wizard().created().map(|c| c.slug.clone()), v.workflows.clone())
            });
            assert_eq!(screen, Screen::Created, "{mode:?}: {:?}", view.read_with(&vcx, |v, _| v.wizard().failure().map(|(s, m)| (s, m.to_string()))));
            let slug = slug.unwrap();
            assert!(opened.borrow().is_empty(), "nothing opens before Okay");
            step(&mut vcx, Focus::Primary);
            assert_eq!(*opened.borrow(), [slug.clone()]);

            let project = shelbi_state::load_project(&slug).unwrap();
            assert_eq!(project.default_workflow.as_deref(), Some(shelbi_core::TASK_WORKFLOW_NAME));
            for card in &shown {
                let on_disk = shelbi_state::load_workflow(&slug, &card.name).unwrap();
                let ids: Vec<&str> = on_disk
                    .statuses
                    .iter()
                    .filter(|s| s.category != shelbi_core::StatusCategory::Archived)
                    .map(|s| s.id.as_str())
                    .collect();
                let cards: Vec<&str> = card.statuses.iter().map(|s| s.id.as_str()).collect();
                assert_eq!(cards, ids, "{mode:?} {}", card.name);
                for status in &card.statuses {
                    let disk = on_disk.status(&status.id).unwrap();
                    assert_eq!(status.category, disk.category);
                    assert_eq!(status.label, wiz::status_label(&disk.name));
                }
            }
            let in_repo = repo.join(".shelbi").exists();
            assert_eq!(in_repo, mode == ConfigMode::InRepo, "{mode:?} kept its settings where chosen");
        }
        match prev {
            Some(v) => std::env::set_var("SHELBI_HOME", v),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[gpui::test]
    fn okay_pressed_again_while_it_opens_does_nothing(cx: &mut TestAppContext) {
        let mut r = rig_with(cx, Origin::Workspace, scripted(None, "repo"));
        r.pick(Some("/repo"));
        r.keys("enter enter");
        // Three presses before anything runs: one open.
        let view = r.view.clone();
        r.cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.click(Focus::Primary, window, cx);
                assert!(v.wizard().opening());
                v.click(Focus::Primary, window, cx);
                v.key(KeyChord { code: Key::Enter, mods: Mods::NONE }, window, cx);
                v.key(KeyChord { code: Key::Esc, mods: Mods::NONE }, window, cx);
            })
        });
        assert!(r.drawn("add-project-opening"), "the spinner shows while it opens");
        r.cx.run_until_parked();
        assert_eq!(*r.opened.borrow(), ["repo"]);
        assert_eq!(r.refreshed.load(Ordering::SeqCst), 1);
        assert!(!r.window_open());
    }

    #[gpui::test]
    fn the_engines_stages_check_the_rows_and_its_warnings_carry_over(cx: &mut TestAppContext) {
        let mut r = rig_with(cx, Origin::Workspace, scripted(None, "repo"));
        r.pick(Some("/repo"));
        r.keys("enter enter");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Created);
        // 04 draws the workflows in the checklist's place; every row was
        // checked on the way.
        for stage in Stage::ALL {
            assert_eq!(r.wizard(|w| w.row(stage)), RowState::Done, "{stage:?}");
        }
        assert_eq!(
            r.wizard(|w| w.created().map(|c| c.warnings.clone())),
            Some(vec!["Installed the commit guard".to_string()])
        );
        assert!(r.drawn("add-project-warnings"));
    }

    #[gpui::test]
    fn a_failed_stage_keeps_its_rows_and_try_again_finishes_the_project(cx: &mut TestAppContext) {
        let mut r = rig_with(cx, Origin::Workspace, scripted(Some(Stage::Agents), "repo"));
        r.pick(Some("/repo"));
        r.keys("enter enter");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Creating, "it stays on the checklist");
        assert!(r.drawn("add-project-row-config-done"));
        assert!(r.drawn("add-project-row-agents-failed"));
        assert!(r.drawn("add-project-row-workflows-pending"));
        assert!(r.drawn("add-project-failure"));
        assert_eq!(r.wizard(|w| w.failure().map(|(s, m)| (s, m.to_string()))), Some((Stage::Agents, "agents is a file".into())));
        assert!(r.opened.borrow().is_empty());

        r.click("add-project-primary");
        let attempts: Vec<Attempt> = r.created.lock().unwrap().iter().map(|c| c.attempt).collect();
        assert_eq!(attempts, [Attempt::New, Attempt::Retry], "Try Again retries the same request");
        assert_eq!(r.created.lock().unwrap()[1].root, "/repo");
        assert_eq!(r.wizard(|w| w.screen()), Screen::Created);
        r.keys("enter");
        assert_eq!(*r.opened.borrow(), ["repo"]);
    }

    #[gpui::test]
    fn each_row_state_draws_from_the_engines_reports(cx: &mut TestAppContext) {
        let mut r = rig(cx, Origin::Workspace, Ok("repo".into()));
        r.pick(Some("/repo"));
        // On to 03 without running an engine, then feed the view reports the
        // way the channel does.
        let view = r.view.clone();
        r.cx.update(|window, cx| view.update(cx, |v, cx| v.preview_creating(Vec::new(), None, window, cx)));
        assert_eq!(r.wizard(|w| (w.screen(), w.running())), (Screen::Creating, true));
        let report = |r: &mut Rig, report: Report| {
            let view = r.view.clone();
            r.cx.update(|window, cx| view.update(cx, |v, cx| v.report(v.run, report, window, cx)));
            r.cx.run_until_parked();
        };
        assert!(r.drawn("add-project-row-config-pending"));
        assert!(!r.drawn("add-project-primary"), "no footer button while it runs");
        assert!(!r.drawn("add-project-step"));
        report(&mut r, Report::Started(Stage::Config));
        assert!(r.drawn("add-project-row-config-active"));
        assert!(r.drawn("add-project-row-agents-pending"));
        report(&mut r, Report::Finished(Stage::Config));
        report(&mut r, Report::Started(Stage::Agents));
        assert!(r.drawn("add-project-row-config-done"));
        assert!(r.drawn("add-project-row-agents-active"));
        assert!(r.drawn("add-project-row-workflows-pending"));
        // A report from an older run is dropped.
        let view = r.view.clone();
        r.cx.update(|window, cx| {
            view.update(cx, |v, cx| v.report(v.run + 1, Report::Finished(Stage::Agents), window, cx))
        });
        assert!(r.drawn("add-project-row-agents-active"));
    }

    #[gpui::test]
    fn closing_mid_creation_drops_its_reports_and_opens_nothing(cx: &mut TestAppContext) {
        let mut r = rig_with(cx, Origin::Workspace, scripted(None, "repo"));
        r.pick(Some("/repo"));
        r.keys("enter");
        // Create Project, and the window closes before the engine reports.
        let view = r.view.clone();
        r.cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.click(Focus::Primary, window, cx);
                window.remove_window();
            })
        });
        r.cx.run_until_parked();
        assert!(!r.window_open());
        assert!(r.opened.borrow().is_empty(), "an abandoned wizard opens nothing");
        assert!(r.created.lock().unwrap().len() <= 1, "no second run");
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
