//! Starting the hub daemon through the CLI.
//!
//! The app never embeds the daemon: it runs `<cli> daemon`, the same
//! on-demand start the CLI and TUI use (`shelbi_state::ensure_daemon_running`),
//! with the discovered CLI and the login environment installed as the
//! process-wide [`DaemonLauncher`]. Installing it process-wide matters: shared
//! code (`shelbi_app::execute_mutation` and friends) also starts the daemon on
//! demand, and without the launcher it would re-run the *app* binary.

use std::path::{Path, PathBuf};
use std::time::Duration;

use shelbi_state::{DaemonLauncher, DaemonStartError};

use super::env::Env;

/// How many trailing stderr lines the failure screen shows.
pub const STDERR_TAIL_LINES: usize = 20;

/// How long to wait for a started daemon to answer. Longer than the CLI's
/// 10s: a first launch after login can be slow, and the screen shows progress.
pub const START_DEADLINE: Duration = Duration::from_secs(20);

/// The daemon could not be brought up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonFailure {
    /// What went wrong, one line.
    pub reason: String,
    /// The last lines the daemon wrote to stderr, oldest first.
    pub stderr_tail: Vec<String>,
    /// The file holding the daemon's full stderr.
    pub stderr_log: Option<PathBuf>,
}

/// Where the started daemon's stderr goes:
/// `<SHELBI_HOME>/logs/desktop-daemon-start.log`, rewritten on each start.
pub fn stderr_log_path() -> Option<PathBuf> {
    shelbi_state::shelbi_home()
        .ok()
        .map(|home| home.join("logs").join("desktop-daemon-start.log"))
}

/// Make `<cli> daemon` this process's daemon launcher and ensure a daemon is
/// running, starting one if none is.
pub fn start(cli: &Path, env: &Env, deadline: Duration) -> Result<(), DaemonFailure> {
    let stderr_log = stderr_log_path();
    let launcher = DaemonLauncher {
        exe: cli.to_path_buf(),
        env: Some(env.clone()),
        stderr_log: stderr_log.clone(),
    };
    shelbi_state::set_daemon_launcher(launcher.clone());
    shelbi_state::ensure_daemon_running_with(&launcher, deadline).map_err(|err| {
        let stderr_tail = match &err {
            // Nothing ran, so there is no stderr to show.
            DaemonStartError::Spawn(_) => Vec::new(),
            DaemonStartError::Exited { .. } | DaemonStartError::Timeout { .. } => stderr_log
                .as_deref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|text| tail_lines(&text, STDERR_TAIL_LINES))
                .unwrap_or_default(),
        };
        DaemonFailure {
            reason: err.to_string(),
            stderr_tail,
            stderr_log,
        }
    })
}

/// The last `n` non-blank lines of `text`, oldest first.
pub fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_last_non_blank_lines_in_order() {
        let text = "one\n\ntwo\nthree\n  \nfour\n";
        assert_eq!(tail_lines(text, 2), ["three", "four"]);
        assert_eq!(tail_lines(text, 10), ["one", "two", "three", "four"]);
        assert!(tail_lines("", 3).is_empty());
    }
}
