//! Whether a remote machine is ready to run a project's workspaces: SSH
//! connects, a compatible `shelbi` is there, Git is usable, and the project's
//! root directory is a usable checkout (`dt-machine-readiness-setup`).
//!
//! [`machine`](crate::machine) answers the CLI half (find or install a
//! compatible `shelbi`); this module adds Git and the project directory on top
//! of the same probe, in one SSH round trip, and returns a structured
//! [`Readiness`] a form can drive (never a printed table). It checks a *draft*
//! host and directory just as well as a saved machine's, so a machine can be
//! tested before it is added to the project.
//!
//! What a check never does: install Git, clone anything, create or touch the
//! project directory, or rewrite SSH config. Each of those comes back as an
//! [`Issue`] saying what needs attention. Installing the Shelbi CLI is its own
//! explicit operation, [`install_cli`], which runs the existing idempotent
//! [`machine::setup`] (reuse a compatible install; install the matching release
//! to `~/.shelbi/bin/shelbi`; leave an incompatible package-manager binary
//! untouched; copy from the hub when the remote has no outbound network) and
//! then checks again.
//!
//! A failed connection is never read as a missing CLI: SSH unreachable or
//! timed out, an auth refusal and a refused host key are each their own
//! [`Readiness`] variant, decided before anything about the remote is known.
//!
//! Every remote call goes through [`RemoteExec`] / [`HubFetch`], so the whole
//! flow runs against scripted fakes in tests.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use shelbi_app::settings::machine_check::{
    CheckOutcome, Reach, ReadinessIntent, ReadinessRequest, ReadinessService, Record, RecordFn, Report, Stage,
    StageKind, StageState, VerifiedCli,
};
use shelbi_core::{shell_escape, Host};
use shelbi_state::machine_state::{self, MachineRecord};

use crate::machine::{
    self as m, between, first_nonempty_line, Compat, HubFetch, MachineProbe,
    PlatformError, RemoteExec, SetupAction, SetupError, Version,
};

/// Wall-clock bound on one readiness check. Longer than the machines view's
/// liveness probe (it loads the login shell and runs Git), far shorter than an
/// install.
pub const READINESS_DEADLINE: Duration = Duration::from_secs(30);

/// The machine to check: an SSH destination and the project's root directory
/// on it, as typed (the draft need not be saved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessTarget {
    /// What `ssh` connects to: a hostname or an SSH config alias.
    pub host: String,
    /// The remote's own absolute path, never expanded on the hub.
    pub work_dir: String,
}

pub use shelbi_app::settings::machine_check::{TargetError, TargetField};

/// Validate a target before anything runs: a destination `ssh` takes as a host
/// (no leading `-` it would read as an option, no whitespace), and an absolute
/// remote directory. The same rules the machine service saves by (shared with
/// the forms: [`shelbi_app::settings::machine_check::validate_target`]).
pub fn validate_target(target: &ReadinessTarget) -> Result<(), TargetError> {
    shelbi_app::settings::machine_check::validate_target(&target.host, &target.work_dir)
}

// ===========================================================================
// Structured result
// ===========================================================================

/// What a readiness check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// The target can't be checked as typed (nothing ran).
    Invalid(TargetError),
    /// SSH couldn't connect. Says nothing about the remote.
    Unreachable { detail: String },
    /// SSH didn't answer within [`READINESS_DEADLINE`] (a wedged host, a VPN
    /// or Tailscale login waiting). Says nothing about the remote.
    TimedOut { detail: String },
    /// SSH connected but refused to log in (keys, agent, user).
    AuthDenied { detail: String },
    /// SSH refused the host's key (unknown under `BatchMode`, or changed).
    HostKey { detail: String },
    /// The check was cancelled before it answered.
    Cancelled,
    /// SSH ran the check: what it found.
    Reached(RemoteReport),
}

/// What a check found on a machine SSH reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteReport {
    /// `uname`'s answer, or why no release artifact fits it.
    pub platform: Result<m::Platform, PlatformError>,
    pub cli: CliStatus,
    pub git: GitStatus,
    pub dir: DirStatus,
}

/// The `shelbi` the machine would run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliStatus {
    /// A compatible binary: on the login PATH, or Shelbi's managed copy.
    Compatible {
        path: String,
        version: Version,
        source: &'static str,
    },
    /// No `shelbi` on the login PATH or in `~/.shelbi/bin`.
    Missing,
    /// One is there but can't be used with this hub: too old, too new, or no
    /// readable version.
    Incompatible {
        path: String,
        version: Option<Version>,
        compat: Option<Compat>,
    },
}

/// Git, as Shelbi's SSH commands (a plain, non-login `sh`) find it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitStatus {
    Usable { path: String, version: String },
    /// Only the interactive login shell finds it, so Shelbi's worktree
    /// commands over SSH wouldn't.
    LoginShellOnly { path: String },
    Missing,
}

/// The project's root directory on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirStatus {
    /// A writable Git checkout, and the path is its root.
    Usable,
    Missing,
    NotADirectory,
    /// It can't be listed or entered.
    NotAccessible,
    /// It can't be written (worktrees go under it).
    ReadOnly,
    /// Not inside a Git repository.
    NotARepository,
    /// Inside a repository rooted higher up, at `root`.
    NotRepoRoot { root: String },
    /// Git refused to read it (e.g. "dubious ownership"): Git's own words.
    GitRefused { detail: String },
    /// Git is missing, so whether it's a checkout couldn't be checked.
    Unchecked,
}

/// One thing a reached machine needs before it's ready, in the order a
/// person should deal with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    UnsupportedPlatform(PlatformError),
    Cli(CliStatus),
    Git(GitStatus),
    Dir(DirStatus),
}

impl Readiness {
    /// SSH, a compatible CLI, Git and the project directory all check out.
    /// The only state a form may show as a successful connection.
    pub fn is_ready(&self) -> bool {
        matches!(self, Readiness::Reached(r) if r.issues().is_empty())
    }

    /// The reached machine's report, if SSH ran the check.
    pub fn report(&self) -> Option<&RemoteReport> {
        match self {
            Readiness::Reached(r) => Some(r),
            _ => None,
        }
    }

    /// Whether [`install_cli`] can help: SSH reached it, its platform has a
    /// release, and its CLI isn't usable. A connection failure never offers it.
    pub fn offers_install(&self) -> bool {
        self.report().is_some_and(RemoteReport::offers_install)
    }

    /// The durable record of the verified binary on `host`, only for a ready
    /// machine.
    pub fn record(&self, host: &str) -> Option<MachineRecord> {
        if !self.is_ready() {
            return None;
        }
        match &self.report()?.cli {
            CliStatus::Compatible {
                path,
                version,
                source,
            } => Some(MachineRecord {
                path: path.clone(),
                version: version.to_string(),
                compatible: true,
                source: source.to_string(),
                checked_at: chrono::Utc::now(),
                host: Some(host.to_string()),
            }),
            _ => None,
        }
    }

    /// One line on why the check couldn't reach the machine, with what to do
    /// next; `None` once SSH ran it.
    pub fn connection_message(&self, host: &str) -> Option<String> {
        Some(match self {
            Readiness::Invalid(e) => e.message.clone(),
            Readiness::Unreachable { detail } => format!(
                "SSH couldn’t reach {host}: {detail}. Check the machine is on and the host or SSH \
                 alias is right, then test again."
            ),
            Readiness::TimedOut { detail } => format!(
                "SSH didn’t answer from {host} within {}s: {detail}. Check the machine is on and \
                 reachable (a VPN or Tailscale login may be waiting), then test again.",
                READINESS_DEADLINE.as_secs()
            ),
            Readiness::AuthDenied { detail } => format!(
                "SSH reached {host} but couldn’t log in: {detail}. Make sure `ssh {host}` logs in \
                 with your keys without a password prompt, then test again."
            ),
            Readiness::HostKey { detail } => format!(
                "SSH doesn’t trust {host}’s host key: {detail}. Run `ssh {host}` in a terminal to \
                 verify the key (or fix its entry in ~/.ssh/known_hosts), then test again."
            ),
            Readiness::Cancelled => "The check was cancelled.".to_string(),
            Readiness::Reached(_) => return None,
        })
    }
}

impl RemoteReport {
    /// What needs attention before it's ready, in order; empty when ready.
    pub fn issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let artifact = self.platform.clone().and_then(m::Platform::artifact);
        if let Err(e) = &artifact {
            // Only when the CLI isn't already there: a compatible build on an
            // unpublished platform (built from source) is fine.
            if !matches!(self.cli, CliStatus::Compatible { .. }) {
                issues.push(Issue::UnsupportedPlatform(e.clone()));
            }
        }
        if !matches!(self.cli, CliStatus::Compatible { .. }) {
            issues.push(Issue::Cli(self.cli.clone()));
        }
        if !matches!(self.git, GitStatus::Usable { .. }) {
            issues.push(Issue::Git(self.git.clone()));
        }
        // Without a usable Git the directory can't be checked, and the Git
        // issue already says why.
        let git_usable = matches!(self.git, GitStatus::Usable { .. });
        if self.dir != DirStatus::Usable && (git_usable || self.dir != DirStatus::Unchecked) {
            issues.push(Issue::Dir(self.dir.clone()));
        }
        issues
    }

    /// The CLI isn't usable and a release can be installed for the platform.
    pub fn offers_install(&self) -> bool {
        !matches!(self.cli, CliStatus::Compatible { .. })
            && self.platform.clone().and_then(m::Platform::artifact).is_ok()
    }

    /// The parts that checked out, for a success line ("Shelbi, Git, and
    /// project directory are ready.").
    pub fn ready_summary(&self) -> String {
        "Shelbi, Git, and project directory are ready.".to_string()
    }
}

impl CliStatus {
    /// What the probe found, as the binary the machine would run: a
    /// compatible one, else the unusable one it would otherwise find (the
    /// managed copy first, as setup would replace it, then the PATH's), else
    /// none.
    fn from_probe(probe: &m::ReachableProbe, hub: &Version) -> CliStatus {
        let e = probe.effective(hub);
        if let (true, Some(version)) = (e.compatible, e.version) {
            return CliStatus::Compatible {
                path: e.path,
                version,
                source: e.source,
            };
        }
        match probe.shelbi_bin.as_ref().or(probe.path_binary.as_ref()) {
            Some(b) => CliStatus::Incompatible {
                path: b.path.clone(),
                version: b.version,
                compat: b.compat(hub),
            },
            None => CliStatus::Missing,
        }
    }
}

impl Issue {
    /// What needs attention and what to do about it, for `work_dir`.
    pub fn message(&self, work_dir: &str) -> String {
        match self {
            Issue::UnsupportedPlatform(e) => match e {
                PlatformError::UnsupportedOs(os) => format!(
                    "Shelbi doesn’t publish a CLI for this machine’s OS ({}). Build Shelbi there \
                     from source, then test again.",
                    if os.is_empty() { "unknown" } else { os }
                ),
                PlatformError::UnsupportedArch(arch) => format!(
                    "Shelbi doesn’t publish a CLI for {arch}. Build Shelbi there from source, then \
                     test again."
                ),
                PlatformError::UnsupportedCombo { os, arch } => format!(
                    "Shelbi doesn’t publish a CLI for {os}/{arch} yet. Build Shelbi there from \
                     source, then test again."
                ),
            },
            Issue::Cli(CliStatus::Missing) => {
                "The Shelbi CLI isn’t installed. Install CLI puts a compatible build in \
                 ~/.shelbi/bin."
                    .to_string()
            }
            Issue::Cli(CliStatus::Incompatible {
                path,
                version,
                compat,
            }) => {
                let hub = m::hub_version();
                let what = match (version, compat) {
                    (Some(v), Some(Compat::TooOld)) => format!("shelbi {v} at {path} is older than this app ({hub})"),
                    (Some(v), Some(Compat::TooNew)) => format!("shelbi {v} at {path} is newer than this app ({hub})"),
                    (Some(v), _) => format!("shelbi {v} at {path} doesn’t match this app ({hub})"),
                    (None, _) => format!("{path} didn’t report a Shelbi version"),
                };
                format!(
                    "{what}. Install CLI adds a compatible build in ~/.shelbi/bin and leaves that one \
                     as it is."
                )
            }
            Issue::Cli(CliStatus::Compatible { .. }) => String::new(),
            Issue::Git(GitStatus::LoginShellOnly { path }) => format!(
                "Git is only on your login shell’s PATH ({path}), so Shelbi’s SSH commands can’t \
                 find it. Put it on the PATH a plain `ssh host git` sees, then test again."
            ),
            Issue::Git(_) => "Git isn’t installed. Install Git on the machine, then test again.".to_string(),
            Issue::Dir(d) => match d {
                DirStatus::Missing => format!(
                    "{work_dir} doesn’t exist. Clone the project there (or correct the path), then \
                     test again."
                ),
                DirStatus::NotADirectory => format!("{work_dir} isn’t a directory. Correct the path."),
                DirStatus::NotAccessible => format!(
                    "{work_dir} can’t be opened by your SSH user. Fix its permissions, then test again."
                ),
                DirStatus::ReadOnly => format!(
                    "{work_dir} isn’t writable by your SSH user, and workspaces are created inside \
                     it. Fix its permissions, then test again."
                ),
                DirStatus::NotARepository => format!(
                    "{work_dir} isn’t a Git checkout. Clone the project there (or correct the path), \
                     then test again."
                ),
                DirStatus::NotRepoRoot { root } => format!(
                    "{work_dir} is inside the checkout at {root}. Use the checkout’s root directory."
                ),
                DirStatus::GitRefused { detail } => format!("Git can’t read {work_dir}: {detail}"),
                DirStatus::Unchecked => format!(
                    "Whether {work_dir} is a Git checkout couldn’t be checked without Git."
                ),
                DirStatus::Usable => String::new(),
            },
        }
    }
}

// ===========================================================================
// The check
// ===========================================================================

// Markers framing the Git and directory answers (see `machine`'s for the CLI).
const M_GITPATH: (&str, &str) = ("\u{1}GPB\u{1}", "\u{1}GPE\u{1}");
const M_GITVER: (&str, &str) = ("\u{1}GVB\u{1}", "\u{1}GVE\u{1}");
const M_GITLOGIN: (&str, &str) = ("\u{1}GLB\u{1}", "\u{1}GLE\u{1}");
const M_DIR: (&str, &str) = ("\u{1}DSB\u{1}", "\u{1}DSE\u{1}");
const M_DIRINFO: (&str, &str) = ("\u{1}DIB\u{1}", "\u{1}DIE\u{1}");

/// The check script: [`machine::probe_script`]'s platform and `shelbi`
/// lookup, then Git (on a plain `sh`'s PATH, as Shelbi's worktree commands
/// run it, else the login shell's) and the directory, which is only read:
/// `test`, `cd`, `git rev-parse`. `work_dir` is shell-quoted. Pure text.
pub fn readiness_script(work_dir: &str) -> String {
    let (gpb, gpe) = M_GITPATH;
    let (gvb, gve) = M_GITVER;
    let (glb, gle) = M_GITLOGIN;
    let (dsb, dse) = M_DIR;
    let (dib, die) = M_DIRINFO;
    let dir = shell_escape(work_dir);
    format!(
        r#"{probe}GP=$(command -v git 2>/dev/null)
printf '{gpb}%s{gpe}\n' "$GP"
if [ -n "$GP" ]; then
  printf '{gvb}%s{gve}\n' "$("$GP" --version 2>/dev/null)"
else
  printf '{glb}%s{gle}\n' "$("$SH" -l -i -c 'command -v git' 2>/dev/null </dev/null)"
fi
D={dir}
I=
if [ ! -e "$D" ]; then S=missing
elif [ ! -d "$D" ]; then S=notdir
elif [ ! -r "$D" ] || [ ! -x "$D" ]; then S=noaccess
elif [ ! -w "$D" ]; then S=readonly
elif [ -z "$GP" ]; then S=unchecked
else
  HERE=$(cd "$D" 2>/dev/null && pwd -P)
  if OUT=$(cd "$D" 2>/dev/null && "$GP" rev-parse --show-toplevel 2>&1); then
    if [ "$OUT" = "$HERE" ]; then S=ok; else S=notroot; I=$OUT; fi
  else
    S=gitfail; I=$(printf '%s\n' "$OUT" | head -n 1)
  fi
fi
printf '{dsb}%s{dse}\n' "$S"
printf '{dib}%s{die}\n' "$I"
"#,
        probe = m::probe_script(),
    )
}

/// Parse [`readiness_script`]'s stdout. Pure.
pub fn parse_readiness_output(stdout: &str, hub: &Version) -> RemoteReport {
    let probe = m::parse_probe_output(stdout, hub);
    let cli = CliStatus::from_probe(&probe, hub);
    let field = |(b, e): (&str, &str)| between(stdout, b, e).map(str::trim).filter(|s| !s.is_empty());
    let git = match field(M_GITPATH) {
        Some(path) => GitStatus::Usable {
            path: path.to_string(),
            version: field(M_GITVER).unwrap_or("").to_string(),
        },
        None => match field(M_GITLOGIN) {
            Some(path) => GitStatus::LoginShellOnly { path: path.to_string() },
            None => GitStatus::Missing,
        },
    };
    let info = field(M_DIRINFO).unwrap_or("").to_string();
    let dir = match field(M_DIR).unwrap_or("") {
        "ok" => DirStatus::Usable,
        "missing" => DirStatus::Missing,
        "notdir" => DirStatus::NotADirectory,
        "noaccess" => DirStatus::NotAccessible,
        "readonly" => DirStatus::ReadOnly,
        "notroot" => DirStatus::NotRepoRoot { root: info },
        "gitfail" if info.to_ascii_lowercase().contains("not a git repository") => DirStatus::NotARepository,
        "gitfail" => DirStatus::GitRefused {
            detail: if info.is_empty() { "(no diagnostic)".into() } else { info },
        },
        // `unchecked`, or no answer at all: nothing claims it's usable.
        _ => DirStatus::Unchecked,
    };
    RemoteReport {
        platform: probe.platform,
        cli,
        git,
        dir,
    }
}

/// Check `target` over `exec`. Validates it first (nothing runs for a target
/// SSH can't take); a transport failure is never reported as anything about
/// the remote.
pub fn check(exec: &dyn RemoteExec, hub: &Version, target: &ReadinessTarget) -> Readiness {
    if let Err(e) = validate_target(target) {
        return Readiness::Invalid(e);
    }
    match exec.run_script(&readiness_script(target.work_dir.trim())) {
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Readiness::Cancelled,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Readiness::TimedOut {
            detail: first_nonempty_line(&e.to_string()),
        },
        Err(e) => Readiness::Unreachable {
            detail: first_nonempty_line(&e.to_string()),
        },
        Ok(out) => match m::classify_probe_failure(&out) {
            Some(MachineProbe::Unreachable { detail }) => Readiness::Unreachable { detail },
            Some(MachineProbe::AuthDenied { detail }) => Readiness::AuthDenied { detail },
            Some(MachineProbe::HostKey { detail }) => Readiness::HostKey { detail },
            Some(MachineProbe::Reachable(_)) | None => {
                Readiness::Reached(parse_readiness_output(&out.stdout, hub))
            }
        },
    }
}

/// What Install CLI did.
#[derive(Debug, Clone)]
pub struct Installed {
    /// [`machine::setup`]'s answer: already compatible, or installed (and how).
    pub action: SetupAction,
    /// The check run again afterwards.
    pub readiness: Readiness,
}

/// Install CLI: run the shared, idempotent [`machine::setup`] against the
/// target's host (it reuses a compatible install, installs the matching release
/// to `~/.shelbi/bin/shelbi`, never touches an incompatible package-manager
/// binary, and copies the release from the hub when the remote has no network),
/// then check readiness again so the answer reflects what's really there.
pub fn install_cli(
    exec: &dyn RemoteExec,
    fetch: &dyn HubFetch,
    hub: &Version,
    target: &ReadinessTarget,
) -> Result<Installed, SetupError> {
    if let Err(e) = validate_target(target) {
        return Err(SetupError::InstallFailed(e.message));
    }
    let action = m::setup(exec, fetch, hub)?;
    Ok(Installed {
        action,
        readiness: check(exec, hub, target),
    })
}

/// [`check`] over the user's own `ssh` (their config, keys and the shared
/// ControlMaster), bounded by [`READINESS_DEADLINE`] and killed when `cancel`
/// trips. Blocking: run it off any UI thread.
pub fn check_host(target: &ReadinessTarget, cancel: Arc<AtomicBool>) -> Readiness {
    let host = target.host.trim().to_string();
    let exec = m::SshExec::with_deadline(Host::Ssh { host: host.clone() }, host, READINESS_DEADLINE)
        .with_cancel(cancel);
    check(&exec, &m::hub_version(), target)
}

/// [`install_cli`] over the user's own `ssh` and the hub's `curl`, with the
/// install-sized deadline. Not cancellable: an install runs to its end once
/// started. Blocking.
pub fn install_cli_on_host(target: &ReadinessTarget) -> Result<Installed, SetupError> {
    let host = target.host.trim().to_string();
    let exec = m::SshExec::new(Host::Ssh { host: host.clone() }, host);
    install_cli(&exec, &m::CurlHubFetch, &m::hub_version(), target)
}

/// Record a verified machine's binary under `machine` (its name in the
/// project), bound to `host`, for dispatch and the machines views. Only a ready
/// [`Readiness`] writes; anything else leaves the record as it was. Returns
/// whether it wrote.
pub fn save_verified_record(machine: &str, host: &str, readiness: &Readiness) -> shelbi_core::Result<bool> {
    match readiness.record(host) {
        Some(rec) => machine_state::save_machine_record(machine, Some(rec)).map(|()| true),
        None => Ok(false),
    }
}

// ===========================================================================
// The forms' report and service (`shelbi_app::settings::machine_check`)
// ===========================================================================

/// What a check found, as the machine forms show it: how far SSH got, one
/// stage per thing checked with a concise result, the actionable messages,
/// whether Install CLI can help, and the verified binary. A connection
/// failure leaves every stage after SSH unchecked.
pub fn report(readiness: &Readiness, target: &ReadinessTarget) -> Report {
    let host = target.host.trim();
    let message = || readiness.connection_message(host).unwrap_or_default();
    let unreached = |reach, result: &str| Report::unreached(reach, result, message());
    match readiness {
        Readiness::Invalid(e) => unreached(Reach::Invalid(e.field), "Can’t test as typed"),
        Readiness::Unreachable { .. } => unreached(Reach::Unreachable, "Couldn’t connect"),
        Readiness::TimedOut { .. } => unreached(
            Reach::TimedOut,
            &format!("No answer in {}s", READINESS_DEADLINE.as_secs()),
        ),
        Readiness::AuthDenied { .. } => unreached(Reach::AuthDenied, "Login refused"),
        Readiness::HostKey { .. } => unreached(Reach::HostKey, "Host key not trusted"),
        Readiness::Cancelled => Report::cancelled(),
        Readiness::Reached(r) => reached_report(readiness, r, target.work_dir.trim()),
    }
}

fn reached_report(readiness: &Readiness, r: &RemoteReport, work_dir: &str) -> Report {
    use StageState::{Attention, Passed, Skipped};
    let cli_ok = matches!(r.cli, CliStatus::Compatible { .. });
    let git_ok = matches!(r.git, GitStatus::Usable { .. });
    let platform = match r.platform.clone().and_then(|p| p.artifact().map(|_| p)) {
        Ok(p) => Stage::new(StageKind::Platform, Passed, format!("{} {}", p.os, p.arch)),
        // A compatible CLI built from source runs there; nothing to install.
        Err(e) if cli_ok => Stage::new(
            StageKind::Platform,
            Passed,
            format!("{} (no release; CLI present)", platform_words(&e)),
        ),
        Err(e) => Stage::new(StageKind::Platform, Attention, format!("No release for {}", platform_words(&e))),
    };
    let cli = match &r.cli {
        CliStatus::Compatible { path, version, .. } => {
            Stage::new(StageKind::Cli, Passed, format!("shelbi {version} at {path}"))
        }
        CliStatus::Missing => Stage::new(StageKind::Cli, Attention, "Not installed"),
        CliStatus::Incompatible { version, compat, .. } => Stage::new(
            StageKind::Cli,
            Attention,
            match (version, compat) {
                (Some(v), Some(Compat::TooOld)) => format!("shelbi {v} is too old"),
                (Some(v), Some(Compat::TooNew)) => format!("shelbi {v} is too new"),
                (Some(v), _) => format!("shelbi {v} doesn’t match"),
                (None, _) => "No version reported".to_string(),
            },
        ),
    };
    let git = match &r.git {
        GitStatus::Usable { path, version } => Stage::new(
            StageKind::Git,
            Passed,
            if version.is_empty() { path.clone() } else { version.clone() },
        ),
        GitStatus::LoginShellOnly { .. } => Stage::new(StageKind::Git, Attention, "Only on the login shell’s PATH"),
        GitStatus::Missing => Stage::new(StageKind::Git, Attention, "Not installed"),
    };
    let dir_result = |d: &DirStatus| match d {
        DirStatus::Usable => "Checkout root".to_string(),
        DirStatus::Missing => "Doesn’t exist".into(),
        DirStatus::NotADirectory => "Not a directory".into(),
        DirStatus::NotAccessible => "Can’t be opened".into(),
        DirStatus::ReadOnly => "Not writable".into(),
        DirStatus::NotARepository => "Not a Git checkout".into(),
        DirStatus::NotRepoRoot { root } => format!("Inside the checkout at {root}"),
        DirStatus::GitRefused { .. } => "Git refused to read it".into(),
        DirStatus::Unchecked => "Couldn’t be checked".into(),
    };
    let dir = match &r.dir {
        DirStatus::Usable => Stage::new(StageKind::Directory, Passed, dir_result(&r.dir)),
        DirStatus::Unchecked if !git_ok => Stage::new(StageKind::Directory, Skipped, "Needs Git to check"),
        d => Stage::new(StageKind::Directory, Attention, dir_result(d)),
    };
    let verified = match (&r.cli, readiness.is_ready()) {
        (CliStatus::Compatible { path, version, source }, true) => Some(VerifiedCli {
            path: path.clone(),
            version: version.to_string(),
            source: source.to_string(),
        }),
        _ => None,
    };
    Report {
        reach: Reach::Reached,
        stages: vec![
            Stage::new(StageKind::Ssh, Passed, "Connected and logged in"),
            platform,
            cli,
            git,
            dir,
        ],
        // Install CLI can't help on a platform without a release, so the CLI's
        // message (which offers it) gives way to the platform's.
        messages: r
            .issues()
            .iter()
            .filter(|i| r.offers_install() || !matches!(i, Issue::Cli(_)))
            .map(|i| i.message(work_dir))
            .collect(),
        offers_install: r.offers_install(),
        verified,
    }
}

/// The OS and CPU a [`PlatformError`] names, in a few words.
fn platform_words(e: &PlatformError) -> String {
    match e {
        PlatformError::UnsupportedOs(os) if os.is_empty() => "an unknown OS".into(),
        PlatformError::UnsupportedOs(os) => os.clone(),
        PlatformError::UnsupportedArch(arch) => arch.clone(),
        PlatformError::UnsupportedCombo { os, arch } => format!("{os} {arch}"),
    }
}

/// Install CLI's answer, from the shared setup's: what it did (and whether it
/// changed the machine) with the check run again afterwards, or why it failed
/// and whether installing again could help.
pub fn install_outcome(result: Result<Installed, SetupError>, target: &ReadinessTarget) -> CheckOutcome {
    match result {
        Ok(done) => CheckOutcome::Installed {
            note: install_note(&done.action),
            changed: matches!(done.action, SetupAction::Installed { .. }),
            report: report(&done.readiness, target),
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
pub fn install_failure(e: &SetupError) -> String {
    let d = |d: &str| d.trim().trim_end_matches('.').to_string();
    let here = shelbi_app::settings::workspaces::LOCAL_PHRASE;
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
            "Neither the machine nor {here} could download the release: {}. Check the network, then install again.",
            d(x)
        ),
        SetupError::InstallFailed(x) => format!(
            "The install failed: {}. Install again, or put a compatible shelbi on the machine’s PATH and test again.",
            d(x)
        ),
    }
}

/// What the shared setup did, in a sentence.
pub fn install_note(action: &SetupAction) -> String {
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
                format!(
                    " (copied from {}; the machine has no outbound network)",
                    shelbi_app::settings::workspaces::LOCAL_PHRASE
                )
            } else {
                String::new()
            };
            let beside = alongside
                .as_ref()
                .map(|old| format!(" The older one at {} was left as it was.", old.path))
                .unwrap_or_default();
            format!("Installed Shelbi {version} at {path}{how}.{beside}")
        }
    }
}

/// Record a verified binary for a machine the project has (hub state,
/// `machine_state`, never project config), bound to the host it was verified
/// on.
pub fn record_verified(record: &Record) -> shelbi_core::Result<()> {
    machine_state::save_machine_record(
        &record.machine,
        Some(MachineRecord {
            path: record.cli.path.clone(),
            version: record.cli.version.clone(),
            compatible: true,
            source: record.cli.source.clone(),
            checked_at: chrono::Utc::now(),
            host: Some(record.host.clone()),
        }),
    )
}

/// Opens a [`RemoteExec`] to `host`: with the stop flag for a check (bounded
/// by [`READINESS_DEADLINE`]), without for an install (install-sized, runs to
/// its end).
pub type Connect = dyn Fn(&str, Option<Arc<AtomicBool>>) -> Box<dyn RemoteExec> + Send + Sync;

/// The live readiness service: checks and installs over the user's own `ssh`
/// (their config, keys and the shared ControlMaster), the hub's `curl` for the
/// no-network copy, and the machine record store.
pub fn service() -> ReadinessService {
    service_with(
        Arc::new(|host, cancel| -> Box<dyn RemoteExec> {
            let ssh = Host::Ssh { host: host.to_string() };
            match cancel {
                Some(cancel) => Box::new(
                    m::SshExec::with_deadline(ssh, host, READINESS_DEADLINE).with_cancel(cancel),
                ),
                None => Box::new(m::SshExec::new(ssh, host)),
            }
        }),
        Arc::new(m::CurlHubFetch),
        Arc::new(|record| {
            if let Err(e) = record_verified(record) {
                tracing::warn!(machine = %record.machine, host = %record.host, error = %e, "couldn't record the verified machine");
            }
        }),
        m::hub_version(),
    )
}

/// [`service`] over `connect`, `fetch` and `record` for this hub's `hub`
/// version: a test's scripted remote, hub fetch and records.
pub fn service_with(
    connect: Arc<Connect>,
    fetch: Arc<dyn HubFetch + Send + Sync>,
    record: Arc<RecordFn>,
    hub: Version,
) -> ReadinessService {
    ReadinessService {
        check: Arc::new(move |request: &ReadinessRequest, cancel| {
            let target = ReadinessTarget {
                host: request.target.host.clone(),
                work_dir: request.target.work_dir.clone(),
            };
            match request.intent {
                ReadinessIntent::Test | ReadinessIntent::TestAgain => {
                    let exec = connect(target.host.trim(), Some(cancel));
                    CheckOutcome::Checked(report(&check(&*exec, &hub, &target), &target))
                }
                ReadinessIntent::InstallCli => {
                    let exec = connect(target.host.trim(), None);
                    install_outcome(install_cli(&*exec, &*fetch, &hub, &target), &target)
                }
            }
        }),
        record,
    }
}

/// Scripted remotes and hub fetches (this crate's tests and the clients'):
/// a remote's answers as fake filesystem facts, replayed in order.
#[cfg(any(test, feature = "test-support"))]
pub mod fake {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{Connect, M_DIR, M_DIRINFO, M_GITLOGIN, M_GITPATH, M_GITVER};
    use crate::machine::{ExecOutput, HubFetch, HubFetchError, RemoteExec};

    pub fn out(code: i32, stdout: &str, stderr: &str) -> ExecOutput {
        ExecOutput {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// The install script's stdout for an install of `version`.
    pub fn installed(version: &str) -> String {
        format!("\u{1}INSB\u{1}shelbi {version}\u{1}INSE\u{1}\n")
    }

    /// A remote's answers to the check, as fake filesystem facts.
    #[derive(Clone)]
    pub struct Remote<'a> {
        pub uname: (&'a str, &'a str),
        /// `(path, shelbi --version)` on the login PATH.
        pub path_bin: Option<(&'a str, &'a str)>,
        /// `(path, shelbi --version)` at `~/.shelbi/bin/shelbi`.
        pub home_bin: Option<(&'a str, &'a str)>,
        pub git: Option<&'a str>,
        pub git_login: Option<&'a str>,
        /// The directory's state token (`ok`, `missing`, `notroot`, ...).
        pub dir: &'a str,
        pub info: &'a str,
    }

    impl Default for Remote<'_> {
        fn default() -> Self {
            Remote {
                uname: ("Linux", "x86_64"),
                path_bin: Some(("/usr/local/bin/shelbi", "shelbi 0.9.2")),
                home_bin: None,
                git: Some("/usr/bin/git"),
                git_login: None,
                dir: "ok",
                info: "",
            }
        }
    }

    impl Remote<'_> {
        /// The check script's stdout, with login-shell noise around it.
        pub fn stdout(&self) -> String {
            let mut s = String::from("Last login: yesterday\n");
            let mark = |s: &mut String, (b, e): (&str, &str), v: &str| s.push_str(&format!("{b}{v}{e}\n"));
            s.push_str(&format!("\u{1}OSB\u{1}{}\u{1}OSM\u{1}{}\u{1}OSE\u{1}\n", self.uname.0, self.uname.1));
            mark(&mut s, ("\u{1}PBB\u{1}", "\u{1}PBE\u{1}"), self.path_bin.map_or("", |b| b.0));
            if let Some((_, ver)) = self.path_bin {
                mark(&mut s, ("\u{1}PVB\u{1}", "\u{1}PVE\u{1}"), ver);
            }
            if let Some((p, ver)) = self.home_bin {
                mark(&mut s, ("\u{1}HBB\u{1}", "\u{1}HBE\u{1}"), p);
                mark(&mut s, ("\u{1}HVB\u{1}", "\u{1}HVE\u{1}"), ver);
            }
            mark(&mut s, M_GITPATH, self.git.unwrap_or(""));
            match self.git {
                Some(_) => mark(&mut s, M_GITVER, "git version 2.43.0"),
                None => mark(&mut s, M_GITLOGIN, self.git_login.unwrap_or("")),
            }
            mark(&mut s, M_DIR, self.dir);
            mark(&mut s, M_DIRINFO, self.info);
            s
        }

        /// The check's reply: this stdout, exit 0.
        pub fn reply(&self) -> io::Result<ExecOutput> {
            Ok(out(0, &self.stdout(), ""))
        }
    }

    /// Scripted remote: each call pops the next reply and remembers the
    /// script (and any stdin) it was sent.
    #[derive(Default)]
    pub struct Script {
        replies: Mutex<Vec<io::Result<ExecOutput>>>,
        pub scripts: Mutex<Vec<String>>,
        pub stdin: Mutex<Vec<Vec<u8>>>,
        /// The hosts [`connector`] opened, in order.
        pub hosts: Mutex<Vec<String>>,
        /// While set, a call through [`connector`] waits: until it's cleared,
        /// or until the connection's stop flag trips (it answers
        /// `Interrupted`, as `ssh` killed by the flag does).
        pub hold: AtomicBool,
    }

    impl Script {
        pub fn new(replies: Vec<io::Result<ExecOutput>>) -> Self {
            let s = Script::default();
            *s.replies.lock().unwrap() = replies.into_iter().rev().collect();
            s
        }

        /// Add replies after those queued.
        pub fn push(&self, replies: Vec<io::Result<ExecOutput>>) {
            let mut queued = self.replies.lock().unwrap();
            let mut all: Vec<_> = queued.drain(..).rev().collect();
            all.extend(replies);
            *queued = all.into_iter().rev().collect();
        }

        /// Replies not yet taken.
        pub fn pending(&self) -> usize {
            self.replies.lock().unwrap().len()
        }

        fn pop(&self) -> io::Result<ExecOutput> {
            self.replies.lock().unwrap().pop().expect("an unexpected remote call")
        }
    }

    impl RemoteExec for Script {
        fn label(&self) -> &str {
            "fake"
        }
        fn run_script(&self, script: &str) -> io::Result<ExecOutput> {
            self.scripts.lock().unwrap().push(script.to_string());
            self.pop()
        }
        fn run_script_with_stdin(&self, script: &str, stdin: &[u8]) -> io::Result<ExecOutput> {
            self.scripts.lock().unwrap().push(script.to_string());
            self.stdin.lock().unwrap().push(stdin.to_vec());
            self.pop()
        }
    }

    /// One connection to the scripted remote, with its stop flag.
    struct Conn {
        script: Arc<Script>,
        cancel: Option<Arc<AtomicBool>>,
    }

    impl Conn {
        /// As `SshExec`: a tripped flag refuses to start (and kills a held
        /// call).
        fn wait(&self) -> io::Result<()> {
            let stopped = || self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Acquire));
            loop {
                if stopped() {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
                if !self.script.hold.load(Ordering::Acquire) {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }

    impl RemoteExec for Conn {
        fn label(&self) -> &str {
            "fake"
        }
        fn run_script(&self, script: &str) -> io::Result<ExecOutput> {
            self.wait()?;
            self.script.run_script(script)
        }
        fn run_script_with_stdin(&self, script: &str, stdin: &[u8]) -> io::Result<ExecOutput> {
            self.wait()?;
            self.script.run_script_with_stdin(script, stdin)
        }
    }

    /// A [`Connect`] onto `script`: every host reaches it.
    pub fn connector(script: Arc<Script>) -> Arc<Connect> {
        Arc::new(move |host, cancel| -> Box<dyn RemoteExec> {
            script.hosts.lock().unwrap().push(host.to_string());
            Box::new(Conn {
                script: script.clone(),
                cancel,
            })
        })
    }

    /// A scripted hub fetch: serves its one answer, once.
    #[derive(Default)]
    pub struct Fetch(Mutex<Option<Result<Vec<u8>, HubFetchError>>>);

    impl Fetch {
        /// No fetch expected.
        pub fn none() -> Self {
            Fetch::default()
        }

        /// Serve the release tarball `bytes`.
        pub fn serving(bytes: Vec<u8>) -> Self {
            Fetch(Mutex::new(Some(Ok(bytes))))
        }

        /// Whether its answer was taken.
        pub fn taken(&self) -> bool {
            self.0.lock().unwrap().is_none()
        }
    }

    impl HubFetch for Fetch {
        fn fetch_verified(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, HubFetchError> {
            self.0.lock().unwrap().take().expect("an unexpected hub fetch")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::fake::{installed, out, Fetch, Remote, Script};
    use crate::machine::ExecOutput;
    use shelbi_state::machine_state::{SOURCE_PATH, SOURCE_SHELBI_BIN};
    use std::io;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version { major, minor, patch }
    }

    fn hub() -> Version {
        v(0, 9, 0)
    }

    fn target() -> ReadinessTarget {
        ReadinessTarget {
            host: "build-server".into(),
            work_dir: "/home/me/my project".into(),
        }
    }

    impl Remote<'_> {
        fn check(&self) -> Readiness {
            check(&Script::new(vec![Ok(out(0, &self.stdout(), ""))]), &hub(), &target())
        }
    }

    fn no_fetch() -> Fetch {
        Fetch::none()
    }

    // ---- target validation ------------------------------------------------

    #[test]
    fn a_target_ssh_cant_take_runs_nothing() {
        let exec = Script::new(vec![]);
        let check = |host: &str, dir: &str| {
            check(
                &exec,
                &hub(),
                &ReadinessTarget {
                    host: host.into(),
                    work_dir: dir.into(),
                },
            )
        };
        let field = |r: Readiness| match r {
            Readiness::Invalid(e) => e.field,
            other => panic!("expected Invalid, got {other:?}"),
        };
        assert_eq!(field(check("", "/srv/p")), TargetField::Host);
        assert_eq!(field(check("-oProxyCommand=evil", "/srv/p")), TargetField::Host, "never an ssh option");
        assert_eq!(field(check("two words", "/srv/p")), TargetField::Host);
        assert_eq!(field(check("box", "")), TargetField::WorkDir);
        assert_eq!(field(check("box", "src/p")), TargetField::WorkDir);
        match check("box", "~/p") {
            Readiness::Invalid(e) => assert!(e.message.contains("home directory out in full")),
            other => panic!("{other:?}"),
        }
        assert!(exec.scripts.lock().unwrap().is_empty());
    }

    #[test]
    fn the_directory_is_quoted_into_the_script() {
        let s = readiness_script("/home/me/it's a project");
        assert!(s.contains(r#"D='/home/me/it'\''s a project'"#), "{s}");
        // The check only reads it.
        for verb in ["mkdir", "git clone", "git init", "rm ", "touch", "> \"$D"] {
            assert!(!s.split("D=").nth(1).unwrap().contains(verb), "the check never runs `{verb}`");
        }
    }

    // ---- connection failures ----------------------------------------------

    #[test]
    fn connection_failures_are_never_a_missing_cli() {
        let run = |reply: io::Result<ExecOutput>| check(&Script::new(vec![reply]), &hub(), &target());
        let timeout = run(Err(io::Error::new(io::ErrorKind::TimedOut, "command did not finish within 30s")));
        assert!(matches!(timeout, Readiness::TimedOut { .. }), "the deadline is its own answer");
        let refused = run(Ok(out(255, "", "ssh: connect to host build-server port 22: Connection refused")));
        assert!(matches!(refused, Readiness::Unreachable { ref detail } if detail.contains("Connection refused")));
        let auth = run(Ok(out(255, "", "me@build-server: Permission denied (publickey).")));
        assert!(matches!(auth, Readiness::AuthDenied { .. }));
        let key = run(Ok(out(255, "", "Host key verification failed.")));
        assert!(matches!(key, Readiness::HostKey { .. }));
        let cancelled = run(Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")));
        assert_eq!(cancelled, Readiness::Cancelled);
        for r in [timeout, refused, auth, key, cancelled] {
            assert!(!r.is_ready() && !r.offers_install() && r.record("build-server").is_none());
            assert!(r.report().is_none(), "nothing is claimed about the remote");
            let msg = r.connection_message("build-server").unwrap();
            assert!(!msg.to_lowercase().contains("cli"), "{msg}");
        }
    }

    // ---- what a reached machine needs -------------------------------------

    #[test]
    fn a_compatible_cli_git_and_checkout_root_is_ready() {
        let r = Remote::default().check();
        assert!(r.is_ready(), "{r:?}");
        assert!(!r.offers_install());
        let report = r.report().unwrap();
        assert_eq!(
            report.cli,
            CliStatus::Compatible {
                path: "/usr/local/bin/shelbi".into(),
                version: v(0, 9, 2),
                source: SOURCE_PATH
            }
        );
        assert!(matches!(&report.git, GitStatus::Usable { version, .. } if version.contains("2.43")));
        let rec = r.record("build-server").unwrap();
        assert_eq!((rec.path.as_str(), rec.compatible), ("/usr/local/bin/shelbi", true));
        assert_eq!(rec.host.as_deref(), Some("build-server"));
    }

    #[test]
    fn a_compatible_managed_copy_is_ready() {
        let r = Remote {
            path_bin: Some(("/usr/bin/shelbi", "shelbi 0.8.0")),
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
            ..Remote::default()
        }
        .check();
        assert!(r.is_ready());
        assert!(matches!(r.report().unwrap().cli, CliStatus::Compatible { source: SOURCE_SHELBI_BIN, .. }));
    }

    #[test]
    fn a_missing_or_incompatible_cli_offers_install() {
        let missing = Remote {
            path_bin: None,
            ..Remote::default()
        }
        .check();
        assert!(!missing.is_ready() && missing.offers_install());
        assert_eq!(missing.report().unwrap().issues(), [Issue::Cli(CliStatus::Missing)]);
        assert!(missing.record("build-server").is_none());

        let old = Remote {
            path_bin: Some(("/usr/bin/shelbi", "shelbi 0.8.0")),
            ..Remote::default()
        }
        .check();
        assert!(old.offers_install());
        let issues = old.report().unwrap().issues();
        assert!(matches!(
            &issues[..],
            [Issue::Cli(CliStatus::Incompatible { compat: Some(Compat::TooOld), .. })]
        ));
        let msg = issues[0].message("/srv");
        assert!(msg.contains("older") && msg.contains("leaves that one"), "{msg}");

        let mute = Remote {
            path_bin: Some(("/usr/bin/shelbi", "")),
            ..Remote::default()
        }
        .check();
        assert!(matches!(
            mute.report().unwrap().cli,
            CliStatus::Incompatible { version: None, .. }
        ));
    }

    #[test]
    fn an_unsupported_platform_offers_no_install() {
        let r = Remote {
            uname: ("FreeBSD", "amd64"),
            path_bin: None,
            ..Remote::default()
        }
        .check();
        assert!(!r.offers_install());
        assert!(matches!(r.report().unwrap().issues()[0], Issue::UnsupportedPlatform(_)));
        // A compatible build there (from source) is fine.
        let built = Remote {
            uname: ("Linux", "riscv64"),
            ..Remote::default()
        }
        .check();
        assert!(built.is_ready());
    }

    #[test]
    fn git_and_directory_problems_need_attention_not_repair() {
        let issues = |r: Remote| r.check().report().unwrap().issues();
        assert_eq!(
            issues(Remote {
                git: None,
                dir: "unchecked",
                ..Remote::default()
            }),
            [Issue::Git(GitStatus::Missing)]
        );
        assert_eq!(
            issues(Remote {
                git: None,
                git_login: Some("/opt/homebrew/bin/git"),
                dir: "unchecked",
                ..Remote::default()
            }),
            [Issue::Git(GitStatus::LoginShellOnly {
                path: "/opt/homebrew/bin/git".into()
            })]
        );
        for (dir, info, want) in [
            ("missing", "", DirStatus::Missing),
            ("notdir", "", DirStatus::NotADirectory),
            ("noaccess", "", DirStatus::NotAccessible),
            ("readonly", "", DirStatus::ReadOnly),
            (
                "gitfail",
                "fatal: not a git repository (or any of the parent directories): .git",
                DirStatus::NotARepository,
            ),
            (
                "notroot",
                "/home/me/repo",
                DirStatus::NotRepoRoot {
                    root: "/home/me/repo".into(),
                },
            ),
            (
                "gitfail",
                "fatal: detected dubious ownership in repository at '/srv/p'",
                DirStatus::GitRefused {
                    detail: "fatal: detected dubious ownership in repository at '/srv/p'".into(),
                },
            ),
        ] {
            let r = Remote {
                dir,
                info,
                ..Remote::default()
            }
            .check();
            assert!(!r.is_ready() && !r.offers_install(), "{dir}");
            assert_eq!(r.report().unwrap().issues(), [Issue::Dir(want.clone())]);
            assert!(!Issue::Dir(want).message("/srv/p").is_empty());
        }
        // No answer about the directory is never usable.
        let r = Remote { dir: "", ..Remote::default() }.check();
        assert!(!r.is_ready());
    }

    // ---- Install CLI ------------------------------------------------------

    #[test]
    fn install_reuses_a_compatible_cli_and_rechecks() {
        let ready = Remote::default().stdout();
        // setup's probe (sees the compatible binary), then the re-check.
        let exec = Script::new(vec![Ok(out(0, &ready, "")), Ok(out(0, &ready, ""))]);
        let done = install_cli(&exec, &no_fetch(), &hub(), &target()).unwrap();
        assert!(matches!(done.action, SetupAction::AlreadyCompatible { .. }));
        assert!(done.readiness.is_ready());
        assert_eq!(exec.scripts.lock().unwrap().len(), 2, "no install script ran");
    }

    #[test]
    fn install_puts_a_build_beside_an_old_one_then_verifies() {
        let before = Remote {
            path_bin: Some(("/usr/bin/shelbi", "shelbi 0.8.0")),
            ..Remote::default()
        };
        let after = Remote {
            path_bin: Some(("/usr/bin/shelbi", "shelbi 0.8.0")),
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
            ..Remote::default()
        };
        let exec = Script::new(vec![
            Ok(out(0, &before.stdout(), "")),
            Ok(out(0, &installed("0.9.0"), "")),
            Ok(out(0, &after.stdout(), "")),
        ]);
        let done = install_cli(&exec, &no_fetch(), &hub(), &target()).unwrap();
        match &done.action {
            SetupAction::Installed { alongside, via_hub_copy, .. } => {
                assert!(!via_hub_copy);
                assert_eq!(alongside.as_ref().unwrap().path, "/usr/bin/shelbi", "left untouched");
            }
            other => panic!("{other:?}"),
        }
        assert!(done.readiness.is_ready());
        let scripts = exec.scripts.lock().unwrap();
        assert!(scripts[1].contains(".shelbi/bin"), "the install targets ~/.shelbi/bin");
        assert!(!scripts[1].contains("/usr/bin/shelbi"), "never over the package binary");
    }

    #[test]
    fn install_copies_from_the_hub_when_the_remote_has_no_network() {
        let before = Remote {
            path_bin: None,
            ..Remote::default()
        };
        let after = Remote {
            path_bin: None,
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
            ..Remote::default()
        };
        let exec = Script::new(vec![
            Ok(out(0, &before.stdout(), "")),
            Ok(out(20, "", "curl: (6) Could not resolve host: github.com")),
            Ok(out(0, &installed("0.9.0"), "")),
            Ok(out(0, &after.stdout(), "")),
        ]);
        let fetch = Fetch::serving(b"tarball".to_vec());
        let done = install_cli(&exec, &fetch, &hub(), &target()).unwrap();
        assert!(matches!(done.action, SetupAction::Installed { via_hub_copy: true, .. }));
        assert_eq!(exec.stdin.lock().unwrap()[0], b"tarball");
        assert!(done.readiness.is_ready());
    }

    #[test]
    fn a_failed_install_reports_and_a_retry_succeeds() {
        let before = Remote {
            path_bin: None,
            ..Remote::default()
        };
        let exec = Script::new(vec![
            Ok(out(0, &before.stdout(), "")),
            Ok(out(10, "", "mkdir: /home/me/.shelbi: Read-only file system")),
        ]);
        let err = install_cli(&exec, &no_fetch(), &hub(), &target()).unwrap_err();
        assert!(matches!(err, SetupError::ReadOnlyHome(_)));

        let after = Remote {
            path_bin: None,
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.1")),
            ..Remote::default()
        };
        let exec = Script::new(vec![
            Ok(out(0, &before.stdout(), "")),
            Ok(out(0, &installed("0.9.1"), "")),
            Ok(out(0, &after.stdout(), "")),
        ]);
        assert!(install_cli(&exec, &no_fetch(), &hub(), &target()).unwrap().readiness.is_ready());
    }

    #[test]
    fn install_on_an_unreachable_host_fails_as_unreachable() {
        let exec = Script::new(vec![Ok(out(255, "", "ssh: connect to host x: Operation timed out"))]);
        assert!(matches!(
            install_cli(&exec, &no_fetch(), &hub(), &target()),
            Err(SetupError::Unreachable(_))
        ));
    }

    #[test]
    fn install_still_needs_git_and_the_directory() {
        let before = Remote {
            path_bin: None,
            dir: "missing",
            ..Remote::default()
        };
        let after = Remote {
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
            path_bin: None,
            dir: "missing",
            ..Remote::default()
        };
        let exec = Script::new(vec![
            Ok(out(0, &before.stdout(), "")),
            Ok(out(0, &installed("0.9.0"), "")),
            Ok(out(0, &after.stdout(), "")),
        ]);
        let done = install_cli(&exec, &no_fetch(), &hub(), &target()).unwrap();
        assert!(!done.readiness.is_ready(), "the directory still needs attention");
        assert_eq!(done.readiness.report().unwrap().issues(), [Issue::Dir(DirStatus::Missing)]);
        // Nothing in any script creates or clones it.
        for s in exec.scripts.lock().unwrap().iter() {
            assert!(!s.contains("git clone") && !s.contains("mkdir -p \"$D\""));
        }
    }

    // ---- the record -------------------------------------------------------

    #[test]
    fn only_a_ready_check_writes_a_record_bound_to_its_host() {
        let _g = crate::test_lock::acquire();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("SHELBI_HOME", home.path());
        let missing = Remote {
            path_bin: None,
            ..Remote::default()
        }
        .check();
        assert!(!save_verified_record("Build server", "build-server", &missing).unwrap());
        assert!(machine_state::load_machine_record("Build server").is_none());

        assert!(save_verified_record("Build server", "build-server", &Remote::default().check()).unwrap());
        let rec = machine_state::load_machine_record("Build server").unwrap();
        assert_eq!(rec.host.as_deref(), Some("build-server"));
        assert!(rec.is_for_host("build-server") && !rec.is_for_host("other"));
        assert_eq!(machine_state::load_machine_records().len(), 1, "no other machine's entry");
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- the script against a real shell ----------------------------------

    /// The check script against a real `/bin/sh` and a fake filesystem: a
    /// stub login shell, no `shelbi`, and a real `git` checkout (when this
    /// host has Git).
    #[cfg(unix)]
    #[test]
    fn the_script_reads_a_real_directory() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let repo = root_path.join("my project");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(init.success());
        let run = |dir: &std::path::Path| {
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(readiness_script(&dir.to_string_lossy()))
                .env("SHELL", "/bin/sh")
                .env("HOME", &root_path)
                .output()
                .unwrap();
            parse_readiness_output(&String::from_utf8_lossy(&out.stdout), &hub())
        };
        assert_eq!(run(&repo).dir, DirStatus::Usable);
        assert!(matches!(run(&repo).git, GitStatus::Usable { .. }));
        assert_eq!(
            run(&repo.join("sub")).dir,
            DirStatus::NotRepoRoot {
                root: repo.to_string_lossy().into_owned()
            }
        );
        assert_eq!(run(&root_path.join("nope")).dir, DirStatus::Missing);
        assert!(!root_path.join("nope").exists(), "the check creates nothing");
        assert_eq!(run(&root_path).dir, DirStatus::NotARepository);
    }

    // ---- the forms' report and service --------------------------------------

    fn stages(r: &Report) -> Vec<(StageKind, StageState)> {
        r.stages.iter().map(|s| (s.kind, s.state)).collect()
    }

    fn state(r: &Report, kind: StageKind) -> (StageState, String) {
        let s = r.stage(kind).unwrap();
        (s.state, s.result.clone())
    }

    #[test]
    fn a_ready_machine_passes_every_stage() {
        let r = report(&Remote::default().check(), &target());
        assert!(r.is_ready());
        assert!(r.stages.iter().all(|s| s.state == StageState::Passed), "{:?}", r.stages);
        assert_eq!(state(&r, StageKind::Platform).1, "Linux x86_64");
        assert_eq!(state(&r, StageKind::Cli).1, "shelbi 0.9.2 at /usr/local/bin/shelbi");
        assert_eq!(state(&r, StageKind::Git).1, "git version 2.43.0");
        let v = r.verified.unwrap();
        assert_eq!((v.path.as_str(), v.version.as_str(), v.source.as_str()), ("/usr/local/bin/shelbi", "0.9.2", SOURCE_PATH));
        assert!(r.messages.is_empty());
    }

    #[test]
    fn connection_failures_fail_ssh_and_check_nothing_else() {
        let run = |reply: io::Result<ExecOutput>| {
            report(&check(&Script::new(vec![reply]), &hub(), &target()), &target())
        };
        for (reply, reach, result) in [
            (
                Err(io::Error::new(io::ErrorKind::TimedOut, "command did not finish within 30s")),
                Reach::TimedOut,
                "No answer in 30s",
            ),
            (Ok(out(255, "", "ssh: connect to host build-server port 22: Connection refused")), Reach::Unreachable, "Couldn’t connect"),
            (Ok(out(255, "", "me@build-server: Permission denied (publickey).")), Reach::AuthDenied, "Login refused"),
            (Ok(out(255, "", "Host key verification failed.")), Reach::HostKey, "Host key not trusted"),
        ] {
            let r = run(reply);
            assert_eq!(r.reach, reach);
            assert_eq!(state(&r, StageKind::Ssh), (StageState::Failed, result.to_string()));
            assert!(r.stages[1..].iter().all(|s| s.state == StageState::Skipped), "{reach:?}");
            assert!(!r.is_ready() && !r.offers_install && r.verified.is_none());
            assert!(r.messages[0].contains("build-server"), "says what to do: {:?}", r.messages);
        }
        let r = run(Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")));
        assert_eq!(r.reach, Reach::Cancelled);
        let invalid = report(
            &check(&Script::new(vec![]), &hub(), &ReadinessTarget { host: "-x".into(), work_dir: "/p".into() }),
            &target(),
        );
        assert_eq!(invalid.reach, Reach::Invalid(TargetField::Host));
    }

    #[test]
    fn each_problem_is_its_own_stage() {
        let cases: Vec<(Remote, StageKind, &str, bool)> = vec![
            (Remote { path_bin: None, ..Remote::default() }, StageKind::Cli, "Not installed", true),
            (
                Remote { path_bin: Some(("/usr/bin/shelbi", "shelbi 0.1.0")), ..Remote::default() },
                StageKind::Cli,
                "shelbi 0.1.0 is too old",
                true,
            ),
            (
                Remote { path_bin: Some(("/usr/bin/shelbi", "shelbi 9.0.0")), ..Remote::default() },
                StageKind::Cli,
                "shelbi 9.0.0 is too new",
                true,
            ),
            (
                Remote { path_bin: Some(("/usr/bin/shelbi", "weird")), ..Remote::default() },
                StageKind::Cli,
                "No version reported",
                true,
            ),
            (
                Remote { uname: ("FreeBSD", "amd64"), path_bin: None, ..Remote::default() },
                StageKind::Platform,
                "No release for FreeBSD",
                false,
            ),
            (
                Remote { uname: ("Linux", "aarch64"), path_bin: None, ..Remote::default() },
                StageKind::Platform,
                "No release for Linux arm64",
                false,
            ),
            (Remote { git: None, dir: "unchecked", ..Remote::default() }, StageKind::Git, "Not installed", false),
            (
                Remote { git: None, git_login: Some("/opt/homebrew/bin/git"), dir: "unchecked", ..Remote::default() },
                StageKind::Git,
                "Only on the login shell’s PATH",
                false,
            ),
            (Remote { dir: "missing", ..Remote::default() }, StageKind::Directory, "Doesn’t exist", false),
            (Remote { dir: "readonly", ..Remote::default() }, StageKind::Directory, "Not writable", false),
            (
                Remote { dir: "notroot", info: "/home/me", ..Remote::default() },
                StageKind::Directory,
                "Inside the checkout at /home/me",
                false,
            ),
            (
                Remote { dir: "gitfail", info: "fatal: not a git repository", ..Remote::default() },
                StageKind::Directory,
                "Not a Git checkout",
                false,
            ),
        ];
        for (remote, kind, result, install) in cases {
            let r = report(&remote.check(), &target());
            assert_eq!(r.reach, Reach::Reached);
            assert_eq!(state(&r, StageKind::Ssh).0, StageState::Passed, "SSH got there");
            assert_eq!(state(&r, kind), (StageState::Attention, result.to_string()), "{kind:?}");
            assert_eq!(r.first_problem().map(|s| s.kind), Some(kind));
            assert_eq!(r.offers_install, install, "{kind:?} {result}");
            assert!(!r.is_ready() && r.verified.is_none());
            assert!(!r.messages.is_empty());
        }
        // Without Git the directory waits for it rather than failing.
        let r = report(&Remote { git: None, dir: "unchecked", ..Remote::default() }.check(), &target());
        assert_eq!(state(&r, StageKind::Directory), (StageState::Skipped, "Needs Git to check".to_string()));
        // A compatible CLI on a platform without a release is fine.
        let r = report(
            &Remote { uname: ("Linux", "aarch64"), ..Remote::default() }.check(),
            &target(),
        );
        assert!(r.is_ready(), "{:?}", stages(&r));
    }

    fn service(script: &Arc<Script>, fetch: fake::Fetch) -> (ReadinessService, Arc<std::sync::Mutex<Vec<Record>>>) {
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = records.clone();
        let s = service_with(
            fake::connector(script.clone()),
            Arc::new(fetch),
            Arc::new(move |r: &Record| seen.lock().unwrap().push(r.clone())),
            hub(),
        );
        (s, records)
    }

    fn request(intent: ReadinessIntent, record_as: Option<&str>) -> ReadinessRequest {
        ReadinessRequest {
            run: 1,
            intent,
            target: shelbi_app::settings::machine_check::ReadinessTarget {
                project: "p".into(),
                machine: record_as.map(str::to_string),
                name: "build".into(),
                host: "build-server".into(),
                work_dir: "/home/me/my project".into(),
                record_as: record_as.map(str::to_string),
            },
        }
    }

    #[test]
    fn the_service_checks_installs_and_records_a_saved_machine() {
        let missing = Remote { path_bin: None, ..Remote::default() };
        let after = Remote {
            path_bin: None,
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
            ..Remote::default()
        };
        let script = Arc::new(Script::new(vec![missing.reply()]));
        let (s, records) = service(&script, fake::Fetch::none());
        let checked = s.run(&request(ReadinessIntent::Test, Some("build")), Arc::default());
        assert!(checked.offers_install() && !checked.is_ready());
        assert!(records.lock().unwrap().is_empty(), "nothing verified");
        assert_eq!(script.hosts.lock().unwrap().as_slice(), ["build-server"]);

        // Install: setup's probe, the install, the check again.
        script.push(vec![missing.reply(), Ok(out(0, &installed("0.9.0"), "")), after.reply()]);
        let done = s.run(&request(ReadinessIntent::InstallCli, Some("build")), Arc::default());
        let CheckOutcome::Installed { note, changed, report } = &done else { panic!("{done:?}") };
        assert!(*changed && note.starts_with("Installed Shelbi 0.9.0 at ~/.shelbi/bin/shelbi"), "{note}");
        assert!(report.is_ready());
        assert!(done.side_effect("build-server").is_some());
        let rec = records.lock().unwrap();
        assert_eq!(rec.len(), 1, "the saved machine is recorded at once");
        assert_eq!((rec[0].machine.as_str(), rec[0].cli.path.as_str()), ("build", "/home/me/.shelbi/bin/shelbi"));
        assert_eq!(script.pending(), 0);
    }

    #[test]
    fn the_service_copies_from_the_hub_and_keeps_a_package_binary() {
        let old = Remote { path_bin: Some(("/usr/bin/shelbi", "shelbi 0.8.0")), ..Remote::default() };
        let after = Remote {
            path_bin: Some(("/usr/bin/shelbi", "shelbi 0.8.0")),
            home_bin: Some(("/home/me/.shelbi/bin/shelbi", "shelbi 0.9.0")),
            ..Remote::default()
        };
        let script = Arc::new(Script::new(vec![
            old.reply(),
            Ok(out(20, "", "curl: (6) Could not resolve host: github.com")),
            Ok(out(0, &installed("0.9.0"), "")),
            after.reply(),
        ]));
        let fetch = fake::Fetch::serving(b"tarball".to_vec());
        let (s, records) = service(&script, fetch);
        let done = s.run(&request(ReadinessIntent::InstallCli, None), Arc::default());
        let CheckOutcome::Installed { note, report, .. } = &done else { panic!("{done:?}") };
        assert!(note.contains("copied from"), "{note}");
        assert!(note.contains("/usr/bin/shelbi was left as it was"), "{note}");
        assert!(report.is_ready());
        assert_eq!(script.stdin.lock().unwrap()[0], b"tarball");
        for sent in script.scripts.lock().unwrap().iter() {
            assert!(!sent.contains("> /usr/bin/shelbi") && !sent.contains("rm /usr/bin/shelbi"), "never over the package binary");
        }
        assert!(records.lock().unwrap().is_empty(), "a draft records nothing");
    }

    #[test]
    fn the_service_reports_a_failed_install_and_retries() {
        let missing = Remote { path_bin: None, ..Remote::default() };
        let script = Arc::new(Script::new(vec![
            missing.reply(),
            Ok(out(10, "", "mkdir: /home/me/.shelbi: Read-only file system")),
        ]));
        let (s, _) = service(&script, fake::Fetch::none());
        let failed = s.run(&request(ReadinessIntent::InstallCli, None), Arc::default());
        let CheckOutcome::InstallFailed { message, retry } = &failed else { panic!("{failed:?}") };
        assert!(*retry && message.contains("~/.shelbi/bin"), "{message}");
        assert!(failed.offers_install());

        let script = Arc::new(Script::new(vec![Ok(out(255, "", "me@build-server: Permission denied (publickey)."))]));
        let (s, _) = service(&script, fake::Fetch::none());
        let failed = s.run(&request(ReadinessIntent::InstallCli, None), Arc::default());
        assert!(matches!(failed, CheckOutcome::InstallFailed { retry: false, .. }), "a login failure isn't retried");
    }

    #[test]
    fn a_stopped_check_answers_cancelled() {
        let script = Arc::new(Script::new(vec![Remote::default().reply()]));
        script.hold.store(true, std::sync::atomic::Ordering::Release);
        let (s, records) = service(&script, fake::Fetch::none());
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let running = std::thread::spawn(move || s.run(&request(ReadinessIntent::Test, Some("build")), flag));
        cancel.store(true, std::sync::atomic::Ordering::Release);
        let outcome = running.join().unwrap();
        assert_eq!(outcome.report().map(|r| r.reach), Some(Reach::Cancelled));
        assert!(records.lock().unwrap().is_empty());
        assert_eq!(script.pending(), 1, "the remote never answered");
    }

    #[test]
    fn install_answers_say_what_setup_did() {
        use crate::machine::{parse_version, ResolvedBinary};
        let v = parse_version("0.9.0").unwrap();
        let ready = Remote::default().check();
        let installed = install_outcome(
            Ok(Installed {
                action: SetupAction::Installed {
                    path: "~/.shelbi/bin/shelbi".into(),
                    version: v,
                    alongside: Some(ResolvedBinary {
                        path: "/usr/bin/shelbi".into(),
                        version: parse_version("0.8.0"),
                    }),
                    via_hub_copy: true,
                },
                readiness: ready.clone(),
            }),
            &target(),
        );
        let CheckOutcome::Installed { note, changed, .. } = &installed else { panic!() };
        let here = shelbi_app::settings::workspaces::LOCAL_PHRASE;
        assert!(note.contains("~/.shelbi/bin/shelbi") && note.contains(&format!("copied from {here}")), "{note}");
        assert!(note.contains("/usr/bin/shelbi was left as it was"));
        assert!(*changed && installed.is_ready());

        let reused = install_outcome(
            Ok(Installed {
                action: SetupAction::AlreadyCompatible {
                    path: "/usr/local/bin/shelbi".into(),
                    version: v,
                    source: "path",
                },
                readiness: ready,
            }),
            &target(),
        );
        assert!(
            matches!(reused, CheckOutcome::Installed { ref note, changed: false, .. } if note.contains("already installed"))
        );
        assert_eq!(reused.side_effect("build-server"), None, "nothing changed there");

        let down = install_outcome(Err(SetupError::Unreachable("timed out".into())), &target());
        assert!(!down.offers_install(), "a failed connection is retried with Test again");
        let home = install_outcome(Err(SetupError::ReadOnlyHome("Read-only file system".into())), &target());
        assert!(home.offers_install());
        let CheckOutcome::InstallFailed { message, .. } = home else { panic!() };
        assert!(message.starts_with("Couldn’t write ~/.shelbi/bin") && message.contains("install again"));
    }
}
