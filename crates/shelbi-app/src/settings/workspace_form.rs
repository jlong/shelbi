//! The workspace dialogs' state, toolkit-free (`dt-workspace-settings-forms`,
//! shared with the TUI by `tui-settings-workspaces`): New workspace and Edit
//! workspace (Figma 2528:9683 and 2526:9184), and the confirmation that
//! removes one.
//!
//! [`WorkspaceForm`] holds the draft (the name, machine, tags and slot as
//! typed), the machines to pick from, what each field's last check said, the
//! keyboard focus, and whether a request is out. Everything a renderer would
//! otherwise decide lives here: the derived worktree location
//! (`workspace::workspace_worktree`, `~`-shortened when local), the slot's
//! Automatic placeholder (the shared resolver, [`Project::workspace_slot`],
//! over the draft), the machine's inherited tags, the Tab order, what each
//! key does, and which request a submit sends.
//!
//! A submit checks the draft with the service's own planners
//! ([`pool::plan_workspace_add`], [`pool::plan_workspace_update`],
//! [`pool::parse_slot`]) against the project as the page last read it,
//! and only then hands back a [`Job`]; the service checks it all again
//! against the config as it is when it runs. While the job is out the form
//! is busy: another submit, Cancel and Esc do nothing. Its result closes the
//! form on success; an error leaves every value as typed and shows beside
//! the field it names, or under the fields when it names none (a refusal, a
//! conflict, a machine that can't be reached).
//!
//! A [`Job`] carries the project the form was opened for, so a result can
//! never land in another one: the window drops the dialog when it switches
//! projects, and the job it already sent finishes against the project it
//! was meant for.
//!
//! [`RemoveConfirm`] is the removal's confirmation, from Edit's Remove
//! workspace or the row's menu: it names the workspace, its worktree and
//! machine, and what removal does. The service re-checks the workspace's
//! active task and its worktree; a refusal shows in the confirmation and
//! nothing is forced.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use shelbi_core::{MachineKind, Project, WorkspaceSpec};
use shelbi_core::pool::{
    self, workspace_worktree, AddWorkspace, PoolError, PoolField, RemoveWorkspace, UpdateWorkspace,
    WorkspaceChange, WorkspaceRemoved,
};

use super::workspaces::{tilde, MachineRow, MachineStatus, LOCAL_LABEL};

/// The names a new workspace is offered, in order: the first one the project
/// doesn't use yet.
const SUGGESTED: [&str; 26] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliett", "kilo", "lima",
    "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango", "uniform", "victor", "whiskey",
    "xray", "yankee", "zulu",
];

/// The Worktree location's helper while the path is the one the workspace
/// has.
pub const LOCATION_HELP: &str = "Determined by the machine’s project directory and workspace name.";
/// The Tags field's helper.
pub const TAGS_HELP: &str = "Tags help workflows find a workspace with the right capabilities.";
/// The Slot field's helper.
pub const SLOT_HELP: &str = "Used by workflows for per-workspace ports. Leave empty for automatic numbering.";
/// What automatic numbering is, where there's room to say it (the TUI's
/// form): the fallback [`Project::workspace_slot`] resolves an empty slot to.
pub const SLOT_AUTOMATIC_HELP: &str =
    "Automatic numbers a machine’s workspaces from 0, in the order the project declares them.";

/// Which dialog the form is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    New,
    /// Editing the workspace as the page read it (a save whose workspace
    /// changed since is a conflict, not an overwrite).
    Edit { original: WorkspaceSpec },
}

/// A machine the workspace can run on, as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineChoice {
    pub name: String,
    pub kind: MachineKind,
    /// The host SSH connects to; `None` when local.
    pub host: Option<String>,
    /// Its badge on the page: Local, or the remote's reachability.
    pub status: MachineStatus,
}

impl MachineChoice {
    pub fn from_row(row: &MachineRow, status: MachineStatus) -> Self {
        MachineChoice {
            name: row.name.clone(),
            kind: row.kind,
            host: row.host.clone(),
            status,
        }
    }

    /// What it is, under its name in the picker: [`LOCAL_LABEL`], or `SSH ·
    /// <host> · Reachable`.
    pub fn detail(&self) -> String {
        match (&self.host, &self.status) {
            (None, _) => LOCAL_LABEL.into(),
            (Some(host), status) => format!("SSH · {host} · {}", status.label()),
        }
    }
}

/// A focusable control, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Name,
    Machine,
    Tags,
    /// The derived location: read-only, but focusable so it can be selected
    /// and copied.
    Location,
    Slot,
    /// Edit only.
    Remove,
    Cancel,
    Submit,
}

impl Focus {
    /// A text field (its own keys: Space and the arrows are its).
    pub fn is_text(self) -> bool {
        matches!(self, Focus::Name | Focus::Tags | Focus::Location | Focus::Slot)
    }
}

/// A key the dialogs take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Tab,
    BackTab,
    Enter,
    Esc,
    Space,
    Up,
    Down,
}

/// A request to the shared service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Add(AddWorkspace),
    Update(UpdateWorkspace),
    Remove(RemoveWorkspace),
}

/// [`Op`] for the project the dialog was opened for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub project: String,
    pub op: Op,
}

/// What a [`Job`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Added(WorkspaceChange),
    Updated(WorkspaceChange),
    Removed(WorkspaceRemoved),
}

impl Outcome {
    /// The workspace's name afterwards (`None` once removed).
    pub fn workspace(&self) -> Option<&str> {
        match self {
            Outcome::Added(c) | Outcome::Updated(c) => Some(&c.workspace.name),
            Outcome::Removed(_) => None,
        }
    }

    /// Whatever didn't fail the change but should be said (an old worktree
    /// left in place).
    pub fn warnings(&self) -> &[String] {
        match self {
            Outcome::Added(c) | Outcome::Updated(c) => &c.warnings,
            Outcome::Removed(r) => &r.warnings,
        }
    }

    /// One line saying what happened, for a page's notice: the warnings
    /// when there are any, else what was done.
    pub fn notice(&self) -> String {
        if !self.warnings().is_empty() {
            return self.warnings().iter().map(|w| sentence(w)).collect::<Vec<_>>().join(" ");
        }
        match self {
            Outcome::Added(c) => format!("Created {}.", c.workspace.name),
            Outcome::Updated(c) => match &c.previous_name {
                Some(old) => format!("Renamed {old} to {}.", c.workspace.name),
                None => format!("Saved {}.", c.workspace.name),
            },
            Outcome::Removed(r) => format!("Removed {}.", r.name),
        }
    }
}

/// What the dialog's owner does after a key, a click or a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Stay,
    /// Send this to the service, off the UI thread.
    Run(Job),
    /// Close, discarding the draft (Cancel, Esc).
    Close,
    /// The job succeeded: close, then reread the pool.
    Done(Outcome),
}

/// The fields' messages from the last check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Errors {
    pub name: Option<String>,
    pub machine: Option<String>,
    pub tags: Option<String>,
    pub slot: Option<String>,
    /// One that names no field: a refusal, a conflict, an unreachable
    /// machine, a write that failed.
    pub general: Option<String>,
}

impl Errors {
    fn is_empty(&self) -> bool {
        *self == Errors::default()
    }

    /// File `e` beside its field, or under the fields.
    fn add(&mut self, e: &PoolError) {
        let text = sentence(&e.to_string());
        let slot = match e {
            PoolError::Invalid { field: Some(PoolField::Name), .. } => &mut self.name,
            PoolError::Invalid { field: Some(PoolField::Machine), .. } => &mut self.machine,
            PoolError::Invalid { field: Some(PoolField::Tags), .. } => &mut self.tags,
            PoolError::Invalid { field: Some(PoolField::Slot), .. } => &mut self.slot,
            _ => &mut self.general,
        };
        if slot.is_none() {
            *slot = Some(text);
        }
    }

    /// The first field in Tab order with a message.
    fn first(&self) -> Option<Focus> {
        [
            (Focus::Name, &self.name),
            (Focus::Machine, &self.machine),
            (Focus::Tags, &self.tags),
            (Focus::Slot, &self.slot),
        ]
        .into_iter()
        .find_map(|(f, e)| e.is_some().then_some(f))
    }
}

/// `text` as a sentence: its first letter capitalized, a full stop at the
/// end. The service's messages read as clauses (`workspace `x` already
/// exists`).
pub fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let mut out = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Tags as typed (commas or spaces between them) → the list to send.
pub fn split_tags(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// `path` as the dialogs show it: `~`-shortened on this Mac, as is on a
/// remote.
fn shown_path(kind: MachineKind, path: &Path, home: Option<&Path>) -> String {
    match kind {
        MachineKind::Local => tilde(path, home),
        MachineKind::Ssh => path.display().to_string(),
    }
}

/// The New / Edit workspace form.
#[derive(Debug, Clone)]
pub struct WorkspaceForm {
    project: String,
    /// The project as the page last read it.
    source: Arc<Project>,
    home: Option<PathBuf>,
    mode: Mode,
    name: String,
    machine: String,
    tags: String,
    slot: String,
    machines: Vec<MachineChoice>,
    errors: Errors,
    busy: bool,
    focus: Focus,
    /// The machine picker, while open: the highlighted machine.
    menu: Option<usize>,
    /// Remove workspace's confirmation, over the form.
    confirm: Option<RemoveConfirm>,
}

impl WorkspaceForm {
    /// New workspace on `project`: the first unused suggested name, on the
    /// first local machine (else the first machine), no tags, an automatic
    /// slot.
    pub fn new_workspace(project: &str, source: Arc<Project>, machines: Vec<MachineChoice>, home: Option<PathBuf>) -> Self {
        let taken = |n: &str| source.workspaces.iter().any(|w| w.name.eq_ignore_ascii_case(n));
        let name = SUGGESTED
            .iter()
            .find(|n| !taken(n))
            .map(|n| n.to_string())
            .or_else(|| (1..).map(|i| format!("workspace-{i}")).find(|n| !taken(n)))
            .unwrap_or_default();
        let machine = source
            .machines
            .iter()
            .find(|m| m.kind == MachineKind::Local)
            .or(source.machines.first())
            .map(|m| m.name.clone())
            .unwrap_or_default();
        WorkspaceForm {
            project: project.to_string(),
            source,
            home,
            mode: Mode::New,
            name,
            machine,
            tags: String::new(),
            slot: String::new(),
            machines,
            errors: Errors::default(),
            busy: false,
            focus: Focus::Name,
            menu: None,
            confirm: None,
        }
    }

    /// Edit workspace `name`, its fields as configured: its own tags (not
    /// the machine's), its explicit slot or none. `None` when the project
    /// doesn't declare it.
    pub fn edit_workspace(
        project: &str,
        source: Arc<Project>,
        machines: Vec<MachineChoice>,
        home: Option<PathBuf>,
        name: &str,
    ) -> Option<Self> {
        let original = source.workspace(name)?.clone();
        Some(WorkspaceForm {
            project: project.to_string(),
            home,
            mode: Mode::Edit {
                original: original.clone(),
            },
            name: original.name.clone(),
            machine: original.machine.clone(),
            tags: original.tags.join(", "),
            slot: original.slot.map(|s| s.to_string()).unwrap_or_default(),
            machines,
            errors: Errors::default(),
            busy: false,
            focus: Focus::Name,
            menu: None,
            confirm: None,
            source,
        })
    }

    pub fn is_new(&self) -> bool {
        self.mode == Mode::New
    }

    pub fn title(&self) -> &'static str {
        if self.is_new() {
            "New workspace"
        } else {
            "Edit workspace"
        }
    }

    pub fn description(&self) -> String {
        match &self.mode {
            Mode::New => "Add a workspace so another agent can work in parallel.".into(),
            Mode::Edit { original } => {
                format!("Configure where {} runs and how Shelbi assigns work to it.", original.name)
            }
        }
    }

    pub fn submit_label(&self) -> &'static str {
        if self.is_new() {
            "Create workspace"
        } else {
            "Save changes"
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn machine(&self) -> &str {
        &self.machine
    }

    pub fn tags(&self) -> &str {
        &self.tags
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }

    pub fn machines(&self) -> &[MachineChoice] {
        &self.machines
    }

    /// The selected machine's choice, when the picker lists it.
    pub fn machine_choice(&self) -> Option<&MachineChoice> {
        self.machines.iter().find(|m| m.name == self.machine)
    }

    pub fn errors(&self) -> &Errors {
        &self.errors
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn menu(&self) -> Option<usize> {
        self.menu
    }

    pub fn confirm(&self) -> Option<&RemoveConfirm> {
        self.confirm.as_ref()
    }

    pub fn confirm_mut(&mut self) -> Option<&mut RemoveConfirm> {
        self.confirm.as_mut()
    }

    /// The name typed, without surrounding blanks.
    fn trimmed_name(&self) -> &str {
        self.name.trim()
    }

    /// The draft as a spec (for the location and the slot's placeholder).
    fn draft_spec(&self) -> WorkspaceSpec {
        WorkspaceSpec {
            name: self.trimmed_name().to_string(),
            machine: self.machine.clone(),
            tags: split_tags(&self.tags),
            slot: pool::parse_slot(&self.slot).ok().flatten(),
        }
    }

    /// The project with the draft in it: appended (New) or in the edited
    /// workspace's place (Edit).
    fn with_draft(&self, spec: WorkspaceSpec) -> Project {
        let mut p = (*self.source).clone();
        match &self.mode {
            Mode::New => p.workspaces.push(spec),
            Mode::Edit { original } => {
                for w in p.workspaces.iter_mut().filter(|w| w.name == original.name) {
                    *w = spec.clone();
                }
            }
        }
        p
    }

    /// Where the worktree is (or would be): `<machine work_dir>/.shelbi/wt/
    /// <name>`, following the name and machine as they change. Empty when the
    /// machine isn't declared.
    pub fn location(&self) -> String {
        let Some(machine) = self.source.machine(&self.machine) else {
            return String::new();
        };
        shown_path(machine.kind, &workspace_worktree(machine, &self.draft_spec()), self.home.as_deref())
    }

    /// The location's helper: what derives it, or what Save does to it once
    /// the name or machine changed.
    pub fn location_help(&self) -> String {
        let Mode::Edit { original } = &self.mode else {
            return LOCATION_HELP.into();
        };
        if self.machine != original.machine {
            format!(
                "Saving creates the worktree on {} and then removes the one on {}. Shelbi checks first that \
                 {} has no active task or unpushed work.",
                self.machine, original.machine, original.name
            )
        } else if self.trimmed_name() != original.name {
            format!(
                "Saving moves the worktree here, keeping its files. Shelbi checks first that {} has no active \
                 task or unpushed work.",
                original.name
            )
        } else {
            LOCATION_HELP.into()
        }
    }

    /// The empty slot's placeholder: Automatic, with the slot the shared
    /// resolver gives the draft.
    pub fn slot_placeholder(&self) -> String {
        let spec = WorkspaceSpec {
            slot: None,
            ..self.draft_spec()
        };
        let p = self.with_draft(spec.clone());
        format!("Automatic ({})", p.workspace_slot(&spec))
    }

    /// The tags the selected machine gives every workspace on it, when it
    /// gives any: said under the field so a save neither copies nor drops
    /// them.
    pub fn inherited_note(&self) -> Option<String> {
        let machine = self.source.machine(&self.machine)?;
        if machine.tags.is_empty() {
            return None;
        }
        let tags = machine.tags.join(", ");
        Some(format!(
            "It also gets {tags} from {}. Change those in the machine’s settings.",
            machine.name
        ))
    }

    /// Whether Edit's draft differs from the workspace as loaded (New always
    /// can be sent).
    pub fn dirty(&self) -> bool {
        let Mode::Edit { original } = &self.mode else {
            return true;
        };
        self.trimmed_name() != original.name
            || self.machine != original.machine
            || split_tags(&self.tags) != original.tags
            || pool::parse_slot(&self.slot) != Ok(original.slot)
    }

    /// Whether the default action (Create workspace, Save changes) can run.
    pub fn can_submit(&self) -> bool {
        !self.busy && self.dirty()
    }

    // --- edits -------------------------------------------------------------

    pub fn set_name(&mut self, text: &str) {
        if self.name != text {
            self.name = text.to_string();
            self.errors.name = None;
            self.errors.general = None;
        }
    }

    pub fn set_tags(&mut self, text: &str) {
        if self.tags != text {
            self.tags = text.to_string();
            self.errors.tags = None;
            self.errors.general = None;
        }
    }

    pub fn set_slot(&mut self, text: &str) {
        if self.slot != text {
            self.slot = text.to_string();
            self.errors.slot = None;
            self.errors.general = None;
        }
    }

    pub fn pick_machine(&mut self, name: &str) {
        self.menu = None;
        self.focus = Focus::Machine;
        if self.machine != name {
            self.machine = name.to_string();
            self.errors.machine = None;
            self.errors.general = None;
        }
    }

    /// The page reread the pool: the picker lists the machines now declared
    /// (one just added included). The draft keeps its machine even if it's
    /// gone; the save says so.
    pub fn set_pool(&mut self, source: Arc<Project>, machines: Vec<MachineChoice>) {
        self.source = source;
        self.machines = machines;
        if let Some(i) = self.menu {
            self.menu = Some(i.min(self.machines.len().saturating_sub(1)));
        }
    }

    pub fn set_focus(&mut self, focus: Focus) {
        if focus == Focus::Remove && self.is_new() {
            return;
        }
        self.focus = focus;
        if focus != Focus::Machine {
            self.menu = None;
        }
    }

    /// The controls in Tab order.
    fn order(&self) -> Vec<Focus> {
        let mut order = vec![Focus::Name, Focus::Machine, Focus::Tags, Focus::Location, Focus::Slot];
        if !self.is_new() {
            order.push(Focus::Remove);
        }
        order.extend([Focus::Cancel, Focus::Submit]);
        order
    }

    fn step(&mut self, forward: bool) {
        let order = self.order();
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if forward {
            (at + 1) % order.len()
        } else {
            (at + order.len() - 1) % order.len()
        };
        self.set_focus(order[next]);
    }

    // --- actions -----------------------------------------------------------

    /// A key, with the confirmation (when open) taking it first.
    pub fn key(&mut self, key: Key) -> Action {
        if let Some(confirm) = self.confirm.as_mut() {
            return match confirm.key(key) {
                Action::Close => {
                    self.confirm = None;
                    self.focus = Focus::Remove;
                    Action::Stay
                }
                other => other,
            };
        }
        if let Some(at) = self.menu {
            let last = self.machines.len().saturating_sub(1);
            match key {
                Key::Up => self.menu = Some(at.saturating_sub(1)),
                Key::Down => self.menu = Some((at + 1).min(last)),
                Key::Enter | Key::Space => {
                    if let Some(m) = self.machines.get(at).map(|m| m.name.clone()) {
                        self.pick_machine(&m);
                    }
                }
                Key::Esc => self.menu = None,
                Key::Tab | Key::BackTab => {
                    self.menu = None;
                    self.step(key == Key::Tab);
                }
            }
            return Action::Stay;
        }
        match key {
            Key::Tab | Key::BackTab => {
                self.step(key == Key::Tab);
                Action::Stay
            }
            Key::Esc => self.cancel(),
            Key::Enter => match self.focus {
                Focus::Cancel | Focus::Remove => self.activate(self.focus),
                _ => self.submit(),
            },
            Key::Space => self.activate(self.focus),
            Key::Up | Key::Down => {
                if self.focus == Focus::Machine {
                    self.open_menu();
                }
                Action::Stay
            }
        }
    }

    /// A click on (or Space at) `focus`.
    pub fn activate(&mut self, focus: Focus) -> Action {
        self.set_focus(focus);
        match focus {
            Focus::Machine => {
                if self.menu.is_some() {
                    self.menu = None;
                } else {
                    self.open_menu();
                }
                Action::Stay
            }
            Focus::Remove => {
                self.ask_remove();
                Action::Stay
            }
            Focus::Cancel => self.cancel(),
            Focus::Submit => self.submit(),
            Focus::Name | Focus::Tags | Focus::Location | Focus::Slot => Action::Stay,
        }
    }

    fn open_menu(&mut self) {
        if self.busy || self.machines.is_empty() {
            return;
        }
        let at = self.machines.iter().position(|m| m.name == self.machine).unwrap_or(0);
        self.menu = Some(at);
    }

    pub fn close_menu(&mut self) {
        self.menu = None;
    }

    /// Cancel / Esc: discard the draft, unless a request is out.
    pub fn cancel(&mut self) -> Action {
        if self.busy {
            Action::Stay
        } else {
            Action::Close
        }
    }

    /// Edit's Remove workspace: the confirmation over the form.
    pub fn ask_remove(&mut self) {
        if self.busy {
            return;
        }
        if let Mode::Edit { original } = &self.mode {
            self.confirm = RemoveConfirm::new(&self.project, &self.source, &original.name, self.home.as_deref());
        }
    }

    /// The confirmation's Cancel: back to the form, on Remove workspace.
    pub fn dismiss_confirm(&mut self) {
        if self.confirm.as_ref().is_some_and(|c| !c.busy) {
            self.confirm = None;
            self.focus = Focus::Remove;
        }
    }

    /// Create workspace / Save changes: check the draft, then hand back the
    /// request. A form that's busy, or an edit with nothing changed, sends
    /// nothing.
    pub fn submit(&mut self) -> Action {
        if !self.can_submit() {
            return Action::Stay;
        }
        match self.plan() {
            Ok(op) => {
                self.errors = Errors::default();
                self.busy = true;
                Action::Run(Job {
                    project: self.project.clone(),
                    op,
                })
            }
            Err(errors) => {
                if let Some(first) = errors.first() {
                    self.focus = first;
                }
                self.errors = errors;
                Action::Stay
            }
        }
    }

    /// The draft as a request, checked as the service will check it.
    fn plan(&self) -> Result<Op, Errors> {
        let mut errors = Errors::default();
        let name = self.trimmed_name().to_string();
        if name.is_empty() {
            errors.name = Some("Enter a name for the workspace.".into());
        }
        let slot = pool::parse_slot(&self.slot).map_err(|e| errors.add(&e)).ok().flatten();
        let tags = split_tags(&self.tags);
        let op = match &self.mode {
            Mode::New => {
                let req = AddWorkspace {
                    name,
                    machine: Some(self.machine.clone()),
                    tags,
                    slot,
                };
                if errors.name.is_none() {
                    if let Err(e) = pool::plan_workspace_add(&self.source, &req) {
                        errors.add(&e);
                    }
                }
                Op::Add(req)
            }
            Mode::Edit { original } => {
                let req = UpdateWorkspace {
                    name: original.name.clone(),
                    expected: Some(original.clone()),
                    new_name: name,
                    machine: self.machine.clone(),
                    tags,
                    slot,
                };
                if errors.name.is_none() {
                    if let Err(e) = pool::plan_workspace_update(&self.source, original, &req) {
                        errors.add(&e);
                    }
                }
                Op::Update(req)
            }
        };
        if errors.is_empty() {
            Ok(op)
        } else {
            Err(errors)
        }
    }

    /// The service answered the form's job (or its confirmation's). Success
    /// closes; an error keeps every value and says what went wrong where it
    /// belongs.
    pub fn finished(&mut self, result: Result<Outcome, PoolError>) -> Action {
        if let Some(confirm) = self.confirm.as_mut() {
            return confirm.finished(result);
        }
        self.busy = false;
        match result {
            Ok(outcome) => Action::Done(outcome),
            Err(e) => {
                let mut errors = Errors::default();
                errors.add(&e);
                if let Some(first) = errors.first() {
                    self.focus = first;
                }
                self.errors = errors;
                Action::Stay
            }
        }
    }
}

/// Which of the confirmation's buttons has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmFocus {
    Cancel,
    Remove,
}

/// The removal's confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveConfirm {
    project: String,
    expected: WorkspaceSpec,
    location: String,
    busy: bool,
    error: Option<String>,
    focus: ConfirmFocus,
}

impl RemoveConfirm {
    /// Removing `name` from `source`; `None` when it isn't declared.
    pub fn new(project: &str, source: &Project, name: &str, home: Option<&Path>) -> Option<Self> {
        let expected = source.workspace(name)?.clone();
        let location = source
            .machine(&expected.machine)
            .map(|m| shown_path(m.kind, &workspace_worktree(m, &expected), home))
            .unwrap_or_default();
        Some(RemoveConfirm {
            project: project.to_string(),
            expected,
            location,
            busy: false,
            error: None,
            // Return and Space press Cancel until the keyboard moves to
            // Remove: removal is never the default.
            focus: ConfirmFocus::Cancel,
        })
    }

    pub fn title(&self) -> String {
        format!("Remove {}?", self.expected.name)
    }

    /// What removal does.
    pub fn body(&self) -> String {
        let name = &self.expected.name;
        let machine = &self.expected.machine;
        format!(
            "Shelbi stops {name}’s session, deletes its worktree at {} on {machine}, and removes {name} from \
             this project. Its branches stay in the repository. If {name} has an active task, or work in its \
             worktree that isn’t committed and pushed, Shelbi won’t remove it.",
            self.location
        )
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn focus(&self) -> ConfirmFocus {
        self.focus
    }

    pub fn set_focus(&mut self, focus: ConfirmFocus) {
        self.focus = focus;
    }

    pub fn key(&mut self, key: Key) -> Action {
        match key {
            Key::Tab | Key::BackTab => {
                self.focus = match self.focus {
                    ConfirmFocus::Cancel => ConfirmFocus::Remove,
                    ConfirmFocus::Remove => ConfirmFocus::Cancel,
                };
                Action::Stay
            }
            Key::Esc => self.cancel(),
            Key::Enter | Key::Space => match self.focus {
                ConfirmFocus::Cancel => self.cancel(),
                ConfirmFocus::Remove => self.remove(),
            },
            Key::Up | Key::Down => Action::Stay,
        }
    }

    /// Cancel / Esc, unless the removal is out.
    pub fn cancel(&mut self) -> Action {
        if self.busy {
            Action::Stay
        } else {
            Action::Close
        }
    }

    /// Remove workspace: the request, never forced. Sent once.
    pub fn remove(&mut self) -> Action {
        if self.busy {
            return Action::Stay;
        }
        self.focus = ConfirmFocus::Remove;
        self.busy = true;
        self.error = None;
        Action::Run(Job {
            project: self.project.clone(),
            op: Op::Remove(RemoveWorkspace {
                name: self.expected.name.clone(),
                expected: Some(self.expected.clone()),
                force: false,
            }),
        })
    }

    /// The service answered: success closes everything; a refusal stays,
    /// saying why.
    pub fn finished(&mut self, result: Result<Outcome, PoolError>) -> Action {
        self.busy = false;
        match result {
            Ok(outcome) => Action::Done(outcome),
            Err(e) => {
                self.error = Some(refusal(&e));
                Action::Stay
            }
        }
    }
}

/// A removal's error as the confirmation says it.
fn refusal(e: &PoolError) -> String {
    match e {
        PoolError::ActiveTask { workspace, task } => format!(
            "{workspace} is working on {task}. Finish or stop that task first; nothing was removed."
        ),
        other => format!("{} Nothing was removed.", sentence(&other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// The service

pub type AddFn = dyn Fn(&str, &AddWorkspace) -> Result<WorkspaceChange, PoolError> + Send + Sync;
pub type UpdateFn = dyn Fn(&str, &UpdateWorkspace) -> Result<WorkspaceChange, PoolError> + Send + Sync;
pub type RemoveFn = dyn Fn(&str, &RemoveWorkspace) -> Result<WorkspaceRemoved, PoolError> + Send + Sync;

/// The shared workspace operations the forms run, off the UI thread. This
/// crate can't reach them (`shelbi_orchestrator::pool_admin` depends on it),
/// so a client builds one from `pool_admin::workspace_service` (the live
/// world) or `pool_admin::workspace_service_with` (a test's scripted
/// machines), or from closures of its own.
#[derive(Clone)]
pub struct PoolService {
    pub add: Arc<AddFn>,
    pub update: Arc<UpdateFn>,
    pub remove: Arc<RemoveFn>,
}

impl PoolService {
    /// Run `job`. Blocks: call it off the UI thread.
    pub fn run(&self, job: &Job) -> Result<Outcome, PoolError> {
        match &job.op {
            Op::Add(req) => (self.add)(&job.project, req).map(Outcome::Added),
            Op::Update(req) => (self.update)(&job.project, req).map(Outcome::Updated),
            Op::Remove(req) => (self.remove)(&job.project, req).map(Outcome::Removed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::workspaces::fixtures::{project, POOL};
    use crate::settings::workspaces::PoolIndex;

    fn source() -> Arc<Project> {
        Arc::new(project(POOL))
    }

    fn choices(p: &Project) -> Vec<MachineChoice> {
        PoolIndex::build(p, Some(Path::new("/Users/me")))
            .machines
            .iter()
            .map(|m| {
                let status = if m.host.is_some() { MachineStatus::Reachable } else { MachineStatus::Local };
                MachineChoice::from_row(m, status)
            })
            .collect()
    }

    fn new_form() -> WorkspaceForm {
        let s = source();
        WorkspaceForm::new_workspace("my-project", s.clone(), choices(&s), Some("/Users/me".into()))
    }

    fn edit_form(name: &str) -> WorkspaceForm {
        let s = source();
        WorkspaceForm::edit_workspace("my-project", s.clone(), choices(&s), Some("/Users/me".into()), name).unwrap()
    }

    fn change(spec: WorkspaceSpec) -> WorkspaceChange {
        WorkspaceChange {
            workspace: spec,
            previous_name: None,
            previous_machine: None,
            slot: 0,
            worktree: PathBuf::new(),
            hooks: pool::HooksOutcome::Skipped,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn new_starts_on_an_unused_name_on_the_local_machine_with_an_automatic_slot() {
        let form = new_form();
        assert_eq!(form.title(), "New workspace");
        assert_eq!(form.submit_label(), "Create workspace");
        assert_eq!(form.name(), "echo", "alpha to delta are taken");
        assert_eq!(form.machine(), "This Mac");
        assert_eq!((form.tags(), form.slot()), ("", ""));
        assert_eq!(form.location(), "~/Projects/my-project/.shelbi/wt/echo");
        assert_eq!(form.slot_placeholder(), "Automatic (3)", "This Mac hosts three before it");
        assert!(form.can_submit());
        assert_eq!(form.focus(), Focus::Name);
    }

    #[test]
    fn edit_starts_from_the_workspace_as_configured() {
        let form = edit_form("delta");
        assert_eq!(form.title(), "Edit workspace");
        assert_eq!(form.description(), "Configure where delta runs and how Shelbi assigns work to it.");
        assert_eq!(form.machine(), "Build server");
        assert_eq!(form.tags(), "gpu, linux", "its own tags");
        assert_eq!(form.slot(), "", "no explicit slot");
        assert_eq!(form.slot_placeholder(), "Automatic (1)");
        assert_eq!(form.location(), "/home/me/my-project/.shelbi/wt/delta", "a remote path is never ~-shortened");
        assert!(!form.can_submit(), "nothing changed yet");
        let review = edit_form("review");
        assert_eq!(review.slot(), "7");
        let charlie = edit_form("charlie");
        assert_eq!(charlie.tags(), "", "the machine's linux is inherited, not its own");
        assert!(charlie.inherited_note().unwrap().contains("linux from Build server"));
        assert!(WorkspaceForm::edit_workspace("my-project", source(), Vec::new(), None, "ghost").is_none());
    }

    #[test]
    fn the_location_and_slot_follow_the_name_and_machine() {
        let mut form = new_form();
        form.set_name("foxtrot");
        assert_eq!(form.location(), "~/Projects/my-project/.shelbi/wt/foxtrot");
        form.pick_machine("Build server");
        assert_eq!(form.location(), "/home/me/my-project/.shelbi/wt/foxtrot");
        assert_eq!(form.slot_placeholder(), "Automatic (2)");
        assert!(form.inherited_note().is_some());
        let mut edit = edit_form("alpha");
        assert_eq!(edit.location_help(), LOCATION_HELP);
        edit.set_name("alpha-2");
        assert!(edit.location_help().starts_with("Saving moves the worktree"));
        edit.pick_machine("Build server");
        assert!(edit.location_help().starts_with("Saving creates the worktree on Build server"));
    }

    #[test]
    fn a_new_workspace_sends_its_fields_and_an_empty_slot_is_none() {
        let mut form = new_form();
        form.set_tags("web, gpu  web");
        let Action::Run(job) = form.submit() else { panic!("sends") };
        assert_eq!(job.project, "my-project");
        assert_eq!(
            job.op,
            Op::Add(AddWorkspace {
                name: "echo".into(),
                machine: Some("This Mac".into()),
                tags: vec!["web".into(), "gpu".into(), "web".into()],
                slot: None,
            })
        );
        assert!(form.busy());
    }

    #[test]
    fn an_explicit_slot_is_any_whole_number_zero_included() {
        let mut form = new_form();
        form.set_slot("0");
        let Action::Run(job) = form.submit() else { panic!("sends") };
        assert!(matches!(job.op, Op::Add(AddWorkspace { slot: Some(0), .. })));
        let mut form = new_form();
        for bad in ["-1", "two", "4294967296"] {
            form.set_slot(bad);
            assert_eq!(form.submit(), Action::Stay, "{bad}");
            assert!(form.errors().slot.is_some(), "{bad}");
            assert_eq!(form.focus(), Focus::Slot);
            assert!(!form.busy());
        }
    }

    #[test]
    fn checks_show_beside_their_field_and_keep_the_draft() {
        let mut form = new_form();
        form.set_name("alpha");
        form.set_tags("two,wo rds");
        form.set_slot("x");
        assert_eq!(form.submit(), Action::Stay);
        assert!(form.errors().name.as_deref().unwrap().contains("already exists"));
        assert!(form.errors().slot.is_some());
        assert_eq!(form.focus(), Focus::Name, "the first field with a message");
        assert_eq!((form.name(), form.slot()), ("alpha", "x"), "kept as typed");
        form.set_name("");
        assert_eq!(form.errors().name, None, "typing clears its message");
        form.set_slot("");
        assert_eq!(form.submit(), Action::Stay);
        assert_eq!(form.errors().name.as_deref(), Some("Enter a name for the workspace."));
    }

    #[test]
    fn a_submit_is_sent_once_and_its_errors_keep_the_values() {
        let mut form = new_form();
        form.set_name("foxtrot");
        assert!(matches!(form.submit(), Action::Run(_)));
        assert_eq!(form.submit(), Action::Stay, "busy: no second request");
        assert_eq!(form.key(Key::Enter), Action::Stay);
        assert_eq!(form.cancel(), Action::Stay, "Cancel waits for the request");
        assert_eq!(form.key(Key::Esc), Action::Stay);
        let action = form.finished(Err(PoolError::Invalid {
            field: Some(PoolField::Name),
            message: "workspace `foxtrot` already exists".into(),
        }));
        assert_eq!(action, Action::Stay);
        assert_eq!(form.errors().name.as_deref(), Some("Workspace `foxtrot` already exists."));
        assert_eq!(form.name(), "foxtrot");
        assert!(!form.busy());
        // A refusal that names no field shows under the fields.
        assert!(matches!(form.submit(), Action::Run(_)));
        form.finished(Err(PoolError::Unsafe("can't reach `Build server`".into())));
        assert_eq!(form.errors().general.as_deref(), Some("Can't reach `Build server`."));
        // Then success closes it.
        assert!(matches!(form.submit(), Action::Run(_)));
        let done = form.finished(Ok(Outcome::Added(change(form.draft_spec()))));
        assert!(matches!(done, Action::Done(Outcome::Added(_))));
    }

    #[test]
    fn an_edit_sends_the_workspace_as_loaded_with_its_changes() {
        let mut form = edit_form("alpha");
        form.set_name("alpha-web");
        form.pick_machine("Build server");
        form.set_tags("frontend");
        let Action::Run(job) = form.submit() else { panic!("sends") };
        let Op::Update(req) = job.op else { panic!("an update") };
        assert_eq!(req.name, "alpha");
        assert_eq!(req.expected.as_ref().unwrap().tags, ["frontend"]);
        assert_eq!((req.new_name.as_str(), req.machine.as_str()), ("alpha-web", "Build server"));
        assert_eq!(req.tags, ["frontend"], "the machine's linux isn't copied in");
        assert_eq!(req.slot, None);
        // Renaming onto another workspace is caught before sending.
        let mut form = edit_form("alpha");
        form.set_name("bravo");
        assert_eq!(form.submit(), Action::Stay);
        assert!(form.errors().name.is_some());
    }

    #[test]
    fn clearing_an_explicit_slot_saves_none() {
        let mut form = edit_form("review");
        form.set_slot("");
        assert!(form.dirty());
        let Action::Run(Job { op: Op::Update(req), .. }) = form.submit() else { panic!("sends") };
        assert_eq!(req.slot, None);
    }

    #[test]
    fn tab_cycles_inside_the_dialog_and_skips_remove_in_new() {
        let mut form = new_form();
        let mut seen = Vec::new();
        for _ in 0..8 {
            form.key(Key::Tab);
            seen.push(form.focus());
        }
        assert_eq!(
            seen,
            [
                Focus::Machine,
                Focus::Tags,
                Focus::Location,
                Focus::Slot,
                Focus::Cancel,
                Focus::Submit,
                Focus::Name,
                Focus::Machine
            ]
        );
        form.key(Key::BackTab);
        form.key(Key::BackTab);
        assert_eq!(form.focus(), Focus::Submit, "⇧Tab wraps backward");
        let mut edit = edit_form("alpha");
        edit.set_focus(Focus::Slot);
        edit.key(Key::Tab);
        assert_eq!(edit.focus(), Focus::Remove);
    }

    #[test]
    fn the_machine_picker_works_from_the_keyboard() {
        let mut form = new_form();
        form.set_focus(Focus::Machine);
        form.key(Key::Down);
        assert_eq!(form.menu(), Some(0), "opens on the selected machine");
        form.key(Key::Down);
        form.key(Key::Down);
        assert_eq!(form.menu(), Some(1), "stops at the last");
        form.key(Key::Enter);
        assert_eq!(form.menu(), None);
        assert_eq!(form.machine(), "Build server");
        assert_eq!(form.focus(), Focus::Machine);
        form.key(Key::Space);
        assert_eq!(form.menu(), Some(1));
        assert_eq!(form.key(Key::Esc), Action::Stay, "Esc closes the picker, not the dialog");
        assert_eq!(form.menu(), None);
        assert_eq!(form.key(Key::Esc), Action::Close);
    }

    #[test]
    fn return_is_the_default_action_and_space_presses_the_focused_button() {
        let mut form = edit_form("alpha");
        assert_eq!(form.key(Key::Enter), Action::Stay, "nothing changed: Save changes is off");
        form.set_tags("frontend web");
        form.set_focus(Focus::Tags);
        assert!(matches!(form.key(Key::Enter), Action::Run(_)));
        let mut form = new_form();
        form.set_focus(Focus::Cancel);
        assert_eq!(form.key(Key::Enter), Action::Close, "on Cancel, Return cancels");
        assert_eq!(form.key(Key::Space), Action::Close);
    }

    #[test]
    fn a_newly_added_machine_is_offered_after_a_reread() {
        let mut form = new_form();
        assert_eq!(form.machines().len(), 2);
        let more = POOL.replace(
            "workspaces:",
            "  - name: GPU box\n    kind: ssh\n    host: gpu.lan\n    work_dir: /srv/p\nworkspaces:",
        );
        let p = Arc::new(project(&more));
        form.set_pool(p.clone(), choices(&p));
        assert_eq!(form.machines()[2].name, "GPU box");
        assert_eq!(form.machines()[2].detail(), "SSH · gpu.lan · Reachable");
        form.pick_machine("GPU box");
        assert_eq!(form.location(), "/srv/p/.shelbi/wt/echo");
        assert!(matches!(form.submit(), Action::Run(_)));
    }

    #[test]
    fn removal_confirms_with_cancel_first_and_never_forces() {
        let mut form = edit_form("charlie");
        form.key(Key::Tab); // Machine
        form.activate(Focus::Remove);
        let confirm = form.confirm().unwrap();
        assert_eq!(confirm.title(), "Remove charlie?");
        assert!(confirm.body().contains("/home/me/my-project/.shelbi/wt/charlie on Build server"));
        assert_eq!(confirm.focus(), ConfirmFocus::Cancel);
        assert_eq!(form.key(Key::Enter), Action::Stay, "Return on Cancel: back to the form");
        assert!(form.confirm().is_none());
        assert_eq!(form.focus(), Focus::Remove);
        form.activate(Focus::Remove);
        form.key(Key::Tab);
        let Action::Run(job) = form.key(Key::Enter) else { panic!("removes") };
        assert_eq!(
            job.op,
            Op::Remove(RemoveWorkspace {
                name: "charlie".into(),
                expected: Some(source().workspace("charlie").unwrap().clone()),
                force: false,
            })
        );
        assert_eq!(form.key(Key::Enter), Action::Stay, "sent once");
        assert_eq!(form.key(Key::Esc), Action::Stay, "waits for it");
        let refused = form.finished(Err(PoolError::ActiveTask {
            workspace: "charlie".into(),
            task: "fix-login".into(),
        }));
        assert_eq!(refused, Action::Stay);
        let confirm = form.confirm().unwrap();
        assert!(confirm.error().unwrap().contains("fix-login"));
        assert!(!confirm.busy());
        assert!(form.confirm_mut().unwrap().remove() != Action::Stay);
        let done = form.finished(Ok(Outcome::Removed(WorkspaceRemoved {
            name: "charlie".into(),
            machine: "Build server".into(),
            warnings: Vec::new(),
        })));
        assert!(matches!(done, Action::Done(Outcome::Removed(_))));
    }

    #[test]
    fn a_refused_removal_says_nothing_was_removed() {
        let mut confirm = RemoveConfirm::new("my-project", &source(), "alpha", None).unwrap();
        confirm.remove();
        confirm.finished(Err(PoolError::Unsafe(
            "workspace `alpha` has uncommitted changes in its worktree".into(),
        )));
        assert_eq!(
            confirm.error(),
            Some("Workspace `alpha` has uncommitted changes in its worktree. Nothing was removed.")
        );
        assert!(RemoveConfirm::new("my-project", &source(), "ghost", None).is_none());
    }

    #[test]
    fn sentences_and_tags() {
        assert_eq!(sentence("tag `a b` can't contain spaces"), "Tag `a b` can't contain spaces.");
        assert_eq!(sentence("Done."), "Done.");
        assert_eq!(split_tags(" gpu, linux  web,,"), ["gpu", "linux", "web"]);
    }
}
