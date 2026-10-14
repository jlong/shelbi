//! How a dead pane is brought back before a client view re-attaches
//! (`rt-auto-restart-killed-panes`): the [`Relauncher`] seam and the concrete
//! relaunchers the TUI and the desktop app install on their views.
//!
//! A view's restart loop is pane-kind agnostic; these supply the one
//! kind-specific step:
//!
//! - **Content sessions** (View-Diff / Edit) are re-`Ensure`d through the daemon
//!   on *every* restart, because the daemon does not resurrect a dead content
//!   session on its own.
//! - **Persistent panes** (orchestrator / agent / review slot) are the daemon
//!   supervisor's to relaunch, so an automatic restart here is a no-op (the
//!   manager just re-attaches once the daemon brings the pane back) and only a
//!   *reopen* drops [`shelbi_state::supervision_relaunch`] markers to reset the
//!   daemon's spent budget.

use shelbi_state::supervision_relaunch as sr;

use super::SessionRef;
use crate::{ReviewRole, ReviewSessionOp, WorkspaceSessionOp};

/// How a dead pane is brought back before the view re-attaches, abstracted so
/// a view's restart loop is one code path for every pane kind and so it is
/// testable without a daemon.
///
/// The step runs off the UI thread (it may block on the daemon socket) and
/// differs by pane kind: a *content* session (View-Diff / Edit) is
/// re-`Ensure`d through the daemon, which won't resurrect it on its own; a
/// *persistent* pane (orchestrator / agent / review slot) is the daemon
/// supervisor's to relaunch, so an auto-restart is a no-op (the view just
/// re-attaches once the daemon brings it back) and only a `reopen` drops a
/// [`shelbi_state::supervision_relaunch`] marker to reset that daemon budget.
pub trait Relauncher: Send + Sync {
    /// (Re)launch the session for `target`. `reopen` is true when the user
    /// re-selected a pane the budget had given up on (reset + relaunch), false
    /// for an automatic in-view restart. `Ok` means the request was placed, not
    /// that the session is live — the following connect proves that.
    fn relaunch(&self, target: &SessionRef, reopen: bool) -> Result<(), String>;
}

/// A relauncher that does nothing: for a view whose pane kind the daemon fully
/// owns and which never needs a reopen marker (and for tests).
pub struct NoopRelauncher;

impl Relauncher for NoopRelauncher {
    fn relaunch(&self, _target: &SessionRef, _reopen: bool) -> Result<(), String> {
        Ok(())
    }
}

/// Map the content session's role token back to the wire role.
fn role_from_str(s: &str) -> Option<ReviewRole> {
    match s {
        "editor" => Some(ReviewRole::Editor),
        "diff" => Some(ReviewRole::Diff),
        _ => None,
    }
}

/// Drop the reopen markers that reset a persistent pane's *daemon* supervision
/// budget. A `Workspace` pane can be governed by more than one daemon pass (the
/// per-workspace supervisor and the stranded-dev / stranded-review resume
/// passes), and the client can't tell which gave up, so it writes every key that
/// could apply — each pass consumes only its own.
fn write_persistent_reopen_markers(project: &str, target: &SessionRef) -> Result<(), String> {
    let res = match target {
        SessionRef::Orchestrator => {
            sr::request_supervision_relaunch(project, &sr::orchestrator_relaunch_key())
        }
        SessionRef::Workspace(w) => {
            sr::request_supervision_relaunch(project, &sr::workspace_supervise_relaunch_key(w))
                .and_then(|()| {
                    sr::request_supervision_relaunch(project, &sr::workspace_resume_relaunch_key(w))
                })
                .and_then(|()| {
                    sr::request_supervision_relaunch(project, &sr::review_resume_relaunch_key(w))
                })
        }
        // Content targets are never persistent.
        SessionRef::Review { .. } | SessionRef::WorkspaceContent { .. } => Ok(()),
    };
    res.map_err(|e| e.to_string())
}

/// Relauncher for the main area, which shows only the orchestrator and agent
/// panes — all daemon-supervised. The desktop's Chat view installs it for the
/// orchestrator too.
pub struct PersistentRelauncher {
    pub project: String,
}

impl Relauncher for PersistentRelauncher {
    fn relaunch(&self, target: &SessionRef, reopen: bool) -> Result<(), String> {
        if reopen {
            write_persistent_reopen_markers(&self.project, target)
        } else {
            // The daemon owns the auto-restart; just re-attach.
            Ok(())
        }
    }
}

/// Relauncher for the review interface's content manager. It shows the review
/// agent chat (`Workspace(slot)`, persistent) and the editor/diff content
/// sessions (`Review`, re-ensured by the review *task*).
pub struct ReviewRelauncher {
    pub project: String,
    pub task: String,
}

impl Relauncher for ReviewRelauncher {
    fn relaunch(&self, target: &SessionRef, reopen: bool) -> Result<(), String> {
        match target {
            SessionRef::Review { role, .. } => {
                let Some(role) = role_from_str(role) else {
                    return Ok(());
                };
                crate::review_session(
                    &self.project,
                    &self.task,
                    ReviewSessionOp::Ensure { role },
                    &mut |_, _| {},
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            }
            SessionRef::Workspace(_) | SessionRef::Orchestrator => {
                if reopen {
                    write_persistent_reopen_markers(&self.project, target)
                } else {
                    Ok(())
                }
            }
            SessionRef::WorkspaceContent { .. } => Ok(()),
        }
    }
}

/// Relauncher for the workspace interface's content manager. It shows the
/// workspace agent (`Workspace`, persistent) and the editor/diff content
/// sessions (`WorkspaceContent`, re-ensured by workspace name).
pub struct WorkspaceContentRelauncher {
    pub project: String,
}

impl Relauncher for WorkspaceContentRelauncher {
    fn relaunch(&self, target: &SessionRef, reopen: bool) -> Result<(), String> {
        match target {
            SessionRef::WorkspaceContent { workspace, role } => {
                let Some(role) = role_from_str(role) else {
                    return Ok(());
                };
                crate::workspace_session(
                    &self.project,
                    workspace,
                    WorkspaceSessionOp::Ensure { role },
                    &mut |_, _| {},
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            }
            SessionRef::Workspace(_) | SessionRef::Orchestrator => {
                if reopen {
                    write_persistent_reopen_markers(&self.project, target)
                } else {
                    Ok(())
                }
            }
            SessionRef::Review { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_tokens_round_trip() {
        assert_eq!(role_from_str("editor"), Some(ReviewRole::Editor));
        assert_eq!(role_from_str("diff"), Some(ReviewRole::Diff));
        assert_eq!(role_from_str("chat"), None);
    }
}
