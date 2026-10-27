//! The Add machine and Configure machine forms' state, toolkit-free
//! (`dt-machine-settings-forms`, Figma frames 2526:9557 and 2528:9870).
//!
//! A form edits a draft of one of the project's machines: [`MachineForm::add`]
//! a new SSH machine, [`MachineForm::configure`] an existing one (SSH or this
//! Mac) from the values the Workspaces page last read, with how many of the
//! project's workspaces it hosts. The draft is the fields' text as typed;
//! nothing is validated or written here. Add machine / Save changes hands it
//! to the shared machine service as a [`SaveRequest`]
//! (`shelbi_orchestrator::pool_admin::{add_machine, update_machine}`), which
//! validates it against the config as it is then, refuses a change that would
//! strand the machine's workspaces, and writes it. Its refusal comes back to
//! [`MachineForm::finish_save`]: a field's error marks the field, anything
//! else shows over the actions, and the draft stays as typed either way.
//!
//! - **Kind** never changes: Add machine is always SSH, and Configure keeps
//!   the machine's own (this Mac's form has no SSH host or connection check).
//! - **Project directory** is `Machine::work_dir`, the project's root on the
//!   machine (workspace worktrees go under it). This Mac's shows `~`-shortened
//!   and the service expands it again; a remote's is never expanded here (a
//!   `~` on it is the remote's home, which the service asks to have written
//!   out in full).
//! - **Machine tags** are typed comma-separated; the service trims and
//!   normalizes them. Every workspace on the machine inherits them.
//! - **Connection check** (SSH only): Test connection, Test again and Install
//!   CLI each make a [`ReadinessRequest`] for the draft as it stands, which
//!   the readiness service runs off the UI thread (`dt-machine-readiness-setup`
//!   owns the real checks). Until one lands, the region says not checked or
//!   checking; never the successful fixture. A result counts only for the host
//!   and directory it was checked with ([`CheckKey`]): changing either drops it
//!   back to not checked, and an answer for an older draft is dropped. A check
//!   writes nothing to the project.
//! - **Saving** runs once at a time ([`MachineForm::begin_save`] refuses a
//!   second); an answer for an older save is dropped.
//!
//! Keys ([`MachineForm::key`]): Tab and ⇧Tab move through the fields and
//! buttons, Return submits from a field (or activates the focused button),
//! Space activates the focused button, Esc cancels.

use std::path::Path;

use shelbi_core::{Machine, MachineKind};
use shelbi_orchestrator::pool_admin::{MachineChange, MachineFields, PoolError, PoolField, UpdateMachine};
use shelbi_state::keymap::{Key, KeyChord, Mods};

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
    /// Choose… beside this Mac's directory.
    Browse,
    /// Test connection / Test again.
    Test,
    InstallCli,
    Cancel,
    Submit,
}

/// What a key or a button asks the view to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing outside the form (it may have moved focus).
    Stay,
    /// Discard the draft and close.
    Cancel,
    /// Run this save.
    Save(u64, SaveRequest),
    /// Open the folder chooser for this Mac's directory.
    Browse,
    /// Run this readiness request.
    Check(ReadinessRequest),
}

/// The save the service runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveRequest {
    Add(MachineFields),
    Update(UpdateMachine),
}

/// Which readiness button was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessIntent {
    /// Test connection: nothing checked yet for this host and directory.
    Test,
    /// Test again, over an earlier result.
    TestAgain,
    /// Install CLI: the check found no compatible Shelbi CLI there.
    InstallCli,
}

/// The machine a readiness request is about: the draft as it stands, which
/// may not be saved (or valid) yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessTarget {
    pub project: String,
    /// The configured machine's name, for Configure; `None` for a new one.
    pub machine: Option<String>,
    /// The name as typed.
    pub name: String,
    /// The SSH destination: the host as typed, else the name (as SSH falls
    /// back to it), trimmed. An SSH config alias resolves through `ssh`.
    pub host: String,
    /// The project directory as typed, trimmed: the remote's own path,
    /// never expanded against this Mac.
    pub work_dir: String,
}

/// The structured handoff to the readiness service: what to do, for which
/// draft. `run` comes back with its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessRequest {
    pub run: u64,
    pub intent: ReadinessIntent,
    pub target: ReadinessTarget,
}

/// What a readiness request found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// SSH, the Shelbi CLI, Git and the project directory all checked out.
    /// (The interim check can't tell; `dt-machine-readiness-setup`'s does.)
    #[allow(dead_code)]
    Ready { detail: String },
    /// SSH answered; nothing past it was checked.
    Reachable,
    /// SSH answered, but there's no compatible Shelbi CLI there. (Likewise
    /// the readiness service's.)
    #[allow(dead_code)]
    NeedsCli { message: String },
    /// It couldn't connect, or a check failed: why.
    Failed { message: String },
}

/// The host and directory a check result is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckKey {
    pub host: String,
    pub work_dir: String,
}

/// The connection check region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    NotChecked,
    Checking { run: u64, key: CheckKey, intent: ReadinessIntent },
    Done { key: CheckKey, outcome: CheckOutcome },
}

/// An add or configure form's state.
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
    check: Check,
    focus: Focus,
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
            check: Check::NotChecked,
            focus: Focus::Field(Field::Name),
        }
    }

    /// Configure `machine` (as the page read it), which hosts `workspaces`
    /// of the project's workspaces. This Mac's directory is shortened
    /// against `home`.
    pub fn configure(project: &str, machine: &Machine, workspaces: usize, home: Option<&Path>) -> Self {
        let work_dir = match machine.kind {
            MachineKind::Local => super::workspaces_state::tilde(&machine.work_dir, home),
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

    pub fn project(&self) -> &str {
        &self.project
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

    pub fn check(&self) -> &Check {
        &self.check
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// `field` was edited to `text`. Its error and the form's go (the user
    /// is acting on them), and a new host or directory drops the check
    /// done (or running) for the old one.
    pub fn set(&mut self, field: Field, text: &str) {
        let slot = match field {
            Field::Name => &mut self.name,
            Field::Host => &mut self.host,
            Field::WorkDir => &mut self.work_dir,
            Field::Tags => &mut self.tags,
        };
        if slot == text {
            return;
        }
        *slot = text.to_string();
        if self.field_error.as_ref().is_some_and(|(f, _)| *f == field) {
            self.field_error = None;
        }
        self.error = None;
        let key = self.check_key();
        let stale = match &self.check {
            Check::NotChecked => false,
            Check::Checking { key: k, .. } | Check::Done { key: k, .. } => *k != key,
        };
        if stale {
            self.check = Check::NotChecked;
        }
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
            Some(f) => {
                self.field_error = Some((f, message));
                self.focus = Focus::Field(f);
            }
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

    /// The readiness button's intent now: Test connection before a result
    /// for this draft, Test again after one.
    pub fn test_intent(&self) -> ReadinessIntent {
        match self.check {
            Check::Done { .. } => ReadinessIntent::TestAgain,
            _ => ReadinessIntent::Test,
        }
    }

    /// Whether Install CLI is offered: the check found the CLI missing.
    pub fn offers_install(&self) -> bool {
        matches!(&self.check, Check::Done { outcome: CheckOutcome::NeedsCli { .. }, .. })
    }

    /// Start `intent` for the draft. `None` for this Mac's form, while a
    /// check runs, and without a host to test (the Host field says so).
    pub fn begin_check(&mut self, intent: ReadinessIntent) -> Option<ReadinessRequest> {
        if !self.remote() || matches!(self.check, Check::Checking { .. }) {
            return None;
        }
        let key = self.check_key();
        if key.host.is_empty() {
            self.field_error = Some((Field::Host, "Enter the SSH host to test.".into()));
            self.focus = Focus::Field(Field::Host);
            return None;
        }
        self.runs += 1;
        let run = self.runs;
        self.check = Check::Checking {
            run,
            key: key.clone(),
            intent,
        };
        Some(ReadinessRequest {
            run,
            intent,
            target: ReadinessTarget {
                project: self.project.clone(),
                machine: self.configured().map(|m| m.name.clone()),
                name: self.name.trim().to_string(),
                host: key.host,
                work_dir: key.work_dir,
            },
        })
    }

    /// Readiness request `run` answered. Dropped unless it's the check
    /// running now (the draft's host and directory haven't moved since).
    pub fn finish_check(&mut self, run: u64, outcome: CheckOutcome) {
        if let Check::Checking { run: r, key, .. } = &self.check {
            if *r == run {
                self.check = Check::Done { key: key.clone(), outcome };
            }
        }
    }

    /// The controls Tab visits, in order.
    pub fn focus_order(&self) -> Vec<Focus> {
        let mut order = vec![Focus::Field(Field::Name)];
        if self.remote() {
            order.push(Focus::Field(Field::Host));
        }
        order.push(Focus::Field(Field::WorkDir));
        if !self.remote() {
            order.push(Focus::Browse);
        }
        order.push(Focus::Field(Field::Tags));
        if self.remote() {
            order.push(Focus::Test);
            if self.offers_install() {
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
    pub fn settle_focus(&mut self) {
        if !self.focus_order().contains(&self.focus) {
            self.focus = Focus::Test;
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let order = self.focus_order();
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
            Focus::Test => self.begin_check(self.test_intent()).map_or(Action::Stay, Action::Check),
            Focus::InstallCli => self
                .begin_check(ReadinessIntent::InstallCli)
                .map_or(Action::Stay, Action::Check),
        }
    }

    /// Add machine / Save changes.
    pub fn submit(&mut self) -> Action {
        self.begin_save().map_or(Action::Stay, |(run, request)| Action::Save(run, request))
    }

    /// A key the form takes: Tab and ⇧Tab, Return and Esc anywhere, Space
    /// on a button. `None` lets it through (to the field).
    pub fn key(&mut self, chord: KeyChord) -> Option<Action> {
        let bare = chord.mods == Mods::NONE;
        let on_button = !matches!(self.focus, Focus::Field(_));
        match chord.code {
            Key::Tab if bare => self.step_focus(true),
            Key::Tab if chord.mods == Mods::SHIFT => self.step_focus(false),
            Key::BackTab if bare || chord.mods == Mods::SHIFT => self.step_focus(false),
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

#[cfg(test)]
pub(crate) mod tests {
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
        assert_eq!(f.check(), &Check::NotChecked);
        assert_eq!(f.focus(), Focus::Field(Field::Name));
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
    fn this_macs_form_has_no_ssh_and_a_shortened_directory() {
        let f = MachineForm::configure("p", &local(), 3, Some(Path::new("/Users/me")));
        assert!(!f.remote());
        assert_eq!(f.text(Field::WorkDir), "~/Projects/my-project");
        assert_eq!(
            f.focus_order(),
            [
                Focus::Field(Field::Name),
                Focus::Field(Field::WorkDir),
                Focus::Browse,
                Focus::Field(Field::Tags),
                Focus::Cancel,
                Focus::Submit,
            ]
        );
        let fields = f.fields();
        assert_eq!((fields.kind, fields.host), (MachineKind::Local, None), "the kind stays local");
        let mut f = f;
        assert_eq!(f.begin_check(ReadinessIntent::Test), None, "nothing to test");
    }

    #[test]
    fn a_remote_directory_is_never_shortened_against_this_mac() {
        let mut m = remote();
        m.work_dir = PathBuf::from("/Users/me/my-project");
        let f = MachineForm::configure("p", &m, 0, Some(Path::new("/Users/me")));
        assert_eq!(f.text(Field::WorkDir), "/Users/me/my-project");
        let mut f = MachineForm::add("p");
        f.set(Field::WorkDir, "~/src/my project");
        assert_eq!(f.fields().work_dir, "~/src/my project", "handed on as typed");
    }

    #[test]
    fn tags_split_on_commas_for_the_service() {
        let mut f = MachineForm::add("p");
        f.set(Field::Tags, "linux, gpu,,  big-ram ");
        assert_eq!(f.fields().tags, ["linux", " gpu", "", "  big-ram "]);
        let normalized = shelbi_orchestrator::pool_admin::normalize_tags(&f.fields().tags).unwrap();
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
    fn checks_count_only_for_their_host_and_directory() {
        let mut f = MachineForm::configure("p", &remote(), 2, None);
        assert_eq!(f.test_intent(), ReadinessIntent::Test);
        let Action::Check(request) = f.press(Focus::Test) else { panic!("a check") };
        assert_eq!(request.intent, ReadinessIntent::Test);
        assert_eq!(
            request.target,
            ReadinessTarget {
                project: "p".into(),
                machine: Some("Build server".into()),
                name: "Build server".into(),
                host: "build-server".into(),
                work_dir: "/home/me/my-project".into(),
            }
        );
        assert!(matches!(f.check(), Check::Checking { .. }));
        assert_eq!(f.press(Focus::Test), Action::Stay, "one at a time");
        f.finish_check(request.run, CheckOutcome::Reachable);
        assert!(matches!(f.check(), Check::Done { outcome: CheckOutcome::Reachable, .. }));
        assert_eq!(f.test_intent(), ReadinessIntent::TestAgain);

        // Tags don't matter to it; the directory does.
        f.set(Field::Tags, "linux");
        assert!(matches!(f.check(), Check::Done { .. }));
        f.set(Field::WorkDir, "/srv/my-project");
        assert_eq!(f.check(), &Check::NotChecked, "a new directory isn't checked");

        // An answer for a host that's since changed is dropped.
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        f.set(Field::Host, "build-3");
        assert_eq!(f.check(), &Check::NotChecked);
        f.finish_check(
            request.run,
            CheckOutcome::Ready {
                detail: "Shelbi, Git, and project directory are ready.".into(),
            },
        );
        assert_eq!(f.check(), &Check::NotChecked);
    }

    #[test]
    fn install_cli_is_offered_only_when_the_check_says_so() {
        let mut f = MachineForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert!(!f.focus_order().contains(&Focus::InstallCli));
        f.finish_check(
            request.run,
            CheckOutcome::NeedsCli {
                message: "shelbi isn't installed".into(),
            },
        );
        assert!(f.offers_install() && f.focus_order().contains(&Focus::InstallCli));
        let Action::Check(install) = f.press(Focus::InstallCli) else { panic!() };
        assert_eq!(install.intent, ReadinessIntent::InstallCli);
        f.finish_check(install.run, CheckOutcome::Reachable);
        assert!(!f.offers_install());
        f.set_focus(Focus::InstallCli);
        f.settle_focus();
        assert_eq!(f.focus(), Focus::Test);
    }

    #[test]
    fn no_host_to_test_marks_the_host_field() {
        let mut f = MachineForm::add("p");
        assert_eq!(f.press(Focus::Test), Action::Stay);
        assert!(f.field_error(Field::Host).is_some());
        assert_eq!(f.focus(), Focus::Field(Field::Host));
        assert_eq!(f.check(), &Check::NotChecked);
    }

    #[test]
    fn keys_move_focus_submit_and_cancel() {
        let mut f = MachineForm::add("p");
        assert_eq!(f.key(chord(Key::Tab, Mods::NONE)), Some(Action::Stay));
        assert_eq!(f.focus(), Focus::Field(Field::Host));
        f.key(chord(Key::Tab, Mods::SHIFT));
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        f.key(chord(Key::BackTab, Mods::SHIFT));
        assert_eq!(f.focus(), Focus::Submit, "wraps");
        f.key(chord(Key::Tab, Mods::NONE));
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        assert_eq!(f.key(chord(Key::Char(' '), Mods::NONE)), None, "a field's space is typed");
        assert_eq!(f.key(chord(Key::Char('a'), Mods::NONE)), None);
        assert_eq!(f.key(chord(Key::Esc, Mods::NONE)), Some(Action::Cancel));
        assert!(matches!(f.key(chord(Key::Enter, Mods::NONE)), Some(Action::Save(..))));

        let mut f = MachineForm::add("p");
        f.set_focus(Focus::Cancel);
        assert_eq!(f.key(chord(Key::Char(' '), Mods::NONE)), Some(Action::Cancel));
    }
}
