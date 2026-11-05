//! Sending a message into a workspace's agent session and knowing it landed:
//! `shelbi send`, and `gY` from the diff view.
//!
//! The delivery is [`crate::submit`]'s text → settle → Enter with its submit
//! proof. This is what a caller says about it: a visible parked message reads as
//! a queue only when the pane was busy before and after, everything else short
//! of a proof is a failure, and every verdict goes in `events.log`.

use std::sync::Arc;

use shelbi_app::diff::{AgentDelivery, AgentSend, DiffTarget};
use shelbi_core::{AgentRunnerSpec, Error, Host, Result};

use crate::session_backend::SessionTarget;
use crate::submit::{self, PaneBaseline, SubmitProfile, SubmitStatus};
use crate::workspace;

/// Deliver `message` through the shared verified-submit primitive and record
/// the verdict. A transport failure is surfaced as `status=stuck` as well:
/// without that event, an orchestrator tailing events.log would still silently
/// assume the nudge arrived.
pub fn send_verified(
    project: &str,
    id: &str,
    runner: &AgentRunnerSpec,
    host: &Host,
    addr: &SessionTarget,
    message: &str,
) -> Result<AgentDelivery> {
    let profile = SubmitProfile::for_runner(runner);
    let baseline = PaneBaseline::capture(host, addr, profile);
    let status = match submit::send_verified(host, addr, message, &baseline) {
        Ok(status) => status,
        Err(e) => {
            shelbi_state::append_send_event(project, id, "stuck", "transport_error").map_err(|log_err| {
                Error::Other(format!(
                    "sending to `{id}` failed ({e}); recording the stuck delivery also failed: {log_err}"
                ))
            })?;
            return Err(Error::Other(format!("sending to `{id}` failed: {e}")));
        }
    };

    // A busy baseline alone is stale by the time the verifier has spent up
    // to two polling windows waiting. Claude may have completed that turn in
    // the meantime, leaving a genuinely wedged prompt in an idle input box.
    // Accept visible input as a queue only when the pane was busy before the
    // send and still has strong current-turn evidence at the final verdict.
    let finally_actively_busy =
        matches!(status, SubmitStatus::StillInBox) && PaneBaseline::capture(host, addr, profile).actively_busy;
    let (event_status, detail, delivery) = classify_delivery(status, baseline.actively_busy, finally_actively_busy);
    shelbi_state::append_send_event(project, id, event_status, detail)?;
    delivery.ok_or_else(|| {
        Error::Other(format!(
            "the message to `{id}` is stuck in {} after a retry Enter",
            addr.label()
        ))
    })
}

/// Map the transport-neutral verifier result to a delivery.
/// A visibly parked message is acceptable only when the pane was genuinely
/// busy both before delivery and at the final verdict: Claude keeps submitted
/// mid-turn input visible as a queue and consumes it when the current turn
/// ends. The same screen after that turn has ended is the bug this must report
/// as stuck.
fn classify_delivery(
    status: SubmitStatus,
    baseline_actively_busy: bool,
    finally_actively_busy: bool,
) -> (&'static str, &'static str, Option<AgentDelivery>) {
    match status {
        SubmitStatus::Submitted { detail } => ("submitted", detail, Some(AgentDelivery::Submitted)),
        SubmitStatus::DeliveredUnverified { detail } => ("unverified", detail, Some(AgentDelivery::Unverified)),
        SubmitStatus::EligibilityRevoked => ("stuck", "eligibility_revoked", None),
        SubmitStatus::StillInBox if baseline_actively_busy && finally_actively_busy => {
            ("queued", "busy_pane_visible_queue", Some(AgentDelivery::Queued))
        }
        SubmitStatus::StillInBox => ("stuck", "still_in_input_after_retry", None),
        SubmitStatus::Unconfirmed => ("stuck", "unconfirmed_after_retry", None),
    }
}

/// Send `message` to the agent in `workspace`'s session, refusing a session
/// that isn't running. Holds the workspace's lock, as `shelbi send` does, so its
/// settle window can't merge with a dispatch's prompt. Blocking, for up to two
/// polling windows.
pub fn send_to_workspace(project_name: &str, workspace_name: &str, message: &str) -> Result<AgentDelivery> {
    let project = shelbi_state::load_project(project_name)?;
    let spec = project
        .workspace(workspace_name)
        .ok_or_else(|| Error::Other(format!("`{workspace_name}` isn't a workspace in `{project_name}`")))?;
    let machine = project
        .machine(&spec.machine)
        .ok_or_else(|| Error::UnknownMachine(spec.machine.clone()))?;
    let addr = workspace::workspace_target(&project, spec)?;
    // The project's baseline runner: a slot doesn't select one, and this is the
    // profile `shelbi send` verifies with.
    let runner = project
        .default_runner_spec()
        .ok_or_else(|| Error::Other(format!("project `{project_name}` declares no runner")))?
        .clone();
    let host = machine.host();
    let _lock = shelbi_state::lock_workspace(project_name, workspace_name)?;
    if !workspace::workspace_pane_alive(&host, &addr)? {
        return Err(Error::Other(format!("no agent is running on `{workspace_name}`")));
    }
    send_verified(project_name, workspace_name, &runner, &host, &addr, message)
}

/// How a diff view on this machine reaches a target's agent.
pub fn service() -> AgentSend {
    Arc::new(|target: &DiffTarget, message: &str| {
        send_to_workspace(&target.project, &target.workspace, message).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_visible_input_is_stuck_but_busy_visible_input_is_queued() {
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, false, false),
            ("stuck", "still_in_input_after_retry", None)
        );
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, true, true),
            (
                "queued",
                "busy_pane_visible_queue",
                Some(AgentDelivery::Queued)
            )
        );
    }

    #[test]
    fn stale_busy_baseline_does_not_hide_a_wedged_idle_prompt() {
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, true, false),
            ("stuck", "still_in_input_after_retry", None)
        );
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, false, true),
            ("stuck", "still_in_input_after_retry", None)
        );
    }

    #[test]
    fn confirmed_and_unconfirmed_verdicts_map_to_delivery_events() {
        assert_eq!(
            classify_delivery(
                SubmitStatus::Submitted {
                    detail: "retry_enter"
                },
                false,
                false,
            ),
            ("submitted", "retry_enter", Some(AgentDelivery::Submitted))
        );
        assert_eq!(
            classify_delivery(SubmitStatus::Unconfirmed, true, true),
            ("stuck", "unconfirmed_after_retry", None)
        );
    }

    #[test]
    fn unsupported_runner_delivery_is_success_but_explicitly_unverified() {
        assert_eq!(
            classify_delivery(
                SubmitStatus::DeliveredUnverified {
                    detail: "verification_unsupported"
                },
                false,
                false,
            ),
            (
                "unverified",
                "verification_unsupported",
                Some(AgentDelivery::Unverified)
            )
        );
    }
}
