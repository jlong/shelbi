//! Spawn a detached `shelbi __session` process.
//!
//! A client runs `shelbi __session` detached, passing argv, cwd, initial size,
//! metadata, and **an explicit environment**. The session process never
//! inherits the environment of whatever happened to launch it:
//!
//! - The environment is the user's interactive login-shell environment,
//!   captured once (`$SHELL -l -i -c env`) and cached, because `.zshrc` is where
//!   nvm, fnm, and Homebrew PATH setup usually live and a plain `-l -c` skips
//!   it.
//! - Terminal-identity variables are scrubbed (`TMUX`, `TMUX_PANE`,
//!   `TERM_PROGRAM`, `STY`). The session sets `TERM=xterm-256color`,
//!   `COLORTERM=truecolor`, and its own `TERM_PROGRAM=shelbi`.
//! - On Linux the process is started with `systemd-run --user --scope` where
//!   available (with lingering enabled, so logind's `KillUserProcesses` does not
//!   reap it at logout); otherwise `setsid`. All stdio is redirected so a
//!   launching `ssh` does not hang.
//!
//! TODO (`rt-session-process`): implement [`spawn`] and the login-shell
//! environment capture/cache.

/// Parameters for launching a new session.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// The command and arguments the session should run.
    pub argv: Vec<String>,
    /// Working directory for the child.
    pub cwd: String,
    /// Initial PTY size, `(cols, rows)`.
    pub size: (u16, u16),
    /// Readable session name recorded in `meta.json`.
    pub name: String,
    /// Optional task id this session serves.
    pub task: Option<String>,
}

/// Spawn a detached session process and return its short id.
///
/// TODO (`rt-session-process`): build the explicit environment, create the
/// session directory, launch `shelbi __session` detached, and wait for its
/// socket to appear.
pub fn spawn(_spec: &SpawnSpec) -> Result<String, crate::ClientError> {
    unimplemented!("rt-session-process implements detached session spawn")
}
