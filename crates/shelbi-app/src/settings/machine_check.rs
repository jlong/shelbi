//! The machine forms' connection check, toolkit-free
//! (`dt-machine-readiness-setup`, moved out of the desktop by
//! `tui-settings-machine-readiness` so the TUI's form runs the same one).
//!
//! [`ReadinessForm`] is the shared draft ([`super::machine_form`]) with its
//! connection check region over it. Test connection, Test again and Install
//! CLI each make a [`ReadinessRequest`] for the draft as it stands (saved or
//! not, valid or not: [`validate_target`] refuses a host or directory that
//! can't be checked as typed, on its field, before anything runs). The client
//! runs it off its UI thread through a [`ReadinessService`]
//! (`shelbi_orchestrator::machine_readiness::service` is the live one: one SSH
//! round trip that checks SSH, the platform, the Shelbi CLI, Git and the
//! project directory; or the shared remote CLI setup, then that check again)
//! and hands the [`CheckOutcome`] back to [`ReadinessForm::finish_check`].
//!
//! - **What a check found** is a [`Report`]: how far SSH got ([`Reach`]), one
//!   [`Stage`] per thing checked, in order, with a concise result, the
//!   actionable messages for whatever needs attention, whether Install CLI
//!   can help, and the verified binary. [`ReadinessForm::panel`] says what
//!   the region shows; only a report where every stage passed is "Connection
//!   successful".
//! - **Binding.** A result counts only for the host and directory it was
//!   checked with ([`CheckKey`]): editing either drops a check back to not
//!   checked (the client stops it, [`CheckStop`]); an install keeps running
//!   and its answer is dropped if the draft has moved on. An answer for an
//!   older request is dropped. One job at a time.
//! - **Nothing is saved** by a check or an install: the draft is persisted
//!   only by Add machine / Save changes. A verified binary is recorded (hub
//!   state, not project config) only for a machine the project has: a check
//!   of a configured machine at its saved host ([`ReadinessRequest::record`]),
//!   or the check that verified a draft once its save commits
//!   ([`SaveVerified::record`]).

use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use shelbi_core::pool::{usable_ssh_host, MachineChange, PoolError};
use shelbi_core::{Machine, MachineKind};
use shelbi_state::keymap::KeyChord;

use super::machine_form::{self as draft, Controls};

pub use super::machine_form::{CheckKey, Field, Focus, SaveRequest};

// ===========================================================================
// The target
// ===========================================================================

/// Which part of a target can't be checked as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetField {
    Host,
    WorkDir,
}

/// Why a target can't be checked at all, before any SSH: what to correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetError {
    pub field: TargetField,
    pub message: String,
}

/// Validate a target before anything runs: a destination `ssh` takes as a host
/// (no leading `-` it would read as an option, no whitespace), and an absolute
/// remote directory. The same rules the machine service saves by.
pub fn validate_target(host: &str, work_dir: &str) -> Result<(), TargetError> {
    let host = host.trim();
    let invalid = |field, message: String| Err(TargetError { field, message });
    if host.is_empty() {
        return invalid(TargetField::Host, "Enter the SSH host to test.".into());
    }
    if !usable_ssh_host(host) {
        return invalid(
            TargetField::Host,
            format!("`{}` isn’t a host SSH can connect to.", host.escape_debug()),
        );
    }
    let dir = work_dir.trim();
    if dir.is_empty() {
        return invalid(
            TargetField::WorkDir,
            "Enter the project’s directory on the machine to test it.".into(),
        );
    }
    if dir.chars().any(char::is_control) {
        return invalid(TargetField::WorkDir, format!("`{}` isn’t a valid path.", dir.escape_debug()));
    }
    if !dir.starts_with('/') {
        let hint = if dir == "~" || dir.starts_with("~/") {
            " Write the remote home directory out in full."
        } else {
            ""
        };
        return invalid(TargetField::WorkDir, format!("`{dir}` isn’t an absolute path.{hint}"));
    }
    Ok(())
}

// ===========================================================================
// What a check found
// ===========================================================================

/// One thing a readiness check verifies, in the order it's reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageKind {
    /// SSH connects and logs in.
    Ssh,
    /// The machine's OS and CPU have a Shelbi release (or a CLI already).
    Platform,
    /// A Shelbi CLI compatible with this one.
    Cli,
    /// Git, on the PATH Shelbi's SSH commands see.
    Git,
    /// The project directory: a writable checkout's root.
    Directory,
}

impl StageKind {
    pub const ALL: [StageKind; 5] = [
        StageKind::Ssh,
        StageKind::Platform,
        StageKind::Cli,
        StageKind::Git,
        StageKind::Directory,
    ];

    /// Its label, short.
    pub fn label(self) -> &'static str {
        match self {
            StageKind::Ssh => "SSH",
            StageKind::Platform => "Platform",
            StageKind::Cli => "Shelbi CLI",
            StageKind::Git => "Git",
            StageKind::Directory => "Project directory",
        }
    }
}

/// Where a stage stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageState {
    /// Not checked (yet): nothing ran, or an earlier stage has to answer.
    Waiting,
    /// Being checked now.
    Running,
    Passed,
    /// Reached and read, and it needs something done.
    Attention,
    /// SSH couldn't connect or log in.
    Failed,
    /// Couldn't be checked because an earlier stage failed.
    Skipped,
}

/// One stage's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub kind: StageKind,
    pub state: StageState,
    /// What it found, in a few words ("Linux x86_64", "Not installed").
    pub result: String,
}

impl Stage {
    pub fn new(kind: StageKind, state: StageState, result: impl Into<String>) -> Self {
        Stage {
            kind,
            state,
            result: result.into(),
        }
    }
}

/// How far SSH got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// It ran the check: the stages say what it found.
    Reached,
    /// The target can't be checked as typed (nothing ran).
    Invalid(TargetField),
    /// SSH couldn't connect.
    Unreachable,
    /// SSH didn't answer within the check's deadline.
    TimedOut,
    /// SSH connected but couldn't log in (keys, agent, user).
    AuthDenied,
    /// SSH refused the host's key.
    HostKey,
    /// The check was stopped before it answered.
    Cancelled,
}

/// The binary a ready check verified: what the hub records for the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCli {
    pub path: String,
    pub version: String,
    /// `machine_state`'s source tag (on the PATH, or Shelbi's managed copy).
    pub source: String,
}

/// What a readiness check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub reach: Reach,
    /// Every [`StageKind`], in order.
    pub stages: Vec<Stage>,
    /// What needs attention and what to do about it, in order. Empty only
    /// when ready.
    pub messages: Vec<String>,
    /// Whether Install CLI can help: SSH reached it, its platform has a
    /// release, and its CLI isn't usable. A connection failure never offers it.
    pub offers_install: bool,
    /// The compatible binary, when everything checked out.
    pub verified: Option<VerifiedCli>,
}

impl Report {
    /// SSH didn't get as far as running the check (`reach` isn't
    /// [`Reach::Reached`]): the SSH stage says `result`, the rest weren't
    /// checked, and `message` says what to do.
    pub fn unreached(reach: Reach, result: impl Into<String>, message: impl Into<String>) -> Self {
        let mut stages = vec![Stage::new(StageKind::Ssh, StageState::Failed, result)];
        stages.extend(
            StageKind::ALL[1..]
                .iter()
                .map(|k| Stage::new(*k, StageState::Skipped, "Not checked")),
        );
        Report {
            reach,
            stages,
            messages: vec![message.into()],
            offers_install: false,
            verified: None,
        }
    }

    /// A check stopped before it answered.
    pub fn cancelled() -> Self {
        Report {
            reach: Reach::Cancelled,
            stages: StageKind::ALL
                .iter()
                .map(|k| Stage::new(*k, StageState::Waiting, "Not checked"))
                .collect(),
            messages: vec!["The check was stopped before it answered; nothing was verified.".into()],
            offers_install: false,
            verified: None,
        }
    }

    /// SSH, the platform, a compatible CLI, Git and the project directory
    /// all check out. The only report a form may show as a successful
    /// connection.
    pub fn is_ready(&self) -> bool {
        self.reach == Reach::Reached
            && self.verified.is_some()
            && self.stages.iter().all(|s| s.state == StageState::Passed)
    }

    pub fn stage(&self, kind: StageKind) -> Option<&Stage> {
        self.stages.iter().find(|s| s.kind == kind)
    }

    /// The first stage that didn't pass.
    pub fn first_problem(&self) -> Option<&Stage> {
        self.stages
            .iter()
            .find(|s| matches!(s.state, StageState::Attention | StageState::Failed))
    }
}

/// What a readiness request found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Test connection / Test again: the check's answer.
    Checked(Report),
    /// Install CLI ran the shared remote setup (`note` says what it did:
    /// already there, installed, or copied from this computer; `changed`
    /// when it put a binary on the machine), then checked again.
    Installed { note: String, changed: bool, report: Report },
    /// Install CLI's setup failed: why, and what to do by hand. `retry` when
    /// installing again could help (not for a connection that failed).
    InstallFailed { message: String, retry: bool },
}

impl CheckOutcome {
    /// The report the outcome carries, if a check ran.
    pub fn report(&self) -> Option<&Report> {
        match self {
            CheckOutcome::Checked(r) | CheckOutcome::Installed { report: r, .. } => Some(r),
            CheckOutcome::InstallFailed { .. } => None,
        }
    }

    /// Everything checked out: the only outcome shown as a successful
    /// connection.
    pub fn is_ready(&self) -> bool {
        self.report().is_some_and(Report::is_ready)
    }

    /// Whether Install CLI is offered after it.
    pub fn offers_install(&self) -> bool {
        match self {
            CheckOutcome::InstallFailed { retry, .. } => *retry,
            other => other.report().is_some_and(|r| r.offers_install),
        }
    }

    /// What an install changed on `host`, said for someone who may no longer
    /// see the form (it closed, or the draft moved on): `None` when it
    /// changed nothing there.
    pub fn side_effect(&self, host: &str) -> Option<String> {
        match self {
            CheckOutcome::Installed { note, changed: true, .. } => Some(format!(
                "On {host}: {note} It stays there; the machine’s configuration is only saved by Add machine \
                 or Save changes."
            )),
            _ => None,
        }
    }
}

// ===========================================================================
// Requests and the service
// ===========================================================================

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
/// may not be saved yet.
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
    /// never expanded on this computer.
    pub work_dir: String,
    /// The configured machine's name when the draft still names its saved
    /// host: a ready answer is true of a machine the project has, so it's
    /// recorded at once ([`ReadinessRequest::record`]).
    pub record_as: Option<String>,
}

/// The structured handoff to the readiness service: what to do, for which
/// draft. `run` comes back with its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessRequest {
    pub run: u64,
    pub intent: ReadinessIntent,
    pub target: ReadinessTarget,
}

/// A verified binary to record for a machine the project has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The machine's name in the project.
    pub machine: String,
    /// The SSH destination it was verified on.
    pub host: String,
    pub cli: VerifiedCli,
}

impl ReadinessRequest {
    /// What to record when `outcome` answers it: a ready check (or install)
    /// of a configured machine at its saved host, whatever the form did
    /// meanwhile (it's still true of that machine). A draft's waits for its
    /// save.
    pub fn record(&self, outcome: &CheckOutcome) -> Option<Record> {
        let machine = self.target.record_as.clone()?;
        let report = outcome.report().filter(|r| r.is_ready())?;
        Some(Record {
            machine,
            host: self.target.host.clone(),
            cli: report.verified.clone()?,
        })
    }
}

/// The readiness service's run: the request, and a flag that trips when no
/// one will read the answer (a check stops; an install runs to its end).
pub type CheckFn = dyn Fn(&ReadinessRequest, Arc<AtomicBool>) -> CheckOutcome + Send + Sync;
/// Record a verified machine's binary (hub state, never project config).
pub type RecordFn = dyn Fn(&Record) + Send + Sync;

/// The readiness operations (`shelbi_orchestrator::machine_readiness::service`
/// builds the live one; tests script their own).
#[derive(Clone)]
pub struct ReadinessService {
    pub check: Arc<CheckFn>,
    pub record: Arc<RecordFn>,
}

impl ReadinessService {
    /// Run `request`, then record what it verified of a configured machine,
    /// unless it was stopped meanwhile (an answer no one asked for any more
    /// counts for nothing). Blocks: call it off the UI thread.
    pub fn run(&self, request: &ReadinessRequest, cancel: Arc<AtomicBool>) -> CheckOutcome {
        let outcome = (self.check)(request, cancel.clone());
        if cancel.load(Ordering::Acquire) {
            return outcome;
        }
        if let Some(record) = request.record(&outcome) {
            (self.record)(&record);
        }
        outcome
    }
}

/// The check that verified a draft as it was saved: recorded once the save
/// commits that host ([`ReadinessForm::save_verified`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveVerified {
    pub host: String,
    pub cli: VerifiedCli,
}

impl SaveVerified {
    /// The record to write now `change` committed, if it committed the
    /// verified host for an SSH machine.
    pub fn record(self, change: &MachineChange) -> Option<Record> {
        let saved = &change.machine;
        let host = saved.host.clone().unwrap_or_else(|| saved.name.clone());
        (host == self.host && saved.kind == MachineKind::Ssh).then(|| Record {
            machine: saved.name.clone(),
            host,
            cli: self.cli,
        })
    }
}

/// The form's running check and whether the form has closed, shared between
/// the form's owner and whatever runs the check: closing stops the check at
/// once, and a closed form takes no answer.
#[derive(Clone, Default)]
pub struct CheckStop(Arc<Mutex<StopState>>);

#[derive(Default)]
struct StopState {
    closed: bool,
    running: Option<(u64, Arc<AtomicBool>)>,
}

impl std::fmt::Debug for CheckStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lock();
        f.debug_struct("CheckStop")
            .field("closed", &s.closed)
            .field("running", &s.running.as_ref().map(|(run, _)| *run))
            .finish()
    }
}

impl CheckStop {
    fn lock(&self) -> std::sync::MutexGuard<'_, StopState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Check `run` starts: its stop flag (already tripped on a closed form).
    pub fn start(&self, run: u64) -> Arc<AtomicBool> {
        let mut s = self.lock();
        let flag = Arc::new(AtomicBool::new(s.closed));
        s.running = Some((run, flag.clone()));
        flag
    }

    /// Check `run` answered.
    pub fn finish(&self, run: u64) {
        let mut s = self.lock();
        if s.running.as_ref().is_some_and(|(r, _)| *r == run) {
            s.running = None;
        }
    }

    /// Stop the running check unless it's `live` (the one the form still
    /// shows).
    pub fn stop_unless(&self, live: Option<u64>) {
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

    /// The running check's flag, if one runs.
    pub fn running_flag(&self) -> Option<Arc<AtomicBool>> {
        self.lock().running.as_ref().map(|(_, f)| f.clone())
    }
}

// ===========================================================================
// The form
// ===========================================================================

/// What a key or a button asks the client to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing outside the form (it may have moved focus).
    Stay,
    /// Discard the draft and close.
    Cancel,
    /// Run this save.
    Save(u64, SaveRequest),
    /// Open the folder chooser for the local machine's directory.
    Browse,
    /// Run this readiness request.
    Check(ReadinessRequest),
}

/// The region's tone: its icon and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Not checked yet.
    Idle,
    /// A check or install is running.
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

/// The region's title and detail before anything checked the draft.
pub const NOT_CHECKED_TITLE: &str = "Not checked yet";
pub const NOT_CHECKED_DETAIL: &str =
    "Test the connection to check SSH, the Shelbi CLI, Git, and the project directory.";

/// An add or configure form: the shared draft (read through `Deref`) and the
/// connection check region.
#[derive(Debug, Clone)]
pub struct ReadinessForm {
    draft: draft::MachineForm,
    check: Check,
    /// Readiness requests made, for their run ids.
    runs: u64,
}

impl Deref for ReadinessForm {
    type Target = draft::MachineForm;

    fn deref(&self) -> &draft::MachineForm {
        &self.draft
    }
}

impl ReadinessForm {
    /// Add machine: an empty SSH draft, with the check's buttons.
    pub fn add(project: &str) -> Self {
        Self::over(draft::MachineForm::add(project))
    }

    /// Configure `machine` (as the page read it), which hosts `workspaces`
    /// of the project's workspaces. The local machine's directory is
    /// shortened against `home`.
    pub fn configure(project: &str, machine: &Machine, workspaces: usize, home: Option<&Path>) -> Self {
        Self::over(draft::MachineForm::configure(project, machine, workspaces, home))
    }

    fn over(draft: draft::MachineForm) -> Self {
        ReadinessForm {
            draft: draft.with_controls(Controls {
                browse: false,
                checks: true,
            }),
            check: Check::NotChecked,
            runs: 0,
        }
    }

    /// The form with Choose… beside the local machine's directory (a client
    /// with a native folder chooser).
    pub fn with_browse(mut self) -> Self {
        self.draft = self.draft.with_controls(Controls {
            browse: true,
            checks: true,
        });
        self
    }

    /// The draft itself (a client's own field editing reads it).
    pub fn draft(&self) -> &draft::MachineForm {
        &self.draft
    }

    pub fn check(&self) -> &Check {
        &self.check
    }

    /// `field` was edited to `text`. Its error and the form's go (the user
    /// is acting on them), and a new host or directory drops the check
    /// done (or running) for the old one: the client stops a check that's no
    /// longer [`Self::running`].
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

    /// The readiness button's label now.
    pub fn test_label(&self) -> &'static str {
        match self.test_intent() {
            ReadinessIntent::TestAgain => "Test again",
            _ => "Test connection",
        }
    }

    /// Whether Install CLI is offered: the check found the CLI missing or
    /// unusable on a platform with a release, or an install failed in a way
    /// trying again could fix.
    pub fn offers_install(&self) -> bool {
        matches!(&self.check, Check::Done { outcome, .. } if outcome.offers_install())
    }

    /// The check that verified the draft's host and directory as they stand,
    /// if any.
    pub fn verified(&self) -> Option<&Report> {
        match &self.check {
            Check::Done { key, outcome } if *key == self.check_key() && outcome.is_ready() => outcome.report(),
            _ => None,
        }
    }

    /// What a save started now records once it commits: the check that
    /// verified the draft, for its host.
    pub fn save_verified(&self) -> Option<SaveVerified> {
        let cli = self.verified()?.verified.clone()?;
        Some(SaveVerified {
            host: self.check_key().host,
            cli,
        })
    }

    /// The run in flight, if a check or install is running.
    pub fn running(&self) -> Option<u64> {
        match &self.check {
            Check::Checking { run, .. } => Some(*run),
            _ => None,
        }
    }

    /// Whether an install is running (it isn't stopped by an edit or a
    /// close).
    pub fn installing(&self) -> bool {
        matches!(
            self.check,
            Check::Checking {
                intent: ReadinessIntent::InstallCli,
                ..
            }
        )
    }

    /// Stop the check running now (not an install, which runs to its end):
    /// the region says the check was stopped. Returns the run stopped, for
    /// the client to trip its flag.
    pub fn stop_check(&mut self) -> Option<u64> {
        let Check::Checking { run, key, intent } = &self.check else {
            return None;
        };
        if *intent == ReadinessIntent::InstallCli {
            return None;
        }
        let run = *run;
        self.check = Check::Done {
            key: key.clone(),
            outcome: CheckOutcome::Checked(Report::cancelled()),
        };
        Some(run)
    }

    /// What the connection check region shows now.
    pub fn panel(&self) -> Panel {
        let panel = |tone, title: &str, detail: String| Panel {
            tone,
            title: title.to_string(),
            detail,
        };
        match &self.check {
            Check::NotChecked => panel(Tone::Idle, NOT_CHECKED_TITLE, NOT_CHECKED_DETAIL.into()),
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
            Check::Done { outcome, .. } => match outcome {
                CheckOutcome::InstallFailed { message, .. } => {
                    panel(Tone::Error, "Couldn’t install the CLI", message.clone())
                }
                CheckOutcome::Checked(r) => report_panel(r, None),
                CheckOutcome::Installed { note, report, .. } => report_panel(report, Some(note)),
            },
        }
    }

    /// The stages the region lists now: what the last answer found for each,
    /// what's running, or that nothing has been checked.
    pub fn stages(&self) -> Vec<Stage> {
        let all = |f: &dyn Fn(StageKind) -> Stage| StageKind::ALL.iter().map(|k| f(*k)).collect();
        match &self.check {
            Check::NotChecked => all(&|k| Stage::new(k, StageState::Waiting, "Not checked")),
            Check::Checking {
                key,
                intent: ReadinessIntent::InstallCli,
                ..
            } => all(&|k| match k {
                StageKind::Cli => Stage::new(k, StageState::Running, format!("Installing on {}", key.host)),
                _ => Stage::new(k, StageState::Waiting, "Checked again after the install"),
            }),
            Check::Checking { key, .. } => all(&|k| match k {
                StageKind::Ssh => Stage::new(k, StageState::Running, format!("Connecting to {}", key.host)),
                _ => Stage::new(k, StageState::Waiting, "Waiting for SSH"),
            }),
            Check::Done { outcome, .. } => match outcome.report() {
                Some(r) => r.stages.clone(),
                // The install failed before anything was checked again.
                None => all(&|k| match k {
                    StageKind::Cli => Stage::new(k, StageState::Failed, "Install failed"),
                    _ => Stage::new(k, StageState::Waiting, "Not checked again"),
                }),
            },
        }
    }

    /// Start `intent` for the draft. `None` for the local machine's form,
    /// while a check or install runs, and for a host or directory that can't
    /// be checked as typed (its field says why, and takes the keyboard).
    pub fn begin_check(&mut self, intent: ReadinessIntent) -> Option<ReadinessRequest> {
        if !self.remote() || matches!(self.check, Check::Checking { .. }) {
            return None;
        }
        let key = self.check_key();
        if let Err(e) = validate_target(&key.host, &key.work_dir) {
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
                record_as: self.committed_name(&key).map(str::to_string),
                host: key.host,
                work_dir: key.work_dir,
            },
        })
    }

    /// Readiness request `run` answered. Dropped unless it's the job
    /// running now; an install that finishes after the draft's host or
    /// directory moved leaves the region not checked (the draft wasn't).
    /// Returns whether the region now shows it (`false` for an answer
    /// dropped, including an install's for a draft that moved on).
    pub fn finish_check(&mut self, run: u64, outcome: CheckOutcome) -> bool {
        let Check::Checking { run: r, key, .. } = &self.check else {
            return false;
        };
        if *r != run {
            return false;
        }
        if *key != self.check_key() {
            self.check = Check::NotChecked;
            return false;
        }
        self.check = Check::Done {
            key: key.clone(),
            outcome,
        };
        true
    }

    /// The controls Tab visits, in order.
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

    /// The shared draft's action, with the check's buttons run here.
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

/// The region for a report (`note`: what an install did first).
fn report_panel(r: &Report, note: Option<&String>) -> Panel {
    let detail = {
        let messages = if r.is_ready() {
            "Shelbi, Git, and project directory are ready.".to_string()
        } else {
            r.messages.join(" ")
        };
        match note {
            Some(n) if !n.is_empty() => format!("{n} {messages}"),
            _ => messages,
        }
    };
    let panel = |tone, title: &str| Panel {
        tone,
        title: title.to_string(),
        detail: detail.clone(),
    };
    match r.reach {
        Reach::Invalid(_) => return panel(Tone::Error, "Can’t test this yet"),
        Reach::Unreachable => return panel(Tone::Error, "Couldn’t connect"),
        Reach::TimedOut => return panel(Tone::Error, "Connection timed out"),
        Reach::AuthDenied => return panel(Tone::Error, "SSH couldn’t log in"),
        Reach::HostKey => return panel(Tone::Error, "Host key not trusted"),
        Reach::Cancelled => return panel(Tone::Idle, "Check stopped"),
        Reach::Reached => {}
    }
    if r.is_ready() {
        return panel(Tone::Success, "Connection successful");
    }
    let title = match r.first_problem().map(|s| s.kind) {
        Some(StageKind::Platform) => "Unsupported platform",
        Some(StageKind::Cli) => "Shelbi CLI needed",
        Some(StageKind::Git) => "Git needed",
        Some(StageKind::Directory) => "Project directory needs attention",
        Some(StageKind::Ssh) | None => "Needs attention",
    };
    panel(Tone::Warning, title)
}

/// Sample reports (tests and the desktop's `--visual-figma`), shaped as the
/// live service reports a remote's answers.
#[cfg(any(test, feature = "test-support"))]
pub mod samples {
    use super::*;

    fn passed(kind: StageKind, result: &str) -> Stage {
        Stage::new(kind, StageState::Passed, result)
    }

    /// Everything checked out.
    pub fn ready() -> Report {
        Report {
            reach: Reach::Reached,
            stages: vec![
                passed(StageKind::Ssh, "Connected"),
                passed(StageKind::Platform, "Linux x86_64"),
                passed(StageKind::Cli, "shelbi 0.9.0 at ~/.shelbi/bin/shelbi"),
                passed(StageKind::Git, "git version 2.43.0"),
                passed(StageKind::Directory, "Checkout root"),
            ],
            messages: vec![],
            offers_install: false,
            verified: Some(VerifiedCli {
                path: "/home/me/.shelbi/bin/shelbi".into(),
                version: "0.9.0".into(),
                source: shelbi_state::machine_state::SOURCE_SHELBI_BIN.into(),
            }),
        }
    }

    /// `kind` needs attention (`result`, `message`).
    pub fn attention(kind: StageKind, result: &str, message: &str, install: bool) -> Report {
        let mut r = ready();
        for s in &mut r.stages {
            if s.kind == kind {
                *s = Stage::new(kind, StageState::Attention, result);
            }
        }
        r.messages = vec![message.to_string()];
        r.offers_install = install;
        r.verified = None;
        r
    }

    /// Reached, but an older CLI is on the PATH.
    pub fn needs_cli() -> Report {
        attention(
            StageKind::Cli,
            "shelbi 0.1.0 is too old",
            "shelbi 0.1.0 at /usr/local/bin/shelbi is older than this app (0.9.0). Install CLI adds a \
             compatible build in ~/.shelbi/bin and leaves that one as it is.",
            true,
        )
    }

    /// Reached, with the project directory missing.
    pub fn no_directory() -> Report {
        attention(
            StageKind::Directory,
            "Doesn’t exist",
            "/home/me/my-project doesn’t exist. Clone the project there (or correct the path), then test again.",
            false,
        )
    }

    /// SSH couldn't connect.
    pub fn unreachable() -> Report {
        Report::unreached(
            Reach::Unreachable,
            "Unreachable",
            "SSH couldn’t reach gpu.lan: ssh: connect to host gpu.lan port 22: Connection refused.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn checked(r: Report) -> CheckOutcome {
        CheckOutcome::Checked(r)
    }

    #[test]
    fn validation_names_the_field_to_correct() {
        let field = |h: &str, d: &str| validate_target(h, d).err().map(|e| e.field);
        assert_eq!(field("", "/srv/p"), Some(TargetField::Host));
        assert_eq!(field("-oProxyCommand=x", "/srv/p"), Some(TargetField::Host));
        assert_eq!(field("my box", "/srv/p"), Some(TargetField::Host));
        assert_eq!(field("box", ""), Some(TargetField::WorkDir));
        assert_eq!(field("box", "srv/p"), Some(TargetField::WorkDir));
        assert!(validate_target("box", "~/p").unwrap_err().message.contains("home directory"));
        assert_eq!(field(" box ", " /srv/p "), None);
    }

    #[test]
    fn add_has_the_check_and_no_browse() {
        let f = ReadinessForm::add("p");
        assert!(f.remote());
        assert_eq!(f.check(), &Check::NotChecked);
        assert_eq!(f.panel().title, NOT_CHECKED_TITLE);
        assert!(f.focus_order().contains(&Focus::Test));
        assert!(!f.focus_order().contains(&Focus::Browse));
        assert!(f.stages().iter().all(|s| s.state == StageState::Waiting), "nothing checked yet");
    }

    #[test]
    fn the_local_machine_has_no_check_and_browse_is_opt_in() {
        let f = ReadinessForm::configure("p", &local(), 3, Some(Path::new("/Users/me")));
        assert_eq!(
            f.focus_order(),
            [
                Focus::Field(Field::Name),
                Focus::Field(Field::WorkDir),
                Focus::Field(Field::Tags),
                Focus::Cancel,
                Focus::Submit,
            ]
        );
        let mut f = f.with_browse();
        assert!(f.focus_order().contains(&Focus::Browse));
        assert_eq!(f.begin_check(ReadinessIntent::Test), None, "nothing to test");
    }

    #[test]
    fn checks_count_only_for_their_host_and_directory() {
        let mut f = ReadinessForm::configure("p", &remote(), 2, None);
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
                record_as: Some("Build server".into()),
            }
        );
        assert!(matches!(f.check(), Check::Checking { .. }));
        assert_eq!(f.press(Focus::Test), Action::Stay, "one at a time");
        assert!(f.finish_check(request.run, checked(samples::ready())));
        assert!(f.verified().is_some());
        assert_eq!(f.test_intent(), ReadinessIntent::TestAgain);
        assert_eq!(f.test_label(), "Test again");

        // Tags don't matter to it; the directory does.
        f.set(Field::Tags, "linux");
        assert!(f.verified().is_some());
        f.set(Field::WorkDir, "/srv/my-project");
        assert_eq!(f.check(), &Check::NotChecked, "a new directory isn't checked");
        assert!(f.verified().is_none(), "editing invalidates the success");

        // An answer for a host that's since changed is dropped.
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert_eq!(
            request.target.record_as.as_deref(),
            Some("Build server"),
            "the binary is the saved host's, whatever the directory"
        );
        f.set(Field::Host, "build-3");
        assert_eq!(f.check(), &Check::NotChecked);
        assert_eq!(f.running(), None, "the stale check is no longer running");
        assert!(!f.finish_check(request.run, checked(samples::ready())));
        assert_eq!(f.check(), &Check::NotChecked);

        // An older request finishing after a newer one started is dropped.
        let Action::Check(old) = f.press(Focus::Test) else { panic!() };
        f.set(Field::Host, "build-4");
        let Action::Check(new) = f.press(Focus::Test) else { panic!() };
        assert!(!f.finish_check(old.run, checked(samples::ready())));
        assert!(matches!(f.check(), Check::Checking { run, .. } if *run == new.run));
        f.finish_check(new.run, checked(samples::unreachable()));
        assert!(f.verified().is_none());
    }

    #[test]
    fn install_cli_is_offered_only_when_the_check_says_so() {
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert!(!f.focus_order().contains(&Focus::InstallCli));
        f.finish_check(request.run, checked(samples::unreachable()));
        assert!(!f.offers_install(), "a failed connection isn't a missing CLI");

        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        f.finish_check(request.run, checked(samples::needs_cli()));
        assert!(f.offers_install() && f.focus_order().contains(&Focus::InstallCli));
        let Action::Check(install) = f.press(Focus::InstallCli) else { panic!() };
        assert_eq!(install.intent, ReadinessIntent::InstallCli);
        assert!(f.installing());
        assert_eq!(f.panel().title, "Installing the Shelbi CLI…");
        let cli = f.stages().into_iter().find(|s| s.kind == StageKind::Cli).unwrap();
        assert_eq!(cli.state, StageState::Running);
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
                note: "Installed Shelbi 0.9.0 at ~/.shelbi/bin/shelbi.".into(),
                changed: true,
                report: samples::ready(),
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
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        f.finish_check(request.run, checked(samples::needs_cli()));
        let Action::Check(install) = f.press(Focus::InstallCli) else { panic!() };
        f.set(Field::Host, "build-2");
        assert_eq!(f.running(), Some(install.run), "an install isn't dropped mid-way");
        assert_eq!(f.stop_check(), None, "nor stopped");
        assert!(f.panel().detail.contains("build-server"), "it says where it's installing");
        assert_eq!(f.press(Focus::Test), Action::Stay, "no second job while it runs");
        f.finish_check(
            install.run,
            CheckOutcome::Installed {
                note: String::new(),
                changed: false,
                report: samples::ready(),
            },
        );
        assert_eq!(f.check(), &Check::NotChecked, "build-2 wasn't checked");
        assert!(f.verified().is_none());
    }

    #[test]
    fn a_stopped_check_says_so_and_drops_its_answer() {
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert_eq!(f.stop_check(), Some(request.run));
        let panel = f.panel();
        assert_eq!((panel.tone, panel.title.as_str()), (Tone::Idle, "Check stopped"));
        assert!(!f.finish_check(request.run, checked(samples::ready())), "the stopped run's answer");
        assert!(f.verified().is_none());
        assert_eq!(f.test_label(), "Test again");
        assert_eq!(f.stop_check(), None, "nothing runs");
    }

    #[test]
    fn the_panel_says_what_needs_attention() {
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        assert_eq!(f.panel().tone, Tone::Idle);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert_eq!((f.panel().tone, f.panel().title.as_str()), (Tone::Busy, "Checking…"));
        let stages = f.stages();
        assert_eq!(stages[0].state, StageState::Running);
        assert!(stages[1..].iter().all(|s| s.state == StageState::Waiting));
        f.finish_check(request.run, checked(samples::no_directory()));
        let panel = f.panel();
        assert_eq!((panel.tone, panel.title.as_str()), (Tone::Warning, "Project directory needs attention"));
        assert!(panel.detail.contains("/home/me/my-project doesn’t exist"));
        assert!(!f.offers_install());

        let report = |reach, result: &str| Report::unreached(reach, result, "why");
        for (r, title) in [
            (samples::unreachable(), "Couldn’t connect"),
            (report(Reach::TimedOut, "Timed out"), "Connection timed out"),
            (report(Reach::AuthDenied, "Login refused"), "SSH couldn’t log in"),
            (report(Reach::HostKey, "Host key not trusted"), "Host key not trusted"),
            (samples::needs_cli(), "Shelbi CLI needed"),
            (
                samples::attention(StageKind::Git, "Not installed", "Git isn’t installed.", false),
                "Git needed",
            ),
            (
                samples::attention(StageKind::Platform, "No release", "No release.", false),
                "Unsupported platform",
            ),
        ] {
            let Action::Check(request) = f.press(Focus::Test) else { panic!() };
            f.finish_check(request.run, checked(r.clone()));
            assert_eq!(f.panel().title, title);
            assert_ne!(f.panel().tone, Tone::Success);
            assert_eq!(f.stages(), r.stages, "the stages are the report's");
        }
    }

    #[test]
    fn success_needs_every_stage_and_the_verified_binary() {
        assert!(samples::ready().is_ready());
        let mut no_binary = samples::ready();
        no_binary.verified = None;
        assert!(!no_binary.is_ready());
        let mut skipped = samples::ready();
        skipped.stages[4].state = StageState::Skipped;
        assert!(!skipped.is_ready(), "a stage that wasn't checked isn't a pass");
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        f.finish_check(request.run, checked(skipped));
        assert_ne!(f.panel().title, "Connection successful");
    }

    #[test]
    fn a_draft_that_cant_be_checked_marks_its_field() {
        let mut f = ReadinessForm::add("p");
        assert_eq!(f.press(Focus::Test), Action::Stay);
        assert!(f.field_error(Field::Host).is_some(), "no host to test");
        assert_eq!(f.focus(), Focus::Field(Field::Host));
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
    fn records_only_a_saved_machine_at_its_saved_host() {
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        let record = request.record(&checked(samples::ready())).expect("the saved machine");
        assert_eq!((record.machine.as_str(), record.host.as_str()), ("Build server", "build-server"));
        assert_eq!(request.record(&checked(samples::needs_cli())), None, "only a ready answer");
        f.finish_check(request.run, checked(samples::ready()));

        // A draft's check waits for the save, and only for the host saved.
        let mut add = ReadinessForm::add("p");
        add.set(Field::Name, "gpu");
        add.set(Field::Host, "gpu.lan");
        add.set(Field::WorkDir, "/srv/p");
        let Action::Check(request) = add.press(Focus::Test) else { panic!() };
        assert_eq!(request.record(&checked(samples::ready())), None);
        add.finish_check(request.run, checked(samples::ready()));
        let verified = add.save_verified().expect("verified");
        let change = |host: Option<&str>| MachineChange {
            machine: Machine {
                name: "gpu".into(),
                kind: MachineKind::Ssh,
                work_dir: PathBuf::from("/srv/p"),
                host: host.map(str::to_string),
                tags: vec![],
                forward: None,
            },
            previous_name: None,
            workspaces: vec![],
        };
        assert_eq!(verified.clone().record(&change(Some("other"))), None);
        let r = verified.record(&change(Some("gpu.lan"))).unwrap();
        assert_eq!((r.machine.as_str(), r.host.as_str()), ("gpu", "gpu.lan"));
    }

    #[test]
    fn the_service_records_what_it_verified() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let service = ReadinessService {
            check: Arc::new(|_, _| CheckOutcome::Checked(samples::ready())),
            record: Arc::new(move |r| s.lock().unwrap().push(r.clone())),
        };
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        assert!(service.run(&request, Arc::default()).is_ready());
        assert_eq!(seen.lock().unwrap().len(), 1);
        let mut add = ReadinessForm::add("p");
        add.set(Field::Host, "gpu.lan");
        add.set(Field::WorkDir, "/srv/p");
        let Action::Check(request) = add.press(Focus::Test) else { panic!() };
        service.run(&request, Arc::default());
        assert_eq!(seen.lock().unwrap().len(), 1, "a draft records nothing");
        let mut f = ReadinessForm::configure("p", &remote(), 0, None);
        let Action::Check(request) = f.press(Focus::Test) else { panic!() };
        service.run(&request, Arc::new(AtomicBool::new(true)));
        assert_eq!(seen.lock().unwrap().len(), 1, "a stopped check records nothing");
    }

    #[test]
    fn an_install_that_changed_the_machine_says_so() {
        let outcome = |changed| CheckOutcome::Installed {
            note: "Installed Shelbi 0.9.0 at ~/.shelbi/bin/shelbi.".into(),
            changed,
            report: samples::ready(),
        };
        let said = outcome(true).side_effect("gpu.lan").unwrap();
        assert!(said.starts_with("On gpu.lan: Installed Shelbi"), "{said}");
        assert_eq!(outcome(false).side_effect("gpu.lan"), None, "already there");
        assert_eq!(checked(samples::ready()).side_effect("gpu.lan"), None);
    }

    #[test]
    fn the_stop_trips_the_running_flag_and_refuses_after_close() {
        let stop = CheckStop::default();
        let flag = stop.start(1);
        stop.stop_unless(Some(1));
        assert!(!flag.load(Ordering::Acquire), "still the live one");
        stop.stop_unless(None);
        assert!(flag.load(Ordering::Acquire));
        let flag = stop.start(2);
        stop.finish(2);
        stop.close();
        assert!(!flag.load(Ordering::Acquire), "a finished run isn't touched");
        assert!(stop.closed());
        assert!(stop.start(3).load(Ordering::Acquire), "a closed form's check starts stopped");
    }

    #[test]
    fn keys_move_focus_submit_and_cancel() {
        let mut f = ReadinessForm::add("p");
        assert_eq!(f.key(chord(Key::Tab, Mods::NONE)), Some(Action::Stay));
        assert_eq!(f.focus(), Focus::Field(Field::Host));
        f.key(chord(Key::Tab, Mods::SHIFT));
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        f.key(chord(Key::BackTab, Mods::SHIFT));
        assert_eq!(f.focus(), Focus::Submit, "wraps");
        f.key(chord(Key::Tab, Mods::NONE));
        assert_eq!(f.focus(), Focus::Field(Field::Name));
        assert_eq!(f.key(chord(Key::Char(' '), Mods::NONE)), None, "a field's space is typed");
        assert_eq!(f.key(chord(Key::Esc, Mods::NONE)), Some(Action::Cancel));
        assert!(matches!(f.key(chord(Key::Enter, Mods::NONE)), Some(Action::Save(..))));

        let mut f = ReadinessForm::add("p");
        f.set_focus(Focus::Cancel);
        assert_eq!(f.key(chord(Key::Char(' '), Mods::NONE)), Some(Action::Cancel));
    }
}
