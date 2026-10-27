//! The Add machine and Configure machine forms' draft, toolkit-free
//! (`dt-machine-settings-forms`, Figma frames 2526:9557 and 2528:9870; shared
//! with the TUI by `tui-settings-machine-forms`).
//!
//! A form edits a draft of one of the project's machines: [`MachineForm::add`]
//! a new SSH machine, [`MachineForm::configure`] an existing one (SSH or the
//! local machine) from the values the Workspaces page last read, with how
//! many of the project's workspaces it hosts. The draft is the fields' text
//! as typed; nothing is validated or written here. Add machine / Save changes
//! hands it to the shared machine service as a [`SaveRequest`]
//! (`shelbi_orchestrator::pool_admin::{add_machine, update_machine}`, as a
//! [`MachineService`]), which validates it against the config as it is then,
//! refuses a change that would strand the machine's workspaces, and writes
//! it. Its refusal comes back to [`MachineForm::finish_save`]: a field's
//! error marks the field, anything else shows over the actions, and the
//! draft stays as typed either way.
//!
//! - **Kind** never changes: Add machine is always SSH, and Configure keeps
//!   the machine's own (the local machine's form has no SSH host or
//!   connection check).
//! - **Project directory** is `Machine::work_dir`, the project's root on the
//!   machine (workspace worktrees go under it). The local machine's shows
//!   `~`-shortened and the service expands it again; a remote's is never
//!   expanded here (a `~` on it is the remote's home, which the service asks
//!   to have written out in full).
//! - **Machine tags** are typed comma-separated; the service trims and
//!   normalizes them. Every workspace on the machine inherits them.
//! - **Saving** runs once at a time ([`MachineForm::begin_save`] refuses a
//!   second); an answer for an older save is dropped.
//!
//! The connection check (Test connection, Install CLI) is the readiness
//! service's, which lives in the orchestrator: a client that runs it layers
//! its region over this draft ([`Controls::checks`] puts its buttons in the
//! Tab order, [`MachineForm::check_key`] says what a result is for, and
//! [`MachineForm::mark`] puts a target's complaint on its field). Until then
//! a draft is [`NOT_CHECKED`], and what the page's reachability probe last
//! saw of the saved host is only ever shown labeled as that
//! ([`MachineForm::probe_note`]).
//!
//! Keys ([`MachineForm::key`]): Tab and ⇧Tab move through the fields and
//! buttons, Return submits from a field (or activates the focused button),
//! Space activates the focused button, Esc cancels.

use std::path::Path;
use std::sync::Arc;

use shelbi_core::pool::{MachineChange, MachineFields, PoolError, PoolField, UpdateMachine};
use shelbi_core::{Machine, MachineKind};
use shelbi_state::keymap::{Key, KeyChord, Mods};

use super::workspaces::{tilde, MachineStatus, LOCAL_PHRASE};

/// What a draft's connection region says before anything checked it.
pub const NOT_CHECKED: &str = "Not checked";

/// The Machine tags field's helper.
pub const TAGS_HELP: &str = "Every workspace on this machine inherits these tags. Separate them with commas.";

/// What the form adds or edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// A new SSH machine.
    Add,
    /// An existing machine: `expected` as the page read it (a save checks
    /// it's still that), and the workspaces it hosts.
    Configure { expected: Machine, workspaces: usize },
}

/// A text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Name,
    Host,
    WorkDir,
    Tags,
}

/// What has the keyboard: a field, or a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Field(Field),
    /// Choose… beside the local machine's directory (a native folder
    /// chooser, where the client has one).
    Browse,
    /// Test connection / Test again.
    Test,
    InstallCli,
    Cancel,
    Submit,
}

/// The controls a client offers beyond the fields and Cancel / Submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Controls {
    /// Choose… beside the local machine's directory.
    pub browse: bool,
    /// The connection check's buttons, on an SSH machine's form.
    pub checks: bool,
}

/// What a key or a button asks the client to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing outside the form (it may have moved focus).
    Stay,
    /// Discard the draft and close.
    Cancel,
    /// Run this save.
    Save(u64, Box<SaveRequest>),
    /// Open the folder chooser for the local machine's directory.
    Browse,
    /// Test connection / Test again was pressed.
    Test,
    /// Install CLI was pressed.
    InstallCli,
}

/// The save the service runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveRequest {
    Add(MachineFields),
    Update(UpdateMachine),
}

/// [`SaveRequest`] for the project the form was opened for: what a client
/// hands its worker thread, so the answer can never land in another project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub project: String,
    pub run: u64,
    pub request: SaveRequest,
}

type AddFn = dyn Fn(&str, &MachineFields) -> Result<MachineChange, PoolError> + Send + Sync;
type UpdateFn = dyn Fn(&str, &UpdateMachine) -> Result<MachineChange, PoolError> + Send + Sync;

/// The machine operations (`shelbi_orchestrator::pool_admin::machine_service`
/// builds the live one; tests script their own).
#[derive(Clone)]
pub struct MachineService {
    pub add: Arc<AddFn>,
    pub update: Arc<UpdateFn>,
}

impl MachineService {
    /// Run `request` for `project`. Blocks: call it off the UI thread.
    pub fn run(&self, project: &str, request: &SaveRequest) -> Result<MachineChange, PoolError> {
        match request {
            SaveRequest::Add(fields) => (self.add)(project, fields),
            SaveRequest::Update(update) => (self.update)(project, update),
        }
    }
}

/// The host and directory a connection check result is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckKey {
    pub host: String,
    pub work_dir: String,
}

/// An add or configure form's draft.
#[derive(Debug, Clone)]
pub struct MachineForm {
    project: String,
    mode: Mode,
    kind: MachineKind,
    name: String,
    host: String,
    work_dir: String,
    tags: String,
    /// The service's complaint about one field.
    field_error: Option<(Field, String)>,
    /// A refusal or failure not about one field.
    error: Option<String>,
    /// The save in flight.
    saving: Option<u64>,
    runs: u64,
    focus: Focus,
    controls: Controls,
}

impl MachineForm {
    /// Add machine: an empty SSH draft.
    pub fn add(project: &str) -> Self {
        MachineForm {
            project: project.to_string(),
            mode: Mode::Add,
            kind: MachineKind::Ssh,
            name: String::new(),
            host: String::new(),
            work_dir: String::new(),
            tags: String::new(),
            field_error: None,
            error: None,
            saving: None,
            runs: 0,
            focus: Focus::Field(Field::Name),
            controls: Controls::default(),
        }
    }

    /// Configure `machine` (as the page read it), which hosts `workspaces`
    /// of the project's workspaces. The local machine's directory is
    /// shortened against `home`.
    pub fn configure(project: &str, machine: &Machine, workspaces: usize, home: Option<&Path>) -> Self {
        let work_dir = match machine.kind {
            MachineKind::Local => tilde(&machine.work_dir, home),
            MachineKind::Ssh => machine.work_dir.display().to_string(),
        };
        MachineForm {
            kind: machine.kind,
            name: machine.name.clone(),
            host: machine.host.clone().unwrap_or_default(),
            work_dir,
            tags: machine.tags.join(", "),
            mode: Mode::Configure {
                expected: machine.clone(),
                workspaces,
            },
            ..MachineForm::add(project)
        }
    }

    /// The form with `controls` (the client's extra buttons).
    pub fn with_controls(mut self, controls: Controls) -> Self {
        self.controls = controls;
        self
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Whether it's an SSH machine's form (SSH host, connection check).
    pub fn remote(&self) -> bool {
        self.kind == MachineKind::Ssh
    }

    /// The machine being configured, as loaded.
    pub fn configured(&self) -> Option<&Machine> {
        match &self.mode {
            Mode::Configure { expected, .. } => Some(expected),
            Mode::Add => None,
        }
    }

    pub fn text(&self, field: Field) -> &str {
        match field {
            Field::Name => &self.name,
            Field::Host => &self.host,
            Field::WorkDir => &self.work_dir,
            Field::Tags => &self.tags,
        }
    }

    /// The fields the form shows, top to bottom (the SSH host only for an
    /// SSH machine).
    pub fn fields_shown(&self) -> Vec<Field> {
        let mut out = vec![Field::Name];
        if self.remote() {
            out.push(Field::Host);
        }
        out.extend([Field::WorkDir, Field::Tags]);
        out
    }

    pub fn title(&self) -> &'static str {
        match self.mode {
            Mode::Add => "Add machine",
            Mode::Configure { .. } => "Configure machine",
        }
    }

    /// The line under the title: what Add does, or the machine's place in
    /// the project.
    pub fn description(&self) -> String {
        match &self.mode {
            Mode::Add => "Connect another machine to run workspaces over SSH.".into(),
            Mode::Configure { expected, workspaces } => {
                let runs = match workspaces {
                    0 => "runs no workspaces".to_string(),
                    1 => "runs 1 workspace".to_string(),
                    n => format!("runs {n} workspaces"),
                };
                format!("{} {runs} for this project.", expected.name)
            }
        }
    }

    /// What the project directory is on this machine: the remote's own
    /// path, or one on the local machine.
    pub fn work_dir_help(&self) -> String {
        if self.remote() {
            "The project’s directory on the remote machine, as an absolute path there (a ~ there is the \
             remote’s home, so write it out in full). Workspace worktrees go under it."
                .into()
        } else {
            format!("The project’s directory on {LOCAL_PHRASE}; ~ is your home. Workspace worktrees go under it.")
        }
    }

    pub fn submit_label(&self) -> &'static str {
        match self.mode {
            Mode::Add => "Add machine",
            Mode::Configure { .. } => "Save changes",
        }
    }

    /// The service's error for `field`, if it marked one.
    pub fn field_error(&self, field: Field) -> Option<&str> {
        self.field_error
            .as_ref()
            .filter(|(f, _)| *f == field)
            .map(|(_, e)| e.as_str())
    }

    /// The error over the actions.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn saving(&self) -> bool {
        self.saving.is_some()
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// `field` was edited to `text`. Its error and the form's go (the user
    /// is acting on them). Returns whether the text changed.
    pub fn set(&mut self, field: Field, text: &str) -> bool {
        let slot = match field {
            Field::Name => &mut self.name,
            Field::Host => &mut self.host,
            Field::WorkDir => &mut self.work_dir,
            Field::Tags => &mut self.tags,
        };
        if slot == text {
            return false;
        }
        *slot = text.to_string();
        if self.field_error.as_ref().is_some_and(|(f, _)| *f == field) {
            self.field_error = None;
        }
        self.error = None;
        true
    }

    /// Mark `field` with `message` and give it the keyboard (a check's
    /// target that can't be checked as typed).
    pub fn mark(&mut self, field: Field, message: String) {
        self.field_error = Some((field, message));
        self.focus = Focus::Field(field);
    }

    /// The fields for the service, as typed (it trims and validates them).
    pub fn fields(&self) -> MachineFields {
        let host = self.host.trim();
        MachineFields {
            name: self.name.clone(),
            kind: self.kind,
            host: (self.remote() && !host.is_empty()).then(|| host.to_string()),
            work_dir: self.work_dir.clone(),
            tags: self.tags.split(',').map(str::to_string).collect(),
        }
    }

    /// The save for the draft, unless one is running.
    pub fn begin_save(&mut self) -> Option<(u64, SaveRequest)> {
        if self.saving.is_some() {
            return None;
        }
        self.runs += 1;
        self.saving = Some(self.runs);
        self.error = None;
        self.field_error = None;
        let fields = self.fields();
        let request = match &self.mode {
            Mode::Add => SaveRequest::Add(fields),
            Mode::Configure { expected, .. } => SaveRequest::Update(UpdateMachine {
                name: expected.name.clone(),
                expected: Some(expected.clone()),
                fields,
            }),
        };
        Some((self.runs, request))
    }

    /// Save `run` answered. Returns the change when it took (the form
    /// closes); a refusal stays with the draft. An answer for another save
    /// is dropped.
    pub fn finish_save(&mut self, run: u64, result: Result<MachineChange, PoolError>) -> Option<MachineChange> {
        if self.saving != Some(run) {
            return None;
        }
        self.saving = None;
        match result {
            Ok(change) => Some(change),
            Err(e) => {
                self.refused(e);
                None
            }
        }
    }

    /// Show the service's refusal: on its field when it names one the form
    /// shows (focus moves there), else over the actions.
    fn refused(&mut self, e: PoolError) {
        let field = match &e {
            PoolError::Invalid { field: Some(f), .. } => match f {
                PoolField::Name => Some(Field::Name),
                PoolField::Host if self.remote() => Some(Field::Host),
                PoolField::WorkDir => Some(Field::WorkDir),
                PoolField::Tags => Some(Field::Tags),
                _ => None,
            },
            _ => None,
        };
        let message = refusal_message(&e);
        match field {
            Some(f) => self.mark(f, message),
            None => self.error = Some(message),
        }
    }

    /// The draft's check key: the SSH destination and the directory.
    pub fn check_key(&self) -> CheckKey {
        let host = match self.host.trim() {
            "" => self.name.trim(),
            h => h,
        };
        CheckKey {
            host: host.to_string(),
            work_dir: self.work_dir.trim().to_string(),
        }
    }

    /// The configured machine's name, when the draft still names its saved
    /// SSH destination: a check there is about a machine the project already
    /// has, so a verified binary can be recorded for it before a save.
    pub fn committed_name(&self, key: &CheckKey) -> Option<&str> {
        let saved = self.configured()?;
        let host = saved.host.as_deref().unwrap_or(&saved.name);
        (saved.kind == MachineKind::Ssh && host == key.host).then_some(saved.name.as_str())
    }

    /// What the page's reachability probe last saw of the saved machine,
    /// labeled as that: only while the draft still names the saved SSH
    /// host (a new host hasn't been probed), and never as a readiness
    /// result (the probe only asks whether SSH answers).
    pub fn probe_note(&self, status: &MachineStatus) -> Option<String> {
        self.committed_name(&self.check_key())?;
        let host = self.check_key().host;
        match status {
            MachineStatus::Local | MachineStatus::Checking => None,
            MachineStatus::Reachable => Some(format!(
                "Last status probe: {host} answered over SSH. The probe doesn’t check the Shelbi CLI, Git, or \
                 the project directory."
            )),
            MachineStatus::Unreachable(why) => Some(format!("Last status probe: {host} didn’t answer over SSH ({why}).")),
        }
    }

    /// The controls Tab visits, in order. `install`: whether the client's
    /// connection check offers Install CLI now.
    pub fn focus_order(&self, install: bool) -> Vec<Focus> {
        let mut order = vec![Focus::Field(Field::Name)];
        if self.remote() {
            order.push(Focus::Field(Field::Host));
        }
        order.push(Focus::Field(Field::WorkDir));
        if !self.remote() && self.controls.browse {
            order.push(Focus::Browse);
        }
        order.push(Focus::Field(Field::Tags));
        if self.remote() && self.controls.checks {
            order.push(Focus::Test);
            if install {
                order.push(Focus::InstallCli);
            }
        }
        order.extend([Focus::Cancel, Focus::Submit]);
        order
    }

    /// Give `focus` the keyboard (a click on a field or button).
    pub fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
    }

    /// Keep focus on a control the form shows (Install CLI comes and goes).
    pub fn settle_focus(&mut self, install: bool) {
        let order = self.focus_order(install);
        if !order.contains(&self.focus) {
            self.focus = if order.contains(&Focus::Test) { Focus::Test } else { Focus::Submit };
        }
    }

    fn step_focus(&mut self, forward: bool, install: bool) {
        let order = self.focus_order(install);
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if forward {
            (at + 1) % order.len()
        } else {
            (at + order.len() - 1) % order.len()
        };
        self.focus = order[next];
    }

    /// A press of `focus` (a click, or Space / Return on it).
    pub fn press(&mut self, focus: Focus) -> Action {
        self.focus = focus;
        match focus {
            Focus::Field(_) | Focus::Submit => self.submit(),
            Focus::Cancel => Action::Cancel,
            Focus::Browse => Action::Browse,
            Focus::Test => Action::Test,
            Focus::InstallCli => Action::InstallCli,
        }
    }

    /// Add machine / Save changes.
    pub fn submit(&mut self) -> Action {
        self.begin_save().map_or(Action::Stay, |(run, request)| Action::Save(run, Box::new(request)))
    }

    /// A key the form takes: Tab and ⇧Tab, Return and Esc anywhere, Space
    /// on a button. `None` lets it through (to the field). `install` as for
    /// [`Self::focus_order`].
    pub fn key(&mut self, chord: KeyChord, install: bool) -> Option<Action> {
        let bare = chord.mods == Mods::NONE;
        let on_button = !matches!(self.focus, Focus::Field(_));
        match chord.code {
            Key::Tab if bare => self.step_focus(true, install),
            Key::Tab if chord.mods == Mods::SHIFT => self.step_focus(false, install),
            Key::BackTab if bare || chord.mods == Mods::SHIFT => self.step_focus(false, install),
            Key::Esc if bare => return Some(Action::Cancel),
            Key::Enter if bare => return Some(self.press(self.focus)),
            Key::Char(' ') if bare && on_button => return Some(self.press(self.focus)),
            _ => return None,
        }
        Some(Action::Stay)
    }
}

/// What a refusal says to the user: the service's message, with what to do
/// next for the kinds that need it.
pub fn refusal_message(e: &PoolError) -> String {
    match e {
        PoolError::Conflict(m) => format!("{m}. Cancel and open the machine again to see the change."),
        PoolError::NotFound(m) => format!("{m}. It may have been removed elsewhere; cancel to see the list again."),
        PoolError::Version(m) => format!("{m}. Restart Shelbi so the app and its daemon match, then try again."),
        e => e.to_string(),
    }
}

/// One line saying what a save did, for the page's notice: a rename says
/// the machine's workspaces moved with it.
pub fn notice(change: &MachineChange, added: bool) -> String {
    let name = &change.machine.name;
    if added {
        return format!("Added {name}.");
    }
    match &change.previous_name {
        Some(old) => match change.workspaces.len() {
            0 => format!("Renamed {old} to {name}."),
            1 => format!("Renamed {old} to {name}; its workspace moved with it."),
            n => format!("Renamed {old} to {name}; its {n} workspaces moved with it."),
        },
        None => format!("Saved {name}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn remote() -> Machine {
        Machine {
            name: "Build server".into(),
            kind: MachineKind::Ssh,
            work_dir: PathBuf::from("/home/me/my-project"),
            host: Some("build-server".into()),
            tags: vec!["linux".into(), "gpu".into()],
            forward: Some(shelbi_core::ForwardMode::Tcp),
        }
    }

    fn local() -> Machine {
        Machine {
            name: "This Mac".into(),
            kind: MachineKind::Local,
            work_dir: PathBuf::from("/Users/me/Projects/my-project"),
            host: None,
            tags: vec![],
            forward: None,
        }
    }

    fn chord(code: Key, mods: Mods) -> KeyChord {
        KeyChord { code, mods }
    }

    #[test]
    fn add_is_an_empty_ssh_draft() {
        let f = MachineForm::add("p");
        assert!(f.remote());
        for field in [Field::Name, Field::Host, Field::WorkDir, Field::Tags] {
            assert_eq!(f.text(field), "", "no sample values");
        }
        assert_eq!((f.title(), f.submit_label()), ("Add machine", "Add machine"));
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        assert_eq!(f.fields_shown(), [Field::Name, Field::Host, Field::WorkDir, Field::Tags]);
    }

    #[test]
    fn configure_fills_the_machines_values_and_its_workspace_count() {
        let f = MachineForm::configure("p", &remote(), 2, None);
        assert_eq!(f.text(Field::Name), "Build server");
        assert_eq!(f.text(Field::Host), "build-server");
        assert_eq!(f.text(Field::WorkDir), "/home/me/my-project");
        assert_eq!(f.text(Field::Tags), "linux, gpu");
        assert_eq!(f.description(), "Build server runs 2 workspaces for this project.");
        assert_eq!(f.submit_label(), "Save changes");
        assert_eq!(
            MachineForm::configure("p", &remote(), 1, None).description(),
            "Build server runs 1 workspace for this project."
        );
        assert_eq!(
            MachineForm::configure("p", &remote(), 0, None).description(),
            "Build server runs no workspaces for this project."
        );
    }

    #[test]
    fn the_local_form_has_no_ssh_host_and_a_shortened_directory() {
        let f = MachineForm::configure("p", &local(), 3, Some(Path::new("/Users/me")));
        assert!(!f.remote());
        assert_eq!(f.fields_shown(), [Field::Name, Field::WorkDir, Field::Tags]);
        assert_eq!(f.text(Field::WorkDir), "~/Projects/my-project");
        assert_eq!(
            f.focus_order(false),
            [Focus::Field(Field::Name), Focus::Field(Field::WorkDir), Focus::Field(Field::Tags), Focus::Cancel, Focus::Submit],
            "no chooser unless the client offers one"
        );
        let browse = f.clone().with_controls(Controls { browse: true, checks: true });
        assert_eq!(
            browse.focus_order(true),
            [
                Focus::Field(Field::Name),
                Focus::Field(Field::WorkDir),
                Focus::Browse,
                Focus::Field(Field::Tags),
                Focus::Cancel,
                Focus::Submit,
            ],
            "a local machine has nothing to test"
        );
        let fields = f.fields();
        assert_eq!((fields.kind, fields.host), (MachineKind::Local, None), "the kind stays local");
        assert!(f.work_dir_help().contains("~ is your home"));
    }

    #[test]
    fn a_remote_directory_is_never_shortened_against_this_machine() {
        let mut m = remote();
        m.work_dir = PathBuf::from("/Users/me/my-project");
        let f = MachineForm::configure("p", &m, 0, Some(Path::new("/Users/me")));
        assert_eq!(f.text(Field::WorkDir), "/Users/me/my-project");
        let mut f = MachineForm::add("p");
        f.set(Field::WorkDir, "~/src/my project");
        assert_eq!(f.fields().work_dir, "~/src/my project", "handed on as typed");
        assert!(f.work_dir_help().contains("remote’s home"));
    }

    #[test]
    fn tags_split_on_commas_for_the_service() {
        let mut f = MachineForm::add("p");
        f.set(Field::Tags, "linux, gpu,,  big-ram ");
        assert_eq!(f.fields().tags, ["linux", " gpu", "", "  big-ram "]);
        let normalized = shelbi_core::pool::normalize_tags(&f.fields().tags).unwrap();
        assert_eq!(normalized, ["linux", "gpu", "big-ram"]);
    }

    #[test]
    fn an_alias_is_the_host_and_an_empty_host_falls_back_to_the_name() {
        let mut f = MachineForm::add("p");
        f.set(Field::Name, "gpu-box");
        assert_eq!(f.fields().host, None);
        assert_eq!(f.check_key().host, "gpu-box", "as ssh does");
        f.set(Field::Host, "  devbox ");
        assert_eq!(f.fields().host.as_deref(), Some("devbox"));
    }

    #[test]
    fn configure_saves_as_an_update_expecting_the_loaded_machine() {
        let mut f = MachineForm::configure("p", &remote(), 2, None);
        f.set(Field::Tags, "linux");
        let (run, request) = f.begin_save().unwrap();
        let SaveRequest::Update(update) = request else { panic!("an update") };
        assert_eq!(update.name, "Build server");
        assert_eq!(update.expected, Some(remote()));
        assert_eq!(update.fields.tags, ["linux"]);
        assert!(f.saving());
        assert_eq!(f.begin_save(), None, "one save at a time");
        assert_eq!(f.submit(), Action::Stay);
        assert_eq!(f.finish_save(run + 1, Err(PoolError::Persistence("x".into()))), None);
        assert!(f.saving(), "another run's answer is dropped");
    }

    #[test]
    fn a_refusal_keeps_the_draft_and_marks_its_field() {
        let mut f = MachineForm::add("p");
        f.set(Field::Name, "build");
        f.set(Field::Host, "build-2");
        let (run, _) = f.begin_save().unwrap();
        let refused = PoolError::Invalid {
            field: Some(PoolField::Name),
            message: "machine `Build` already exists".into(),
        };
        assert_eq!(f.finish_save(run, Err(refused)), None);
        assert!(!f.saving());
        assert_eq!(f.field_error(Field::Name), Some("machine `Build` already exists"));
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        assert_eq!((f.text(Field::Name), f.text(Field::Host)), ("build", "build-2"));
        f.set(Field::Name, "build-3");
        assert_eq!(f.field_error(Field::Name), None, "editing it clears its error");

        let (run, _) = f.begin_save().unwrap();
        f.finish_save(run, Err(PoolError::Unsafe("workspace `alpha` has a live session".into())));
        assert_eq!(f.error(), Some("workspace `alpha` has a live session"));
        let (run, _) = f.begin_save().unwrap();
        f.finish_save(run, Err(PoolError::Conflict("machine `Build server` changed since it was loaded".into())));
        assert!(f.error().unwrap().contains("open the machine again"));
    }

    #[test]
    fn a_host_refusal_on_the_local_form_shows_over_the_actions() {
        let mut f = MachineForm::configure("p", &local(), 0, None);
        let (run, _) = f.begin_save().unwrap();
        f.finish_save(run, Err(PoolError::invalid(PoolField::Host, "no host")));
        assert_eq!(f.field_error(Field::Host), None, "the local form has no host field");
        assert_eq!(f.error(), Some("no host"));
    }

    #[test]
    fn only_the_saved_host_is_a_committed_identity() {
        let f = MachineForm::configure("p", &remote(), 0, None);
        assert_eq!(f.committed_name(&f.check_key()), Some("Build server"));
        let mut f = f;
        f.set(Field::Host, "build-2");
        assert_eq!(f.committed_name(&f.check_key()), None);
        let add = MachineForm::add("p");
        assert_eq!(add.committed_name(&add.check_key()), None, "an unsaved draft commits nothing");
    }

    #[test]
    fn the_probe_note_is_labeled_and_only_for_the_saved_host() {
        let mut f = MachineForm::configure("p", &remote(), 0, None);
        let note = f.probe_note(&MachineStatus::Reachable).unwrap();
        assert!(note.starts_with("Last status probe:") && note.contains("doesn’t check the Shelbi CLI"), "{note}");
        assert!(f.probe_note(&MachineStatus::Unreachable("timed out".into())).unwrap().contains("timed out"));
        assert_eq!(f.probe_note(&MachineStatus::Checking), None);
        f.set(Field::Host, "build-2");
        assert_eq!(f.probe_note(&MachineStatus::Reachable), None, "a new host wasn't probed");
        assert_eq!(MachineForm::add("p").probe_note(&MachineStatus::Reachable), None);
        assert_eq!(MachineForm::configure("p", &local(), 0, None).probe_note(&MachineStatus::Local), None);
    }

    #[test]
    fn check_buttons_join_the_order_only_when_offered() {
        let f = MachineForm::configure("p", &remote(), 0, None);
        assert!(!f.focus_order(true).contains(&Focus::Test), "no check controls by default");
        let f = f.with_controls(Controls { browse: true, checks: true });
        assert!(f.focus_order(false).contains(&Focus::Test));
        assert!(!f.focus_order(false).contains(&Focus::InstallCli));
        assert!(f.focus_order(true).contains(&Focus::InstallCli));
        assert!(!f.focus_order(true).contains(&Focus::Browse), "no chooser for a remote");
        let mut f = f;
        f.set_focus(Focus::InstallCli);
        f.settle_focus(false);
        assert_eq!(f.focus(), Focus::Test);
        let mut plain = MachineForm::add("p");
        plain.set_focus(Focus::Test);
        plain.settle_focus(false);
        assert_eq!(plain.focus(), Focus::Submit);
        assert_eq!(plain.press(Focus::Test), Action::Test);
        assert_eq!(plain.press(Focus::InstallCli), Action::InstallCli);
    }

    #[test]
    fn keys_move_focus_submit_and_cancel() {
        let mut f = MachineForm::add("p");
        assert_eq!(f.key(chord(Key::Tab, Mods::NONE), false), Some(Action::Stay));
        assert_eq!(f.focus(), Focus::Field(Field::Host));
        f.key(chord(Key::Tab, Mods::SHIFT), false);
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        f.key(chord(Key::BackTab, Mods::SHIFT), false);
        assert_eq!(f.focus(), Focus::Submit, "wraps");
        f.key(chord(Key::Tab, Mods::NONE), false);
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        assert_eq!(f.key(chord(Key::Char(' '), Mods::NONE), false), None, "a field's space is typed");
        assert_eq!(f.key(chord(Key::Char('a'), Mods::NONE), false), None);
        assert_eq!(f.key(chord(Key::Esc, Mods::NONE), false), Some(Action::Cancel));
        assert!(matches!(f.key(chord(Key::Enter, Mods::NONE), false), Some(Action::Save(..))));

        let mut f = MachineForm::add("p");
        f.set_focus(Focus::Cancel);
        assert_eq!(f.key(chord(Key::Char(' '), Mods::NONE), false), Some(Action::Cancel));
    }

    #[test]
    fn notices_say_what_a_save_did() {
        let change = |previous: Option<&str>, workspaces: &[&str]| MachineChange {
            machine: remote(),
            previous_name: previous.map(str::to_string),
            workspaces: workspaces.iter().map(|w| w.to_string()).collect(),
        };
        assert_eq!(notice(&change(None, &[]), true), "Added Build server.");
        assert_eq!(notice(&change(None, &["a"]), false), "Saved Build server.");
        assert_eq!(notice(&change(Some("gpu"), &[]), false), "Renamed gpu to Build server.");
        assert_eq!(
            notice(&change(Some("gpu"), &["a", "b"]), false),
            "Renamed gpu to Build server; its 2 workspaces moved with it."
        );
    }
}
