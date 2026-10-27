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
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use shelbi_app::reachability::{ReachabilityProber, REACHABILITY_BACKOFF_MAX, REACHABILITY_CADENCE};
use shelbi_app::settings::workspace_form::{
    self as form, Action, ConfirmFocus, Focus, Key, MachineChoice, Outcome, RemoveConfirm,
    WorkspaceForm, LOCATION_HELP,
};
use shelbi_app::settings::machine_form::{self as mform, MachineForm};
use shelbi_app::settings::workspaces::{
    kind_label, workspace_count, Intent, Load, MachineRow, MachineStatus, PoolIndex, WorkspaceRow,
    WorkspacesState,
};
use shelbi_app::settings::{Job, Ticket};
use shelbi_core::pool::{MachineChange, PoolError};

use super::machine_form;
use crate::panel;
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
}

/// What a finished request asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Landed {
    /// A change was committed to this project's config: refresh what shows
    /// it (the page, the sidebar) when it's the shown project.
    pub committed: Option<String>,
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
    Machine(Box<MachineForm>),
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
    pub(crate) fn machine_form(&self) -> Option<&MachineForm> {
        match &self.layer {
            Some(Layer::Machine(form)) => Some(form),
            _ => None,
        }
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
        self.layer = None;
        self.cursor = 0;
        self.follow = None;
        self.follow_machine = None;
        self.retarget_prober();
    }

    /// Settings closed, or another page shows: the form's draft goes, as the
    /// desktop's dialog does, and the prober stops. A request already sent
    /// still lands ([`Self::finished`]).
    pub(crate) fn hidden(&mut self) {
        self.layer = None;
        self.prober = None;
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
                    self.layer = Some(Layer::Machine(Box::new(MachineForm::add(&project))));
                }
                Ask::Stay
            }
            Intent::ConfigureMachine(name) => {
                let row = self.index().and_then(|i| i.machines.iter().find(|m| m.name == name)).cloned();
                match row {
                    Some(row) => {
                        let form = MachineForm::configure(&project, &row.config, row.workspaces, self.home.as_deref());
                        self.layer = Some(Layer::Machine(Box::new(form)));
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
            Some(Layer::Machine(form)) => {
                let action = machine_form::key(form, k);
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
        if let Some(Layer::Machine(form)) = self.layer.as_mut() {
            return machine_form::paste(form, pasted);
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
        if let (Some(Layer::Machine(form)), Hit::Machine(focus)) = (self.layer.as_mut(), hit) {
            let action = machine_form::click(form, focus);
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

    /// Carry out what the machine form said.
    fn act_machine(&mut self, action: mform::Action) -> Ask {
        match action {
            mform::Action::Cancel => {
                self.layer = None;
                Ask::Stay
            }
            mform::Action::Save(run, request) => {
                let Some(Layer::Machine(form)) = &self.layer else {
                    return Ask::Stay;
                };
                let project = form.project().to_string();
                let ticket = self.machine_run.begin(Some(&project));
                Ask::RunMachine(ticket, Box::new(mform::Job { project, run, request: *request }))
            }
            // The TUI has no folder chooser (the directory is typed) and no
            // connection check yet: the form never offers them.
            mform::Action::Stay | mform::Action::Browse | mform::Action::Test | mform::Action::InstallCli => {
                Ask::Stay
            }
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
            return Landed { committed };
        }
        let added = matches!(job.request, mform::SaveRequest::Add(_));
        // The ticket is current, so a form that's saving sent it; any other
        // form up now was opened after this one went.
        let change = match self.layer.as_mut() {
            Some(Layer::Machine(form)) if form.saving() => {
                let change = form.finish_save(job.run, result);
                if change.is_some() {
                    self.layer = None;
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
        if let Some(change) = change {
            self.note(mform::notice(&change, added));
            self.follow_machine = Some(change.machine.name.clone());
        }
        Landed { committed }
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
            return Landed { committed };
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
        Landed { committed }
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
        rows.push(PageRow::text(Line::from(Span::styled(
            "Workspaces",
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        ))));
        text(
            &mut rows,
            "Where this project’s agents work: each workspace is a worktree on one of its machines. \
             The Machines view shows them at work; this page is their configuration.",
            Style::default().fg(palette().muted),
        );
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
        let items = self.items();
        let sel = |item: Item| focused && items.get(self.cursor) == Some(&item);
        let at = |item: Item| items.iter().position(|i| *i == item);
        match state.load() {
            Load::Loading => {
                rows.push(PageRow::text(Line::from(Span::styled(
                    "Loading…",
                    Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
                ))));
            }
            Load::Failed(e) => {
                text(&mut rows, &format!("Couldn't read the workspaces: {e}"), Style::default().fg(palette().red));
                text(&mut rows, "Press r, or choose Try again.", Style::default().fg(palette().muted));
                rows.push(PageRow::blank());
                rows.push(PageRow::item(action_line("Try again", None, sel(Item::Retry), width), at(Item::Retry)));
            }
            Load::Loaded(index) => {
                rows.push(PageRow::item(
                    action_line("New workspace…", Some("n"), sel(Item::New), width),
                    at(Item::New),
                ));
                rows.push(PageRow::blank());
                if index.workspaces.is_empty() {
                    text(&mut rows, "No workspaces yet: New workspace adds one.", Style::default().fg(palette().muted));
                } else {
                    let cols = Columns::for_rows(&index.workspaces, width);
                    rows.push(PageRow::text(cols.header()));
                    for (i, w) in index.workspaces.iter().enumerate() {
                        let item = Item::Workspace(i);
                        let selected = sel(item);
                        for line in cols.workspace(w, selected) {
                            rows.push(PageRow::item(line, at(item)));
                        }
                    }
                }
                rows.push(PageRow::blank());
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
                rows.push(PageRow::blank());
                rows.push(PageRow::text(Line::from(Span::styled(
                    "Machines",
                    Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
                ))));
                rows.push(PageRow::item(
                    action_line("Add machine…", Some("m"), sel(Item::AddMachine), width),
                    at(Item::AddMachine),
                ));
                for (i, m) in index.machines.iter().enumerate() {
                    let item = Item::Machine(i);
                    for line in machine_lines(m, &state.status(m), sel(item), width) {
                        rows.push(PageRow::item(line, at(item)));
                    }
                }
                rows.push(PageRow::blank());
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
            Layer::Machine(form) => {
                let probe = self.configured_status(form);
                let mut hits = Vec::new();
                machine_form::render(f, main, form, probe.as_ref(), &mut hits);
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
    fn configured_status(&self, form: &MachineForm) -> Option<MachineStatus> {
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
        let label_w = 11usize.min(inner_w / 3);
        let value_w = inner_w.saturating_sub(label_w);
        let errors = form.errors();
        let wrap = |s: &str, w: usize| shelbi_app::overlay::error_log::wrap(s, w.max(1));

        // Lay the content out as lines, recording which line each control
        // sits on for clicks.
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut controls: Vec<(usize, u16, u16, Hit)> = Vec::new();
        let muted = Style::default().fg(palette().muted);
        let red = Style::default().fg(palette().red);
        for l in wrap(&form.description(), inner_w) {
            lines.push(Line::from(Span::styled(l, muted)));
        }
        lines.push(Line::raw(""));
        let under = |lines: &mut Vec<Line<'static>>, text: &str, style: Style| {
            for l in wrap(text, value_w) {
                lines.push(Line::from(vec![Span::raw(" ".repeat(label_w)), Span::styled(l, style)]));
            }
        };
        let box_w = value_w.saturating_sub(2).clamp(4, 40);
        // Name.
        let name_focus = form.focus() == Focus::Name;
        controls.push((lines.len(), label_w as u16, box_w as u16 + 2, Hit::Control(Focus::Name)));
        lines.push(field_line("Name", label_w, text_box(form.name(), box_w, name_focus, fresh && name_focus)));
        if let Some(e) = &errors.name {
            under(&mut lines, e, red);
        }
        // Machine: the picker, and what the machine is.
        let machine_focus = form.focus() == Focus::Machine;
        let detail = form.machine_choice().map(MachineChoice::detail).unwrap_or_else(|| "Not declared".into());
        let picker = format!(" {} ▾ ", form.machine());
        let picker_w = panel::display_width(&picker).min(value_w) as u16;
        controls.push((lines.len(), label_w as u16, picker_w, Hit::Control(Focus::Machine)));
        lines.push(field_line(
            "Machine",
            label_w,
            vec![
                Span::styled(panel::truncate_right(&picker, value_w), control_style(machine_focus)),
                Span::styled(format!("  {detail}"), muted),
            ],
        ));
        if let Some(open) = form.menu() {
            for (i, m) in form.machines().iter().enumerate() {
                let text = format!(" {}  {} ", m.name, m.detail());
                let text = panel::truncate_right(&text, value_w);
                let style = if i == open {
                    Style::default().fg(palette().white).bg(palette().selection).add_modifier(Modifier::BOLD)
                } else if matches!(m.status, MachineStatus::Unreachable(_)) {
                    Style::default().fg(palette().red)
                } else {
                    Style::default().fg(palette().gray)
                };
                controls.push((lines.len(), label_w as u16, panel::display_width(&text) as u16, Hit::Option(i)));
                lines.push(Line::from(vec![Span::raw(" ".repeat(label_w)), Span::styled(text, style)]));
            }
        }
        if let Some(e) = &errors.machine {
            under(&mut lines, e, red);
        }
        // Tags: the workspace's own; the machine's said under.
        let tags_focus = form.focus() == Focus::Tags;
        controls.push((lines.len(), label_w as u16, box_w as u16 + 2, Hit::Control(Focus::Tags)));
        lines.push(field_line("Tags", label_w, text_box(form.tags(), box_w, tags_focus, false)));
        under(&mut lines, form::TAGS_HELP, muted);
        if let Some(note) = form.inherited_note() {
            under(&mut lines, &note, muted);
        }
        if let Some(e) = &errors.tags {
            under(&mut lines, e, red);
        }
        // Worktree location: derived, read-only.
        let loc_focus = form.focus() == Focus::Location;
        controls.push((lines.len(), label_w as u16, value_w as u16, Hit::Control(Focus::Location)));
        let location = panel::truncate_left(&form.location(), value_w.saturating_sub(12).max(8));
        lines.push(field_line(
            "Worktree",
            label_w,
            vec![
                Span::styled(location, if loc_focus { control_style(true) } else { Style::default().fg(palette().gray) }),
                Span::styled("  read-only", muted),
            ],
        ));
        let help = form.location_help();
        under(&mut lines, &help, if help == LOCATION_HELP { muted } else { Style::default().fg(palette().yellow) });
        // Slot: empty is Automatic.
        let slot_focus = form.focus() == Focus::Slot;
        let slot_w = 12.min(box_w);
        controls.push((lines.len(), label_w as u16, slot_w as u16 + 2, Hit::Control(Focus::Slot)));
        let mut slot = text_box(form.slot(), slot_w, slot_focus, false);
        if form.slot().trim().is_empty() {
            slot.push(Span::styled(format!("  {}", form.slot_placeholder()), muted));
        }
        lines.push(field_line("Slot", label_w, slot));
        under(&mut lines, form::SLOT_HELP, muted);
        under(&mut lines, form::SLOT_AUTOMATIC_HELP, muted);
        if let Some(e) = &errors.slot {
            under(&mut lines, e, red);
        }
        if let Some(e) = &errors.general {
            lines.push(Line::raw(""));
            for l in wrap(e, inner_w) {
                lines.push(Line::from(Span::styled(l, red)));
            }
        }
        lines.push(Line::raw(""));
        // The buttons: Remove at the left (Edit), Cancel and the default
        // action at the right.
        let button_row = lines.len();
        let mut buttons: Vec<(Focus, String, Style)> = Vec::new();
        if !form.is_new() {
            buttons.push((Focus::Remove, " Remove workspace… ".into(), button_style(form.focus() == Focus::Remove, true, false)));
        }
        buttons.push((Focus::Cancel, " Cancel ".into(), button_style(form.focus() == Focus::Cancel, !form.busy(), false)));
        buttons.push((
            Focus::Submit,
            format!(" {} ", form.submit_label()),
            button_style(form.focus() == Focus::Submit, form.can_submit(), true),
        ));
        let total: usize = buttons.iter().map(|(_, t, _)| panel::display_width(t) + 1).sum();
        let mut spans = Vec::new();
        let mut x = 0usize;
        for (i, (focus, label, style)) in buttons.iter().enumerate() {
            if i == 0 && *focus == Focus::Remove {
                spans.push(Span::styled(label.clone(), *style));
                controls.push((button_row, x as u16, panel::display_width(label) as u16, Hit::Control(*focus)));
                x += panel::display_width(label);
                let gap = inner_w.saturating_sub(total).max(1);
                spans.push(Span::raw(" ".repeat(gap)));
                x += gap;
                continue;
            }
            if i == 0 {
                let gap = inner_w.saturating_sub(total);
                spans.push(Span::raw(" ".repeat(gap)));
                x += gap;
            }
            spans.push(Span::styled(label.clone(), *style));
            controls.push((button_row, x as u16, panel::display_width(label) as u16, Hit::Control(*focus)));
            x += panel::display_width(label);
            spans.push(Span::raw(" "));
            x += 1;
        }
        lines.push(Line::from(spans));
        let hint = if form.busy() {
            "Working… (nothing else runs until this lands)".to_string()
        } else if form.menu().is_some() {
            "↑↓ choose · ⏎ pick · Esc close".to_string()
        } else {
            format!("Tab next · ⏎ {} · Esc cancel", form.submit_label().to_lowercase())
        };
        lines.push(Line::from(Span::styled(hint, muted)));

        let height = (lines.len() as u16 + 2).min(main.height);
        let rect = Rect {
            x: main.x + (main.width - width) / 2,
            y: main.y + (main.height - height) / 2,
            width,
            height,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette().divider_active))
            .title(Span::styled(
                format!(" {} ", form.title()),
                Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let content = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
        // Keep the focused control in view when the terminal is short.
        let focus_line = controls
            .iter()
            .find(|(_, _, _, h)| *h == Hit::Control(form.focus()))
            .map(|(l, ..)| *l)
            .unwrap_or(0);
        let skip = if (focus_line as u16) < content.height {
            0
        } else {
            focus_line + 1 - content.height as usize
        };
        f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), content);
        for (line, x, w, hit) in controls {
            let Some(y) = (line as u16).checked_sub(skip as u16) else {
                continue;
            };
            if y < content.height {
                self.hits.push((Rect { x: content.x + x, y: content.y + y, width: w.max(1), height: 1 }, hit));
            }
        }
    }

    fn render_confirm(&mut self, f: &mut Frame, main: Rect, c: &RemoveConfirm) {
        let width = 64.min(main.width.saturating_sub(2));
        if width < 20 || main.height < 5 {
            return;
        }
        let inner_w = width.saturating_sub(4) as usize;
        let wrap = |s: &str| shelbi_app::overlay::error_log::wrap(s, inner_w.max(1));
        let mut lines: Vec<Line<'static>> = wrap(&c.body())
            .into_iter()
            .map(|l| Line::from(Span::styled(l, Style::default().fg(palette().foreground))))
            .collect();
        if let Some(e) = c.error() {
            lines.push(Line::raw(""));
            lines.extend(wrap(e).into_iter().map(|l| Line::from(Span::styled(l, Style::default().fg(palette().red)))));
        }
        lines.push(Line::raw(""));
        let cancel = " Cancel ".to_string();
        let remove = " Remove workspace ".to_string();
        let gap = inner_w.saturating_sub(panel::display_width(&cancel) + panel::display_width(&remove) + 1);
        let button_row = lines.len();
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(gap)),
            Span::styled(cancel.clone(), button_style(c.focus() == ConfirmFocus::Cancel, !c.busy(), false)),
            Span::raw(" "),
            Span::styled(remove.clone(), danger_style(c.focus() == ConfirmFocus::Remove, !c.busy())),
        ]));
        let hint = if c.busy() { "Removing…" } else { "Tab switch · ⏎ press · Esc cancel" };
        lines.push(Line::from(Span::styled(hint, Style::default().fg(palette().muted))));
        let height = (lines.len() as u16 + 2).min(main.height);
        let rect = Rect {
            x: main.x + (main.width - width) / 2,
            y: main.y + (main.height - height) / 2,
            width,
            height,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette().red))
            .title(Span::styled(
                format!(" {} ", c.title()),
                Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let content = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
        f.render_widget(Paragraph::new(lines), content);
        // The confirmation sits over the form: its rects come last and win.
        if (button_row as u16) < content.height {
            let y = content.y + button_row as u16;
            let x = content.x + gap as u16;
            let cw = panel::display_width(&cancel) as u16;
            self.hits.push((Rect { x, y, width: cw, height: 1 }, Hit::ConfirmCancel));
            self.hits.push((
                Rect { x: x + cw + 1, y, width: panel::display_width(&remove) as u16, height: 1 },
                Hit::ConfirmRemove,
            ));
        }
        // The confirmation is modal over the form: only its buttons answer.
        self.hits.retain(|(_, h)| matches!(h, Hit::ConfirmCancel | Hit::ConfirmRemove));
    }
}

/// One rendered row of the page: a line, and the selectable row it belongs
/// to (a workspace takes two lines).
pub(crate) struct PageRow {
    pub line: Line<'static>,
    pub item: Option<usize>,
}

impl PageRow {
    fn text(line: Line<'static>) -> PageRow {
        PageRow { line, item: None }
    }

    fn blank() -> PageRow {
        PageRow::text(Line::raw(""))
    }

    fn item(line: Line<'static>, item: Option<usize>) -> PageRow {
        PageRow { line, item }
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

/// `label` padded to the label column, then `value`.
pub(super) fn field_line(label: &str, label_w: usize, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{:<label_w$}", panel::truncate_right(label, label_w.saturating_sub(1))),
        Style::default().fg(palette().gray),
    )];
    spans.extend(value);
    Line::from(spans)
}

/// A one-line text box `[text▏   ]`: the caret at the end while focused,
/// the whole text selected while a suggestion waits to be typed over.
pub(super) fn text_box(text: &str, width: usize, focused: bool, selected: bool) -> Vec<Span<'static>> {
    let room = width.saturating_sub(1);
    let shown = panel::truncate_left(text, room);
    let caret = if focused && !selected { "▏" } else { "" };
    let pad = width.saturating_sub(panel::display_width(&shown) + panel::display_width(caret));
    let frame = Style::default().fg(if focused { palette().cyan } else { palette().ansi_dark_gray });
    let body = if selected {
        Style::default().fg(palette().white).bg(palette().selection)
    } else {
        Style::default().fg(palette().foreground)
    };
    vec![
        Span::styled("[", frame),
        Span::styled(shown, body),
        Span::styled(caret.to_string(), Style::default().fg(palette().cyan)),
        Span::raw(" ".repeat(pad)),
        Span::styled("]", frame),
    ]
}

/// A focusable non-text control (the picker).
fn control_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(palette().white).bg(palette().selection).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().cyan)
    }
}

/// A button: the selection fill while focused; dim while it can't run.
pub(super) fn button_style(focused: bool, enabled: bool, primary: bool) -> Style {
    if !enabled {
        return Style::default().fg(palette().muted).bg(palette().status_bar);
    }
    let style = Style::default().bg(palette().selection);
    match (focused, primary) {
        (true, _) => style.fg(palette().white).add_modifier(Modifier::BOLD | Modifier::REVERSED),
        (false, true) => style.fg(palette().cyan).add_modifier(Modifier::BOLD),
        (false, false) => style.fg(palette().gray),
    }
}

/// The confirmation's destructive button.
fn danger_style(focused: bool, enabled: bool) -> Style {
    if !enabled {
        return Style::default().fg(palette().muted).bg(palette().status_bar);
    }
    let style = Style::default().fg(palette().red).bg(palette().selection).add_modifier(Modifier::BOLD);
    if focused {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

/// An action row: `› label   note`, the selection fill when selected.
fn action_line(label: &str, note: Option<&str>, selected: bool, width: usize) -> Line<'static> {
    let marker = if selected { "› " } else { "  " };
    let mut text = format!("{marker}{}", panel::truncate_right(label, width.saturating_sub(2)));
    if let Some(note) = note {
        let room = width.saturating_sub(panel::display_width(&text) + 2);
        if room >= panel::display_width(note) {
            text.push_str("  ");
            text.push_str(note);
        }
    }
    fill(text, selected, Style::default().fg(palette().cyan), width)
}

/// `text` padded across `width` with the selection fill when `selected`.
fn fill(text: String, selected: bool, style: Style, width: usize) -> Line<'static> {
    if selected {
        let pad = width.saturating_sub(panel::display_width(&text));
        Line::from(Span::styled(
            format!("{text}{}", " ".repeat(pad)),
            Style::default().fg(palette().white).bg(palette().selection).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(text, style))
    }
}

/// The workspace table's column widths.
struct Columns {
    name: usize,
    machine: usize,
    tags: usize,
    slot: usize,
    width: usize,
}

impl Columns {
    fn for_rows(rows: &[WorkspaceRow], width: usize) -> Columns {
        let name = rows.iter().map(|w| panel::display_width(&w.name)).max().unwrap_or(0).clamp(4, 18) + 2;
        let machine = rows
            .iter()
            .map(|w| panel::display_width(&machine_cell(w)))
            .max()
            .unwrap_or(0)
            .clamp(7, 26)
            + 2;
        let slot = 8;
        let tags = width.saturating_sub(2 + name + machine + slot).max(4);
        Columns { name, machine, tags, slot, width }
    }

    fn header(&self) -> Line<'static> {
        let text = format!(
            "  {:<n$}{:<m$}{:<t$}{:<s$}",
            "Name",
            "Machine",
            "Tags",
            "Slot",
            n = self.name,
            m = self.machine,
            t = self.tags,
            s = self.slot
        );
        Line::from(Span::styled(
            panel::truncate_right(&text, self.width),
            Style::default().fg(palette().muted).add_modifier(Modifier::BOLD),
        ))
    }

    /// A workspace's two lines: its fields, then its worktree under the name.
    fn workspace(&self, w: &WorkspaceRow, selected: bool) -> Vec<Line<'static>> {
        let bg = |s: Style| if selected { s.bg(palette().selection) } else { s };
        let marker = if selected { "› " } else { "  " };
        let cell = |s: &str, w: usize| format!("{:<w$}", panel::truncate_right(s, w.saturating_sub(1)));
        let mut spans = vec![
            Span::styled(marker.to_string(), bg(Style::default().fg(palette().white))),
            Span::styled(
                cell(&w.name, self.name),
                bg(Style::default().fg(palette().white).add_modifier(Modifier::BOLD)),
            ),
            Span::styled(cell(&machine_cell(w), self.machine), bg(Style::default().fg(palette().foreground))),
        ];
        // Tags: own plain, inherited `+tag` muted, or No tags.
        let mut used = 0usize;
        if w.tags.is_empty() {
            let t = "No tags";
            spans.push(Span::styled(t.to_string(), bg(Style::default().fg(palette().muted))));
            used += t.len();
        } else {
            for (i, tag) in w.tags.iter().enumerate() {
                let text = format!("{}{}{}", if i > 0 { " " } else { "" }, if tag.inherited { "+" } else { "" }, tag.name);
                let tw = panel::display_width(&text);
                if used + tw + 1 > self.tags {
                    spans.push(Span::styled("…".to_string(), bg(Style::default().fg(palette().muted))));
                    used += 1;
                    break;
                }
                let style = if tag.inherited {
                    Style::default().fg(palette().muted)
                } else {
                    Style::default().fg(palette().cyan)
                };
                spans.push(Span::styled(text, bg(style)));
                used += tw;
            }
        }
        spans.push(Span::styled(" ".repeat(self.tags.saturating_sub(used)), bg(Style::default())));
        let slot = if w.slot_explicit { w.slot.to_string() } else { format!("{} auto", w.slot) };
        spans.push(Span::styled(
            cell(&slot, self.slot),
            bg(Style::default().fg(if w.slot_explicit { palette().foreground } else { palette().muted })),
        ));
        let first = pad_line(spans, self.width, selected);
        let path = format!("    {}", panel::truncate_left(&w.path, self.width.saturating_sub(4)));
        let second = pad_line(
            vec![Span::styled(path, bg(Style::default().fg(palette().muted)))],
            self.width,
            selected,
        );
        vec![first, second]
    }
}

/// A workspace's machine cell: `This Mac · Local`, `gpu · SSH`.
fn machine_cell(w: &WorkspaceRow) -> String {
    format!("{} · {}", w.machine, kind_label(w.machine_kind))
}

/// A machine's lines: name, kind and host, status; then its root directory
/// and workspace count; then why it's unreachable.
fn machine_lines(m: &MachineRow, status: &MachineStatus, selected: bool, width: usize) -> Vec<Line<'static>> {
    let bg = |s: Style| if selected { s.bg(palette().selection) } else { s };
    let marker = if selected { "› " } else { "  " };
    let kind = match &m.host {
        Some(host) => format!("SSH · {host}"),
        None => "Local".into(),
    };
    let status_color = match status {
        MachineStatus::Reachable => palette().green,
        MachineStatus::Unreachable(_) => palette().red,
        MachineStatus::Local => palette().gray,
        MachineStatus::Checking => palette().muted,
    };
    let mut spans = vec![
        Span::styled(marker.to_string(), bg(Style::default().fg(palette().white))),
        Span::styled(m.name.clone(), bg(Style::default().fg(palette().white).add_modifier(Modifier::BOLD))),
        Span::styled(format!("  {kind}"), bg(Style::default().fg(palette().foreground))),
    ];
    if m.host.is_some() {
        spans.push(Span::styled(format!("  {}", status.label()), bg(Style::default().fg(status_color))));
    }
    if selected {
        spans.push(Span::styled("  ⏎ configure".to_string(), bg(Style::default().fg(palette().muted))));
    }
    let mut out = vec![pad_line(spans, width, selected)];
    let detail = format!("    {} · {}", m.work_dir, workspace_count(m.workspaces));
    out.push(pad_line(
        vec![Span::styled(panel::truncate_right(&detail, width), bg(Style::default().fg(palette().muted)))],
        width,
        selected,
    ));
    if let MachineStatus::Unreachable(e) = status {
        let why = format!("    SSH couldn’t reach it: {e}");
        out.push(pad_line(
            vec![Span::styled(panel::truncate_right(&why, width), bg(Style::default().fg(palette().red)))],
            width,
            selected,
        ));
    }
    out
}

/// `spans` clipped to `width`, padded with the selection fill when
/// selected.
fn pad_line(spans: Vec<Span<'static>>, width: usize, selected: bool) -> Line<'static> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for s in spans {
        let w = panel::display_width(&s.content);
        if used + w > width {
            let room = width.saturating_sub(used);
            if room > 0 {
                out.push(Span::styled(panel::truncate_right(&s.content, room), s.style));
                used = width;
            }
            break;
        }
        used += w;
        out.push(s);
    }
    if selected && used < width {
        out.push(Span::styled(" ".repeat(width - used), Style::default().bg(palette().selection)));
    }
    Line::from(out)
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
            "New workspace…",
            "alpha",
            "This Mac · Local",
            "Build server · SSH",
            "~/Projects/my-project/.shelbi/wt/alpha",
            "/home/me/my-project/.shelbi/wt/charlie",
            "frontend",
            "+linux",
            "No tags",
            "0 auto",
            "from 0, in the order the project declares them",
            "Machines",
            "Add machine…",
            "⏎ or c on a machine configures it",
            "SSH · build-server",
            "Checking…",
            "3 workspaces",
            "2 workspaces",
        ] {
            assert!(text.contains(want), "{want:?} in:\n{text}");
        }
        // review's explicit slot shows bare; delta's own linux isn't marked.
        let review = text.lines().find(|l| l.contains("review") && l.contains("This Mac")).unwrap();
        assert!(review.trim_end().ends_with('7'), "{review}");
        let delta = text.lines().find(|l| l.contains("delta")).unwrap();
        assert!(delta.contains("gpu linux") && !delta.contains("+linux"), "{delta}");
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
        assert!(text.contains("/home/me/my-project · 2 workspaces"), "the configuration stays:\n{text}");
        assert!(text.contains("Reachable means SSH answered"), "{text}");
        page.state_mut().unwrap().set_reachability(HashMap::from([(key, Reachability::Reachable)]));
        assert!(self::text(&page, 100).contains("SSH · build-server  Reachable"));
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
        assert!(shown.contains("Couldn't read the workspaces: projects/my-project.yaml: bad indent"), "{shown}");
        assert_eq!(page.items(), [Item::Retry]);
        assert_eq!(page.activate(0), Ask::Reload, "Try again reads again");
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
        assert!(text.contains("Add machine…  m"), "{text}");
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
        for want in ["Add machine", "Kind", "SSH", "Name", "SSH host", "never asks for a password", "Project directory", "remote’s home", "Machine tags", "Connection", "Not checked", "Cancel"] {
            assert!(shown.contains(want), "{want:?}:\n{shown}");
        }
        page.layer_key(key(KeyCode::Esc));
        page.activate(item_at(&page, Item::Machine(0)));
        let shown = screen(&mut page, 100, 44);
        for want in ["Configure machine", &format!("Local ({})", shelbi_app::settings::workspaces::LOCAL_LABEL), "~/Projects/my-project", "~ is your home", "Save changes"] {
            assert!(shown.contains(want), "{want:?}:\n{shown}");
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
        let shown = screen(&mut page, 100, 44);
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
        let shown = screen(&mut page, 100, 44);
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
        for want in ["New workspace", "Name", "echo", "Machine", "This Mac ▾", "Worktree", "read-only", "Automatic (3)", "Create workspace", "Cancel"] {
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
}
