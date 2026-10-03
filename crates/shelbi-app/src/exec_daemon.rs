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
    AddSpec, EditSpec, ExpectedState, MutationKind, MutationRequest, Stream,
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

    let req = MutationRequest {
        request_id: 1,
        project: project.to_string(),
        id: id.to_string(),
        expected,
        kind,
    };
    client.mutate(&req, on_line).map_err(map_client_err)
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
