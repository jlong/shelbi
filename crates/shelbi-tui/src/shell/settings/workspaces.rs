//! Settings ▸ Workspaces in the TUI (`tui-settings-workspaces`), after the
//! desktop app's page and dialogs (`desktop/src/views/workspaces.rs`,
//! `workspace_dialog.rs`) over the same toolkit-free state:
//! `shelbi_app::settings::workspaces` (the pool's rows, the machines and
//! their reachability) and `shelbi_app::settings::workspace_form` (the New /
//! Edit workspace form and the removal confirmation).
//!
//! The page lists the project's workspaces (name, machine and whether it's
//! local or SSH, the derived worktree under the name, own and inherited tags,
//! the effective slot) and, below them, the machines that host them (SSH host
//! or Local, root directory, workspace count, reachability). It's
//! configuration: the runtime Machines view stays where they're watched at
//! work.
//!
//! Keys on the page: ↑ / ↓ (or the sidebar's nav keys) move between New
//! workspace, the workspace rows, Add machine and the machine rows; Enter
//! runs the selected one (a workspace row opens Edit); `n` is New workspace
//! from anywhere on the page, `e` edits and `d` / Delete removes the selected
//! workspace (through the confirmation), `m` is Add machine and `c`
//! configures the selected machine; `r` reads the config again.
//!
//! The form and the confirmation are a modal layer over the main area: while
//! one is up it takes every key and click. Its fields edit at the end of the
//! text (type, Backspace, Ctrl+U clears); a New form's suggested name is
//! replaced by the first key typed. Tab / Shift-Tab (and ↑ / ↓ between text
//! fields) cycle the controls, ↑ / ↓ open and walk the machine picker, Enter
//! is the default action (on Cancel or Remove, that button), Esc cancels.
//! Nothing is written until Create workspace, Save changes or Remove
//! workspace; the request then runs off the UI thread through the shared
//! service, and the form stays busy (sent once) until it lands.
//!
//! Add machine and a machine row (Enter, or `c` on it) open the machine form
//! ([`super::machine_form`]): Add machine for a new SSH machine, Configure
//! machine for an existing one (SSH or local). It's the same modal layer, and
//! its save runs the same way, through the shared machine service; a rename
//! carries the machine's workspaces along and the cursor follows the machine.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    Frame,
};
use shelbi_app::reachability::{ReachabilityProber, REACHABILITY_BACKOFF_MAX, REACHABILITY_CADENCE};
use shelbi_app::settings::workspace_form::{
    self as form, Action, ConfirmFocus, Focus, Key, MachineChoice, Outcome, RemoveConfirm,
    WorkspaceForm, LOCATION_HELP,
};
use shelbi_app::settings::machine_check::{self as mcheck, CheckOutcome, CheckStop, ReadinessForm, ReadinessRequest};
use shelbi_app::settings::machine_form as mform;
use shelbi_app::settings::workspaces::{
    kind_label, Intent, Load, MachineRow, MachineStatus, PoolIndex, WorkspaceRow,
    WorkspacesState,
};
use shelbi_app::settings::{Job, Ticket};
use shelbi_core::pool::{MachineChange, PoolError};

use super::machine_form;
use crate::button::Button;
use super::HoverKey;
use crate::modal_form::{self, Field, FormLines};
use crate::panel;
use crate::table::{Cell, Column, Table, TableLine};
use crate::textbox::Textbox;
use crate::theme::palette;

/// Reads a project's pool (blocking; run off the UI thread).
pub(crate) type PoolReader = Arc<dyn Fn(&str) -> Result<PoolIndex, String> + Send + Sync>;

/// How long a shown page goes before it reads the config again on its own,
/// so an edit made elsewhere (the CLI, a hand edit) shows up.
pub(crate) const EXTERNAL_RELOAD: Duration = Duration::from_secs(5);

/// A selectable row of the page, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    New,
    Workspace(usize),
    AddMachine,
    Machine(usize),
    /// Try again, after a read failed.
    Retry,
}

/// What the page asks the shell for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    Stay,
    /// Read the pool again.
    Reload,
    /// Run this request off the UI thread; its result comes back under the
    /// ticket ([`WorkspacesPage::finished`]).
    Run(Ticket, Box<form::Job>),
    /// Run this machine save off the UI thread; its result comes back under
    /// the ticket ([`WorkspacesPage::machine_finished`]).
    RunMachine(Ticket, Box<mform::Job>),
    /// Run this readiness request off the UI thread; its outcome comes back
    /// to [`WorkspacesPage::check_finished`].
    RunCheck(Box<CheckJob>),
}

/// A machine form's readiness request, for the shell's worker thread: the
/// form it's for (an answer for a form since closed answers no form), and
/// the flag that stops it.
#[derive(Debug, Clone)]
pub(crate) struct CheckJob {
    pub form: u64,
    pub request: ReadinessRequest,
    pub cancel: Arc<AtomicBool>,
}

impl PartialEq for CheckJob {
    fn eq(&self, other: &Self) -> bool {
        self.form == other.form && self.request == other.request
    }
}

impl Eq for CheckJob {}

/// What a finished request asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Landed {
    /// A change was committed to this project's config: refresh what shows
    /// it (the page, the sidebar) when it's the shown project.
    pub committed: Option<String>,
    /// The verified binary to record for the machine the save committed
    /// (hub state, off the UI thread).
    pub record: Option<mcheck::Record>,
}

/// An open Add machine / Configure machine form.
#[derive(Debug, Clone)]
pub(crate) struct MachineLayer {
    /// Which form this is (a check's answer names it).
    id: u64,
    form: ReadinessForm,
    /// Stops its running check when it goes stale or the form closes.
    stop: CheckStop,
    /// When the job running now started (the region's elapsed time).
    started: Option<Instant>,
    /// What an install whose answer the form dropped did on the machine.
    aside: Option<String>,
}

/// The modal layer over the page.
#[derive(Debug, Clone)]
enum Layer {
    /// New / Edit workspace. `fresh` while a New form's suggested name
    /// hasn't been typed over: the first key replaces it.
    Form { form: Box<WorkspaceForm>, fresh: bool },
    /// Remove workspace…, from a row.
    Confirm(RemoveConfirm),
    /// Add machine / Configure machine.
    Machine(Box<MachineLayer>),
}

/// What a click at a recorded rect hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Control(Focus),
    Option(usize),
    ConfirmCancel,
    ConfirmRemove,
    /// A control of the machine form.
    Machine(mform::Focus),
}

/// The page's state. One per Settings: it outlives a visit.
pub(crate) struct WorkspacesPage {
    /// The shared state, for the project the page is for (`None` with no
    /// project open).
    state: Option<WorkspacesState>,
    /// The latest read's ticket.
    load: Job<()>,
    /// When the latest read landed (for [`EXTERNAL_RELOAD`]).
    loaded_at: Option<Instant>,
    cursor: usize,
    /// The workspace the cursor goes to once the next read lands (the one
    /// just created or renamed).
    follow: Option<String>,
    /// The machine the cursor goes to once the next read lands (the one just
    /// added or saved).
    follow_machine: Option<String>,
    layer: Option<Layer>,
    /// The latest request's ticket.
    run: Job<()>,
    /// The latest machine save's ticket.
    machine_run: Job<()>,
    /// What that save records once it commits: the check that verified its
    /// draft.
    save_verified: Option<mcheck::SaveVerified>,
    /// The last machine form's id.
    forms: u64,
    /// The remotes' prober, while the page shows (never under test).
    prober: Option<ReachabilityProber>,
    home: Option<PathBuf>,
    /// Results that landed after the page moved on (another project, a
    /// newer request).
    dropped: usize,
    hits: Vec<(Rect, Hit)>,
}

impl Default for WorkspacesPage {
    fn default() -> Self {
        WorkspacesPage {
            state: None,
            load: Job::default(),
            loaded_at: None,
            cursor: 0,
            follow: None,
            follow_machine: None,
            layer: None,
            run: Job::default(),
            machine_run: Job::default(),
            save_verified: None,
            forms: 0,
            prober: None,
            home: std::env::var_os("HOME").map(PathBuf::from),
            dropped: 0,
            hits: Vec::new(),
        }
    }
}

impl WorkspacesPage {
    #[cfg(test)]
    pub(crate) fn state(&self) -> Option<&WorkspacesState> {
        self.state.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn form(&self) -> Option<&WorkspaceForm> {
        match &self.layer {
            Some(Layer::Form { form, .. }) => Some(form),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn confirm(&self) -> Option<&RemoveConfirm> {
        match &self.layer {
            Some(Layer::Confirm(c)) => Some(c),
            Some(Layer::Form { form, .. }) => form.confirm(),
            Some(Layer::Machine(_)) | None => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn machine_form(&self) -> Option<&ReadinessForm> {
        match &self.layer {
            Some(Layer::Machine(m)) => Some(&m.form),
            _ => None,
        }
    }

    /// The open machine form's stop, and what it says of a dropped install.
    #[cfg(test)]
    pub(crate) fn machine_layer(&self) -> Option<(&CheckStop, Option<&str>)> {
        match &self.layer {
            Some(Layer::Machine(m)) => Some((&m.stop, m.aside.as_deref())),
            _ => None,
        }
    }

    /// Whether a machine form's check or install is running (the region
    /// animates).
    pub(crate) fn checking(&self) -> bool {
        matches!(&self.layer, Some(Layer::Machine(m)) if m.form.running().is_some())
    }

    pub(crate) fn dropped(&self) -> usize {
        self.dropped
    }

    #[cfg(test)]
    pub(crate) fn set_home(&mut self, home: Option<PathBuf>) {
        self.home = home;
    }

    #[cfg(test)]
    pub(crate) fn state_mut(&mut self) -> Option<&mut WorkspacesState> {
        self.state.as_mut()
    }

    /// Whether the form or the confirmation is up (it takes every key).
    pub(crate) fn modal(&self) -> bool {
        self.layer.is_some()
    }

    /// The selected row.
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    // --- loading -------------------------------------------------------------

    /// Start a read for `project`: a ticket the shell's reader carries, or
    /// `None` when it's read (or reading) for it already and `force` is off,
    /// or no project is open.
    pub(crate) fn begin_load(&mut self, project: Option<&str>, force: bool) -> Option<Ticket> {
        let Some(project) = project else {
            self.state = None;
            self.load.reset();
            return None;
        };
        if self.state.as_ref().map(WorkspacesState::project) != Some(project) {
            self.state = Some(WorkspacesState::new(project));
            self.cursor = 0;
        }
        let current = self.load.project() == Some(project) && !self.load.stale_for(Some(project));
        if current && !force {
            return None;
        }
        if let Some(state) = self.state.as_mut() {
            state.retry();
        }
        Some(self.load.begin(Some(project)))
    }

    /// A read landed. Returns whether it changed the rows from an earlier
    /// read (an edit made elsewhere: the shell refreshes its sidebar too). A
    /// stale one is dropped.
    pub(crate) fn loaded(&mut self, ticket: &Ticket, read: Result<PoolIndex, String>) -> bool {
        if !self.load.complete(ticket, read.as_ref().map(|_| ()).map_err(Clone::clone)) {
            self.dropped += 1;
            return false;
        }
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let project = ticket.project().unwrap_or_default().to_string();
        let changed = state.loaded(&project, read);
        self.loaded_at = Some(Instant::now());
        if let Some(name) = self.follow.take() {
            self.select_workspace(&name);
        }
        if let Some(name) = self.follow_machine.take() {
            self.select_machine(&name);
        }
        self.cursor = self.cursor.min(self.items().len().saturating_sub(1));
        self.retarget_prober();
        // An open form offers the machines as now declared.
        if let (Some(Layer::Form { form, .. }), Some((source, machines))) = (self.layer.as_mut(), choices(self.state.as_ref())) {
            form.set_pool(source, machines);
        }
        changed
    }

    /// Whether a shown page is due to read the config again on its own. A
    /// read that failed waits for `r` / Try again instead.
    pub(crate) fn reload_due(&self, now: Instant) -> bool {
        !self.load.running()
            && self.index().is_some()
            && self.loaded_at.is_some_and(|at| now.duration_since(at) >= EXTERNAL_RELOAD)
    }

    /// The shown project changed: everything for the old one goes (an open
    /// form's draft too), and its in-flight results are dropped when they
    /// land.
    pub(crate) fn project_changed(&mut self) {
        self.state = None;
        self.load.reset();
        self.loaded_at = None;
        self.run.reset();
        self.machine_run.reset();
        self.save_verified = None;
        self.close_layer();
        self.cursor = 0;
        self.follow = None;
        self.follow_machine = None;
        self.retarget_prober();
    }

    /// Settings closed, or another page shows: the form's draft goes, as the
    /// desktop's dialog does, and the prober stops. A request already sent
    /// still lands ([`Self::finished`]).
    pub(crate) fn hidden(&mut self) {
        if let Some(note) = self.close_layer() {
            self.note(note);
        }
        self.prober = None;
    }

    /// Close the layer. A machine form's check stops; an install keeps
    /// running, and what it did (or that it's still going) is what's left to
    /// say once the form is gone: nothing about the machine is saved.
    fn close_layer(&mut self) -> Option<String> {
        let Some(Layer::Machine(m)) = self.layer.take() else {
            return None;
        };
        m.stop.close();
        let host = m.form.check_key().host;
        if m.form.installing() {
            return Some(format!(
                "Nothing was saved. The Shelbi CLI install on {host} keeps running; this page says what it \
                 did when it finishes."
            ));
        }
        match m.form.check() {
            mcheck::Check::Done { outcome, key } => outcome.side_effect(&key.host).map(|e| format!("Nothing was saved. {e}")),
            _ => None,
        }
    }

    fn new_machine_layer(form: ReadinessForm, id: u64) -> Layer {
        Layer::Machine(Box::new(MachineLayer {
            id,
            form,
            stop: CheckStop::default(),
            started: None,
            aside: None,
        }))
    }

    /// The page shows: start probing its remotes.
    pub(crate) fn shown(&mut self) {
        self.retarget_prober();
    }

    fn retarget_prober(&mut self) {
        let targets = self.state.as_ref().map(WorkspacesState::probe_targets).unwrap_or_default();
        if cfg!(test) {
            // Tests fold results in by hand; nothing probes SSH.
            return;
        }
        if self.prober.is_none() && !targets.is_empty() {
            self.prober = Some(ReachabilityProber::spawn(
                REACHABILITY_CADENCE,
                REACHABILITY_BACKOFF_MAX,
                |_key: &str, host: &str| shelbi_orchestrator::machine::probe_host_reachability(host),
            ));
        }
        if let Some(prober) = &self.prober {
            prober.set_targets(targets);
        }
    }

    /// Fold the prober's latest results in. Returns whether anything changed.
    pub(crate) fn take_probes(&mut self) -> bool {
        let (Some(prober), Some(state)) = (&self.prober, self.state.as_mut()) else {
            return false;
        };
        state.set_reachability(prober.snapshot())
    }

    // --- the rows --------------------------------------------------------------

    fn index(&self) -> Option<&PoolIndex> {
        self.state.as_ref().and_then(WorkspacesState::index)
    }

    /// The selectable rows, top to bottom.
    pub(crate) fn items(&self) -> Vec<Item> {
        let Some(state) = &self.state else {
            return Vec::new();
        };
        match state.load() {
            Load::Loading => Vec::new(),
            Load::Failed(_) => vec![Item::Retry],
            Load::Loaded(index) => {
                let mut out = vec![Item::New];
                out.extend((0..index.workspaces.len()).map(Item::Workspace));
                out.push(Item::AddMachine);
                out.extend((0..index.machines.len()).map(Item::Machine));
                out
            }
        }
    }

    pub(crate) fn step(&mut self, down: bool) {
        let n = self.items().len();
        self.cursor = if down {
            (self.cursor + 1).min(n.saturating_sub(1))
        } else {
            self.cursor.saturating_sub(1)
        };
    }

    /// Run row `i` (Enter, a click).
    pub(crate) fn activate(&mut self, i: usize) -> Ask {
        let Some(item) = self.items().get(i).copied() else {
            return Ask::Stay;
        };
        self.cursor = i;
        match item {
            Item::New => self.open(Intent::NewWorkspace),
            Item::Workspace(w) => match self.workspace(w) {
                Some(name) => self.open(Intent::EditWorkspace(name)),
                None => Ask::Stay,
            },
            Item::AddMachine => self.open(Intent::AddMachine),
            Item::Machine(m) => match self.index().and_then(|i| i.machines.get(m)).map(|m| m.name.clone()) {
                Some(name) => self.open(Intent::ConfigureMachine(name)),
                None => Ask::Stay,
            },
            Item::Retry => Ask::Reload,
        }
    }

    fn workspace(&self, w: usize) -> Option<String> {
        self.index().and_then(|i| i.workspaces.get(w)).map(|w| w.name.clone())
    }

    /// The selected machine's name, when a machine row is selected.
    fn selected_machine(&self) -> Option<String> {
        match self.items().get(self.cursor) {
            Some(Item::Machine(m)) => self.index().and_then(|i| i.machines.get(*m)).map(|m| m.name.clone()),
            _ => None,
        }
    }

    /// The selected workspace's name, when a workspace row is selected.
    fn selected_workspace(&self) -> Option<String> {
        match self.items().get(self.cursor) {
            Some(Item::Workspace(w)) => self.workspace(*w),
            _ => None,
        }
    }

    /// Carry out what a row asks for.
    pub(crate) fn open(&mut self, intent: Intent) -> Ask {
        let project = match &self.state {
            Some(s) => s.project().to_string(),
            None => return Ask::Stay,
        };
        match intent {
            Intent::NewWorkspace => {
                if let Some((source, machines)) = choices(self.state.as_ref()) {
                    let form = WorkspaceForm::new_workspace(&project, source, machines, self.home.clone());
                    self.layer = Some(Layer::Form { form: Box::new(form), fresh: true });
                }
                Ask::Stay
            }
            Intent::EditWorkspace(name) => {
                if let Some((source, machines)) = choices(self.state.as_ref()) {
                    match WorkspaceForm::edit_workspace(&project, source, machines, self.home.clone(), &name) {
                        Some(form) => self.layer = Some(Layer::Form { form: Box::new(form), fresh: false }),
                        None => self.note(format!("{name} isn’t declared any more.")),
                    }
                }
                Ask::Stay
            }
            Intent::RemoveWorkspace(name) => {
                if let Some((source, _)) = choices(self.state.as_ref()) {
                    match RemoveConfirm::new(&project, &source, &name, self.home.as_deref()) {
                        Some(c) => self.layer = Some(Layer::Confirm(c)),
                        None => self.note(format!("{name} isn’t declared any more.")),
                    }
                }
                Ask::Stay
            }
            Intent::AddMachine => {
                if self.index().is_some() {
                    self.forms += 1;
                    self.layer = Some(Self::new_machine_layer(ReadinessForm::add(&project), self.forms));
                }
                Ask::Stay
            }
            Intent::ConfigureMachine(name) => {
                let row = self.index().and_then(|i| i.machines.iter().find(|m| m.name == name)).cloned();
                match row {
                    Some(row) => {
                        let form = ReadinessForm::configure(&project, &row.config, row.workspaces, self.home.as_deref());
                        self.forms += 1;
                        self.layer = Some(Self::new_machine_layer(form, self.forms));
                    }
                    None => self.note(format!("{name} isn’t declared any more.")),
                }
                Ask::Stay
            }
        }
    }

    fn note(&mut self, text: String) {
        if let Some(state) = self.state.as_mut() {
            state.set_notice(Some(text));
        }
    }

    // --- keys and clicks ---------------------------------------------------

    /// A page key that isn't the shared moves (those the settings shell
    /// handles): `n`, `e`, `d` / Delete, `m` (Add machine), `c` (Configure
    /// the selected machine).
    pub(crate) fn page_key(&mut self, k: KeyEvent) -> Option<Ask> {
        if !k.modifiers.difference(KeyModifiers::SHIFT).is_empty() || self.index().is_none() {
            return None;
        }
        match k.code {
            KeyCode::Char('n') => Some(self.open(Intent::NewWorkspace)),
            KeyCode::Char('e') => self.selected_workspace().map(|w| self.open(Intent::EditWorkspace(w))),
            KeyCode::Char('d') | KeyCode::Delete => {
                self.selected_workspace().map(|w| self.open(Intent::RemoveWorkspace(w)))
            }
            KeyCode::Char('m') => Some(self.open(Intent::AddMachine)),
            KeyCode::Char('c') => self.selected_machine().map(|m| self.open(Intent::ConfigureMachine(m))),
            _ => None,
        }
    }

    /// A key while the layer is up: every key is the layer's.
    pub(crate) fn layer_key(&mut self, k: KeyEvent) -> Ask {
        let action = match self.layer.as_mut() {
            None => return Ask::Stay,
            Some(Layer::Machine(m)) => {
                let action = machine_form::key(&mut m.form, k);
                return self.act_machine(action);
            }
            Some(Layer::Confirm(c)) => match confirm_key(k) {
                Some(key) => c.key(key),
                None => Action::Stay,
            },
            Some(Layer::Form { form, fresh }) => form_key(form, fresh, k),
        };
        self.act(action)
    }

    /// A paste (or a dropped file's path) while the form shows: onto the
    /// focused text field, as one line. Returns whether a field took it.
    pub(crate) fn paste(&mut self, pasted: &str) -> bool {
        if let Some(Layer::Machine(m)) = self.layer.as_mut() {
            let took = machine_form::paste(&mut m.form, pasted);
            m.stop.stop_unless(m.form.running());
            return took;
        }
        let Some(Layer::Form { form, fresh }) = self.layer.as_mut() else {
            return false;
        };
        let focus = form.focus();
        let editable = matches!(focus, Focus::Name | Focus::Tags | Focus::Slot) && form.menu().is_none();
        if !editable || form.busy() || form.confirm().is_some() {
            return false;
        }
        let line: String = pasted.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        let mut text = if *fresh && focus == Focus::Name { String::new() } else { field_text(form, focus) };
        text.push_str(&line);
        *fresh = false;
        set_field(form, focus, &text);
        true
    }

    /// A click while the layer is up (it's modal: only its controls answer).
    pub(crate) fn layer_mouse(&mut self, m: MouseEvent) -> Ask {
        if !matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            return Ask::Stay;
        }
        let Some(hit) = self.hits.iter().rev().find(|(r, _)| panel::in_rect(*r, m.column, m.row)).map(|(_, h)| *h)
        else {
            return Ask::Stay;
        };
        if let (Some(Layer::Machine(m)), Hit::Machine(focus)) = (self.layer.as_mut(), hit) {
            let action = machine_form::click(&mut m.form, focus);
            return self.act_machine(action);
        }
        let action = match (self.layer.as_mut(), hit) {
            (Some(Layer::Confirm(c)), Hit::ConfirmCancel) => c.cancel(),
            (Some(Layer::Confirm(c)), Hit::ConfirmRemove) => c.remove(),
            (Some(Layer::Form { form, .. }), Hit::ConfirmCancel) => {
                form.dismiss_confirm();
                Action::Stay
            }
            (Some(Layer::Form { form, .. }), Hit::ConfirmRemove) => match form.confirm_mut() {
                Some(c) => c.remove(),
                None => Action::Stay,
            },
            (Some(Layer::Form { form, .. }), Hit::Option(i)) => {
                if let Some(name) = form.machines().get(i).map(|m| m.name.clone()) {
                    form.pick_machine(&name);
                }
                Action::Stay
            }
            (Some(Layer::Form { form, fresh }), Hit::Control(focus)) => {
                if form.busy() || form.confirm().is_some() {
                    Action::Stay
                } else {
                    *fresh &= focus == Focus::Name;
                    form.activate(focus)
                }
            }
            _ => Action::Stay,
        };
        self.act(action)
    }

    /// Carry out what the shared form said.
    fn act(&mut self, action: Action) -> Ask {
        match action {
            Action::Stay => Ask::Stay,
            Action::Close => {
                self.layer = None;
                Ask::Stay
            }
            Action::Run(job) => {
                let ticket = self.run.begin(Some(&job.project));
                Ask::Run(ticket, Box::new(job))
            }
            Action::Done(outcome) => {
                self.done(&outcome);
                Ask::Reload
            }
        }
    }

    /// Carry out what the machine form said. Whatever it did, a check the
    /// draft has moved off (an edit to its host or directory, a Stop check)
    /// is stopped.
    fn act_machine(&mut self, action: mcheck::Action) -> Ask {
        let Some(Layer::Machine(m)) = self.layer.as_mut() else {
            return Ask::Stay;
        };
        m.stop.stop_unless(m.form.running());
        if m.form.running().is_none() {
            m.started = None;
        }
        m.form.settle_focus();
        match action {
            mcheck::Action::Cancel => {
                if let Some(note) = self.close_layer() {
                    self.note(note);
                }
                Ask::Stay
            }
            mcheck::Action::Save(run, request) => {
                let project = m.form.project().to_string();
                self.save_verified = m.form.save_verified();
                let ticket = self.machine_run.begin(Some(&project));
                Ask::RunMachine(ticket, Box::new(mform::Job { project, run, request }))
            }
            mcheck::Action::Check(request) => {
                m.started = Some(Instant::now());
                m.aside = None;
                let cancel = m.stop.start(request.run);
                Ask::RunCheck(Box::new(CheckJob {
                    form: m.id,
                    request,
                    cancel,
                }))
            }
            // The TUI has no folder chooser (the directory is typed): the
            // form never offers it.
            mcheck::Action::Stay | mcheck::Action::Browse => Ask::Stay,
        }
    }

    /// A readiness request finished. It answers its form when that form is
    /// still up and still waiting on it; an install's answer otherwise still
    /// says what it did to the machine: on the form, when the draft has moved
    /// on, else on the page (nothing about the machine was saved). Returns
    /// what to say on the shell's status line when the page can't (the
    /// project switched).
    pub(crate) fn check_finished(&mut self, form: u64, request: &ReadinessRequest, outcome: CheckOutcome) -> Option<String> {
        let effect = outcome.side_effect(&request.target.host);
        if let Some(Layer::Machine(m)) = self.layer.as_mut() {
            if m.id == form {
                m.stop.finish(request.run);
                if m.form.finish_check(request.run, outcome) {
                    m.started = None;
                    m.form.settle_focus();
                    return None;
                }
                if effect.is_some() {
                    m.aside = effect;
                }
                return None;
            }
        }
        let effect = effect?;
        match &self.state {
            Some(s) if s.project() == request.target.project => {
                self.note(effect);
                None
            }
            _ => Some(format!("{}: {effect}", request.target.project)),
        }
    }

    /// A machine save finished. A current one answers its form (closing it
    /// on success, the cursor following the machine; keeping the draft and
    /// saying why on failure); one whose form went (Settings closed) still
    /// says what happened; one for another project, or superseded, is
    /// dropped. Any committed change asks for a refresh of its project.
    pub(crate) fn machine_finished(
        &mut self,
        ticket: &Ticket,
        job: &mform::Job,
        result: Result<MachineChange, PoolError>,
    ) -> Landed {
        let committed = result.is_ok().then(|| job.project.clone());
        if !self.machine_run.complete(ticket, Ok(())) {
            self.dropped += 1;
            return Landed { committed, record: None };
        }
        let verified = self.save_verified.take();
        let added = matches!(job.request, mform::SaveRequest::Add(_));
        // The ticket is current, so a form that's saving sent it; any other
        // form up now was opened after this one went.
        let change = match self.layer.as_mut() {
            Some(Layer::Machine(m)) if m.form.saving() => {
                let change = m.form.finish_save(job.run, result);
                if change.is_some() {
                    if let Some(Layer::Machine(m)) = self.layer.take() {
                        m.stop.close();
                    }
                }
                change
            }
            _ => match result {
                Ok(change) => Some(change),
                Err(e) => {
                    self.note(form::sentence(&mform::refusal_message(&e)));
                    None
                }
            },
        };
        let record = change.as_ref().and_then(|c| verified.and_then(|v| v.record(c)));
        if let Some(change) = change {
            self.note(mform::notice(&change, added));
            self.follow_machine = Some(change.machine.name.clone());
        }
        Landed { committed, record }
    }

    /// Put the cursor on machine `name`'s row, when it's listed.
    fn select_machine(&mut self, name: &str) {
        let Some(m) = self.index().and_then(|i| i.machines.iter().position(|m| m.name == name)) else {
            return;
        };
        if let Some(at) = self.items().iter().position(|i| *i == Item::Machine(m)) {
            self.cursor = at;
        }
    }

    fn done(&mut self, outcome: &Outcome) {
        self.layer = None;
        if let Some(state) = self.state.as_mut() {
            state.set_notice(Some(outcome.notice()));
        }
        // The cursor follows the workspace, the renamed or new one, once the
        // read that shows it lands; after a removal it stays put, on the row
        // that took its place.
        self.follow = outcome.workspace().map(str::to_string);
    }

    /// Put the cursor on workspace `name`'s row, when it's listed.
    fn select_workspace(&mut self, name: &str) {
        let Some(w) = self.index().and_then(|i| i.workspaces.iter().position(|w| w.name == name)) else {
            return;
        };
        if let Some(at) = self.items().iter().position(|i| *i == Item::Workspace(w)) {
            self.cursor = at;
        }
    }

    /// A request finished. A current one answers its form (closing it on
    /// success, keeping the draft and saying why on failure); one whose form
    /// went (Settings closed) still says what happened; a superseded one is
    /// dropped. Any committed change asks for a refresh of its project.
    pub(crate) fn finished(&mut self, ticket: &Ticket, result: Result<Outcome, PoolError>) -> Landed {
        let committed = result.is_ok().then(|| ticket.project().unwrap_or_default().to_string());
        if !self.run.complete(ticket, Ok(())) {
            self.dropped += 1;
            return Landed { committed, record: None };
        }
        let action = match self.layer.as_mut() {
            Some(Layer::Form { form, .. }) => form.finished(result),
            Some(Layer::Confirm(c)) => c.finished(result),
            Some(Layer::Machine(_)) | None => match result {
                Ok(outcome) => Action::Done(outcome),
                Err(e) => {
                    self.note(form::sentence(&e.to_string()));
                    Action::Stay
                }
            },
        };
        if let Action::Done(outcome) = &action {
            self.done(outcome);
        }
        Landed { committed, record: None }
    }

    /// A committed change elsewhere: the page's next show reads again.
    pub(crate) fn mark_stale(&mut self) {
        self.loaded_at = None;
        self.load.reset();
    }

    // --- drawing -------------------------------------------------------------

    /// The page's rows at `width`. `selected` is the cursor while the page
    /// has focus.
    pub(crate) fn rows(&self, width: usize, focused: bool) -> Vec<PageRow> {
        let mut rows = Vec::new();
        let text = |rows: &mut Vec<PageRow>, s: &str, style: Style| {
            for line in shelbi_app::overlay::error_log::wrap(s, width.max(1)) {
                rows.push(PageRow::text(Line::from(Span::styled(line, style))));
            }
        };
        let items = self.items();
        let sel = |item: Item| focused && items.get(self.cursor) == Some(&item);
        let at = |item: Item| items.iter().position(|i| *i == item);
        let loaded = matches!(self.state.as_ref().map(|s| s.load()), Some(Load::Loaded(_)));
        // The Workspaces section's heading is the page's, as the desktop's.
        let new = Button::secondary("New workspace").focused(sel(Item::New));
        let buttons: &[Button] = if loaded { &[new] } else { &[] };
        for h in super::section_heading("Workspaces", Some(WORKSPACES_HELP), buttons, width) {
            rows.push(PageRow::heading(h, &[at(Item::New)]));
        }
        let Some(state) = &self.state else {
            rows.push(PageRow::blank());
            text(&mut rows, "No project is open.", Style::default().fg(palette().foreground));
            text(&mut rows, "Switch to a project to see its workspaces.", Style::default().fg(palette().muted));
            return rows;
        };
        if let Some(notice) = state.notice() {
            text(&mut rows, notice, Style::default().fg(palette().green));
        }
        if let Some(error) = state.error() {
            text(&mut rows, error, Style::default().fg(palette().red));
        }
        rows.push(PageRow::blank());
        match state.load() {
            Load::Loading => {
                rows.push(PageRow::text(Line::from(Span::styled(
                    "Loading…",
                    Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
                ))));
            }
            Load::Failed(e) => {
                // The desktop's failed-read panel: what failed, then Retry.
                rows.push(PageRow::text(Line::from(Span::styled(
                    "Couldn’t read this project’s workspaces",
                    Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
                ))));
                text(&mut rows, &e.to_string(), Style::default().fg(palette().red));
                text(&mut rows, "Press r, or choose Retry.", Style::default().fg(palette().muted));
                rows.push(PageRow::blank());
                let retry = Button::secondary("Retry").focused(sel(Item::Retry));
                for h in super::button_lines(&[retry], width, crate::button::Align::Left) {
                    rows.push(PageRow::heading(h, &[at(Item::Retry)]));
                }
            }
            Load::Loaded(index) => {
                let selected = (0..index.workspaces.len()).find(|&i| sel(Item::Workspace(i)));
                for line in workspace_table(&index.workspaces).selected(selected, true).lines(width) {
                    let item = line.row.and_then(|i| at(Item::Workspace(i)));
                    rows.push(PageRow::table(line, WORKSPACES_TABLE, item));
                }
                rows.push(PageRow::blank());
                text(&mut rows, WORKSPACES_FOOTNOTE, Style::default().fg(palette().muted));
                text(
                    &mut rows,
                    &format!("Slot: {} {}", form::SLOT_HELP, form::SLOT_AUTOMATIC_HELP),
                    Style::default().fg(palette().muted),
                );
                text(
                    &mut rows,
                    "+tag: inherited from the workspace’s machine.",
                    Style::default().fg(palette().muted),
                );
                rows.extend(super::section_rule(width).map(PageRow::text));
                let add = Button::secondary("Add machine").focused(sel(Item::AddMachine));
                for h in super::section_heading("Machines", Some(MACHINES_HELP), &[add], width) {
                    rows.push(PageRow::heading(h, &[at(Item::AddMachine)]));
                }
                rows.push(PageRow::blank());
                let statuses: Vec<MachineStatus> = index.machines.iter().map(|m| state.status(m)).collect();
                let selected = (0..index.machines.len()).find(|&i| sel(Item::Machine(i)));
                let lines = machine_table(&index.machines, &statuses).selected(selected, true).lines(width);
                let ends: Vec<bool> = (0..lines.len()).map(|n| lines.get(n + 1).map(|l| l.row) != Some(lines[n].row)).collect();
                for (line, ends_row) in lines.into_iter().zip(ends) {
                    let row = line.row;
                    let item = row.and_then(|i| at(Item::Machine(i)));
                    rows.push(PageRow::table(line, MACHINES_TABLE, item));
                    // Why a machine is unreachable, under its row's last line.
                    if let (Some(i), true) = (row, ends_row) {
                        if let MachineStatus::Unreachable(e) = &statuses[i] {
                            for l in shelbi_app::overlay::error_log::wrap(&format!("SSH couldn’t reach it: {e}"), width.saturating_sub(2).max(1)) {
                                rows.push(PageRow::item(
                                    Line::from(Span::styled(format!("  {l}"), Style::default().fg(palette().red))),
                                    item,
                                ));
                            }
                        }
                    }
                }
                rows.push(PageRow::blank());
                text(&mut rows, MACHINES_FOOTNOTE, Style::default().fg(palette().muted));
                text(
                    &mut rows,
                    "Reachable means SSH answered the status probe: it doesn’t check the Shelbi CLI, Git, or the \
                     project there. ⏎ or c on a machine configures it.",
                    Style::default().fg(palette().muted),
                );
            }
        }
        rows
    }

    /// Draw the layer (when up) over `main`.
    pub(crate) fn render_layer(&mut self, f: &mut Frame, main: Rect) {
        self.hits.clear();
        let Some(layer) = &self.layer else {
            return;
        };
        match layer {
            Layer::Confirm(c) => {
                let c = c.clone();
                self.render_confirm(f, main, &c);
            }
            Layer::Machine(m) => {
                let probe = self.configured_status(&m.form);
                let extra = machine_form::Extra {
                    elapsed: m.started.map(|at| at.elapsed()),
                    aside: m.aside.as_deref(),
                };
                let mut hits = Vec::new();
                machine_form::render(f, main, &m.form, probe.as_ref(), extra, &mut hits);
                self.hits = hits.into_iter().map(|(r, focus)| (r, Hit::Machine(focus))).collect();
            }
            Layer::Form { form, fresh } => {
                let (form, fresh) = (form.clone(), *fresh);
                self.render_form(f, main, &form, fresh);
                if let Some(c) = form.confirm() {
                    let c = c.clone();
                    self.render_confirm(f, main, &c);
                }
            }
        }
    }

    /// What the reachability probe last saw of the machine `form`
    /// configures, as the page shows it.
    fn configured_status(&self, form: &ReadinessForm) -> Option<MachineStatus> {
        let name = &form.configured()?.name;
        let row = self.index()?.machines.iter().find(|m| &m.name == name)?;
        Some(self.state.as_ref()?.status(row))
    }

    fn render_form(&mut self, f: &mut Frame, main: Rect, form: &WorkspaceForm, fresh: bool) {
        let width = 78.min(main.width.saturating_sub(2));
        if width < 24 || main.height < 6 {
            return;
        }
        let inner_w = width.saturating_sub(4) as usize;
        let errors = form.errors();
        let muted = Style::default().fg(palette().muted);
        let red = Style::default().fg(palette().red);
        let focus = form.focus();

        // Lay the content out as lines, each field its label over its box,
        // recording where each control sits for clicks.
        let mut out: FormLines<Hit> = FormLines::new(inner_w).with_rest(palette().input_well);
        out.heading(form.title(), &form.description());
        fn error(e: &Option<String>, red: Style) -> Option<(&str, Style)> {
            e.as_deref().map(|e| (e, red))
        }
        // Name and Machine side by side, as the desktop's.
        let on = focus == Focus::Name;
        let name = form.name();
        let field = Textbox::new(name).focused(on).invalid(errors.name.is_some()).selected(fresh && on);
        let field = if on && !fresh { field.caret_at_end() } else { field };
        let name_field = Field {
            label: "Name",
            textbox: field,
            focused: on,
            disabled: false,
            under: [error(&errors.name, red)].into_iter().flatten().collect(),
            hit: Hit::Control(Focus::Name),
        };
        // Machine: the picker (its menu under it while open), and what the
        // machine is.
        let on = focus == Focus::Machine;
        let detail = form.machine_choice().map(MachineChoice::detail).unwrap_or_else(|| "Not declared".into());
        let picker = Textbox::new(form.machine())
            .placeholder("Choose a machine")
            .focused(on)
            .invalid(errors.machine.is_some())
            .trail("▾");
        let mut under = vec![(detail.as_str(), muted)];
        under.extend(error(&errors.machine, red));
        if form.menu().is_some() {
            under.clear();
        }
        let machine_field = Field { label: "Machine", textbox: picker, focused: on, disabled: false, under, hit: Hit::Control(Focus::Machine) };
        out.pair(name_field, machine_field);
        if let Some(open) = form.menu() {
            // The menu drops under the picker's column.
            let (x, w) = modal_form::pair_right(inner_w).unwrap_or((0, inner_w));
            let first = out.lines.len();
            for (i, m) in form.machines().iter().enumerate() {
                let text = panel::truncate_right(&format!(" {}  {} ", m.name, m.detail()), w);
                let pad = w.saturating_sub(panel::display_width(&text));
                let style = if i == open {
                    Style::default().fg(palette().white).bg(palette().selection).add_modifier(Modifier::BOLD)
                } else if matches!(m.status, MachineStatus::Unreachable(_)) {
                    Style::default().fg(palette().red).bg(palette().input_well)
                } else {
                    Style::default().fg(palette().gray).bg(palette().input_well)
                };
                let line = out.lines.len();
                out.lines.push(Line::from(vec![
                    Span::raw(" ".repeat(x)),
                    Span::styled(format!("{text}{}", " ".repeat(pad)), style),
                ]));
                out.control(first, line, x, w, Hit::Option(i));
            }
            if let Some(e) = &errors.machine {
                out.text(e, red);
            }
        }
        out.blank();
        // Tags: the workspace's own; the machine's said under.
        let on = focus == Focus::Tags;
        let tags = form.tags();
        let field = Textbox::new(tags).placeholder("Optional tags").focused(on).invalid(errors.tags.is_some());
        let field = if on { field.caret_at_end() } else { field };
        let note = form.inherited_note();
        let mut under = vec![(form::TAGS_HELP, muted)];
        under.extend(note.as_deref().map(|n| (n, muted)));
        under.extend(error(&errors.tags, red));
        out.field("Tags", field, on, false, &under, Hit::Control(Focus::Tags));
        out.blank();
        // Worktree location: derived, read-only.
        let on = focus == Focus::Location;
        let location = form.location();
        let help = form.location_help();
        let help_style = if help == LOCATION_HELP { muted } else { Style::default().fg(palette().yellow) };
        let field = Textbox::new(&location).focused(on).disabled(true).clip_left(true).trail("read-only");
        out.field("Worktree location", field, on, true, &[(help.as_str(), help_style)], Hit::Control(Focus::Location));
        out.blank();
        // Slot: empty is Automatic.
        let on = focus == Focus::Slot;
        let slot = form.slot();
        let placeholder = form.slot_placeholder();
        let field = Textbox::new(slot).placeholder(&placeholder).focused(on).invalid(errors.slot.is_some());
        let field = if on { field.caret_at_end() } else { field };
        let mut under = vec![(form::SLOT_HELP, muted), (form::SLOT_AUTOMATIC_HELP, muted)];
        under.extend(error(&errors.slot, red));
        out.field("Slot (optional)", field, on, false, &under, Hit::Control(Focus::Slot));
        // The footer: a refusal, the rule, the buttons, then the key hint.
        out.footer();
        if let Some(e) = &errors.general {
            out.blank();
            out.text(e, red);
        }
        out.blank();
        out.rule();
        // Remove at the left (Edit), Cancel and the default action at the
        // right.
        let busy = form.busy();
        let mut left = Vec::new();
        if !form.is_new() {
            left.push((Button::danger("Remove workspace").focused(focus == Focus::Remove).enabled(!busy), Hit::Control(Focus::Remove)));
        }
        let submit = form.submit_label();
        let right = [
            (Button::secondary("Cancel").focused(focus == Focus::Cancel).enabled(!busy), Hit::Control(Focus::Cancel)),
            (Button::primary(submit).focused(focus == Focus::Submit).enabled(form.can_submit()), Hit::Control(Focus::Submit)),
        ];
        out.buttons_apart(&left, &right);
        let hint = if busy {
            "Working… (nothing else runs until this lands)".to_string()
        } else if form.menu().is_some() {
            "↑↓ choose · ⏎ pick · Esc close".to_string()
        } else {
            format!("Tab next · ⏎ {} · Esc cancel", submit.to_lowercase())
        };
        out.lines.push(Line::from(Span::styled(panel::truncate_right(&hint, inner_w), muted)));

        let height = (out.lines.len() as u16 + 2).min(main.height);
        let rect = Rect {
            x: main.x + (main.width - width) / 2,
            y: main.y + (main.height - height) / 2,
            width,
            height,
        };
        let content = super::overlay_panel(f, rect, main, None);
        // An open menu keeps its highlighted machine in view.
        let focus = match form.menu() {
            Some(open) => Hit::Option(open),
            None => Hit::Control(focus),
        };
        out.render(f, content, focus, &mut self.hits);
    }

    fn render_confirm(&mut self, f: &mut Frame, main: Rect, c: &RemoveConfirm) {
        let mut body = vec![(c.body(), Style::default().fg(palette().foreground))];
        if let Some(e) = c.error() {
            body.push((String::new(), Style::default()));
            body.push((e.to_string(), Style::default().fg(palette().red)));
        }
        let enabled = !c.busy();
        let buttons = [
            (Button::secondary("Cancel").focused(c.focus() == ConfirmFocus::Cancel).enabled(enabled), 0),
            (Button::danger("Remove workspace").focused(c.focus() == ConfirmFocus::Remove).enabled(enabled), 1),
        ];
        let hint = if c.busy() { "Removing…" } else { "Tab switch · ⏎ press · Esc cancel" };
        // The confirmation is modal over the form: its rects come last and win.
        for (r, i) in super::render_dialog(f, main, 64, &c.title(), &body, &buttons, hint) {
            self.hits.push((r, if i == 0 { Hit::ConfirmCancel } else { Hit::ConfirmRemove }));
        }
    }
}

/// One rendered row of the page: a line, and the selectable row it belongs
/// to (a workspace takes two lines).
/// The Workspaces section's description, as the desktop's.
const WORKSPACES_HELP: &str = "Give agents their own place to work, on this Mac or another machine.";

/// Under the workspaces table, as the desktop's.
const WORKSPACES_FOOTNOTE: &str = "Each workspace has its own worktree. Tags help Shelbi route tasks to the right workspace.";

/// The Machines section's description, as the desktop's.
const MACHINES_HELP: &str = "Choose where this project’s workspaces run.";

/// Under the machines table, as the desktop's.
const MACHINES_FOOTNOTE: &str = "Remote machines connect over SSH using your existing SSH configuration.";

pub(crate) struct PageRow {
    pub line: Line<'static>,
    pub item: Option<usize>,
    /// A table's line: its row, for the pointer's hover (`Row::table`).
    pub table_row: Option<HoverKey>,
    pub hover: Option<Line<'static>>,
    /// Buttons on it: `x`, `w`, the item each is.
    pub targets: Vec<(u16, u16, usize)>,
}

impl PageRow {
    fn text(line: Line<'static>) -> PageRow {
        PageRow { line, item: None, table_row: None, hover: None, targets: Vec::new() }
    }

    /// A line of a section heading, its buttons each one of `items`.
    fn heading(h: super::HeadingLine, items: &[Option<usize>]) -> PageRow {
        let targets: Vec<(u16, u16, usize)> =
            h.targets.iter().filter_map(|&(x, w, b)| items.get(b).copied().flatten().map(|i| (x, w, i))).collect();
        PageRow { item: targets.first().map(|t| t.2), targets, ..PageRow::text(h.line) }
    }

    /// Table `table`'s line, a line of selectable row `item`.
    fn table(l: TableLine, table: u8, item: Option<usize>) -> PageRow {
        PageRow {
            line: l.line,
            item,
            table_row: Some((table, l.row.unwrap_or(usize::MAX))),
            hover: l.hover,
            targets: Vec::new(),
        }
    }

    fn blank() -> PageRow {
        PageRow::text(Line::raw(""))
    }

    fn item(line: Line<'static>, item: Option<usize>) -> PageRow {
        PageRow { item, ..PageRow::text(line) }
    }
}

/// The project the form edits against and the machines it offers, from the
/// page's last read.
fn choices(state: Option<&WorkspacesState>) -> Option<(Arc<shelbi_core::Project>, Vec<MachineChoice>)> {
    let state = state?;
    let index = state.index()?;
    let source = index.source.as_ref()?.0.clone();
    let machines = index
        .machines
        .iter()
        .map(|m| MachineChoice::from_row(m, state.status(m)))
        .collect();
    Some((source, machines))
}

/// The form's key: the TUI's text editing (at the end of the text) and its
/// arrows, over the shared form's keys.
fn form_key(form: &mut WorkspaceForm, fresh: &mut bool, k: KeyEvent) -> Action {
    if form.confirm().is_some() {
        return match confirm_key(k) {
            Some(key) => form.key(key),
            None => Action::Stay,
        };
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let focus = form.focus();
    let editable = matches!(focus, Focus::Name | Focus::Tags | Focus::Slot) && form.menu().is_none();
    let shared = |form: &mut WorkspaceForm, key: Key, fresh: &mut bool| {
        *fresh &= !matches!(key, Key::Tab | Key::BackTab);
        form.key(key)
    };
    match k.code {
        KeyCode::Tab => shared(form, Key::Tab, fresh),
        KeyCode::BackTab => shared(form, Key::BackTab, fresh),
        KeyCode::Enter => shared(form, Key::Enter, fresh),
        KeyCode::Esc => shared(form, Key::Esc, fresh),
        // Between text fields ↑ / ↓ move like Shift-Tab / Tab; on the
        // machine they open and walk its picker.
        KeyCode::Up | KeyCode::Down => {
            let down = k.code == KeyCode::Down;
            if focus == Focus::Machine || form.menu().is_some() {
                shared(form, if down { Key::Down } else { Key::Up }, fresh)
            } else {
                shared(form, if down { Key::Tab } else { Key::BackTab }, fresh)
            }
        }
        // ← / → walk the buttons.
        KeyCode::Left | KeyCode::Right if matches!(focus, Focus::Remove | Focus::Cancel | Focus::Submit) => {
            shared(form, if k.code == KeyCode::Right { Key::Tab } else { Key::BackTab }, fresh)
        }
        _ if form.busy() => Action::Stay,
        KeyCode::Backspace if editable => {
            let mut text = field_text(form, focus);
            if *fresh && focus == Focus::Name {
                text.clear();
            } else {
                text.pop();
            }
            *fresh = false;
            set_field(form, focus, &text);
            Action::Stay
        }
        KeyCode::Char('u') if ctrl && editable => {
            *fresh = false;
            set_field(form, focus, "");
            Action::Stay
        }
        KeyCode::Char(c) if editable && !ctrl && !alt => {
            let mut text = if *fresh && focus == Focus::Name { String::new() } else { field_text(form, focus) };
            text.push(c);
            *fresh = false;
            set_field(form, focus, &text);
            Action::Stay
        }
        KeyCode::Char(' ') if !ctrl && !alt => form.key(Key::Space),
        _ => Action::Stay,
    }
}

fn field_text(form: &WorkspaceForm, focus: Focus) -> String {
    match focus {
        Focus::Name => form.name(),
        Focus::Tags => form.tags(),
        Focus::Slot => form.slot(),
        _ => "",
    }
    .to_string()
}

fn set_field(form: &mut WorkspaceForm, focus: Focus, text: &str) {
    match focus {
        Focus::Name => form.set_name(text),
        Focus::Tags => form.set_tags(text),
        Focus::Slot => form.set_slot(text),
        _ => {}
    }
}

/// The confirmation's keys (← / → walk its two buttons too).
fn confirm_key(k: KeyEvent) -> Option<Key> {
    match k.code {
        KeyCode::Tab | KeyCode::Left | KeyCode::Right => Some(Key::Tab),
        KeyCode::BackTab => Some(Key::BackTab),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Char(' ') => Some(Key::Space),
        _ => None,
    }
}

/// The page's tables, for the pointer's hover (`HoverKey`).
const WORKSPACES_TABLE: u8 = 0;
const MACHINES_TABLE: u8 = 1;

/// The workspaces table, as the desktop's: Workspace (its worktree under
/// the name), Machine (its kind under it), Tags, Slot. Tags go first when
/// it's narrow, then Slot, then Machine.
fn workspace_table(rows: &[WorkspaceRow]) -> Table {
    let mut table = Table::new(vec![
        Column::new("Workspace").min(12).max(48).fill(),
        Column::new("Machine").min(8).max(24).drop(1),
        Column::new("Tags").min(8).max(32).drop(3),
        Column::new("Slot").min(4).max(8).drop(2),
    ])
    .empty("No workspaces yet. New workspace adds one.");
    let muted = Style::default().fg(palette().muted);
    for w in rows {
        let name = Cell::text(w.name.clone(), Style::default().fg(palette().white).add_modifier(Modifier::BOLD))
            .and_end(w.path.clone(), muted);
        let machine = Cell::text(w.machine.clone(), Style::default().fg(palette().foreground))
            .and(kind_label(w.machine_kind), muted);
        // Own tags plain, inherited `+tag` muted, or No tags.
        let tags = if w.tags.is_empty() {
            Cell::text("No tags", muted)
        } else {
            let mut spans = Vec::new();
            for (i, tag) in w.tags.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                let (text, style) = if tag.inherited {
                    (format!("+{}", tag.name), muted)
                } else {
                    (tag.name.clone(), Style::default().fg(palette().cyan))
                };
                spans.push(Span::styled(text, style));
            }
            Cell::spans(spans)
        };
        let slot = if w.slot_explicit {
            Cell::text(w.slot.to_string(), Style::default().fg(palette().foreground))
        } else {
            Cell::text(format!("{} auto", w.slot), muted)
        };
        table.push(vec![name, machine, tags, slot]);
    }
    table
}

/// The machines table: Machine, SSH host (with the probe's status under a
/// remote's host), Project directory, Workspaces. The directory goes first
/// when it's narrow, then the count, then the host.
fn machine_table(rows: &[MachineRow], statuses: &[MachineStatus]) -> Table {
    let mut table = Table::new(vec![
        Column::new("Machine").min(12).max(32).fill(),
        Column::new("SSH host").min(8).max(28).drop(1),
        Column::new("Project directory").min(10).max(40).drop(3),
        Column::new("Workspaces").min(4).max(10).drop(2),
    ])
    .empty("No machines yet. Add machine adds one.");
    let muted = Style::default().fg(palette().muted);
    for (m, status) in rows.iter().zip(statuses) {
        let name = Cell::text(m.name.clone(), Style::default().fg(palette().white).add_modifier(Modifier::BOLD));
        let host = match &m.host {
            Some(host) => {
                let color = match status {
                    MachineStatus::Reachable => palette().green,
                    MachineStatus::Unreachable(_) => palette().red,
                    MachineStatus::Local => palette().gray,
                    MachineStatus::Checking => palette().muted,
                };
                Cell::text(host.clone(), Style::default().fg(palette().foreground))
                    .and(status.label(), Style::default().fg(color))
            }
            None => Cell::text(status.label(), muted),
        };
        let dir = Cell::end(m.work_dir.clone(), Style::default().fg(palette().foreground));
        table.push(vec![name, host, dir, Cell::text(m.workspaces.to_string(), Style::default().fg(palette().foreground))]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::pool::{AddWorkspace, RemoveWorkspace, UpdateWorkspace, WorkspaceChange, WorkspaceRemoved};
    use shelbi_core::{Reachability, WorkspaceSpec};
    use std::collections::HashMap;

    const PROJECT: &str = "my-project";

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A page with the shared fixture's pool loaded for [`PROJECT`].
    fn loaded() -> WorkspacesPage {
        let mut page = WorkspacesPage::default();
        page.set_home(Some("/Users/me".into()));
        let t = page.begin_load(Some(PROJECT), false).expect("a first read");
        page.loaded(&t, super::super::fixture_pool(PROJECT));
        page
    }

    fn type_text(page: &mut WorkspacesPage, text: &str) {
        for c in text.chars() {
            assert_eq!(page.layer_key(key(KeyCode::Char(c))), Ask::Stay);
        }
    }

    /// Tab to `focus` inside the open form.
    fn tab_to(page: &mut WorkspacesPage, focus: Focus) {
        for _ in 0..10 {
            if page.form().unwrap().focus() == focus {
                return;
            }
            page.layer_key(key(KeyCode::Tab));
        }
        panic!("never reached {focus:?}");
    }

    fn run(ask: Ask) -> (Ticket, form::Op) {
        match ask {
            Ask::Run(ticket, job) => {
                assert_eq!(job.project, PROJECT);
                (ticket, job.op)
            }
            other => panic!("expected a request, got {other:?}"),
        }
    }

    fn change(spec: WorkspaceSpec) -> WorkspaceChange {
        WorkspaceChange {
            workspace: spec,
            previous_name: None,
            previous_machine: None,
            slot: 0,
            worktree: PathBuf::new(),
            hooks: shelbi_core::pool::HooksOutcome::Skipped,
            warnings: Vec::new(),
        }
    }

    /// The page's rows as text, one line each.
    fn text(page: &WorkspacesPage, width: usize) -> String {
        page.rows(width, true)
            .iter()
            .map(|r| r.line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The page with its layer drawn into a `w`×`h` main area, as text.
    fn screen(page: &mut WorkspacesPage, w: u16, h: u16) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// A drawn layer read as prose: its frame dropped and every run of
    /// whitespace, line breaks included, one space. A hint wraps wherever
    /// the platform's wording puts its words (`this Mac` on macOS, `this
    /// computer` elsewhere), so a phrase is looked for here, not per line.
    fn prose(shown: &str) -> String {
        shown
            .chars()
            .map(|c| if matches!(c, '│' | '─' | '┌' | '┐' | '└' | '┘') { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn the_page_lists_workspaces_then_machines_with_derived_paths_tags_and_slots() {
        let page = loaded();
        assert_eq!(
            page.items(),
            [
                Item::New,
                Item::Workspace(0),
                Item::Workspace(1),
                Item::Workspace(2),
                Item::Workspace(3),
                Item::Workspace(4),
                Item::AddMachine,
                Item::Machine(0),
                Item::Machine(1),
            ]
        );
        let text = text(&page, 100);
        for want in [
            " New workspace ",
            "Workspace ",
            "│ Machine ",
            "│ Tags ",
            "│ Slot",
            "alpha",
            "~/Projects/my-project/.shelbi/wt/alpha",
            "/home/me/my-project/.shelbi/wt/charlie",
            "frontend",
            "+linux",
            "No tags",
            "0 auto",
            "from 0, in the order the project declares them",
            "Machines",
            "Add machine",
            "⏎ or c on a machine configures it",
            "│ SSH host ",
            "│ Project directory ",
            "│ Workspaces",
            "Checking…",
        ] {
            assert!(text.contains(want), "{want:?} in:\n{text}");
        }
        // A workspace's machine has its kind under it, as the desktop's.
        let lines: Vec<&str> = text.lines().collect();
        let at = |name: &str| lines.iter().position(|l| l.starts_with(&format!("  {name} "))).unwrap();
        assert!(lines[at("alpha")].contains("│ This Mac ") && lines[at("alpha") + 1].contains("│ Local "));
        assert!(lines[at("charlie")].contains("│ Build server ") && lines[at("charlie") + 1].contains("│ SSH "));
        // A machine: its host (local says so), its directory, its count.
        let mac = lines[at("This Mac")];
        assert!(mac.contains("│ Local ") && mac.contains("│ ~/Projects/my-project ") && mac.contains("│ 3 "), "{mac}");
        let build = lines[at("Build server")];
        assert!(build.contains("│ build-server ") && build.contains("│ 2 "), "{build}");
        // review's explicit slot shows bare; delta's own linux isn't marked.
        let review = text.lines().find(|l| l.contains("review") && l.contains("This Mac")).unwrap();
        assert!(review.trim_end().ends_with('7'), "{review}");
        let delta = text.lines().find(|l| l.contains("delta")).unwrap();
        assert!(delta.contains("gpu linux") && !delta.contains("+linux"), "{delta}");
    }

    #[test]
    fn the_selected_row_is_a_pill_over_both_its_lines_and_narrow_drops_columns() {
        let mut page = loaded();
        page.step(true); // Workspace(0), alpha
        let rows = page.rows(100, true);
        let alpha: Vec<&PageRow> = rows.iter().filter(|r| r.item == Some(1)).collect();
        assert_eq!(alpha.len(), 2, "name over its worktree");
        for r in &alpha {
            let text = r.line.to_string();
            assert!(text.starts_with('▐') && text.ends_with('▌'), "{text:?}");
            assert_eq!(r.line.spans[0].style.fg, Some(palette().selection), "the cap carries the fill");
            assert_eq!(crate::table::bg_at(&r.line, 10), Some(palette().selection));
            assert!(r.hover.is_none(), "the selection has no hover look");
        }
        // The other rows have one, for the pointer.
        let bravo = rows.iter().find(|r| r.item == Some(2)).unwrap();
        assert_eq!(crate::table::bg_at(bravo.hover.as_ref().unwrap(), 10), Some(palette().row_hover));
        // The header and its rule are table lines nothing lights.
        let header = rows.iter().find(|r| r.line.to_string().contains("Workspace ")).unwrap();
        assert_eq!(header.table_row.map(|k| k.1), Some(usize::MAX));
        // Narrow: Tags go, then Slot; the rest is cut, never wrapped.
        let narrow = text(&page, 40);
        assert!(!narrow.contains("│ Tags") && narrow.contains("│ Slot"), "{narrow}");
        let tiny = text(&page, 24);
        assert!(!tiny.contains("│ Slot") && tiny.contains("│ Machine"), "{tiny}");
        for w in 0..24 {
            for r in page.rows(w, true) {
                if r.table_row.is_some() {
                    assert!(crate::panel::display_width(&r.line.to_string()) <= w.max(1) + 2 * crate::table::BLEED);
                }
            }
        }
    }

    #[test]
    fn reachability_is_honest_and_never_hides_the_configuration() {
        let mut page = loaded();
        let key = shelbi_app::settings::workspaces::probe_key("Build server", "build-server");
        page.state_mut().unwrap().set_reachability(HashMap::from([(
            key.clone(),
            Reachability::Unreachable {
                error: "Connection timed out".into(),
            },
        )]));
        let text = text(&page, 100);
        assert!(text.contains("Unreachable"), "{text}");
        assert!(text.contains("SSH couldn’t reach it: Connection timed out"), "{text}");
        let build = text.lines().find(|l| l.starts_with("  Build server ")).unwrap();
        assert!(build.contains("│ /home/me/my-project ") && build.contains("│ 2 "), "the configuration stays:\n{text}");
        assert!(text.contains("Reachable means SSH answered"), "{text}");
        page.state_mut().unwrap().set_reachability(HashMap::from([(key, Reachability::Reachable)]));
        let text = self::text(&page, 100);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|l| l.starts_with("  Build server ")).unwrap();
        assert!(lines[at].contains("│ build-server ") && lines[at + 1].contains("│ Reachable "), "{text}");
    }

    #[test]
    fn loading_failure_and_no_project_say_what_to_do() {
        let mut page = WorkspacesPage::default();
        assert!(page.begin_load(None, false).is_none());
        assert!(text(&page, 80).contains("No project is open."));
        let t = page.begin_load(Some(PROJECT), false).unwrap();
        assert!(page.begin_load(Some(PROJECT), false).is_none(), "already reading");
        assert!(text(&page, 80).contains("Loading…"));
        assert!(page.items().is_empty());
        page.loaded(&t, Err("projects/my-project.yaml: bad indent".into()));
        let shown = text(&page, 80);
        assert!(shown.contains("Couldn’t read this project’s workspaces"), "{shown}");
        assert!(shown.contains("projects/my-project.yaml: bad indent") && shown.contains("Retry"), "{shown}");
        assert_eq!(page.items(), [Item::Retry]);
        assert_eq!(page.activate(0), Ask::Reload, "Retry reads again");
        let t = page.begin_load(Some(PROJECT), true).unwrap();
        assert!(text(&page, 80).contains("Loading…"), "a retry shows it's reading");
        page.loaded(&t, super::super::fixture_pool(PROJECT));
        assert_eq!(page.items().len(), 9);
        // An empty pool still offers New workspace and its machines.
        let mut empty = super::super::fixture_pool(PROJECT).unwrap();
        empty.workspaces.clear();
        let t = page.begin_load(Some(PROJECT), true).unwrap();
        page.loaded(&t, Ok(empty));
        assert!(text(&page, 80).contains("No workspaces yet"));
        assert_eq!(page.items()[..2], [Item::New, Item::AddMachine]);
    }

    #[test]
    fn a_read_for_a_project_left_behind_is_dropped() {
        let mut page = WorkspacesPage::default();
        let old = page.begin_load(Some("alpha"), false).unwrap();
        page.project_changed();
        let new = page.begin_load(Some(PROJECT), false).unwrap();
        assert!(!page.loaded(&old, super::super::fixture_pool("alpha")));
        assert_eq!(page.dropped(), 1);
        assert!(page.items().is_empty(), "still reading the new project");
        page.loaded(&new, super::super::fixture_pool(PROJECT));
        assert_eq!(page.state().unwrap().project(), PROJECT);
    }

    #[test]
    fn new_workspace_from_the_keyboard_sends_exactly_the_draft() {
        let mut page = loaded();
        assert_eq!(page.page_key(key(KeyCode::Char('n'))), Some(Ask::Stay));
        let form = page.form().expect("n opens New workspace");
        assert_eq!((form.title(), form.name(), form.machine()), ("New workspace", "echo", "This Mac"));
        // The suggestion is typed over, not appended to.
        type_text(&mut page, "gpu-box");
        assert_eq!(page.form().unwrap().name(), "gpu-box");
        assert_eq!(
            page.form().unwrap().location(),
            "~/Projects/my-project/.shelbi/wt/gpu-box",
            "the location follows the name"
        );
        tab_to(&mut page, Focus::Tags);
        // Case is kept: GPU and gpu are two tags.
        type_text(&mut page, "gpu GPU");
        tab_to(&mut page, Focus::Location);
        type_text(&mut page, "zzz");
        assert_eq!(page.form().unwrap().location(), "~/Projects/my-project/.shelbi/wt/gpu-box", "read-only");
        tab_to(&mut page, Focus::Slot);
        assert_eq!(page.form().unwrap().slot_placeholder(), "Automatic (3)", "This Mac hosts three before it");
        type_text(&mut page, "0");
        let (_, op) = run(page.layer_key(key(KeyCode::Enter)));
        assert_eq!(
            op,
            form::Op::Add(AddWorkspace {
                name: "gpu-box".into(),
                machine: Some("This Mac".into()),
                tags: vec!["gpu".into(), "GPU".into()],
                slot: Some(0),
            })
        );
        // Sent once: Enter and Esc wait for it, and typing doesn't change
        // the draft it sent.
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay);
        assert_eq!(page.layer_key(key(KeyCode::Esc)), Ask::Stay);
        type_text(&mut page, "9");
        assert!(page.form().unwrap().busy());
        assert_eq!(page.form().unwrap().slot(), "0");
    }

    #[test]
    fn a_paste_lands_on_the_focused_field_as_one_line() {
        let mut page = loaded();
        assert!(!page.paste("nowhere"), "no form, no field");
        page.page_key(key(KeyCode::Char('n')));
        // Over the suggested name, as typing would.
        assert!(page.paste("gpu-box\n"));
        assert_eq!(page.form().unwrap().name(), "gpu-box");
        tab_to(&mut page, Focus::Tags);
        assert!(page.paste("gpu\r\nweb"));
        assert_eq!(page.form().unwrap().tags(), "gpu web");
        tab_to(&mut page, Focus::Location);
        assert!(!page.paste("zzz"), "the location is read-only");
    }

    #[test]
    fn an_empty_slot_is_automatic_and_an_overflow_is_refused_beside_the_slot() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        tab_to(&mut page, Focus::Slot);
        type_text(&mut page, "4294967296");
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay, "nothing sent");
        let form = page.form().unwrap();
        assert!(form.errors().slot.as_deref().unwrap().contains("too large"), "{:?}", form.errors());
        assert_eq!(form.slot(), "4294967296", "the draft stays");
        assert_eq!(form.focus(), Focus::Slot);
        page.layer_key(key(KeyCode::Backspace));
        assert_eq!(page.form().unwrap().errors().slot, None, "editing clears it");
        type_text(&mut page, "5");
        let (_, op) = run(page.layer_key(key(KeyCode::Enter)));
        let form::Op::Add(req) = op else { panic!() };
        assert_eq!(req.slot, Some(u32::MAX), "the largest u32 is fine");

        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        tab_to(&mut page, Focus::Slot);
        type_text(&mut page, "x1");
        page.layer_key(key(KeyCode::Enter));
        assert!(page.form().unwrap().errors().slot.as_deref().unwrap().contains("whole number"));
        page.layer_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(page.form().unwrap().slot(), "", "Ctrl+U clears");
        let (_, op) = run(page.layer_key(key(KeyCode::Enter)));
        let form::Op::Add(req) = op else { panic!() };
        assert_eq!(req.slot, None, "empty is automatic");
    }

    #[test]
    fn names_are_checked_by_the_shared_rules_before_anything_is_sent() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        type_text(&mut page, "alpha");
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay);
        let errors = page.form().unwrap().errors().clone();
        assert!(errors.name.as_deref().unwrap().contains("already exists"), "{errors:?}");
        // Names are lowercase ids (the shared rule), and one whose slug is
        // taken collides.
        page.layer_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut page, "Alpha");
        page.layer_key(key(KeyCode::Enter));
        assert!(page.form().unwrap().errors().name.as_deref().unwrap().contains("Invalid workspace name"));
        page.layer_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut page, "alpha_");
        page.layer_key(key(KeyCode::Enter));
        assert!(page.form().unwrap().errors().name.as_deref().unwrap().contains("slug-collides"));
        // A tag with a comma inside can't be typed into one: commas split.
        page.layer_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut page, "zeta");
        tab_to(&mut page, Focus::Tags);
        type_text(&mut page, "a,b");
        let (_, op) = run(page.layer_key(key(KeyCode::Enter)));
        let form::Op::Add(req) = op else { panic!() };
        assert_eq!(req.tags, ["a", "b"]);
    }

    #[test]
    fn edit_shows_own_tags_and_the_machines_separately_and_sends_the_loaded_entry() {
        let mut page = loaded();
        // charlie: the fourth workspace row.
        let at = page.items().iter().position(|i| *i == Item::Workspace(3)).unwrap();
        page.cursor = at;
        assert_eq!(page.activate(at), Ask::Stay);
        let form = page.form().expect("Enter on a row edits it");
        assert_eq!(form.title(), "Edit workspace");
        assert_eq!(form.tags(), "", "the machine's linux isn't charlie's own");
        let note = form.inherited_note().unwrap();
        assert!(note.contains("linux") && note.contains("Build server"), "{note}");
        assert!(!form.can_submit(), "nothing changed yet");
        tab_to(&mut page, Focus::Slot);
        type_text(&mut page, "4");
        let (_, op) = run(page.layer_key(key(KeyCode::Enter)));
        let source = super::super::fixture_pool(PROJECT).unwrap().source.unwrap().0;
        assert_eq!(
            op,
            form::Op::Update(UpdateWorkspace {
                name: "charlie".into(),
                expected: Some(source.workspace("charlie").unwrap().clone()),
                new_name: "charlie".into(),
                machine: "Build server".into(),
                tags: Vec::new(),
                slot: Some(4),
            })
        );
    }

    #[test]
    fn the_machine_picker_reassigns_from_the_keyboard() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        tab_to(&mut page, Focus::Machine);
        page.layer_key(key(KeyCode::Down));
        assert_eq!(page.form().unwrap().menu(), Some(0), "↓ opens the picker on the current machine");
        page.layer_key(key(KeyCode::Down));
        page.layer_key(key(KeyCode::Enter));
        let form = page.form().unwrap();
        assert_eq!((form.machine(), form.menu()), ("Build server", None));
        assert_eq!(form.location(), "/home/me/my-project/.shelbi/wt/echo");
        assert_eq!(form.slot_placeholder(), "Automatic (2)");
        let shown = screen(&mut page, 90, 40);
        assert!(shown.contains("It also gets linux from Build server"), "{shown}");
    }

    #[test]
    fn cancel_and_esc_write_nothing_and_drop_the_draft() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        type_text(&mut page, "zeta");
        assert_eq!(page.layer_key(key(KeyCode::Esc)), Ask::Stay);
        assert!(!page.modal(), "Esc closes it");
        let at = page.items().iter().position(|i| *i == Item::Workspace(0)).unwrap();
        page.activate(at);
        tab_to(&mut page, Focus::Cancel);
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay, "Enter on Cancel cancels");
        assert!(!page.modal());
    }

    #[test]
    fn removal_confirms_with_cancel_first_and_a_refusal_keeps_everything() {
        let mut page = loaded();
        page.cursor = 1; // alpha
        page.page_key(key(KeyCode::Char('d')));
        let c = page.confirm().expect("d asks first");
        assert_eq!(c.title(), "Remove alpha?");
        assert!(c.body().contains("~/Projects/my-project/.shelbi/wt/alpha"), "{}", c.body());
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay, "Enter starts on Cancel");
        assert!(!page.modal());
        page.page_key(key(KeyCode::Delete));
        page.layer_key(key(KeyCode::Tab));
        let (ticket, op) = run(page.layer_key(key(KeyCode::Enter)));
        let source = super::super::fixture_pool(PROJECT).unwrap().source.unwrap().0;
        assert_eq!(
            op,
            form::Op::Remove(RemoveWorkspace {
                name: "alpha".into(),
                expected: Some(source.workspace("alpha").unwrap().clone()),
                force: false,
            }),
            "never forced"
        );
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay, "sent once");
        let landed = page.finished(
            &ticket,
            Err(PoolError::ActiveTask {
                workspace: "alpha".into(),
                task: "fix-login".into(),
            }),
        );
        assert_eq!(landed, Landed::default(), "nothing committed");
        let c = page.confirm().expect("a refusal stays up");
        assert!(c.error().unwrap().contains("fix-login") && c.error().unwrap().contains("nothing was removed"));
        let shown = screen(&mut page, 80, 30);
        assert!(shown.contains("Remove alpha?") && shown.contains("fix-login"), "{shown}");
        // A worktree with unpushed work is refused too; then a retry works.
        let (ticket, _) = run(page.layer_key(key(KeyCode::Enter)));
        page.finished(&ticket, Err(PoolError::Unsafe("workspace `alpha`'s worktree has 2 unpushed commits".into())));
        assert!(page.confirm().unwrap().error().unwrap().contains("unpushed"));
        let (ticket, _) = run(page.layer_key(key(KeyCode::Enter)));
        let landed = page.finished(
            &ticket,
            Ok(Outcome::Removed(WorkspaceRemoved {
                name: "alpha".into(),
                machine: "This Mac".into(),
                warnings: Vec::new(),
            })),
        );
        assert_eq!(landed.committed.as_deref(), Some(PROJECT));
        assert!(!page.modal());
        assert_eq!(page.state().unwrap().notice(), Some("Removed alpha."));
    }

    #[test]
    fn provisioning_and_hook_errors_keep_the_draft_and_allow_a_retry() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        tab_to(&mut page, Focus::Tags);
        type_text(&mut page, "web");
        let (ticket, first) = run(page.layer_key(key(KeyCode::Enter)));
        page.finished(
            &ticket,
            Err(PoolError::HooksMergeRequired {
                workspace: "echo".into(),
                message: "merge Shelbi's hooks into .claude/settings.local.json".into(),
            }),
        );
        let form = page.form().expect("the form stays");
        assert!(!form.busy());
        assert_eq!((form.name(), form.tags()), ("echo", "web"), "every value as typed");
        assert!(form.errors().general.as_deref().unwrap().contains("settings.local.json"));
        let shown = screen(&mut page, 90, 40);
        assert!(shown.contains("settings.local.json"), "{shown}");
        let (ticket, again) = run(page.layer_key(key(KeyCode::Enter)));
        assert_eq!(first, again, "the retry sends the same request");
        let spec = WorkspaceSpec {
            name: "echo".into(),
            machine: "This Mac".into(),
            tags: vec!["web".into()],
            slot: None,
        };
        let mut done = change(spec);
        done.warnings.push("couldn't remove the old worktree".into());
        let landed = page.finished(&ticket, Ok(Outcome::Added(done)));
        assert_eq!(landed.committed.as_deref(), Some(PROJECT));
        assert!(!page.modal());
        assert_eq!(page.state().unwrap().notice(), Some("Couldn't remove the old worktree."));
    }

    #[test]
    fn a_result_after_a_project_switch_is_dropped_but_still_reports_its_commit() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        let (ticket, _) = run(page.layer_key(key(KeyCode::Enter)));
        page.project_changed();
        assert!(!page.modal(), "the draft goes with the project");
        let spec = WorkspaceSpec {
            name: "echo".into(),
            machine: "This Mac".into(),
            tags: Vec::new(),
            slot: None,
        };
        let landed = page.finished(&ticket, Ok(Outcome::Added(change(spec))));
        assert_eq!(page.dropped(), 1);
        assert_eq!(landed.committed.as_deref(), Some(PROJECT), "the project it was for refreshes");
        assert!(page.state().is_none(), "nothing lands on the new project's page");
    }

    #[test]
    fn a_result_after_settings_closed_still_lands_on_the_page() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        let (ticket, _) = run(page.layer_key(key(KeyCode::Enter)));
        page.hidden();
        assert!(!page.modal());
        let spec = WorkspaceSpec {
            name: "echo".into(),
            machine: "This Mac".into(),
            tags: Vec::new(),
            slot: None,
        };
        let landed = page.finished(&ticket, Ok(Outcome::Added(change(spec))));
        assert_eq!(page.dropped(), 0);
        assert_eq!(landed.committed.as_deref(), Some(PROJECT));
        assert_eq!(page.state().unwrap().notice(), Some("Created echo."));
    }

    // --- the machine form (tui-settings-machine-forms) ----------------------

    fn item_at(page: &WorkspacesPage, item: Item) -> usize {
        page.items().iter().position(|i| *i == item).expect("listed")
    }

    /// Tab to `focus` inside the open machine form.
    fn tab_to_machine(page: &mut WorkspacesPage, focus: mform::Focus) {
        for _ in 0..10 {
            if page.machine_form().unwrap().focus() == focus {
                return;
            }
            page.layer_key(key(KeyCode::Tab));
        }
        panic!("never reached {focus:?}");
    }

    fn run_machine(ask: Ask) -> (Ticket, mform::Job) {
        match ask {
            Ask::RunMachine(ticket, job) => {
                assert_eq!(job.project, PROJECT);
                (ticket, *job)
            }
            other => panic!("expected a machine save, got {other:?}"),
        }
    }

    fn machine_change(name: &str, previous: Option<&str>, workspaces: &[&str]) -> MachineChange {
        MachineChange {
            machine: shelbi_core::Machine {
                name: name.into(),
                kind: shelbi_core::MachineKind::Ssh,
                work_dir: "/srv/p".into(),
                host: None,
                tags: Vec::new(),
                forward: None,
            },
            previous_name: previous.map(str::to_string),
            workspaces: workspaces.iter().map(|w| w.to_string()).collect(),
        }
    }

    #[test]
    fn machine_rows_open_the_form_with_the_machines_own_values() {
        let mut page = loaded();
        let text = text(&page, 100);
        assert!(text.contains(" Add machine "), "{text}");
        assert!(!text.contains("in your editor"), "{text}");
        assert_eq!(page.activate(item_at(&page, Item::AddMachine)), Ask::Stay);
        let form = page.machine_form().expect("Add machine opens the form");
        assert_eq!((form.title(), form.remote()), ("Add machine", true));
        for field in [mform::Field::Name, mform::Field::Host, mform::Field::WorkDir, mform::Field::Tags] {
            assert_eq!(form.text(field), "", "an empty draft");
        }
        page.layer_key(key(KeyCode::Esc));
        assert!(!page.modal());

        // Enter on the SSH machine: its values, as the page read them.
        page.activate(item_at(&page, Item::Machine(1)));
        let form = page.machine_form().unwrap();
        assert_eq!(form.title(), "Configure machine");
        assert_eq!(form.text(mform::Field::Name), "Build server");
        assert_eq!(form.text(mform::Field::Host), "build-server");
        assert_eq!(form.text(mform::Field::WorkDir), "/home/me/my-project", "a remote path as declared");
        assert_eq!(form.description(), "Build server runs 2 workspaces for this project.");
        page.layer_key(key(KeyCode::Esc));

        // `c` on the local machine: its directory `~`-shortened, no SSH host.
        page.cursor = item_at(&page, Item::Machine(0));
        assert_eq!(page.page_key(key(KeyCode::Char('c'))), Some(Ask::Stay));
        let form = page.machine_form().unwrap();
        assert!(!form.remote());
        assert_eq!(form.text(mform::Field::WorkDir), "~/Projects/my-project");
        assert_eq!(form.fields_shown(), [mform::Field::Name, mform::Field::WorkDir, mform::Field::Tags]);
        page.layer_key(key(KeyCode::Esc));
        // `m` from anywhere on the page; `c` only on a machine row.
        page.cursor = 0;
        assert_eq!(page.page_key(key(KeyCode::Char('c'))), None);
        page.page_key(key(KeyCode::Char('m')));
        assert_eq!(page.machine_form().unwrap().title(), "Add machine");
    }

    #[test]
    fn the_ssh_and_local_forms_show_their_own_fields() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('m')));
        let shown = screen(&mut page, 100, 44);
        let help = page.machine_form().unwrap().work_dir_help();
        assert!(help.contains("remote’s home"), "{help}");
        let read = prose(&shown);
        for want in ["Add machine", "Name", "SSH host", "Your existing SSH keys and settings are observed", "Project directory", &help, "Machine tags", "Connection", "Not checked", "Cancel"] {
            assert!(read.contains(want), "{want:?}:\n{shown}");
        }
        page.layer_key(key(KeyCode::Esc));
        page.activate(item_at(&page, Item::Machine(0)));
        let shown = screen(&mut page, 100, 44);
        let help = page.machine_form().unwrap().work_dir_help();
        assert!(help.contains("~ is your home"), "{help}");
        let read = prose(&shown);
        for want in ["Configure machine", "~/Projects/my-project", &help, "Save changes"] {
            assert!(read.contains(want), "{want:?}:\n{shown}");
        }
        for absent in ["SSH host", "Connection", "Not checked"] {
            assert!(!shown.contains(absent), "the local form has no {absent:?}:\n{shown}");
        }
    }

    #[test]
    fn add_machine_sends_exactly_the_draft_once_and_remote_paths_stay_remote() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('m')));
        type_text(&mut page, "gpu box");
        page.layer_key(key(KeyCode::Tab));
        type_text(&mut page, "gpu.lan");
        page.layer_key(key(KeyCode::Down)); // ↓ moves like Tab
        assert!(page.paste("~/src/my project\n"), "a path pastes onto the field");
        page.layer_key(key(KeyCode::Tab));
        type_text(&mut page, "cuda, linux");
        let (ticket, job) = run_machine(page.layer_key(key(KeyCode::Enter)));
        let mform::SaveRequest::Add(fields) = &job.request else { panic!("an add") };
        assert_eq!(fields.name, "gpu box");
        assert_eq!(fields.host.as_deref(), Some("gpu.lan"));
        assert_eq!(fields.work_dir, "~/src/my project", "a remote `~` is the remote's: never expanded here");
        assert_eq!(fields.tags, ["cuda", " linux"]);
        assert!(page.machine_form().unwrap().saving());
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay, "one save at a time");
        assert_eq!(page.layer_key(key(KeyCode::Esc)), Ask::Stay, "Cancel waits for the save");
        type_text(&mut page, "x");
        assert_eq!(page.machine_form().unwrap().text(mform::Field::Tags), "cuda, linux", "no edits while saving");
        assert!(screen(&mut page, 100, 44).contains("Saving…"));

        let landed = page.machine_finished(&ticket, &job, Ok(machine_change("gpu box", None, &[])));
        assert_eq!(landed.committed.as_deref(), Some(PROJECT));
        assert!(!page.modal(), "success closes the form");
        assert_eq!(page.state().unwrap().notice(), Some("Added gpu box."));
        assert_eq!(page.follow_machine.as_deref(), Some("gpu box"), "the cursor follows once the page rereads");
    }

    #[test]
    fn refusals_keep_the_draft_beside_their_field_and_a_retry_saves() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('m')));
        type_text(&mut page, "build server");
        let (ticket, job) = run_machine(page.layer_key(key(KeyCode::Enter)));
        let refused = PoolError::invalid(shelbi_core::pool::PoolField::Name, "machine `Build server` already exists");
        let landed = page.machine_finished(&ticket, &job, Err(refused));
        assert_eq!(landed.committed, None);
        let form = page.machine_form().expect("the draft stays");
        assert_eq!(form.text(mform::Field::Name), "build server");
        assert_eq!(form.focus(), mform::Focus::Field(mform::Field::Name));
        // Under its own field: beside SSH host on a wide form, so read it
        // where the two stack.
        let shown = prose(&screen(&mut page, 60, 60));
        assert!(shown.contains("Name ▄▄▄"), "{shown}");
        assert!(shown.contains("Machine `Build server` already exists."), "{shown}");

        // A lifecycle refusal shows over the buttons; the draft is kept.
        page.layer_key(key(KeyCode::Backspace));
        let (ticket, job) = run_machine(page.layer_key(key(KeyCode::Enter)));
        page.machine_finished(&ticket, &job, Err(PoolError::Unsafe("workspace `alpha` has a live session".into())));
        let shown = screen(&mut page, 100, 44);
        assert!(shown.contains("Workspace `alpha` has a live session."), "{shown}");
        assert_eq!(page.machine_form().unwrap().text(mform::Field::Name), "build serve");
        // Retry.
        let (ticket, job) = run_machine(page.layer_key(key(KeyCode::Enter)));
        page.machine_finished(&ticket, &job, Ok(machine_change("build serve", None, &[])));
        assert!(!page.modal());
    }

    #[test]
    fn cancel_writes_nothing_and_drops_the_draft() {
        let mut page = loaded();
        page.activate(item_at(&page, Item::Machine(1)));
        type_text(&mut page, "-2");
        tab_to_machine(&mut page, mform::Focus::Cancel);
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay);
        assert!(!page.modal(), "Cancel closes");
        page.activate(item_at(&page, Item::Machine(1)));
        assert_eq!(page.machine_form().unwrap().text(mform::Field::Name), "Build server", "a fresh draft");
        page.hidden();
        assert!(!page.modal(), "leaving the page drops the draft");
    }

    #[test]
    fn a_rename_says_its_workspaces_followed() {
        let mut page = loaded();
        page.activate(item_at(&page, Item::Machine(1)));
        page.layer_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut page, "GPU box");
        let (ticket, job) = run_machine(page.layer_key(key(KeyCode::Enter)));
        let mform::SaveRequest::Update(update) = &job.request else { panic!("an update") };
        assert_eq!(update.name, "Build server");
        assert_eq!(update.fields.name, "GPU box");
        assert_eq!(update.expected.as_ref().map(|m| m.host.as_deref()), Some(Some("build-server")));
        page.machine_finished(&ticket, &job, Ok(machine_change("GPU box", Some("Build server"), &["bravo", "review"])));
        assert_eq!(
            page.state().unwrap().notice(),
            Some("Renamed Build server to GPU box; its 2 workspaces moved with it.")
        );
    }

    #[test]
    fn the_probe_is_labeled_and_a_draft_starts_not_checked() {
        let mut page = loaded();
        let probe = shelbi_app::settings::workspaces::probe_key("Build server", "build-server");
        page.state_mut().unwrap().set_reachability(HashMap::from([(probe, Reachability::Reachable)]));
        page.activate(item_at(&page, Item::Machine(1)));
        let shown = screen(&mut page, 100, 64);
        assert!(shown.contains("Not checked"), "{shown}");
        assert!(shown.contains("Last status probe: build-server answered over SSH"), "{shown}");
        for never in ["Connection successful", "Ready", "Verified"] {
            assert!(!shown.contains(never), "nothing claims readiness: {never:?}\n{shown}");
        }
        // A new host was never probed: the old host's word goes.
        page.layer_key(key(KeyCode::Tab));
        type_text(&mut page, "-2");
        let shown = screen(&mut page, 100, 44);
        assert!(shown.contains("Not checked") && !shown.contains("Last status probe"), "{shown}");
        // A new draft has nothing probed at all.
        page.layer_key(key(KeyCode::Esc));
        page.page_key(key(KeyCode::Char('m')));
        assert!(!screen(&mut page, 100, 44).contains("Last status probe"));
    }

    #[test]
    fn machine_results_for_a_closed_form_or_another_project_are_handled_safely() {
        // Settings closed mid-save: the result still says what happened, and
        // a form opened since isn't answered by it.
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('m')));
        type_text(&mut page, "gpu");
        let (ticket, job) = run_machine(page.layer_key(key(KeyCode::Enter)));
        page.hidden();
        page.page_key(key(KeyCode::Char('m')));
        type_text(&mut page, "other");
        let landed = page.machine_finished(&ticket, &job, Ok(machine_change("gpu", None, &[])));
        assert_eq!(landed.committed.as_deref(), Some(PROJECT));
        assert_eq!(page.state().unwrap().notice(), Some("Added gpu."));
        assert_eq!(page.machine_form().unwrap().text(mform::Field::Name), "other", "the new draft is untouched");

        // A failure for a closed form says why on the page.
        page.layer_key(key(KeyCode::Enter));
        let (ticket, job) = (page.machine_run.begin(Some(PROJECT)), job);
        page.hidden();
        page.machine_finished(&ticket, &job, Err(PoolError::Persistence("disk full".into())));
        assert_eq!(page.state().unwrap().notice(), Some("Disk full."));

        // A project switch: dropped, though the commit is still reported.
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('m')));
        type_text(&mut page, "gpu");
        let (ticket, job) = run_machine(page.layer_key(key(KeyCode::Enter)));
        page.project_changed();
        let landed = page.machine_finished(&ticket, &job, Ok(machine_change("gpu", None, &[])));
        assert_eq!(page.dropped(), 1);
        assert_eq!(landed.committed.as_deref(), Some(PROJECT));
        assert!(!page.modal());
    }

    #[test]
    fn the_machine_form_draws_at_any_size_and_scrolls_to_the_focus() {
        let mut page = loaded();
        page.activate(item_at(&page, Item::Machine(1)));
        for (w, h) in [(1, 1), (10, 3), (20, 8), (30, 10), (50, 12), (200, 60)] {
            screen(&mut page, w, h);
        }
        // Narrow: labels stack over their fields.
        let shown = screen(&mut page, 34, 60);
        assert!(shown.contains("SSH host") && shown.contains("build-server"), "{shown}");
        // Short: the focused control stays in view.
        tab_to_machine(&mut page, mform::Focus::Submit);
        let shown = screen(&mut page, 90, 12);
        assert!(shown.contains("Save changes"), "{shown}");
        assert!(page.hits.iter().any(|(_, h)| *h == Hit::Machine(mform::Focus::Submit)));
        tab_to_machine(&mut page, mform::Focus::Field(mform::Field::Name));
        let shown = screen(&mut page, 90, 12);
        assert!(shown.contains("Build server"), "{shown}");
    }

    #[test]
    fn clicks_reach_the_machine_forms_controls() {
        let mut page = loaded();
        page.activate(item_at(&page, Item::Machine(1)));
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 44)).unwrap();
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        let click = |page: &mut WorkspacesPage, hit: Hit| {
            let (r, _) = *page.hits.iter().find(|(_, h)| *h == hit).expect("drawn");
            page.layer_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            })
        };
        click(&mut page, Hit::Machine(mform::Focus::Field(mform::Field::Tags)));
        assert_eq!(page.machine_form().unwrap().focus(), mform::Focus::Field(mform::Field::Tags));
        assert!(matches!(click(&mut page, Hit::Machine(mform::Focus::Submit)), Ask::RunMachine(..)));
        page.layer = None;
        page.activate(item_at(&page, Item::Machine(1)));
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        click(&mut page, Hit::Machine(mform::Focus::Cancel));
        assert!(!page.modal());
    }

    #[test]
    fn path_entry_edits_at_the_end_like_a_terminal_line() {
        let mut page = loaded();
        page.activate(item_at(&page, Item::Machine(1)));
        tab_to_machine(&mut page, mform::Focus::Field(mform::Field::WorkDir));
        page.layer_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(page.machine_form().unwrap().text(mform::Field::WorkDir), "/home/me/");
        type_text(&mut page, "other");
        page.layer_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        page.layer_key(key(KeyCode::Backspace));
        assert_eq!(page.machine_form().unwrap().text(mform::Field::WorkDir), "/home/me");
        page.layer_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(page.machine_form().unwrap().text(mform::Field::WorkDir), "");
    }

    #[test]
    fn a_reread_offers_a_newly_added_machine_to_an_open_form() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        let mut more = super::super::fixture_pool(PROJECT).unwrap();
        let mut p = (*more.source.take().unwrap().0).clone();
        p.machines.push(shelbi_core::Machine {
            name: "gpu".into(),
            kind: shelbi_core::MachineKind::Ssh,
            work_dir: "/srv/p".into(),
            host: None,
            tags: Vec::new(),
            forward: None,
        });
        let mut index = PoolIndex::build(&p, Some(std::path::Path::new("/Users/me")));
        index.file = more.file;
        let t = page.begin_load(Some(PROJECT), true).unwrap();
        assert!(page.loaded(&t, Ok(index)), "the pool changed");
        assert_eq!(page.form().unwrap().machines().len(), 3);
    }

    #[test]
    fn the_form_and_confirmation_draw_at_any_size() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        let shown = screen(&mut page, 90, 40);
        for want in ["New workspace", "Name", "echo", "Machine", "This Mac", "▾", "Worktree location", "read-only", "Automatic (3)", "Create workspace", "Cancel"] {
            assert!(shown.contains(want), "{want:?}:\n{shown}");
        }
        assert!(!shown.contains("Remove workspace"), "New has no Remove");
        for (w, h) in [(1, 1), (10, 3), (30, 8), (50, 12), (200, 60)] {
            screen(&mut page, w, h);
            let _ = text(&page, w as usize);
        }
        page.layer_key(key(KeyCode::Esc));
        page.cursor = 1;
        page.activate(1);
        let shown = screen(&mut page, 90, 40);
        assert!(shown.contains("Edit workspace") && shown.contains("Remove workspace"), "{shown}");
        tab_to(&mut page, Focus::Remove);
        page.layer_key(key(KeyCode::Enter));
        assert!(page.confirm().is_some(), "Remove workspace… asks over the form");
        for (w, h) in [(1, 1), (24, 6), (90, 40)] {
            screen(&mut page, w, h);
        }
        page.layer_key(key(KeyCode::Esc));
        assert!(page.form().is_some() && page.confirm().is_none(), "Esc goes back to the form");
    }

    /// The cells of `hit`'s rects on the last frame, top to bottom.
    fn hit_rows(page: &WorkspacesPage, hit: Hit) -> Vec<Rect> {
        let mut rects: Vec<Rect> = page.hits.iter().filter(|(_, h)| *h == hit).map(|(r, _)| *r).collect();
        rects.sort_by_key(|r| r.y);
        rects
    }

    #[test]
    fn a_fields_three_rows_take_its_click_and_its_state_shows_in_the_fill() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 60)).unwrap();
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        // Tags: its cap above, its text, its cap below, each the box's width.
        let rows = hit_rows(&page, Hit::Control(Focus::Tags));
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!((rows[1].y, rows[2].y), (rows[0].y + 1, rows[0].y + 2));
        assert!(rows.iter().all(|r| r.width == rows[0].width && r.width > 40));
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(rows[0].x, rows[0].y)].symbol(), "▄");
        assert_eq!(buf[(rows[2].x, rows[2].y)].symbol(), "▀");
        assert_eq!(buf[(rows[1].x + 1, rows[1].y)].bg, palette().input_well, "at rest, a well in the dialog");
        for row in &rows {
            tab_to(&mut page, Focus::Name);
            page.layer_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: row.x + row.width / 2,
                row: row.y,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(page.form().unwrap().focus(), Focus::Tags, "a click on row {}", row.y);
        }
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let tags = hit_rows(&page, Hit::Control(Focus::Tags));
        // (Its first cell is the caret, on the empty field.)
        assert_eq!(buf[(tags[1].x + 1, tags[1].y)].bg, palette().white, "the caret");
        assert_eq!(buf[(tags[1].right() - 1, tags[1].y)].bg, palette().input_focus, "focused");
        assert_eq!(buf[(tags[0].x, tags[0].y)].fg, palette().input_focus, "the caps take the fill");
        // Read-only: Worktree location, its text muted on the quiet fill.
        let location = hit_rows(&page, Hit::Control(Focus::Location));
        let cell = &buf[(location[1].x + 1, location[1].y)];
        assert_eq!((cell.bg, cell.fg), (palette().input_disabled, palette().muted));
        // Refused: the error fill, and why under it.
        tab_to(&mut page, Focus::Name);
        while !page.form().unwrap().name().is_empty() {
            page.layer_key(key(KeyCode::Backspace));
        }
        tab_to(&mut page, Focus::Submit);
        page.layer_key(key(KeyCode::Enter));
        let error = page.form().unwrap().errors().name.clone().expect("a refusal");
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let name = hit_rows(&page, Hit::Control(Focus::Name));
        assert_eq!(buf[(name[1].right() - 1, name[1].y)].bg, palette().input_invalid);
        let under: String = (name[2].x..name[2].right()).map(|x| buf[(x, name[2].y + 1)].symbol().to_string()).collect();
        assert!(error.starts_with(under.trim()), "{under:?} under the field: {error:?}");
    }

    #[test]
    fn name_and_ssh_host_sit_side_by_side_and_each_takes_its_own_clicks() {
        let mut page = loaded();
        page.activate(item_at(&page, Item::Machine(1)));
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 60)).unwrap();
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        let name = hit_rows(&page, Hit::Machine(mform::Focus::Field(mform::Field::Name)));
        let host = hit_rows(&page, Hit::Machine(mform::Focus::Field(mform::Field::Host)));
        assert_eq!((name.len(), host.len()), (3, 3));
        assert_eq!(name[0].y, host[0].y, "one row");
        assert!(name[0].right() < host[0].x, "a gap between: {name:?} {host:?}");
        for row in &host {
            tab_to_machine(&mut page, mform::Focus::Field(mform::Field::Name));
            page.layer_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: row.right() - 1,
                row: row.y,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(page.machine_form().unwrap().focus(), mform::Focus::Field(mform::Field::Host), "row {}", row.y);
        }
        // Narrow, they stack.
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 60)).unwrap();
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        let name = hit_rows(&page, Hit::Machine(mform::Focus::Field(mform::Field::Name)));
        let host = hit_rows(&page, Hit::Machine(mform::Focus::Field(mform::Field::Host)));
        assert!(host[0].y > name[2].y && host[0].x == name[0].x, "{name:?} {host:?}");
    }

    #[test]
    fn the_forms_read_in_light() {
        crate::theme::with_appearance(shelbi_app::theme::Appearance::Light, || {
            let mut page = loaded();
            page.page_key(key(KeyCode::Char('n')));
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 50)).unwrap();
            term.draw(|f| page.render_layer(f, f.area())).unwrap();
            crate::theme::assert_light_readable("workspace-form", term.backend().buffer());
            page.layer = None;
            page.activate(item_at(&page, Item::Machine(1)));
            term.draw(|f| page.render_layer(f, f.area())).unwrap();
            crate::theme::assert_light_readable("machine-form", term.backend().buffer());
        });
    }

    #[test]
    fn clicks_reach_the_controls_and_the_confirmation_is_modal() {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('n')));
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 40)).unwrap();
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        let click = |page: &mut WorkspacesPage, hit: Hit| {
            let (r, _) = *page.hits.iter().find(|(_, h)| *h == hit).expect("drawn");
            page.layer_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            })
        };
        click(&mut page, Hit::Control(Focus::Tags));
        assert_eq!(page.form().unwrap().focus(), Focus::Tags);
        click(&mut page, Hit::Control(Focus::Machine));
        assert_eq!(page.form().unwrap().menu(), Some(0));
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        click(&mut page, Hit::Option(1));
        assert_eq!(page.form().unwrap().machine(), "Build server");
        term.draw(|f| page.render_layer(f, f.area())).unwrap();
        assert!(matches!(click(&mut page, Hit::Control(Focus::Submit)), Ask::Run(..)));
    }

    // --- the connection check ------------------------------------------------

    use shelbi_app::settings::machine_check::{Reach, ReadinessIntent, ReadinessService, Record, StageKind, StageState};
    use shelbi_orchestrator::machine_readiness::fake::{self, installed, out, Remote, Script};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    /// The live readiness service over a scripted remote and hub fetch (hub
    /// version 0.9.0), its records kept in memory.
    fn readiness(script: &Arc<Script>, fetch: fake::Fetch) -> (ReadinessService, Arc<Mutex<Vec<Record>>>) {
        let records = Arc::new(Mutex::new(Vec::new()));
        let seen = records.clone();
        let service = shelbi_orchestrator::machine_readiness::service_with(
            fake::connector(script.clone()),
            Arc::new(fetch),
            Arc::new(move |r: &Record| seen.lock().unwrap().push(r.clone())),
            shelbi_orchestrator::machine::parse_version("0.9.0").unwrap(),
        );
        (service, records)
    }

    fn run_check(ask: Ask) -> CheckJob {
        match ask {
            Ask::RunCheck(job) => *job,
            other => panic!("expected a readiness request, got {other:?}"),
        }
    }

    /// Run `job` through `service` as the shell's thread would, and hand the
    /// outcome back.
    fn answer(page: &mut WorkspacesPage, job: &CheckJob, service: &ReadinessService) -> Option<String> {
        let outcome = service.run(&job.request, job.cancel.clone());
        page.check_finished(job.form, &job.request, outcome)
    }

    /// Add machine with `gpu` at `gpu.lan`, `/srv/proj`, its Test focused.
    fn gpu_draft() -> WorkspacesPage {
        let mut page = loaded();
        page.page_key(key(KeyCode::Char('m')));
        type_text(&mut page, "gpu");
        page.layer_key(key(KeyCode::Tab));
        type_text(&mut page, "gpu.lan");
        page.layer_key(key(KeyCode::Tab));
        type_text(&mut page, "/srv/proj");
        tab_to_machine(&mut page, mform::Focus::Test);
        page
    }

    fn missing_cli<'a>() -> Remote<'a> {
        Remote { path_bin: None, ..Remote::default() }
    }

    #[test]
    fn a_check_tests_the_unsaved_draft_and_lists_each_stage() {
        let mut page = gpu_draft();
        let before = prose(&screen(&mut page, 100, 50));
        for stage in ["SSH", "Platform", "Shelbi CLI", "Git", "Project directory"] {
            assert!(before.contains(stage), "{stage} is listed before a check: {before}");
        }
        assert!(before.contains("Not checked yet") && before.contains("Test connection"), "{before}");

        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        assert_eq!(job.request.intent, ReadinessIntent::Test);
        assert_eq!(
            (job.request.target.host.as_str(), job.request.target.work_dir.as_str(), job.request.target.record_as.as_deref()),
            ("gpu.lan", "/srv/proj", None),
            "exactly the draft as typed, and it isn't a saved machine"
        );
        assert!(page.checking());
        let running = prose(&screen(&mut page, 100, 50));
        for want in ["Checking…", "Connecting to gpu.lan", "Waiting for SSH", "Stop check"] {
            assert!(running.contains(want), "{want:?}: {running}");
        }
        assert_eq!(page.layer_key(key(KeyCode::Char('x'))), Ask::Stay, "the keys stay in the form");

        let script = Arc::new(Script::new(vec![Remote::default().reply()]));
        let (service, records) = readiness(&script, fake::Fetch::none());
        assert_eq!(answer(&mut page, &job, &service), None);
        assert!(!page.checking());
        let done = prose(&screen(&mut page, 100, 50));
        for want in [
            "✓ Connection successful",
            "✓ SSH Connected and logged in",
            "✓ Platform Linux x86_64",
            "✓ Shelbi CLI shelbi 0.9.2 at /usr/local/bin/shelbi",
            "✓ Git git version 2.43.0",
            "✓ Project directory Checkout root",
            "Test again",
        ] {
            assert!(done.contains(want), "{want:?}: {done}");
        }
        assert!(records.lock().unwrap().is_empty(), "an unsaved draft records nothing");
        assert!(page.machine_form().is_some() && page.state().unwrap().notice().is_none(), "nothing was saved");
        assert_eq!(script.hosts.lock().unwrap().as_slice(), ["gpu.lan"]);
    }

    #[test]
    fn each_failure_says_which_stage_and_what_to_do() {
        type Case<'a> = (std::io::Result<shelbi_orchestrator::machine::ExecOutput>, &'a str, StageKind, StageState, bool);
        let timed_out = || Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "command did not finish within 30s"));
        let cases: Vec<Case> = vec![
            (Ok(out(255, "", "gpu.lan: Permission denied (publickey).")), "SSH couldn’t log in", StageKind::Ssh, StageState::Failed, false),
            (timed_out(), "Connection timed out", StageKind::Ssh, StageState::Failed, false),
            (Ok(out(255, "", "ssh: connect to host gpu.lan port 22: Connection refused")), "Couldn’t connect", StageKind::Ssh, StageState::Failed, false),
            (Remote { uname: ("FreeBSD", "amd64"), path_bin: None, ..Remote::default() }.reply(), "Unsupported platform", StageKind::Platform, StageState::Attention, false),
            (missing_cli().reply(), "Shelbi CLI needed", StageKind::Cli, StageState::Attention, true),
            (
                Remote { path_bin: Some(("/usr/bin/shelbi", "shelbi 0.1.0")), ..Remote::default() }.reply(),
                "Shelbi CLI needed",
                StageKind::Cli,
                StageState::Attention,
                true,
            ),
            (Remote { git: None, dir: "unchecked", ..Remote::default() }.reply(), "Git needed", StageKind::Git, StageState::Attention, false),
            (Remote { dir: "missing", ..Remote::default() }.reply(), "Project directory needs attention", StageKind::Directory, StageState::Attention, false),
        ];
        let mut page = gpu_draft();
        for (reply, title, kind, state, install) in cases {
            let script = Arc::new(Script::new(vec![reply]));
            let (service, _) = readiness(&script, fake::Fetch::none());
            tab_to_machine(&mut page, mform::Focus::Test);
            let job = run_check(page.layer_key(key(KeyCode::Enter)));
            answer(&mut page, &job, &service);
            let form = page.machine_form().unwrap();
            assert_eq!(form.panel().title, title);
            let stage = form.stages().into_iter().find(|s| s.kind == kind).unwrap();
            assert_eq!(stage.state, state, "{title}");
            assert_eq!(form.offers_install(), install, "{title}");
            let shown = prose(&screen(&mut page, 100, 50));
            assert!(shown.contains(title), "{title}: {shown}");
            assert!(!shown.contains("Connection successful"), "{shown}");
            let offered = page.hits.iter().any(|(_, h)| *h == Hit::Machine(mform::Focus::InstallCli));
            assert_eq!(offered, install, "{title}: {shown}");
            if !install {
                assert!(!shown.contains("Install CLI"), "nothing points at a button that isn't there: {shown}");
            }
        }
    }

    #[test]
    fn install_cli_installs_rechecks_and_a_failure_can_be_retried() {
        let mut page = gpu_draft();
        let script = Arc::new(Script::new(vec![missing_cli().reply()]));
        let (service, _) = readiness(&script, fake::Fetch::none());
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        answer(&mut page, &job, &service);
        tab_to_machine(&mut page, mform::Focus::InstallCli);

        // The first install can't write ~/.shelbi/bin.
        script.push(vec![missing_cli().reply(), Ok(out(10, "", "mkdir: /home/me/.shelbi: Read-only file system"))]);
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        assert_eq!(job.request.intent, ReadinessIntent::InstallCli);
        let shown = prose(&screen(&mut page, 100, 50));
        assert!(shown.contains("Installing the Shelbi CLI…") && shown.contains("Installing on gpu.lan"), "{shown}");
        assert!(!shown.contains("Stop check"), "an install runs to its end: {shown}");
        answer(&mut page, &job, &service);
        let form = page.machine_form().unwrap();
        assert_eq!(form.panel().title, "Couldn’t install the CLI");
        assert!(form.panel().detail.contains("~/.shelbi/bin"));
        assert!(form.offers_install(), "it can be tried again");

        // Again, with no outbound network on the machine: copied from here.
        let after = Remote {
            path_bin: None,
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
            ..Remote::default()
        };
        let script = Arc::new(Script::new(vec![
            missing_cli().reply(),
            Ok(out(20, "", "curl: (6) Could not resolve host: github.com")),
            Ok(out(0, &installed("0.9.0"), "")),
            after.reply(),
        ]));
        let (service, _) = readiness(&script, fake::Fetch::serving(b"tarball".to_vec()));
        tab_to_machine(&mut page, mform::Focus::InstallCli);
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        answer(&mut page, &job, &service);
        let form = page.machine_form().unwrap();
        assert!(form.verified().is_some(), "checked again after the install");
        let shown = prose(&screen(&mut page, 100, 50));
        assert!(shown.contains("Connection successful"), "{shown}");
        assert!(shown.contains("Installed Shelbi 0.9.0 at ~/.shelbi/bin/shelbi (copied from"), "{shown}");
        assert_eq!(script.stdin.lock().unwrap()[0], b"tarball");
        assert_eq!(page.machine_form().unwrap().focus(), mform::Focus::Test, "Install CLI went; focus stays in the form");
    }

    #[test]
    fn stop_an_edit_or_closing_stops_the_check_and_drops_its_answer() {
        let script = Arc::new(Script::new(vec![Remote::default().reply()]));
        let (service, _) = readiness(&script, fake::Fetch::none());

        // Stop check.
        let mut page = gpu_draft();
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        assert!(!job.cancel.load(Ordering::Acquire));
        assert_eq!(page.layer_key(key(KeyCode::Enter)), Ask::Stay, "Enter on Stop check stops it");
        assert!(job.cancel.load(Ordering::Acquire), "its flag trips");
        assert!(prose(&screen(&mut page, 100, 50)).contains("Check stopped"));
        let outcome = service.run(&job.request, job.cancel.clone());
        assert_eq!(outcome.report().map(|r| r.reach), Some(Reach::Cancelled), "the check stopped");
        page.check_finished(job.form, &job.request, outcome);
        assert!(page.machine_form().unwrap().verified().is_none());

        // An edit to the host.
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        page.layer_key(key(KeyCode::BackTab)); // Tags
        page.layer_key(key(KeyCode::BackTab)); // Project directory
        page.layer_key(key(KeyCode::BackTab)); // SSH host
        type_text(&mut page, "2");
        assert!(job.cancel.load(Ordering::Acquire), "a new host stops the old host's check");
        assert!(!page.checking());
        page.check_finished(job.form, &job.request, mcheck::CheckOutcome::Checked(mcheck::samples::ready()));
        assert!(page.machine_form().unwrap().verified().is_none(), "the old host's answer is dropped");

        // Closing the form.
        tab_to_machine(&mut page, mform::Focus::Test);
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        page.layer_key(key(KeyCode::Esc));
        assert!(!page.modal() && job.cancel.load(Ordering::Acquire));
        assert_eq!(page.check_finished(job.form, &job.request, mcheck::CheckOutcome::Checked(mcheck::samples::ready())), None);
        assert_eq!(page.state().unwrap().notice(), None, "a closed form's check says nothing");
        page.page_key(key(KeyCode::Char('m')));
        assert_eq!(page.machine_form().unwrap().check(), &mcheck::Check::NotChecked, "a new form isn't answered");
    }

    #[test]
    fn an_install_outlives_its_form_and_says_what_it_did() {
        let installed_ready = || {
            let after = Remote {
                path_bin: None,
                home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
                ..Remote::default()
            };
            Arc::new(Script::new(vec![missing_cli().reply(), Ok(out(0, &installed("0.9.0"), "")), after.reply()]))
        };
        let needs_cli = |page: &mut WorkspacesPage| {
            let script = Arc::new(Script::new(vec![missing_cli().reply()]));
            let (service, _) = readiness(&script, fake::Fetch::none());
            tab_to_machine(page, mform::Focus::Test);
            let job = run_check(page.layer_key(key(KeyCode::Enter)));
            answer(page, &job, &service);
            tab_to_machine(page, mform::Focus::InstallCli);
            run_check(page.layer_key(key(KeyCode::Enter)))
        };

        // Closed while installing: the page says it continues, then what it did.
        let mut page = gpu_draft();
        let job = needs_cli(&mut page);
        page.layer_key(key(KeyCode::Esc));
        assert!(!job.cancel.load(Ordering::Acquire) || page.machine_form().is_none());
        let notice = page.state().unwrap().notice().unwrap().to_string();
        assert!(notice.starts_with("Nothing was saved. The Shelbi CLI install on gpu.lan keeps running"), "{notice}");
        let (service, _) = readiness(&installed_ready(), fake::Fetch::none());
        assert_eq!(answer(&mut page, &job, &service), None);
        let notice = page.state().unwrap().notice().unwrap().to_string();
        assert!(notice.starts_with("On gpu.lan: Installed Shelbi 0.9.0"), "{notice}");

        // Installed, then cancelled: the page says the CLI stays.
        let mut page = gpu_draft();
        let job = needs_cli(&mut page);
        answer(&mut page, &job, &service_for(installed_ready()));
        page.layer_key(key(KeyCode::Esc));
        let notice = page.state().unwrap().notice().unwrap().to_string();
        assert!(notice.starts_with("Nothing was saved. On gpu.lan: Installed Shelbi 0.9.0"), "{notice}");

        // The draft moved on while installing: the form says what it did.
        let mut page = gpu_draft();
        let job = needs_cli(&mut page);
        page.layer_key(key(KeyCode::BackTab));
        page.layer_key(key(KeyCode::BackTab));
        type_text(&mut page, "x"); // the directory changes
        answer(&mut page, &job, &service_for(installed_ready()));
        let (_, aside) = page.machine_layer().unwrap();
        assert!(aside.is_some_and(|a| a.starts_with("On gpu.lan: Installed")), "{aside:?}");
        assert_eq!(page.machine_form().unwrap().check(), &mcheck::Check::NotChecked, "the new directory isn't checked");
        assert!(prose(&screen(&mut page, 100, 50)).contains("On gpu.lan: Installed"));

        // The project switched: the status line says it.
        let mut page = gpu_draft();
        let job = needs_cli(&mut page);
        page.project_changed();
        let status = answer(&mut page, &job, &service_for(installed_ready())).expect("said on the status line");
        assert!(status.starts_with(&format!("{PROJECT}: On gpu.lan: Installed")), "{status}");
    }

    fn service_for(script: Arc<Script>) -> ReadinessService {
        readiness(&script, fake::Fetch::none()).0
    }

    #[test]
    fn a_save_records_only_the_check_that_verified_what_it_saved() {
        let mut page = gpu_draft();
        let script = Arc::new(Script::new(vec![Remote::default().reply()]));
        let (service, records) = readiness(&script, fake::Fetch::none());
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        answer(&mut page, &job, &service);
        tab_to_machine(&mut page, mform::Focus::Submit);
        let (ticket, save) = run_machine(page.layer_key(key(KeyCode::Enter)));
        let mut change = machine_change("gpu", None, &[]);
        change.machine.host = Some("gpu.lan".into());
        let landed = page.machine_finished(&ticket, &save, Ok(change));
        let record = landed.record.expect("the verified binary is recorded once saved");
        assert_eq!((record.machine.as_str(), record.host.as_str()), ("gpu", "gpu.lan"));
        assert_eq!(record.cli.path, "/usr/local/bin/shelbi");
        assert!(records.lock().unwrap().is_empty(), "the check itself recorded nothing");

        // Unverified: nothing to record.
        let mut page = gpu_draft();
        tab_to_machine(&mut page, mform::Focus::Submit);
        let (ticket, save) = run_machine(page.layer_key(key(KeyCode::Enter)));
        let landed = page.machine_finished(&ticket, &save, Ok(machine_change("gpu", None, &[])));
        assert_eq!(landed.record, None);

        // A configured machine checked at its saved host records at once.
        let mut page = loaded();
        page.activate(item_at(&page, Item::Machine(1)));
        tab_to_machine(&mut page, mform::Focus::Test);
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        assert_eq!(job.request.target.record_as.as_deref(), Some("Build server"));
        let script = Arc::new(Script::new(vec![Remote::default().reply()]));
        let (service, records) = readiness(&script, fake::Fetch::none());
        answer(&mut page, &job, &service);
        assert_eq!(records.lock().unwrap().len(), 1);
        assert_eq!(page.state().unwrap().notice(), None, "and saves nothing");
    }

    #[test]
    fn the_region_fits_a_narrow_terminal() {
        let mut page = gpu_draft();
        let job = run_check(page.layer_key(key(KeyCode::Enter)));
        let script = Arc::new(Script::new(vec![missing_cli().reply()]));
        answer(&mut page, &job, &service_for(script));
        let shown = screen(&mut page, 36, 60);
        let read = prose(&shown);
        for want in ["Shelbi CLI needed", "Install CLI", "Test again"] {
            assert!(read.contains(want), "{want:?}:\n{shown}");
        }
        // Every control is still reachable by click.
        assert!(page.hits.iter().any(|(_, h)| *h == Hit::Machine(mform::Focus::InstallCli)));
    }
}
