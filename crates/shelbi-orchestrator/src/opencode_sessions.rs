//! Archiving Shelbi's OpenCode sessions once their task is done.
//!
//! Every OpenCode pane Shelbi launches runs on a session Shelbi names
//! (`--session <id>`, recorded in the workspace's OpenCode ledger, see
//! [`shelbi_state::opencode_sessions`]). Those sessions live in the user's own
//! OpenCode history, titled `Shelbi · <workspace> · <task>`. Once the task
//! reaches a done status, this pass archives them (never deletes): they drop
//! out of OpenCode's session lists but stay restorable.
//!
//! It runs on the poller's supervisor tick at its own slow cadence. Each
//! session is archived with
//! `opencode api --standalone session.update --param sessionID=<id> -d '{"archived":true}'`
//! on the workspace's host: a private server for the one call, so the user's
//! background OpenCode service is never touched. A session OpenCode never
//! created (a launch that died first) counts as archived. A failure (host
//! unreachable, OpenCode gone) is retried on later passes, up to
//! [`MAX_ARCHIVE_ATTEMPTS`].

use std::collections::HashSet;
use std::time::{Duration, Instant};

use shelbi_core::{Host, Project, StatusCategory};

/// How often the pass looks for done tasks with unarchived sessions.
const ARCHIVE_CADENCE: Duration = Duration::from_secs(60);

/// Failed archive attempts before the pass stops trying a task's sessions.
pub(crate) const MAX_ARCHIVE_ATTEMPTS: u32 = 3;

/// Upper bound on one `opencode api` call (it starts a private server).
const ARCHIVE_DEADLINE: Duration = Duration::from_secs(30);

/// What archiving one session came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArchiveOutcome {
    Archived,
    /// OpenCode has no such session: nothing to archive.
    Missing,
    Failed(String),
}

/// Archives one OpenCode session on a host. The live one runs `opencode
/// api`; tests substitute a fake.
pub(crate) trait SessionArchiver {
    fn archive(&self, host: &Host, command: &str, session_id: &str) -> ArchiveOutcome;
}

/// The [`SessionArchiver`] that runs OpenCode on the workspace's host,
/// through a login shell so it finds `opencode` on the same PATH the pane
/// did.
pub(crate) struct LiveArchiver;

impl SessionArchiver for LiveArchiver {
    fn archive(&self, host: &Host, command: &str, session_id: &str) -> ArchiveOutcome {
        let script = archive_script(command, session_id);
        match crate::git::run_login_shell_script_with_deadline(host, &script, ARCHIVE_DEADLINE) {
            Ok(out) if out.status.success() => ArchiveOutcome::Archived,
            Ok(out) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                if text.contains("SessionNotFoundError") {
                    ArchiveOutcome::Missing
                } else {
                    ArchiveOutcome::Failed(text.trim().lines().last().unwrap_or("").to_string())
                }
            }
            Err(e) => ArchiveOutcome::Failed(e.to_string()),
        }
    }
}

/// The shell line that archives `session_id` with OpenCode `command`.
fn archive_script(command: &str, session_id: &str) -> String {
    format!(
        "OPENCODE_DISABLE_AUTOUPDATE=1 exec {command} api --standalone session.update \
         --param {param} -d '{{\"archived\":true}}'",
        command = shelbi_agent::shell_escape(command),
        param = shelbi_agent::shell_escape(&format!("sessionID={session_id}")),
    )
}

/// Run [`archive_done_sessions`] when its cadence is due.
pub(crate) fn maybe_archive_done_sessions(project: &Project, next_due: &mut Option<Instant>) {
    if next_due.is_some_and(|due| Instant::now() < due) {
        return;
    }
    *next_due = Some(Instant::now() + ARCHIVE_CADENCE);
    archive_done_sessions(project, &LiveArchiver);
}

/// Archive the OpenCode sessions of every task that has reached done, on
/// every workspace that ran one. Reads only the small ledgers when nothing
/// is pending, and the board only when something is.
pub(crate) fn archive_done_sessions(project: &Project, archiver: &dyn SessionArchiver) {
    let mut pending = Vec::new();
    for workspace in &project.workspaces {
        let Ok(ledger) = shelbi_state::load_opencode_sessions(&project.name, &workspace.name) else {
            continue;
        };
        for (task, session) in ledger.tasks {
            if !session.archived && session.archive_attempts < MAX_ARCHIVE_ATTEMPTS {
                pending.push((workspace, task, session));
            }
        }
    }
    if pending.is_empty() {
        return;
    }
    let Ok(board) = shelbi_state::read_board_with_cfg(&project.name, &project.issue_tracker) else {
        return;
    };
    let done: HashSet<String> = board
        .into_issues()
        .into_iter()
        .filter(|tf| tf.task.column.category() == StatusCategory::Done)
        .map(|tf| tf.task.id)
        .collect();

    for (workspace, task, session) in pending {
        if !done.contains(&task) {
            continue;
        }
        let Some(machine) = project.machine(&workspace.machine) else {
            continue;
        };
        let host = machine.host();
        let ids = session.all_ids(&project.name, &workspace.name, &task);
        let failure = ids.iter().find_map(|id| match archiver.archive(&host, &session.command, id) {
            ArchiveOutcome::Archived | ArchiveOutcome::Missing => None,
            ArchiveOutcome::Failed(why) => Some(why),
        });
        let recorded = shelbi_state::update_opencode_sessions(&project.name, &workspace.name, |ledger| {
            // A launch since the read above owns the entry now: leave it to
            // the next pass.
            let Some(entry) = ledger.tasks.get_mut(&task).filter(|e| e.generation == session.generation) else {
                return (false, ());
            };
            match &failure {
                None => entry.archived = true,
                Some(_) => entry.archive_attempts += 1,
            }
            (true, ())
        });
        if let Err(e) = recorded {
            tracing::warn!(workspace = %workspace.name, task = %task, error = %e, "recording OpenCode archive failed");
        }
        match failure {
            None => tracing::info!(
                workspace = %workspace.name,
                task = %task,
                sessions = ids.len(),
                "archived the done task's OpenCode sessions"
            ),
            Some(why) => tracing::warn!(
                workspace = %workspace.name,
                task = %task,
                error = %why,
                "archiving the done task's OpenCode sessions failed"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_line_runs_a_private_server_for_one_update() {
        assert_eq!(
            archive_script("opencode", "ses_shelbi0123"),
            "OPENCODE_DISABLE_AUTOUPDATE=1 exec opencode api --standalone session.update \
             --param sessionID=ses_shelbi0123 -d '{\"archived\":true}'"
        );
        // A command path with a space stays one word.
        assert!(archive_script("/opt/my tools/opencode", "ses_x").contains("exec '/opt/my tools/opencode' api"));
    }
}
