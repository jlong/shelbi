//! Quit Project / Quit Shelbi on the plain terminal, with live progress.
//!
//! Both `shelbi quit` and the TUI's quit actions come here (the TUI once it has
//! given the terminal back), so a quit always shows what it's doing instead of
//! sitting silent while the daemon works: the daemon reports each step of its
//! quit ([`shelbi_proto::control::ServerMsg::Progress`]) and the shared
//! [`Progress`] printer shows them, by the same rules as startup's (stderr,
//! terminal only, quiet unless a step outlasts its grace, a gave-up line always
//! naming what it stopped waiting on). Quit Shelbi then waits, as a step of its
//! own, for the daemon process to be gone, so "done" means done.

use std::time::{Duration, Instant};

use shelbi_client::{ClientError, ControlClient};

use crate::progress::{seconds, Progress};

/// How long Quit Shelbi waits for the daemon to exit once it has acked. It
/// stops right after the ack, finishing only the control jobs already running.
const DAEMON_EXIT_WAIT: Duration = Duration::from_secs(10);

/// What to quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitTarget<'a> {
    /// One project: its sessions end, other projects keep running.
    Project(&'a str),
    /// Every project, and then the daemon.
    Shelbi,
}

impl QuitTarget<'_> {
    fn title(&self) -> String {
        match self {
            QuitTarget::Project(p) => format!("Quitting {p}…"),
            QuitTarget::Shelbi => "Shutting down Shelbi…".to_string(),
        }
    }
}

/// Quit `target` through `client`, showing each step as it runs.
pub fn quit_with_progress(client: &mut ControlClient, target: QuitTarget) -> Result<(), ClientError> {
    let started = Instant::now();
    let progress = Progress::stderr(&target.title());
    let mut on_step = |step: &shelbi_proto::control::Step| progress.apply(step);
    match target {
        QuitTarget::Project(project) => client.quit_project_with_progress(project, &mut on_step)?,
        QuitTarget::Shelbi => {
            client.quit_shelbi_with_progress(&mut on_step)?;
            wait_for_daemon_exit(&progress);
        }
    }
    progress.footer(&format!("Done in {}.", seconds(started.elapsed())));
    Ok(())
}

/// Wait (bounded) for the daemon to release its single-instance lock, which it
/// holds until the process is gone.
fn wait_for_daemon_exit(progress: &Progress) {
    let pid = shelbi_state::read_daemon_pid().ok().flatten();
    progress.step("waiting for daemon to exit", |step| {
        step.finish_as("stopped the daemon");
        let deadline = Instant::now() + DAEMON_EXIT_WAIT;
        while shelbi_state::daemon_lock_held() {
            if Instant::now() >= deadline {
                let what = match pid {
                    Some(pid) => format!("the hub daemon (pid {pid})"),
                    None => "the hub daemon".to_string(),
                };
                step.gave_up("stopped the daemon", &what);
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}
