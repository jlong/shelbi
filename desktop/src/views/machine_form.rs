//! The Add machine and Configure machine dialogs (`dt-machine-settings-forms`,
//! Figma "Settings · Workspaces · Add machine" 2526:9557 and "Configure
//! machine" 2528:9870), opened from Settings ▸ Workspaces' Add machine and a
//! machine's Configure. [`machine_form_state`](super::machine_form_state)
//! holds the draft, what each key does and what the connection check shows;
//! this view paints it and does the toolkit's part.
//!
//! The 600pt dialog, over the window's scrim (the window draws it,
//! [`render_layer`], centered over the main area): the title and what the
//! form is about; Name and SSH host side by side (this Mac's form has Name
//! alone); the SSH help; Project directory (this Mac's with Choose…, the
//! native folder chooser); Machine tags; for an SSH machine the connection
//! check and its footnote; a rule; Cancel and Add machine / Save changes.
//!
//! The blocking work is the [`Engine`]'s, off the UI thread: a save runs the
//! shared machine service (`pool_admin::add_machine` / `update_machine`), a
//! readiness request runs the readiness service. Their answers land through
//! a weak handle, so a form cancelled (or a project switched away from) in
//! the meantime takes nothing; a save already running still finishes for the
//! project it was started for. A test swaps in its own engine, so nothing
//! touches a real Shelbi home or the hub.
//!
//! Until `dt-machine-readiness-setup` replaces it, [`Engine::real`]'s
//! readiness service only asks whether SSH reaches the host (the page's own
//! reachability probe), and says so: SSH reached, the rest not checked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Global, Pixels, Stateful, Subscription, Window,
};
use gpui_kit::component::input::{InputEvent, InputState};
use shelbi_core::Reachability;
use shelbi_orchestrator::pool_admin::{self, MachineChange, PoolError};

use super::machine_form_state::{
    Action, Check, CheckOutcome, Field, Focus, MachineForm, ReadinessIntent, ReadinessRequest, SaveRequest,
};
use super::workflow_editor::{footnote, text_field};
use crate::assets::{heroicon, icons};
use crate::elements::spinner;
use crate::keys::chord_from_keystroke;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The dialog's width (the frames').
pub const DIALOG_W: f32 = 600.0;
/// Its inset and the gap between its blocks.
const DIALOG_PAD: f32 = 32.0;
const DIALOG_GAP: f32 = 24.0;
/// Its corners (`radius/default`).
const DIALOG_RADIUS: f32 = 12.0;
/// The least room kept above and below it in a short window.
const DIALOG_MARGIN: f32 = 24.0;
/// The connection check's status icon.
const CHECK_ICON: f32 = 16.0;

/// [`Engine::save`]'s signature: the project, the save.
pub type SaveFn = dyn Fn(&str, &SaveRequest) -> Result<MachineChange, PoolError> + Send + Sync;
/// [`Engine::readiness`]'s signature.
pub type ReadinessFn = dyn Fn(&ReadinessRequest) -> CheckOutcome + Send + Sync;

/// The blocking work the form runs off the UI thread. A gpui global, so a
/// test can set its own; otherwise [`Engine::real`].
#[derive(Clone)]
pub struct Engine {
    /// Validate and persist the draft through the shared machine service.
    pub save: Arc<SaveFn>,
    /// The readiness service: run a Test connection / Test again / Install
    /// CLI request for the draft. Never writes the project's config.
    pub readiness: Arc<ReadinessFn>,
}

impl Global for Engine {}

impl Engine {
    /// The shared machine service, and SSH reachability as the readiness
    /// check until the readiness service lands.
    #[cfg_attr(test, allow(dead_code))]
    pub fn real() -> Self {
        Engine {
            save: Arc::new(|project, request| match request {
                SaveRequest::Add(fields) => pool_admin::add_machine(project, fields),
                SaveRequest::Update(update) => pool_admin::update_machine(project, update),
            }),
            readiness: Arc::new(reachability_only),
        }
    }

    /// The engine a form opened now uses: the global, else the real one (a
    /// test's default never reaches the hub or a real home).
    pub fn current(cx: &App) -> Self {
        if let Some(engine) = cx.try_global::<Engine>() {
            return engine.clone();
        }
        #[cfg(test)]
        return tests::quiet_engine();
        #[cfg(not(test))]
        Engine::real()
    }
}

/// The interim readiness check: whether SSH reaches the host. It can't tell
/// whether the CLI is missing, so it never asks for Install CLI; asked
/// anyway, it says what to do by hand.
fn reachability_only(request: &ReadinessRequest) -> CheckOutcome {
    match request.intent {
        ReadinessIntent::Test | ReadinessIntent::TestAgain => match super::machines::probe(&request.target.host) {
            Reachability::Reachable => CheckOutcome::Reachable,
            Reachability::Unreachable { error } => CheckOutcome::Failed { message: error },
            Reachability::Unknown => CheckOutcome::Failed {
                message: "Shelbi couldn’t tell whether SSH reached it.".into(),
            },
        },
        ReadinessIntent::InstallCli => CheckOutcome::Failed {
            message: format!(
                "Shelbi can’t install its CLI on {} from here yet. Install it there, then Test again.",
                request.target.host
            ),
        },
    }
}

/// What the form tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineFormEvent {
    /// Cancel or Esc: the draft is gone, nothing was written.
    Cancelled,
    /// The machine was saved.
    Saved(MachineChange),
}

/// An Add machine or Configure machine dialog.
pub struct MachineFormView {
    state: MachineForm,
    name: Entity<InputState>,
    host: Entity<InputState>,
    work_dir: Entity<InputState>,
    tags: Entity<InputState>,
    /// The dialog's own focus, holding the keys while a button has them.
    focus: FocusHandle,
    /// Whether the keyboard has moved focus (the buttons' focus ring shows).
    keyboard: bool,
    engine: Engine,
    _subscriptions: Vec<Subscription>,
}

impl MachineFormView {
    pub fn new(state: MachineForm, engine: Engine, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |field: Field, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            let text = state.text(field).to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(text))
        };
        let name = input(Field::Name, "Machine name", window, cx);
        let host = input(Field::Host, "Hostname or SSH alias", window, cx);
        let work_dir = input(Field::WorkDir, "Path on the machine", window, cx);
        let tags = input(Field::Tags, "Comma-separated tags", window, cx);
        let mut subscriptions = Vec::new();
        for (entity, field) in [
            (&name, Field::Name),
            (&host, Field::Host),
            (&work_dir, Field::WorkDir),
            (&tags, Field::Tags),
        ] {
            subscriptions.push(cx.subscribe_in(entity, window, move |this, input, event: &InputEvent, _, cx| {
                match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        this.state.set(field, &text);
                        this.state.settle_focus();
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.state.set_focus(Focus::Field(field));
                        cx.notify();
                    }
                    _ => {}
                }
            }));
        }
        // Ahead of the fields' own bindings (Return and Esc are theirs
        // otherwise), while the keyboard is in the dialog: a palette opened
        // over it keeps its keys.
        let this_window = window.window_handle();
        let me = cx.weak_entity();
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
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
        }));
        let mut view = MachineFormView {
            state,
            name,
            host,
            work_dir,
            tags,
            focus: cx.focus_handle(),
            keyboard: false,
            engine,
            _subscriptions: subscriptions,
        };
        view.sync_focus(window, cx);
        view
    }

    /// The form's state.
    #[cfg(test)]
    pub fn state(&self) -> &MachineForm {
        &self.state
    }

    /// Field `field`'s input.
    pub fn input(&self, field: Field) -> &Entity<InputState> {
        match field {
            Field::Name => &self.name,
            Field::Host => &self.host,
            Field::WorkDir => &self.work_dir,
            Field::Tags => &self.tags,
        }
    }

    /// Type `text` into `field` (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn type_into(&mut self, field: Field, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input(field).clone().update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
        self.state.set(field, text);
        cx.notify();
    }

    /// Land a readiness outcome as the service would (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_check(&mut self, outcome: CheckOutcome, cx: &mut Context<Self>) {
        if let Some(request) = self.state.begin_check(self.state.test_intent()) {
            self.state.finish_check(request.run, outcome);
        }
        cx.notify();
    }

    /// Whether the keyboard is anywhere in the dialog.
    fn has_keys(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
            || [&self.name, &self.host, &self.work_dir, &self.tags]
                .iter()
                .any(|i| i.read(cx).focus_handle(cx).is_focused(window))
    }

    fn key(&mut self, chord: shelbi_state::keymap::KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.has_keys(window, cx) {
            return false;
        }
        let Some(action) = self.state.key(chord) else {
            return false;
        };
        if matches!(action, Action::Stay) {
            self.keyboard = true;
        }
        self.act(action, window, cx);
        true
    }

    /// A click on a button.
    fn click(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        self.keyboard = false;
        let action = self.state.press(focus);
        self.act(action, window, cx);
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Stay => {}
            Action::Cancel => {
                cx.emit(MachineFormEvent::Cancelled);
                return;
            }
            Action::Save(run, request) => self.save(run, request, window, cx),
            Action::Browse => self.browse(window, cx),
            Action::Check(request) => self.check(request, window, cx),
        }
        self.state.settle_focus();
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// Give the keyboard to the state's focus: a field's input, else the
    /// dialog's own handle (which paints the ring on a button).
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.state.focus() {
            Focus::Field(field) => self.input(field).read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    fn save(&mut self, run: u64, request: SaveRequest, window: &mut Window, cx: &mut Context<Self>) {
        let save = self.engine.save.clone();
        let project = self.state.project().to_string();
        let task = cx.background_executor().spawn(async move { save(&project, &request) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                if let Some(change) = this.state.finish_save(run, result) {
                    cx.emit(MachineFormEvent::Saved(change));
                    return;
                }
                this.sync_focus(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn check(&mut self, request: ReadinessRequest, window: &mut Window, cx: &mut Context<Self>) {
        let readiness = self.engine.readiness.clone();
        let run = request.run;
        let task = cx.background_executor().spawn(async move { readiness(&request) });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.state.finish_check(run, outcome);
                this.state.settle_focus();
                this.sync_focus(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Choose…: the native folder chooser, for this Mac's directory. A
    /// cancel keeps the directory there was.
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
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

    /// The folder chooser closed with `path` (`None`: cancelled).
    pub fn folder_picked(&mut self, path: Option<&Path>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = path {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let text = super::workspaces_state::tilde(path, home.as_deref());
            self.work_dir.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
            self.state.set(Field::WorkDir, &text);
            self.state.set_focus(Focus::Field(Field::WorkDir));
            self.sync_focus(window, cx);
        }
        cx.notify();
    }
}

impl EventEmitter<MachineFormEvent> for MachineFormView {}

impl Focusable for MachineFormView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The scrim over the whole window with `form` centered over the main area,
/// `main_left` from the window's left edge: the window's top layer while a
/// form is open. A press on the scrim does nothing (Cancel and Esc
/// dismiss); none reaches the window under it.
pub fn render_layer(form: Entity<MachineFormView>, main_left: Pixels, theme: &Theme) -> AnyElement {
    div()
        .id("machine-form-layer")
        .debug_selector(|| "machine-form-layer".into())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .bg(theme.scrim)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .pl(main_left)
        .py(px(DIALOG_MARGIN))
        .font_family("Geist")
        .type_style(Type::Base)
        .child(form)
        .into_any_element()
}

/// A field's label over its control, and its helper (or the service's
/// error) under it.
fn labeled(label: &'static str, control: impl IntoElement, under: Option<AnyElement>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .w_full()
        .min_w_0()
        .child(div().type_style(Type::SmallMedium).text_color(theme.foreground).child(label))
        .child(control)
        .children(under)
}

/// A helper line, or the field's error in its place.
fn helper_or_error(selector: &'static str, help: Option<&'static str>, error: Option<&str>, theme: &Theme) -> Option<AnyElement> {
    match error {
        Some(e) => Some(
            div()
                .debug_selector(move || format!("{selector}-error"))
                .type_style(Type::Small)
                .text_color(theme.error)
                .child(e.to_string())
                .into_any_element(),
        ),
        None => help.map(|h| footnote(h, theme).into_any_element()),
    }
}

impl MachineFormView {
    fn field(&self, field: Field, selector: &'static str, window: &Window, cx: &App) -> Div {
        text_field(self.input(field), selector, None, self.state.field_error(field).is_some(), window, cx)
    }

    fn button(&self, id: &'static str, label: &'static str, focus: Focus, kind: ButtonKind, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.global::<Theme>();
        let focused = self.keyboard && self.state.focus() == focus;
        style::button(id, label, kind, ButtonSize::Small, focused, theme)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.click(focus, window, cx)))
    }

    /// The connection check region: what's known for the draft's host and
    /// directory, and its buttons.
    fn render_check(&self, cx: &mut Context<Self>) -> Div {
        let theme = *cx.global::<Theme>();
        let host = self.state.check_key().host;
        let (icon, title, detail): (AnyElement, String, String) = match self.state.check() {
            Check::NotChecked => (
                heroicon(icons::SIGNAL_SLASH).size(px(CHECK_ICON)).text_color(theme.form_hint).into_any_element(),
                "Not checked yet".into(),
                "Test the connection to check SSH, the Shelbi CLI, Git, and the project directory.".into(),
            ),
            Check::Checking { .. } => (
                spinner::spinner_sized("machine-check-spinner", theme.form_hint, CHECK_ICON).into_any_element(),
                "Checking…".into(),
                format!("Connecting to {host} over SSH."),
            ),
            Check::Done { outcome, .. } => match outcome {
                CheckOutcome::Ready { detail } => (
                    heroicon(icons::CHECK).size(px(CHECK_ICON)).text_color(theme.green).into_any_element(),
                    "Connection successful".into(),
                    detail.clone(),
                ),
                CheckOutcome::Reachable => (
                    heroicon(icons::CHECK).size(px(CHECK_ICON)).text_color(theme.form_hint).into_any_element(),
                    "SSH connected".into(),
                    "The Shelbi CLI, Git, and the project directory aren’t checked yet.".into(),
                ),
                CheckOutcome::NeedsCli { message } => (
                    heroicon(icons::EXCLAMATION_TRIANGLE)
                        .size(px(CHECK_ICON))
                        .text_color(theme.warning)
                        .into_any_element(),
                    "Shelbi CLI needed".into(),
                    message.clone(),
                ),
                CheckOutcome::Failed { message } => (
                    heroicon(icons::EXCLAMATION_TRIANGLE)
                        .size(px(CHECK_ICON))
                        .text_color(theme.error)
                        .into_any_element(),
                    "Couldn’t connect".into(),
                    message.clone(),
                ),
            },
        };
        let checking = matches!(self.state.check(), Check::Checking { .. });
        let test_label = match self.state.test_intent() {
            ReadinessIntent::TestAgain => "Test again",
            _ => "Test connection",
        };
        let test = if checking {
            style::disabled_button("machine-test", test_label, ButtonKind::Secondary, ButtonSize::Small, &theme)
                .into_any_element()
        } else {
            self.button("machine-test", test_label, Focus::Test, ButtonKind::Secondary, cx)
                .into_any_element()
        };
        let install = (self.state.offers_install() && !checking).then(|| {
            self.button("machine-install-cli", "Install CLI", Focus::InstallCli, ButtonKind::Primary, cx)
        });
        div()
            .debug_selector(|| "machine-check".into())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .w_full()
            .p(px(16.0))
            .rounded(px(6.0))
            .bg(theme.textbox)
            .child(div().flex().flex_none().items_center().justify_center().size(px(CHECK_ICON)).child(icon))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .type_style(Type::Small)
                    .child(
                        div()
                            .debug_selector(|| "machine-check-title".into())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_loud)
                            .child(title),
                    )
                    .child(
                        div()
                            .debug_selector(|| "machine-check-detail".into())
                            .text_color(theme.form_hint)
                            .child(detail),
                    ),
            )
            .child(div().flex().flex_row().flex_none().gap(px(8.0)).children(install).child(test))
    }
}

impl Render for MachineFormView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let remote = self.state.remote();
        let heading = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(
                div()
                    .debug_selector(|| "machine-form-title".into())
                    .type_style(Type::Heading)
                    .text_size(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_loud)
                    .child(self.state.title()),
            )
            .child(
                div()
                    .debug_selector(|| "machine-form-description".into())
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child(self.state.description()),
            );
        let name = labeled(
            "Name",
            self.field(Field::Name, "machine-name", window, cx),
            helper_or_error("machine-name", None, self.state.field_error(Field::Name), &theme),
            &theme,
        );
        let identity = if remote {
            let host = labeled(
                "SSH host",
                self.field(Field::Host, "machine-host", window, cx),
                helper_or_error("machine-host", None, self.state.field_error(Field::Host), &theme),
                &theme,
            );
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(16.0))
                .w_full()
                .child(name.flex_1())
                .child(host.flex_1())
        } else {
            name
        };
        let work_dir_field = self.field(Field::WorkDir, "machine-work-dir", window, cx);
        let work_dir_control = if remote {
            work_dir_field.into_any_element()
        } else {
            let browse = self.button("machine-browse", "Choose…", Focus::Browse, ButtonKind::Secondary, cx);
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .w_full()
                .child(div().flex_1().min_w_0().child(work_dir_field))
                .child(browse)
                .into_any_element()
        };
        let work_dir = labeled(
            "Project directory",
            work_dir_control,
            helper_or_error(
                "machine-work-dir",
                Some("The root project directory on this machine."),
                self.state.field_error(Field::WorkDir),
                &theme,
            ),
            &theme,
        );
        let tags = labeled(
            "Machine tags",
            self.field(Field::Tags, "machine-tags", window, cx),
            helper_or_error(
                "machine-tags",
                Some("Inherited by every workspace on this machine."),
                self.state.field_error(Field::Tags),
                &theme,
            ),
            &theme,
        );
        let error = self.state.error().map(|e| {
            div()
                .debug_selector(|| "machine-form-error".into())
                .w_full()
                .type_style(Type::Small)
                .text_color(theme.error)
                .child(e.to_string())
        });
        let cancel = self.button("machine-cancel", "Cancel", Focus::Cancel, ButtonKind::Secondary, cx);
        let label = self.state.submit_label();
        let submit = if self.state.saving() {
            style::disabled_button("machine-submit", label, ButtonKind::Primary, ButtonSize::Small, &theme)
        } else {
            self.button("machine-submit", label, Focus::Submit, ButtonKind::Primary, cx)
        };
        let actions = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .w_full()
            .child(div().flex_1())
            .when(self.state.saving(), |d| {
                d.child(spinner::spinner_sized("machine-saving", theme.form_hint, CHECK_ICON))
            })
            .child(cancel)
            .child(submit);
        let body = div()
            .id("machine-form-body")
            .flex()
            .flex_col()
            .flex_shrink(1.0)
            .min_h_0()
            .gap(px(DIALOG_GAP))
            .w_full()
            .overflow_y_scroll()
            .child(heading)
            .child(identity)
            .when(remote, |d| {
                d.child(footnote(
                    "Use a hostname or an alias from your SSH config. Your existing SSH keys and settings are observed.",
                    &theme,
                ))
            })
            .child(work_dir)
            .child(tags)
            .when(remote, |d| {
                d.child(self.render_check(cx)).child(footnote(
                    "Shelbi checks the remote CLI and offers to install a compatible version when needed.",
                    &theme,
                ))
            })
            .children(error);
        div()
            .id("machine-form")
            .debug_selector(|| "machine-form".into())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_none()
            .max_h_full()
            .gap(px(DIALOG_GAP))
            .w(px(DIALOG_W))
            .p(px(DIALOG_PAD))
            .rounded(px(DIALOG_RADIUS))
            .border_1()
            .border_color(theme.divider)
            .bg(theme.canvas)
            .shadow(style::dialog_shadow(&theme))
            .text_color(theme.foreground)
            .child(body)
            .child(div().flex_none().h(px(1.0)).w_full().bg(theme.divider))
            .child(actions)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    use shelbi_core::{Machine, Project, WorkspaceSpec};
    use shelbi_orchestrator::pool_admin::{HookWiring, PoolOps, SessionState, WorktreeState};

    /// A [`PoolOps`] with nothing live: no daemon to check, no tasks, no
    /// sessions; worktrees are whatever `worktree` says.
    pub(crate) struct QuietOps {
        pub active: Option<(String, String)>,
        pub worktree: WorktreeState,
    }

    impl Default for QuietOps {
        fn default() -> Self {
            QuietOps {
                active: None,
                worktree: WorktreeState::Missing,
            }
        }
    }

    impl PoolOps for QuietOps {
        fn check_version(&self) -> Result<(), String> {
            Ok(())
        }
        fn active_task(&self, _: &Project, workspace: &str) -> Result<Option<String>, String> {
            Ok(self.active.as_ref().filter(|(w, _)| w == workspace).map(|(_, t)| t.clone()))
        }
        fn open_assignments(&self, _: &Project, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
        fn session(&self, _: &Project, _: &WorkspaceSpec) -> SessionState {
            SessionState::Dead
        }
        fn worktree(&self, _: &Machine, _: &WorkspaceSpec) -> WorktreeState {
            self.worktree.clone()
        }
        fn materialize(&self, _: &Project, _: &Machine, _: &WorkspaceSpec) -> Result<(), String> {
            Ok(())
        }
        fn wire_hooks(&self, _: &Project, _: &Machine, _: &WorkspaceSpec) -> Result<HookWiring, String> {
            Ok(HookWiring::Wired)
        }
        fn move_worktree(&self, _: &Project, _: &Machine, _: &WorkspaceSpec, _: &WorkspaceSpec) -> Result<(), String> {
            Ok(())
        }
        fn remove_worktree(&self, _: &Project, _: &Machine, _: &WorkspaceSpec) -> Result<(), String> {
            Ok(())
        }
        fn kill_session(&self, _: &Project, _: &WorkspaceSpec) -> Result<(), String> {
            Ok(())
        }
    }

    /// The shared machine service over `ops`, against the test's home.
    pub(crate) fn save_with(ops: QuietOps) -> Arc<SaveFn> {
        Arc::new(move |project, request| match request {
            SaveRequest::Add(fields) => pool_admin::add_machine_with(&ops, project, fields),
            SaveRequest::Update(update) => pool_admin::update_machine_with(&ops, project, update),
        })
    }

    /// The readiness requests made, answering each with `outcome`.
    pub(crate) fn recording_readiness(
        outcome: CheckOutcome,
    ) -> (Arc<ReadinessFn>, Arc<Mutex<Vec<ReadinessRequest>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let f: Arc<ReadinessFn> = Arc::new(move |request: &ReadinessRequest| {
            log.lock().unwrap().push(request.clone());
            outcome.clone()
        });
        (f, seen)
    }

    /// A test's default engine: the real service over [`QuietOps`], and a
    /// readiness check that answers SSH-only.
    pub(crate) fn quiet_engine() -> Engine {
        Engine {
            save: save_with(QuietOps::default()),
            readiness: recording_readiness(CheckOutcome::Reachable).0,
        }
    }

    #[test]
    fn the_interim_readiness_check_never_claims_more_than_ssh() {
        let request = |intent| ReadinessRequest {
            run: 1,
            intent,
            target: super::super::machine_form_state::ReadinessTarget {
                project: "p".into(),
                machine: None,
                name: "x".into(),
                host: "build".into(),
                work_dir: "/srv".into(),
            },
        };
        let install = reachability_only(&request(ReadinessIntent::InstallCli));
        assert!(matches!(install, CheckOutcome::Failed { ref message } if message.contains("Test again")));
    }
}
