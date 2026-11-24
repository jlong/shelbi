//! Sending a message into a workspace's agent session and knowing it landed:
//! `shelbi send`, and `gY` from the diff view.
//!
//! The delivery is [`crate::submit`]'s text → settle → Enter with its submit
//! proof. This is what a caller says about it: a visible parked message reads as
//! a queue only when the pane was busy before and after, everything else short
//! of a proof is a failure, and every verdict goes in `events.log`.
//!
//! A slot Shelbi launched OpenCode in (its OpenCode ledger names a current
//! session) takes messages through Shelbi's plugin instead: an inbox file the
//! plugin admits and acks ([`crate::submit::send_through_opencode_inbox`]).
//! When that channel is down the message is pasted, and a `status=degraded`
//! send line says so.

use std::path::Path;
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
    worktree: &Path,
    message: &str,
) -> Result<AgentDelivery> {
    let transport_error = |e: Error| -> Error {
        if let Err(log_err) = shelbi_state::append_send_event(project, id, "stuck", "transport_error") {
            return Error::Other(format!(
                "sending to `{id}` failed ({e}); recording the stuck delivery also failed: {log_err}"
            ));
        }
        Error::Other(format!("sending to `{id}` failed: {e}"))
    };
    match send_route(project, id, runner) {
        SendRoute::Pane(_) => {}
        SendRoute::OpenCodeInbox => {
            let sent = submit::send_through_opencode_inbox(host, addr, worktree, message).map_err(transport_error)?;
            if let Some(reason) = sent.degraded {
                let _ = shelbi_state::append_send_degraded_event(project, id, reason, &sent.delivery_id);
            }
            if sent.status == SubmitStatus::Unconfirmed {
                // The plugin took the message and never answered. Pasting it
                // too could deliver it twice, so it's reported instead.
                shelbi_state::append_send_event(project, id, "stuck", "inbox_unacked")?;
                return Err(Error::Other(format!(
                    "OpenCode's plugin in {} took the message to `{id}` (delivery {}) but never \
                     confirmed it; it may still arrive",
                    addr.label(),
                    sent.delivery_id,
                )));
            }
            return record_verdict(project, id, addr, sent.status, false, false);
        }
        SendRoute::OpenCodePluginSilent => {
            // The plugin never reported in after launch, so nothing reads the
            // inbox: paste straight away rather than wait out an ack.
            let _ = shelbi_state::append_send_degraded_event(
                project,
                id,
                crate::workspace::OPENCODE_PLUGIN_SILENT_REASON,
                "none",
            );
            submit::deliver_text(host, addr, message).map_err(transport_error)?;
            let status = SubmitStatus::DeliveredUnverified { detail: "plugin_silent_paste" };
            return record_verdict(project, id, addr, status, false, false);
        }
    }
    let profile = SubmitProfile::for_runner(runner);
    let baseline = PaneBaseline::capture(host, addr, profile);
    let status = submit::send_verified(host, addr, message, &baseline).map_err(transport_error)?;

    // A busy baseline alone is stale by the time the verifier has spent up
    // to two polling windows waiting. Claude may have completed that turn in
    // the meantime, leaving a genuinely wedged prompt in an idle input box.
    // Accept visible input as a queue only when the pane was busy before the
    // send and still has strong current-turn evidence at the final verdict.
    let finally_actively_busy =
        matches!(status, SubmitStatus::StillInBox) && PaneBaseline::capture(host, addr, profile).actively_busy;
    record_verdict(project, id, addr, status, baseline.actively_busy, finally_actively_busy)
}

/// Record a send's verdict on `events.log` and turn it into the delivery (or
/// the stuck error).
fn record_verdict(
    project: &str,
    id: &str,
    addr: &SessionTarget,
    status: SubmitStatus,
    baseline_actively_busy: bool,
    finally_actively_busy: bool,
) -> Result<AgentDelivery> {
    let (event_status, detail, delivery) = classify_delivery(status, baseline_actively_busy, finally_actively_busy);
    shelbi_state::append_send_event(project, id, event_status, detail)?;
    delivery.ok_or_else(|| {
        Error::Other(format!(
            "the message to `{id}` is stuck in {} after a retry Enter",
            addr.label()
        ))
    })
}

/// How a message reaches a workspace's agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendRoute {
    /// Typed into the pane and verified on its screen, per the profile.
    Pane(SubmitProfile),
    /// Through the OpenCode plugin's inbox.
    OpenCodeInbox,
    /// An OpenCode pane whose plugin never reported in: pasted.
    OpenCodePluginSilent,
}

/// The route for a send to `workspace`. Its OpenCode ledger decides, not the
/// project's baseline runner: a slot whose current launch is an OpenCode one
/// has Shelbi's plugin in it whatever the baseline is, and an OpenCode
/// baseline with no such launch has no plugin to answer.
fn send_route(project: &str, workspace: &str, runner: &AgentRunnerSpec) -> SendRoute {
    let opencode_launch = shelbi_state::load_opencode_sessions(project, workspace)
        .map(|ledger| ledger.current.is_some())
        .unwrap_or(false);
    send_route_for(
        opencode_launch,
        opencode_launch && shelbi_state::observation_degraded(project, workspace),
        SubmitProfile::for_runner(runner),
    )
}

fn send_route_for(opencode_launch: bool, plugin_silent: bool, profile: SubmitProfile) -> SendRoute {
    match (opencode_launch, plugin_silent) {
        (true, false) => SendRoute::OpenCodeInbox,
        (true, true) => SendRoute::OpenCodePluginSilent,
        (false, _) => SendRoute::Pane(profile),
    }
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
    let target = resolve_workspace(project_name, workspace_name)?;
    let _lock = shelbi_state::lock_workspace(project_name, workspace_name)?;
    if !workspace::workspace_pane_alive(&target.host, &target.addr)? {
        return Err(Error::Other(format!("no agent is running on `{workspace_name}`")));
    }
    send_verified(
        project_name,
        workspace_name,
        &target.runner,
        &target.host,
        &target.addr,
        &target.worktree,
        message,
    )
}

/// [`send_to_workspace`] for a message meant only for the session as it is
/// now: `still_wanted` is asked under the workspace's lock, after any dispatch
/// holding it has finished, and `Ok(None)` means it said no and nothing was
/// sent. A job's result uses it so it never lands in a session that has moved
/// on to another task.
pub fn send_to_workspace_if(
    project_name: &str,
    workspace_name: &str,
    message: &str,
    still_wanted: impl FnOnce(&shelbi_core::Project) -> bool,
) -> Result<Option<AgentDelivery>> {
    let target = resolve_workspace(project_name, workspace_name)?;
    let _lock = shelbi_state::lock_workspace(project_name, workspace_name)?;
    if !still_wanted(&target.project) {
        return Ok(None);
    }
    send_verified(
        project_name,
        workspace_name,
        &target.runner,
        &target.host,
        &target.addr,
        &target.worktree,
        message,
    )
    .map(Some)
}

/// Where a workspace's agent is, and the runner profile a send verifies with.
struct WorkspaceTarget {
    project: shelbi_core::Project,
    host: Host,
    addr: SessionTarget,
    /// The slot's worktree, where an OpenCode pane's inbox lives.
    worktree: std::path::PathBuf,
    runner: AgentRunnerSpec,
}

fn resolve_workspace(project_name: &str, workspace_name: &str) -> Result<WorkspaceTarget> {
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
    let worktree = workspace::workspace_worktree(machine, spec);
    Ok(WorkspaceTarget { project, host, addr, worktree, runner })
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
    fn a_slot_launched_on_opencode_takes_sends_through_its_inbox() {
        // The slot's ledger decides, whatever the project's baseline runner.
        assert_eq!(send_route_for(true, false, SubmitProfile::ClaudeUi), SendRoute::OpenCodeInbox);
        assert_eq!(send_route_for(true, false, SubmitProfile::OpenCodeInbox), SendRoute::OpenCodeInbox);
        // A plugin that never reported in reads nothing: paste at once.
        assert_eq!(send_route_for(true, true, SubmitProfile::OpenCodeInbox), SendRoute::OpenCodePluginSilent);
        // No OpenCode launch on the slot: no plugin to answer.
        assert_eq!(send_route_for(false, false, SubmitProfile::OpenCodeInbox), SendRoute::Pane(SubmitProfile::OpenCodeInbox));
        assert_eq!(send_route_for(false, true, SubmitProfile::CodexUi), SendRoute::Pane(SubmitProfile::CodexUi));
    }

    #[test]
    fn the_route_reads_the_slots_opencode_ledger() {
        let _g = crate::test_lock::isolated_home();
        let claude = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let _ = shelbi_state::clear_observation_degraded("inboxdemo", "alpha");
        assert_eq!(send_route("inboxdemo", "alpha", &claude), SendRoute::Pane(SubmitProfile::ClaudeUi));
        shelbi_state::begin_opencode_session("inboxdemo", "alpha", "t1", false, "opencode").unwrap();
        assert_eq!(send_route("inboxdemo", "alpha", &claude), SendRoute::OpenCodeInbox);
        shelbi_state::mark_observation_degraded("inboxdemo", "alpha", "opencode-plugin-silent").unwrap();
        assert_eq!(send_route("inboxdemo", "alpha", &claude), SendRoute::OpenCodePluginSilent);
        let _ = shelbi_state::clear_observation_degraded("inboxdemo", "alpha");
    }

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
