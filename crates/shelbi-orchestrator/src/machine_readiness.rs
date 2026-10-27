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

/// Which part of a [`ReadinessTarget`] can't be checked as typed.
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
pub fn validate_target(target: &ReadinessTarget) -> Result<(), TargetError> {
    let host = target.host.trim();
    let invalid = |field, message: String| Err(TargetError { field, message });
    if host.is_empty() {
        return invalid(TargetField::Host, "Enter the SSH host to test.".into());
    }
    if !crate::pool_admin::usable_ssh_host(host) {
        return invalid(
            TargetField::Host,
            format!("`{}` isn’t a host SSH can connect to.", host.escape_debug()),
        );
    }
    let dir = target.work_dir.trim();
    if dir.is_empty() {
        return invalid(
            TargetField::WorkDir,
            "Enter the project’s directory on the machine to test it.".into(),
        );
    }
    if dir.chars().any(char::is_control) {
        return invalid(
            TargetField::WorkDir,
            format!("`{}` isn’t a valid path.", dir.escape_debug()),
        );
    }
    if !dir.starts_with('/') {
        let hint = if dir == "~" || dir.starts_with("~/") {
            " Write the remote home directory out in full."
        } else {
            ""
        };
        return invalid(
            TargetField::WorkDir,
            format!("`{dir}` isn’t an absolute path.{hint}"),
        );
    }
    Ok(())
}

// ===========================================================================
// Structured result
// ===========================================================================

/// What a readiness check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// The target can't be checked as typed (nothing ran).
    Invalid(TargetError),
    /// SSH couldn't connect or timed out. Says nothing about the remote.
    Unreachable { detail: String },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{ExecOutput, HubFetchError};
    use shelbi_state::machine_state::{SOURCE_PATH, SOURCE_SHELBI_BIN};
    use std::cell::RefCell;
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

    fn out(code: i32, stdout: &str, stderr: &str) -> ExecOutput {
        ExecOutput {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// A remote's answers to the check, as fake filesystem facts.
    struct Remote<'a> {
        uname: (&'a str, &'a str),
        path_bin: Option<(&'a str, &'a str)>,
        home_bin: Option<(&'a str, &'a str)>,
        git: Option<&'a str>,
        git_login: Option<&'a str>,
        dir: &'a str,
        info: &'a str,
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
        fn stdout(&self) -> String {
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

        fn check(&self) -> Readiness {
            check(&Script::new(vec![Ok(out(0, &self.stdout(), ""))]), &hub(), &target())
        }
    }

    /// Scripted remote: each call pops the next reply and remembers the script.
    struct Script {
        replies: RefCell<Vec<io::Result<ExecOutput>>>,
        scripts: RefCell<Vec<String>>,
        stdin: RefCell<Vec<Vec<u8>>>,
    }

    impl Script {
        fn new(replies: Vec<io::Result<ExecOutput>>) -> Self {
            Script {
                replies: RefCell::new(replies.into_iter().rev().collect()),
                scripts: RefCell::new(Vec::new()),
                stdin: RefCell::new(Vec::new()),
            }
        }
        fn pop(&self) -> io::Result<ExecOutput> {
            self.replies
                .borrow_mut()
                .pop()
                .expect("an unexpected remote call")
        }
    }

    impl RemoteExec for Script {
        fn label(&self) -> &str {
            "fake"
        }
        fn run_script(&self, script: &str) -> io::Result<ExecOutput> {
            self.scripts.borrow_mut().push(script.to_string());
            self.pop()
        }
        fn run_script_with_stdin(&self, script: &str, stdin: &[u8]) -> io::Result<ExecOutput> {
            self.scripts.borrow_mut().push(script.to_string());
            self.stdin.borrow_mut().push(stdin.to_vec());
            self.pop()
        }
    }

    struct Fetch(RefCell<Option<Result<Vec<u8>, HubFetchError>>>);
    impl HubFetch for Fetch {
        fn fetch_verified(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, HubFetchError> {
            self.0.borrow_mut().take().expect("an unexpected hub fetch")
        }
    }
    fn no_fetch() -> Fetch {
        Fetch(RefCell::new(None))
    }

    fn installed(version: &str) -> String {
        format!("\u{1}INSB\u{1}shelbi {version}\u{1}INSE\u{1}\n")
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
        assert!(exec.scripts.borrow().is_empty());
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
        assert!(matches!(timeout, Readiness::Unreachable { .. }));
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
        assert_eq!(exec.scripts.borrow().len(), 2, "no install script ran");
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
        let scripts = exec.scripts.borrow();
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
        let fetch = Fetch(RefCell::new(Some(Ok(b"tarball".to_vec()))));
        let done = install_cli(&exec, &fetch, &hub(), &target()).unwrap();
        assert!(matches!(done.action, SetupAction::Installed { via_hub_copy: true, .. }));
        assert_eq!(exec.stdin.borrow()[0], b"tarball");
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
        for s in exec.scripts.borrow().iter() {
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
}
