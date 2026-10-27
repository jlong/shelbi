//! The Add machine and Configure machine forms' state
//! (`dt-machine-settings-forms`, Figma frames 2526:9557 and 2528:9870): the
//! shared draft (`shelbi_app::settings::machine_form`, which the TUI's form
//! edits too) with the desktop's connection check region over it.
//!
//! The draft is the fields' text as typed; Add machine / Save changes hands
//! it to the shared machine service as a [`SaveRequest`] and its refusal
//! comes back to [`MachineForm::finish_save`] (see the shared module). This
//! Mac's form adds Choose… beside its directory (the native folder chooser).
//!
//! - **Connection check** (SSH only): Test connection, Test again and Install
//!   CLI each make a [`ReadinessRequest`] for the draft as it stands, which
//!   the readiness service (`shelbi_orchestrator::machine_readiness`) runs off
//!   the UI thread: a check of SSH, the Shelbi CLI, Git and the project
//!   directory, or the shared remote CLI setup followed by that check again.
//!   [`MachineForm::panel`] says what the region shows; only a check that
//!   verified everything is "Connection successful". A result counts only for
//!   the host and directory it was checked with ([`CheckKey`]): changing
//!   either drops a check back to not checked (an install keeps running, and
//!   its answer is dropped if the draft has moved on), and an answer for an
//!   older request is dropped. A check writes nothing to the project.
//!
//! Keys ([`MachineForm::key`]): the shared form's (Tab and ⇧Tab move, Return
//! submits from a field or activates the focused button, Space activates the
//! focused button, Esc cancels), with the check's buttons in the Tab order.

use std::ops::Deref;
use std::path::Path;

use shelbi_app::settings::machine_form::{self as draft, Controls};
use shelbi_core::Machine;
use shelbi_orchestrator::machine_readiness::{self as readiness, Issue, Readiness, TargetField};
use shelbi_orchestrator::pool_admin::{MachineChange, PoolError};
use shelbi_state::keymap::KeyChord;

pub use shelbi_app::settings::machine_form::{CheckKey, Field, Focus, SaveRequest};

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

impl ReadinessTarget {
    /// The orchestrator's target: where to connect, which directory.
    pub fn remote(&self) -> readiness::ReadinessTarget {
        readiness::ReadinessTarget {
            host: self.host.clone(),
            work_dir: self.work_dir.clone(),
        }
    }
}

/// What a readiness request found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Test connection / Test again: the readiness check's answer.
    Checked(Readiness),
    /// Install CLI ran the shared remote setup (`note` says what it did:
    /// already there, installed, or copied from this Mac), then checked again.
    Installed { note: String, readiness: Readiness },
    /// Install CLI's setup failed: why, and what to do by hand. `retry` when
    /// installing again could help (not for a connection that failed).
    InstallFailed { message: String, retry: bool },
}

impl CheckOutcome {
    /// The readiness the outcome carries, if a check ran.
    pub fn readiness(&self) -> Option<&Readiness> {
        match self {
            CheckOutcome::Checked(r) | CheckOutcome::Installed { readiness: r, .. } => Some(r),
            CheckOutcome::InstallFailed { .. } => None,
        }
    }

    /// Everything checked out: the only outcome shown as a successful
    /// connection.
    pub fn is_ready(&self) -> bool {
        self.readiness().is_some_and(Readiness::is_ready)
    }

    /// Whether Install CLI is offered after it.
    pub fn offers_install(&self) -> bool {
        match self {
            CheckOutcome::InstallFailed { retry, .. } => *retry,
            other => other.readiness().is_some_and(Readiness::offers_install),
        }
    }
}

/// The region's tone: its icon and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Not checked yet.
    Idle,
    /// A check or install is running (a spinner).
    Busy,
    /// Verified ready.
    Success,
    /// Reached, but something there needs attention.
    Warning,
    /// Couldn't connect, or the install failed.
    Error,
}

/// What the connection check region shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    pub tone: Tone,
    pub title: String,
    pub detail: String,
}

/// The connection check region. `Checking` with [`ReadinessIntent::InstallCli`]
/// is an install in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    NotChecked,
    Checking { run: u64, key: CheckKey, intent: ReadinessIntent },
    Done { key: CheckKey, outcome: CheckOutcome },
}

/// An add or configure form's state: the shared draft (read through
/// `Deref`) and the connection check region.
#[derive(Debug, Clone)]
pub struct MachineForm {
    draft: draft::MachineForm,
    check: Check,
    /// Readiness requests made, for their run ids.
    runs: u64,
}

impl Deref for MachineForm {
    type Target = draft::MachineForm;

    fn deref(&self) -> &draft::MachineForm {
        &self.draft
    }
}

impl MachineForm {
    /// Add machine: an empty SSH draft.
    pub fn add(project: &str) -> Self {
        Self::over(draft::MachineForm::add(project))
    }

    /// Configure `machine` (as the page read it), which hosts `workspaces`
    /// of the project's workspaces. This Mac's directory is shortened
    /// against `home`.
    pub fn configure(project: &str, machine: &Machine, workspaces: usize, home: Option<&Path>) -> Self {
        Self::over(draft::MachineForm::configure(project, machine, workspaces, home))
    }

    fn over(draft: draft::MachineForm) -> Self {
        MachineForm {
            draft: draft.with_controls(Controls { browse: true, checks: true }),
            check: Check::NotChecked,
            runs: 0,
        }
    }

    pub fn check(&self) -> &Check {
        &self.check
    }

    /// `field` was edited to `text`. Its error and the form's go (the user
    /// is acting on them), and a new host or directory drops the check
    /// done (or running) for the old one.
    pub fn set(&mut self, field: Field, text: &str) {
        if !self.draft.set(field, text) {
            return;
        }
        let key = self.check_key();
        let stale = match &self.check {
            Check::NotChecked => false,
            // An install runs to its end; its answer is dropped if the draft
            // has moved on by then.
            Check::Checking {
                intent: ReadinessIntent::InstallCli,
                ..
            } => false,
            Check::Checking { key: k, .. } | Check::Done { key: k, .. } => *k != key,
        };
        if stale {
            self.check = Check::NotChecked;
        }
    }

    /// Save `run` answered. Returns the change when it took (the form
    /// closes); a refusal stays with the draft. An answer for another save
    /// is dropped.
    pub fn finish_save(&mut self, run: u64, result: Result<MachineChange, PoolError>) -> Option<MachineChange> {
        self.draft.finish_save(run, result)
    }

    /// The readiness button's intent now: Test connection before a result
    /// for this draft, Test again after one.
    pub fn test_intent(&self) -> ReadinessIntent {
        match self.check {
            Check::Done { .. }
            | Check::Checking {
                intent: ReadinessIntent::TestAgain | ReadinessIntent::InstallCli,
                ..
            } => ReadinessIntent::TestAgain,
            _ => ReadinessIntent::Test,
        }
    }

    /// Whether Install CLI is offered: the check found the CLI missing or
    /// unusable on a platform with a release, or an install failed in a way
    /// trying again could fix.
    pub fn offers_install(&self) -> bool {
        matches!(&self.check, Check::Done { outcome, .. } if outcome.offers_install())
    }

    /// The check that verified the draft's host and directory as they stand,
    /// if any: what a save records.
    pub fn verified(&self) -> Option<&Readiness> {
        match &self.check {
            Check::Done { key, outcome } if *key == self.check_key() && outcome.is_ready() => outcome.readiness(),
            _ => None,
        }
    }

    /// The run in flight, if a check or install is running.
    pub fn running(&self) -> Option<u64> {
        match &self.check {
            Check::Checking { run, .. } => Some(*run),
            _ => None,
        }
    }

    /// What the connection check region shows now.
    pub fn panel(&self) -> Panel {
        let panel = |tone, title: &str, detail: String| Panel {
            tone,
            title: title.to_string(),
            detail,
        };
        match &self.check {
            Check::NotChecked => panel(
                Tone::Idle,
                "Not checked yet",
                "Test the connection to check SSH, the Shelbi CLI, Git, and the project directory.".into(),
            ),
            Check::Checking {
                key,
                intent: ReadinessIntent::InstallCli,
                ..
            } => panel(
                Tone::Busy,
                "Installing the Shelbi CLI…",
                format!("Setting up a compatible CLI on {}, then checking again.", key.host),
            ),
            Check::Checking { key, .. } => panel(
                Tone::Busy,
                "Checking…",
                format!("Connecting to {} over SSH.", key.host),
            ),
            Check::Done { key, outcome } => match outcome {
                CheckOutcome::InstallFailed { message, .. } => {
                    panel(Tone::Error, "Couldn’t install the CLI", message.clone())
                }
                CheckOutcome::Checked(r) => readiness_panel(r, None, key),
                CheckOutcome::Installed { note, readiness } => readiness_panel(readiness, Some(note), key),
            },
        }
    }

    /// Start `intent` for the draft. `None` for this Mac's form, while a
    /// check or install runs, and for a host or directory that can't be
    /// checked as typed (its field says why).
    pub fn begin_check(&mut self, intent: ReadinessIntent) -> Option<ReadinessRequest> {
        if !self.remote() || matches!(self.check, Check::Checking { .. }) {
            return None;
        }
        let key = self.check_key();
        let target = readiness::ReadinessTarget {
            host: key.host.clone(),
            work_dir: key.work_dir.clone(),
        };
        if let Err(e) = readiness::validate_target(&target) {
            let field = match e.field {
                TargetField::Host => Field::Host,
                TargetField::WorkDir => Field::WorkDir,
            };
            self.draft.mark(field, e.message);
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
                project: self.project().to_string(),
                machine: self.configured().map(|m| m.name.clone()),
                name: self.text(Field::Name).trim().to_string(),
                host: key.host,
                work_dir: key.work_dir,
            },
        })
    }

    /// Readiness request `run` answered. Dropped unless it's the check
    /// running now; an install that finishes after the draft's host or
    /// directory moved leaves the region not checked (the draft wasn't).
    pub fn finish_check(&mut self, run: u64, outcome: CheckOutcome) {
        if let Check::Checking { run: r, key, .. } = &self.check {
            if *r == run {
                self.check = if *key == self.check_key() {
                    Check::Done { key: key.clone(), outcome }
                } else {
                    Check::NotChecked
                };
            }
        }
    }

    /// The controls Tab visits, in order.
    #[cfg(test)]
    pub fn focus_order(&self) -> Vec<Focus> {
        self.draft.focus_order(self.offers_install())
    }

    /// Give `focus` the keyboard (a click on a field or button).
    pub fn set_focus(&mut self, focus: Focus) {
        self.draft.set_focus(focus);
    }

    /// Keep focus on a control the form shows (Install CLI comes and goes).
    pub fn settle_focus(&mut self) {
        let install = self.offers_install();
        self.draft.settle_focus(install);
    }

    /// A press of `focus` (a click, or Space / Return on it).
    pub fn press(&mut self, focus: Focus) -> Action {
        let action = self.draft.press(focus);
        self.lift(action)
    }

    /// A key the form takes: Tab and ⇧Tab, Return and Esc anywhere, Space
    /// on a button. `None` lets it through (to the field).
    pub fn key(&mut self, chord: KeyChord) -> Option<Action> {
        let install = self.offers_install();
        let action = self.draft.key(chord, install)?;
        Some(self.lift(action))
    }

    /// The shared form's action, with the check's buttons run here.
    fn lift(&mut self, action: draft::Action) -> Action {
        match action {
            draft::Action::Stay => Action::Stay,
            draft::Action::Cancel => Action::Cancel,
            draft::Action::Save(run, request) => Action::Save(run, *request),
            draft::Action::Browse => Action::Browse,
            draft::Action::Test => self.begin_check(self.test_intent()).map_or(Action::Stay, Action::Check),
            draft::Action::InstallCli => self
                .begin_check(ReadinessIntent::InstallCli)
                .map_or(Action::Stay, Action::Check),
        }
    }
}

/// The region for a check's answer (`note`: what an install did first).
fn readiness_panel(r: &Readiness, note: Option<&String>, key: &CheckKey) -> Panel {
    let with_note = |detail: String| match note {
        Some(n) => format!("{n} {detail}"),
        None => detail,
    };
    let panel = |tone, title: &str, detail: String| Panel {
        tone,
        title: title.to_string(),
        detail: with_note(detail),
    };
    let Some(report) = r.report() else {
        let message = r.connection_message(&key.host).unwrap_or_default();
        let title = match r {
            Readiness::Invalid(_) => "Can’t test this yet",
            Readiness::AuthDenied { .. } => "SSH couldn’t log in",
            Readiness::HostKey { .. } => "Host key not trusted",
            Readiness::Cancelled => return panel(Tone::Idle, "Not checked yet", message),
            _ => "Couldn’t connect",
        };
        return panel(Tone::Error, title, message);
    };
    let issues = report.issues();
    let Some(first) = issues.first() else {
        return panel(Tone::Success, "Connection successful", report.ready_summary());
    };
    let title = match first {
        Issue::UnsupportedPlatform(_) => "Unsupported platform",
        Issue::Cli(_) => "Shelbi CLI needed",
        Issue::Git(_) => "Git needed",
        Issue::Dir(_) => "Project directory needs attention",
    };
    let detail = issues
        .iter()
        .map(|i| i.message(&key.work_dir))
        .collect::<Vec<_>>()
        .join(" ");
    panel(Tone::Warning, title, detail)
}

/// Sample outcomes (tests and `--visual-figma`): a remote's answers as the
/// readiness check would parse them.
#[cfg(any(test, feature = "visual-check"))]
pub mod samples {
    use shelbi_orchestrator::machine::{parse_version, Platform, Version};
    use shelbi_orchestrator::machine_readiness::{CliStatus, DirStatus, GitStatus, Readiness, RemoteReport};

    fn platform() -> Result<Platform, shelbi_orchestrator::machine::PlatformError> {
        shelbi_orchestrator::machine::detect_platform("Linux", "x86_64")
    }

    fn hub() -> Version {
        shelbi_orchestrator::machine::hub_version()
    }

    /// Everything checked out.
    pub fn ready() -> Readiness {
        Readiness::Reached(RemoteReport {
            platform: platform(),
            cli: CliStatus::Compatible {
                path: "/home/me/.shelbi/bin/shelbi".into(),
                version: hub(),
                source: shelbi_state::machine_state::SOURCE_SHELBI_BIN,
            },
            git: GitStatus::Usable {
                path: "/usr/bin/git".into(),
                version: "git version 2.43.0".into(),
            },
            dir: DirStatus::Usable,
        })
    }

    /// Reached, but an older CLI is on the PATH.
    pub fn needs_cli() -> Readiness {
        let Readiness::Reached(mut report) = ready() else { unreachable!() };
        let old = parse_version("0.1.0").unwrap();
        report.cli = CliStatus::Incompatible {
            path: "/usr/local/bin/shelbi".into(),
            version: Some(old),
            compat: Some(shelbi_orchestrator::machine::classify_compat(&old, &hub())),
        };
        Readiness::Reached(report)
    }

    /// Reached, with the project directory missing.
    pub fn no_directory() -> Readiness {
        let Readiness::Reached(mut report) = ready() else { unreachable!() };
        report.dir = DirStatus::Missing;
        Readiness::Reached(report)
    }

    /// SSH timed out.
    pub fn unreachable() -> Readiness {
        Readiness::Unreachable {
            detail: "ssh: connect to host gpu.lan port 22: Operation timed out".into(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use shelbi_core::MachineKind;
    use shelbi_state::keymap::{Key, Mods};
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

    use super::samples;

    fn checked(r: Readiness) -> CheckOutcome {
        CheckOutcome::Checked(r)
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
        f.finish_check(request.run, checked(samples::ready()));
        assert!(f.check().clone() == Check::Done { key: f.check_key(), outcome: checked(samples::ready()) });
        assert!(f.verified().is_some());
        assert_eq!(f.test_intent(), ReadinessIntent::TestAgain);

        // Tags don't matter to it; the directory does.
        f.set(Field::Tags, "linux");
        assert!(f.verified().is_some());
        f.set(Field::WorkDir, "/srv/my-project");
        assert_eq!(f.check(), &Check::NotChecked, "a new directory isn't checked");
        assert!(f.verified().is_none(), "editing invalidates the success");

        // An answer for a host that's since changed is dropped.
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        f.set(Field::Host, "build-3");
        assert_eq!(f.check(), &Check::NotChecked);
        assert_eq!(f.running(), None, "the stale check is no longer running");
        f.finish_check(request.run, checked(samples::ready()));
        assert_eq!(f.check(), &Check::NotChecked);

        // An older request finishing after a newer one started is dropped.
        let Action::Check(old) = f.press(Focus::Test) else { panic!() };
        f.set(Field::Host, "build-4");
        let Action::Check(new) = f.press(Focus::Test) else { panic!() };
        f.finish_check(old.run, checked(samples::ready()));
        assert!(matches!(f.check(), Check::Checking { run, .. } if *run == new.run));
        f.finish_check(new.run, checked(samples::unreachable()));
        assert!(f.verified().is_none());
    }

    #[test]
    fn install_cli_is_offered_only_when_the_check_says_so() {
        let mut f = MachineForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert!(!f.focus_order().contains(&Focus::InstallCli));
        f.finish_check(request.run, checked(samples::unreachable()));
        assert!(!f.offers_install(), "a failed connection isn't a missing CLI");

        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        f.finish_check(request.run, checked(samples::needs_cli()));
        assert!(f.offers_install() && f.focus_order().contains(&Focus::InstallCli));
        let Action::Check(install) = f.press(Focus::InstallCli) else { panic!() };
        assert_eq!(install.intent, ReadinessIntent::InstallCli);
        assert_eq!(f.panel().title, "Installing the Shelbi CLI…");
        f.finish_check(
            install.run,
            CheckOutcome::InstallFailed {
                message: "cannot write ~/.shelbi/bin".into(),
                retry: true,
            },
        );
        assert_eq!(f.panel().tone, Tone::Error);
        assert!(f.offers_install(), "a failed install can be retried");
        let Action::Check(retry) = f.press(Focus::InstallCli) else { panic!() };
        f.finish_check(
            retry.run,
            CheckOutcome::Installed {
                note: "Installed Shelbi in ~/.shelbi/bin.".into(),
                readiness: samples::ready(),
            },
        );
        assert!(!f.offers_install());
        assert!(f.verified().is_some(), "verified again after the install");
        let panel = f.panel();
        assert_eq!((panel.tone, panel.title.as_str()), (Tone::Success, "Connection successful"));
        assert!(panel.detail.starts_with("Installed Shelbi"));
        f.set_focus(Focus::InstallCli);
        f.settle_focus();
        assert_eq!(f.focus(), Focus::Test);
    }

    #[test]
    fn an_install_keeps_running_through_edits_and_its_answer_goes_stale() {
        let mut f = MachineForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        f.finish_check(request.run, checked(samples::needs_cli()));
        let Action::Check(install) = f.press(Focus::InstallCli) else { panic!() };
        f.set(Field::Host, "build-2");
        assert_eq!(f.running(), Some(install.run), "an install isn't dropped mid-way");
        assert!(f.panel().detail.contains("build-server"), "it says where it's installing");
        assert_eq!(f.press(Focus::Test), Action::Stay, "no second job while it runs");
        f.finish_check(
            install.run,
            CheckOutcome::Installed {
                note: String::new(),
                readiness: samples::ready(),
            },
        );
        assert_eq!(f.check(), &Check::NotChecked, "build-2 wasn't checked");
        assert!(f.verified().is_none());
    }

    #[test]
    fn the_panel_says_what_needs_attention() {
        let mut f = MachineForm::configure("p", &remote(), 0, None);
        assert_eq!(f.panel().tone, Tone::Idle);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert_eq!((f.panel().tone, f.panel().title.as_str()), (Tone::Busy, "Checking…"));
        f.finish_check(request.run, checked(samples::no_directory()));
        let panel = f.panel();
        assert_eq!((panel.tone, panel.title.as_str()), (Tone::Warning, "Project directory needs attention"));
        assert!(panel.detail.contains("/home/me/my-project doesn’t exist"));
        assert!(!f.offers_install());

        for (r, title) in [
            (samples::unreachable(), "Couldn’t connect"),
            (Readiness::AuthDenied { detail: "Permission denied (publickey).".into() }, "SSH couldn’t log in"),
            (Readiness::HostKey { detail: "Host key verification failed.".into() }, "Host key not trusted"),
            (samples::needs_cli(), "Shelbi CLI needed"),
        ] {
            let Action::Check(request) = f.press(Focus::Test) else { panic!() };
            f.finish_check(request.run, checked(r));
            assert_eq!(f.panel().title, title);
            assert_ne!(f.panel().tone, Tone::Success);
        }
    }

    #[test]
    fn a_draft_that_cant_be_checked_marks_its_field() {
        let mut f = MachineForm::add("p");
        f.set(Field::Host, "-oProxyCommand=x");
        f.set(Field::WorkDir, "/srv/p");
        assert_eq!(f.press(Focus::Test), Action::Stay);
        assert!(f.field_error(Field::Host).is_some());
        f.set(Field::Host, "box");
        f.set(Field::WorkDir, "~/p");
        assert_eq!(f.press(Focus::Test), Action::Stay);
        assert!(f.field_error(Field::WorkDir).unwrap().contains("home directory"));
        assert_eq!(f.focus(), Focus::Field(Field::WorkDir));
        assert_eq!(f.check(), &Check::NotChecked);
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
