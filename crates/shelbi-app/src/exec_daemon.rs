//! The production mutation path for the app model: route an [`Effect::Mutate`]
//! to the daemon's control socket.
//!
//! [`exec`](crate::exec) defines the executor seam as pure data; this module is
//! the `rt-mutations-daemon` half the doc there promises — it maps the issue
//! [`Mutation`]s onto the [`shelbi_proto::control`] wire protocol and runs them
//! through [`shelbi_client::ControlClient`], starting the daemon on demand. The
//! app composes this with its own client-local navigation handling to form a
//! full [`Executor`](crate::exec::Executor).
//!
//! Only the issue mutations (`MoveIssue` … `AddIssue`) are daemon-routed here;
//! the lifecycle mutations ([`Mutation::ToggleZen`], [`Mutation::AddProject`])
//! are not part of the issue control protocol and are left to the host.

use shelbi_proto::control::{
    AddSpec, EditSpec, ExpectedState, MutationKind, MutationRequest, ReviewSessionOp,
    ReviewSessionRequest, Stream,
};

use crate::exec::{ExecError, ExecOutcome, Mutation};

/// Run one [`Mutation`] against the daemon. Output lines the daemon streams back
/// are passed to `on_line` (the app routes them to its UI; pass a no-op to
/// discard). Issue mutations route through the control socket; `ToggleZen` and
/// `AddProject` are not issue mutations and return a [`ExecError::Backend`] so
/// the host performs them.
pub fn execute_mutation(
    mutation: &Mutation,
    on_line: &mut dyn FnMut(Stream, &str),
) -> ExecOutcome {
    let (project, id, kind) = map_to_kind(mutation)?;
    route(&project, &id, kind, on_line)
}

/// Ask the daemon to start or stop a review slot's editor/diff/server sessions
/// (`rt-tui-review`). The single-process TUI calls this off its UI thread when
/// a review view is switched to (`Ensure`) and when a review is closed
/// (`Close`); the daemon owns the sessions so they outlive a client detach and
/// the teardown frees the slot's port. Streamed lines go to `on_line`.
pub fn review_session(
    project: &str,
    task: &str,
    op: ReviewSessionOp,
    on_line: &mut dyn FnMut(Stream, &str),
) -> ExecOutcome {
    shelbi_state::ensure_daemon_running().map_err(|e| ExecError::Backend(e.to_string()))?;
    let sock = shelbi_state::control_socket_path().map_err(|e| ExecError::Backend(e.to_string()))?;
    let mut client = shelbi_client::ControlClient::connect(&sock, shelbi_state::CLIENT_VERSION)
        .map_err(|e| ExecError::Backend(e.to_string()))?;
    let req = ReviewSessionRequest {
        request_id: 1,
        project: project.to_string(),
        task: task.to_string(),
        op,
    };
    client.review_session(&req, on_line).map_err(map_client_err)
}

/// Map an app [`Mutation`] to `(project, issue id, wire kind)`. The two
/// lifecycle variants have no issue-control-socket representation.
fn map_to_kind(m: &Mutation) -> Result<(String, String, MutationKind), ExecError> {
    Ok(match m {
        Mutation::MoveIssue {
            project,
            id,
            to_status,
        } => (
            project.clone(),
            id.clone(),
            MutationKind::Move {
                to: to_status.clone(),
                reason: Some("user:app".to_string()),
                skip_transition_actions: false,
            },
        ),
        Mutation::StartIssue { project, id } => (
            project.clone(),
            id.clone(),
            MutationKind::Start {
                workspace: None,
                branch: None,
                reason: Some("user:app:start".to_string()),
                force: false,
            },
        ),
        Mutation::AssignIssue {
            project,
            id,
            workspace,
        } => (
            project.clone(),
            id.clone(),
            MutationKind::Assign {
                to: workspace.clone(),
                force: false,
            },
        ),
        Mutation::ApproveReview { project, id } => {
            (project.clone(), id.clone(), MutationKind::Approve)
        }
        Mutation::RejectReview {
            project,
            id,
            reason,
        } => (
            project.clone(),
            id.clone(),
            MutationKind::Reject {
                reason: reason.clone(),
            },
        ),
        Mutation::EditIssue {
            project,
            id,
            title,
            body,
        } => (
            project.clone(),
            id.clone(),
            MutationKind::Edit(Box::new(EditSpec {
                title: title.clone(),
                body_replace: body.clone(),
                ..Default::default()
            })),
        ),
        Mutation::AddIssue {
            project,
            title,
            workflow,
        } => (
            project.clone(),
            String::new(),
            MutationKind::Add(Box::new(AddSpec {
                title: title.clone(),
                id: None,
                status: "backlog".to_string(),
                body: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                workflow: workflow.clone(),
                branch: None,
            })),
        ),
        Mutation::ToggleZen { .. } | Mutation::AddProject { .. } => {
            return Err(ExecError::Backend(
                "lifecycle mutation not handled by the daemon mutation executor".to_string(),
            ))
        }
    })
}

/// Ensure the daemon, connect, send the mutation, and map the result to an
/// [`ExecError`].
fn route(
    project: &str,
    id: &str,
    kind: MutationKind,
    on_line: &mut dyn FnMut(Stream, &str),
) -> ExecOutcome {
    // The `(status, revision)` the app last saw, so the daemon can reject a
    // command whose issue moved on. `add` has no prior state. Read through the
    // store (shelbi-state), not the orchestrator, to keep this crate light.
    let expected = match &kind {
        MutationKind::Add(_) => None,
        _ => read_expected(project, id),
    };

    shelbi_state::ensure_daemon_running().map_err(|e| ExecError::Backend(e.to_string()))?;
    let sock = shelbi_state::control_socket_path().map_err(|e| ExecError::Backend(e.to_string()))?;
    let mut client = shelbi_client::ControlClient::connect(&sock, shelbi_state::CLIENT_VERSION)
        .map_err(|e| ExecError::Backend(e.to_string()))?;

    // Out-of-date gate (removing-tmux Phase 4f, "Versions and upgrades"): if the
    // daemon answered its hello with a different version than ours, this client
    // is out of date and must NOT send a mutation — it should re-exec / relaunch
    // first. Refusing here, after the hello but before the `Mutate` frame, is
    // what makes "an out-of-date client sends no mutations before it re-execs"
    // hold for the daemon-routed path.
    if let Some(err) = out_of_date_gate(&client.daemon_version, shelbi_state::CLIENT_VERSION) {
        return Err(err);
    }

    let req = MutationRequest {
        request_id: 1,
        project: project.to_string(),
        id: id.to_string(),
        expected,
        kind,
    };
    client.mutate(&req, on_line).map_err(map_client_err)
}

/// The out-of-date mutation gate: `Some(err)` when `daemon_version` differs
/// from `client_version`, so the caller refuses the mutation before sending it.
/// Pure so "an out-of-date client sends no mutations before it re-execs" is
/// unit-testable without a daemon.
fn out_of_date_gate(daemon_version: &str, client_version: &str) -> Option<ExecError> {
    if daemon_version == client_version {
        return None;
    }
    Some(ExecError::Backend(format!(
        "hub daemon is {daemon_version} but this client is {client_version} — \
         relaunch to continue; no change was made"
    )))
}

fn read_expected(project: &str, id: &str) -> Option<ExpectedState> {
    let store = shelbi_state::issue_store_for(project).ok()?;
    let tf = store.get(id).ok()??;
    Some(ExpectedState {
        status: tf.task.column.as_str().to_string(),
        updated_at: tf.task.updated_at.to_rfc3339(),
    })
}

fn map_client_err(e: shelbi_client::ClientError) -> ExecError {
    use shelbi_proto::control::MutationError;
    match e {
        shelbi_client::ClientError::Mutation(MutationError::Stale { expected, actual }) => {
            ExecError::Conflict(format!(
                "you saw `{}` rev {}, it is now `{}` rev {}",
                expected.status, expected.updated_at, actual.status, actual.updated_at
            ))
        }
        shelbi_client::ClientError::Mutation(MutationError::NotFound { id }) => {
            ExecError::NotFound(id)
        }
        other => ExecError::Backend(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_date_gate_refuses_a_version_mismatch_and_allows_a_match() {
        // AC7: an out-of-date client sends no mutations before it re-execs. A
        // version mismatch yields a refusal (returned before the Mutate frame),
        // while a matching version lets the mutation through.
        assert!(out_of_date_gate("0.9.0", "0.9.0").is_none(), "a match sends");
        let err = out_of_date_gate("0.10.0", "0.9.0").expect("a mismatch refuses");
        match err {
            ExecError::Backend(msg) => {
                assert!(msg.contains("0.10.0") && msg.contains("0.9.0"), "msg: {msg}");
                assert!(msg.contains("no change was made"), "msg names the no-op: {msg}");
            }
            other => panic!("expected a Backend refusal, got {other:?}"),
        }
    }
}
