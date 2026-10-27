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
//! [`Engine::real`]'s readiness service is `shelbi_orchestrator::machine_readiness`
//! (`dt-machine-readiness-setup`): Test connection checks SSH, the Shelbi CLI,
//! Git and the project directory over the user's own `ssh`, bounded and off
//! the UI thread; Install CLI runs the shared remote CLI setup (the one
//! `shelbi machine setup` runs) and checks again. A check still running when
//! the form closes, or when its host or directory is edited, is killed; an
//! install runs to its end (it's its own operation, not part of the draft).
//!
//! A machine's verified binary is recorded (`machine_state`, for dispatch and
//! the machines views) only for a machine the project has: when a save
//! commits a draft whose check verified it, or when a check verifies a
//! configured machine's own saved host. A check of an unsaved draft records
//! nothing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Global, Pixels, Stateful, Subscription, Window,
};
use gpui_kit::component::input::{InputEvent, InputState};
use shelbi_orchestrator::machine::SetupAction;
use shelbi_orchestrator::machine_readiness::{self, Readiness};
use shelbi_orchestrator::pool_admin::{self, MachineChange, PoolError};

use super::machine_form_state::{
    Action, Check, CheckOutcome, Field, Focus, MachineForm, ReadinessIntent, ReadinessRequest, SaveRequest, Tone,
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
/// [`Engine::readiness`]'s signature: the request, and a flag that trips
/// when no one will read the answer (a check should stop).
pub type ReadinessFn = dyn Fn(&ReadinessRequest, Arc<AtomicBool>) -> CheckOutcome + Send + Sync;
/// [`Engine::record`]'s signature: the machine's name in the project, its SSH
/// destination, and the check that verified it.
pub type RecordFn = dyn Fn(&str, &str, &Readiness) + Send + Sync;

/// The blocking work the form runs off the UI thread. A gpui global, so a
/// test can set its own; otherwise [`Engine::real`].
#[derive(Clone)]
pub struct Engine {
    /// Validate and persist the draft through the shared machine service.
    pub save: Arc<SaveFn>,
    /// The readiness service: run a Test connection / Test again / Install
    /// CLI request for the draft. Never writes the project's config.
    pub readiness: Arc<ReadinessFn>,
    /// Record a verified machine's binary (hub state, not project config).
    pub record: Arc<RecordFn>,
}

impl Global for Engine {}

impl Engine {
    /// The shared machine service, the readiness check and remote CLI setup,
    /// and the machine record store.
    #[cfg_attr(test, allow(dead_code))]
    pub fn real() -> Self {
        Engine {
            save: Arc::new(|project, request| match request {
                SaveRequest::Add(fields) => pool_admin::add_machine(project, fields),
                SaveRequest::Update(update) => pool_admin::update_machine(project, update),
            }),
            readiness: Arc::new(remote_readiness),
            record: Arc::new(|machine, host, readiness| {
                if let Err(e) = machine_readiness::save_verified_record(machine, host, readiness) {
                    tracing::warn!(machine, host, error = %e, "couldn't record the verified machine");
                }
            }),
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

/// The readiness service: Test connection / Test again check the draft's
/// host and directory; Install CLI runs the shared remote setup and checks
/// again.
fn remote_readiness(request: &ReadinessRequest, cancel: Arc<AtomicBool>) -> CheckOutcome {
    let target = request.target.remote();
    match request.intent {
        ReadinessIntent::Test | ReadinessIntent::TestAgain => {
            CheckOutcome::Checked(machine_readiness::check_host(&target, cancel))
        }
        ReadinessIntent::InstallCli => install_outcome(machine_readiness::install_cli_on_host(&target)),
    }
}

/// Install CLI's answer, from the shared setup's.
pub fn install_outcome(
    result: Result<machine_readiness::Installed, shelbi_orchestrator::machine::SetupError>,
) -> CheckOutcome {
    use shelbi_orchestrator::machine::SetupError;
    match result {
        Ok(done) => CheckOutcome::Installed {
            note: install_note(&done.action),
            readiness: done.readiness,
        },
        Err(e) => CheckOutcome::InstallFailed {
            retry: !matches!(
                e,
                SetupError::Unreachable(_) | SetupError::AuthDenied(_) | SetupError::HostKey(_)
            ),
            message: install_failure(&e),
        },
    }
}

/// Why setup failed, and what to do next, in the form's words.
fn install_failure(e: &shelbi_orchestrator::machine::SetupError) -> String {
    use shelbi_orchestrator::machine::SetupError;
    use shelbi_orchestrator::machine_readiness::Issue;
    let d = |d: &str| d.trim().trim_end_matches('.').to_string();
    match e {
        SetupError::Unreachable(x) => format!("SSH couldn’t reach the machine: {}. Check it’s on, then test again.", d(x)),
        SetupError::AuthDenied(x) => format!(
            "SSH couldn’t log in: {}. Make sure your keys log in without a prompt, then test again.",
            d(x)
        ),
        SetupError::HostKey(x) => format!(
            "SSH doesn’t trust the machine’s host key: {}. Verify it with `ssh` in a terminal, then test again.",
            d(x)
        ),
        SetupError::UnsupportedPlatform(p) => Issue::UnsupportedPlatform(p.clone()).message(""),
        SetupError::ReadOnlyHome(x) => format!(
            "Couldn’t write ~/.shelbi/bin on the machine: {}. Make its home directory writable, then install again.",
            d(x)
        ),
        SetupError::NoNetwork(x) => format!(
            "Neither the machine nor this Mac could download the release: {}. Check the network, then install again.",
            d(x)
        ),
        SetupError::InstallFailed(x) => format!(
            "The install failed: {}. Install again, or put a compatible shelbi on the machine’s PATH and test again.",
            d(x)
        ),
    }
}

/// What the shared setup did, in a sentence.
fn install_note(action: &SetupAction) -> String {
    match action {
        SetupAction::AlreadyCompatible { path, version, .. } => {
            format!("Shelbi {version} was already installed at {path}.")
        }
        SetupAction::Installed {
            path,
            version,
            alongside,
            via_hub_copy,
        } => {
            let how = if *via_hub_copy {
                " (copied from this Mac; the machine has no outbound network)"
            } else {
                ""
            };
            let beside = alongside
                .as_ref()
                .map(|old| format!(" The older one at {} was left as it was.", old.path))
                .unwrap_or_default();
            format!("Installed Shelbi {version} at {path}{how}.{beside}")
        }
    }
}

/// The form's running check and whether the form has closed, shared with
/// its owner: closing stops the check at once (the view itself lives on until
/// the next frame), and a closed form takes no answer.
#[derive(Clone, Default)]
pub struct CheckStop(Arc<Mutex<StopState>>);

#[derive(Default)]
struct StopState {
    closed: bool,
    running: Option<(u64, Arc<AtomicBool>)>,
}

impl CheckStop {
    fn lock(&self) -> std::sync::MutexGuard<'_, StopState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Check `run` starts: its stop flag (already tripped on a closed form).
    fn start(&self, run: u64) -> Arc<AtomicBool> {
        let mut s = self.lock();
        let flag = Arc::new(AtomicBool::new(s.closed));
        s.running = Some((run, flag.clone()));
        flag
    }

    /// Check `run` answered.
    fn finish(&self, run: u64) {
        let mut s = self.lock();
        if s.running.as_ref().is_some_and(|(r, _)| *r == run) {
            s.running = None;
        }
    }

    /// Stop the running check unless it's `live` (the one the form still
    /// shows).
    fn stop_unless(&self, live: Option<u64>) {
        let mut s = self.lock();
        if s.running.as_ref().is_some_and(|(r, _)| Some(*r) != live) {
            if let Some((_, flag)) = s.running.take() {
                flag.store(true, Ordering::Release);
            }
        }
    }

    /// The form closed: stop its check, and take no more answers. (An
    /// install doesn't read its flag; it runs to its end.)
    pub fn close(&self) {
        let mut s = self.lock();
        s.closed = true;
        if let Some((_, flag)) = s.running.take() {
            flag.store(true, Ordering::Release);
        }
    }

    pub fn closed(&self) -> bool {
        self.lock().closed
    }

    #[cfg(test)]
    fn running_flag(&self) -> Option<Arc<AtomicBool>> {
        self.lock().running.as_ref().map(|(_, f)| f.clone())
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
    /// The check in flight: stopped when the form closes or the draft moves
    /// off it.
    stop: CheckStop,
    _subscriptions: Vec<Subscription>,
}

impl Drop for MachineFormView {
    fn drop(&mut self) {
        self.stop.close();
    }
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
                        this.stop_stale_check();
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
            stop: CheckStop::default(),
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

    /// Press a button as a click would.
    #[cfg(test)]
    pub fn press_button(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        self.click(focus, window, cx);
    }

    /// The running check's stop flag.
    #[cfg(test)]
    pub fn running_flag(&self) -> Option<Arc<AtomicBool>> {
        self.stop.running_flag()
    }

    /// What the owner trips when the form closes.
    pub fn stopper(&self) -> CheckStop {
        self.stop.clone()
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

    /// Land a readiness outcome as the service would (`--visual-figma`);
    /// `None` leaves `intent` running.
    #[cfg(feature = "visual-check")]
    pub fn offscreen_check(&mut self, intent: ReadinessIntent, outcome: Option<CheckOutcome>, cx: &mut Context<Self>) {
        if let Some(request) = self.state.begin_check(intent) {
            if let Some(outcome) = outcome {
                self.state.finish_check(request.run, outcome);
            }
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
                self.stop.close();
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

    /// Stop the check in flight if the draft has moved off it (an edit to
    /// its host or directory dropped it).
    fn stop_stale_check(&mut self) {
        self.stop.stop_unless(self.state.running());
    }

    /// Record `readiness` for `machine` at `host`, off the UI thread.
    fn record(&self, machine: String, host: String, readiness: Readiness, cx: &mut Context<Self>) {
        let record = self.engine.record.clone();
        cx.background_executor()
            .spawn(async move { record(&machine, &host, &readiness) })
            .detach();
    }

    fn save(&mut self, run: u64, request: SaveRequest, window: &mut Window, cx: &mut Context<Self>) {
        let save = self.engine.save.clone();
        let project = self.state.project().to_string();
        // What the check verified for the draft being saved: recorded once
        // the save commits it, for the host it commits.
        let verified = self
            .state
            .verified()
            .cloned()
            .map(|r| (self.state.check_key().host, r));
        let task = cx.background_executor().spawn(async move { save(&project, &request) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                if let Some(change) = this.state.finish_save(run, result) {
                    let saved = &change.machine;
                    let host = saved.host.clone().unwrap_or_else(|| saved.name.clone());
                    if let Some((checked, readiness)) = verified {
                        if checked == host && saved.kind == shelbi_core::MachineKind::Ssh {
                            this.record(saved.name.clone(), host, readiness, cx);
                        }
                    }
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
        let cancel = self.stop.start(run);
        let task = cx.background_executor().spawn(async move { readiness(&request, cancel) });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.stop.finish(run);
                // A check or install that verified a machine the project
                // already has, at its saved host, is recorded now (even if
                // the form closed or moved on meanwhile: it's still true of
                // that machine); a draft's waits for a save.
                let key = match this.state.check() {
                    Check::Checking { run: r, key, .. } if *r == run => Some(key.clone()),
                    _ => None,
                };
                let committed = key.as_ref().and_then(|k| this.state.committed_name(k)).map(str::to_string);
                if let (Some(name), Some(key), Some(readiness)) = (committed, key, outcome.readiness()) {
                    if readiness.is_ready() {
                        this.record(name, key.host, readiness.clone(), cx);
                    }
                }
                if this.stop.closed() {
                    return;
                }
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
            let text = shelbi_app::settings::workspaces::tilde(path, home.as_deref());
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
        let panel = self.state.panel();
        let warning = |color| {
            heroicon(icons::EXCLAMATION_TRIANGLE)
                .size(px(CHECK_ICON))
                .text_color(color)
                .into_any_element()
        };
        let icon: AnyElement = match panel.tone {
            Tone::Idle => heroicon(icons::SIGNAL_SLASH)
                .size(px(CHECK_ICON))
                .text_color(theme.form_hint)
                .into_any_element(),
            Tone::Busy => spinner::spinner_sized("machine-check-spinner", theme.form_hint, CHECK_ICON).into_any_element(),
            Tone::Success => heroicon(icons::CHECK)
                .size(px(CHECK_ICON))
                .text_color(theme.green)
                .into_any_element(),
            Tone::Warning => warning(theme.warning),
            Tone::Error => warning(theme.error),
        };
        let (title, detail) = (panel.title, panel.detail);
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

    use super::super::machine_form_state::samples;
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
        let f: Arc<ReadinessFn> = Arc::new(move |request: &ReadinessRequest, _| {
            log.lock().unwrap().push(request.clone());
            outcome.clone()
        });
        (f, seen)
    }

    /// The records written: `(machine, host, ready)`.
    pub(crate) type Records = Arc<Mutex<Vec<(String, String, bool)>>>;

    /// A record store that remembers what it was asked to write.
    pub(crate) fn recording_records() -> (Arc<RecordFn>, Records) {
        let seen: Records = Arc::default();
        let log = seen.clone();
        let f: Arc<RecordFn> = Arc::new(move |machine: &str, host: &str, r: &Readiness| {
            log.lock().unwrap().push((machine.into(), host.into(), r.is_ready()));
        });
        (f, seen)
    }

    /// A test's default engine: the real service over [`QuietOps`], a
    /// readiness check that finds the CLI missing, and records kept in memory.
    pub(crate) fn quiet_engine() -> Engine {
        Engine {
            save: save_with(QuietOps::default()),
            readiness: recording_readiness(CheckOutcome::Checked(samples::needs_cli())).0,
            record: recording_records().0,
        }
    }

    #[test]
    fn install_answers_say_what_setup_did() {
        use shelbi_orchestrator::machine::{parse_version, ResolvedBinary, SetupError};
        use shelbi_orchestrator::machine_readiness::Installed;
        let v = parse_version("0.9.0").unwrap();
        let installed = install_outcome(Ok(Installed {
            action: SetupAction::Installed {
                path: "~/.shelbi/bin/shelbi".into(),
                version: v,
                alongside: Some(ResolvedBinary {
                    path: "/usr/bin/shelbi".into(),
                    version: parse_version("0.8.0"),
                }),
                via_hub_copy: true,
            },
            readiness: samples::ready(),
        }));
        let CheckOutcome::Installed { note, .. } = &installed else { panic!() };
        assert!(note.contains("~/.shelbi/bin/shelbi") && note.contains("copied from this Mac"));
        assert!(note.contains("/usr/bin/shelbi was left as it was"));
        assert!(installed.is_ready());

        let reused = install_outcome(Ok(Installed {
            action: SetupAction::AlreadyCompatible {
                path: "/usr/local/bin/shelbi".into(),
                version: v,
                source: "path",
            },
            readiness: samples::ready(),
        }));
        assert!(matches!(reused, CheckOutcome::Installed { ref note, .. } if note.contains("already installed")));

        let down = install_outcome(Err(SetupError::Unreachable("timed out".into())));
        assert!(!down.offers_install(), "a failed connection is retried with Test again");
        let home = install_outcome(Err(SetupError::ReadOnlyHome("Read-only file system".into())));
        assert!(home.offers_install());
        let CheckOutcome::InstallFailed { message, .. } = home else { panic!() };
        assert!(message.starts_with("Couldn’t write ~/.shelbi/bin") && message.contains("install again"));
    }
}
