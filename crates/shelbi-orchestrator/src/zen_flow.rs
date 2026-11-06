//! The pin on Zen's ordinary landing.
//!
//! Zen lands a task by moving its card to `done`, which fires the workflow's
//! `merge` action. That action knows nothing about the probe, so on its own it
//! would merge whatever the PR holds when the card moves, including a commit CI
//! never graded. This module closes that gap with the durable flow record
//! ([`shelbi_state::zen_flow`]): the probe writes it, `pr-create` and `ci-watch`
//! advance it, and [`landing_pin`] is what `merge` asks before it touches the
//! PR. The orchestrator passes nothing extra to any of this.
//!
//! A flow step that can't update the record only warns, because the record
//! failing to advance leaves the task short of green, and that refuses.

use shelbi_core::{Error, Host, Issue, Project, Result};
use shelbi_state::zen_flow::{self, ZenFlowPhase, ZenFlowRecord};
use shelbi_state::ZenModeState;

use crate::git::{lookup_origin_repository_with_push_target, lookup_pr_identity};
use crate::zen::{remote_branch_sha, CiVerdict, PinnedPrIdentity, ProbeReport};

/// What a pinned landing tells `gh pr merge`: GitHub refuses the merge if the
/// head isn't this commit, so the head can't move between the check here and
/// the merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LandingPin {
    pub repository: String,
    pub integration_sha: String,
}

/// Start the task's flow from a fresh probe. A new probe replaces whatever came
/// before it, including a green verdict for a commit it no longer pins.
pub(crate) fn record_probe(project: &Project, task_id: &str, report: &ProbeReport) -> Result<()> {
    zen_flow::save(
        &project.name,
        &ZenFlowRecord {
            task_id: task_id.to_string(),
            repository: report.repository.clone(),
            repository_id: report.repository_id.clone(),
            base_branch: report.base_branch.clone(),
            base_sha: report.base_sha.clone(),
            integration_sha: report.integration_sha.clone(),
            head_sha: report.head_sha.clone(),
            published_head_sha: report.published_head_sha.clone(),
            phase: ZenFlowPhase::Probed,
        },
    )
}

/// Whether the identity a command was handed is the one the record pinned. An
/// orchestrator holding an older probe's values mustn't advance the newer one.
fn pins_same_commits(record: &ZenFlowRecord, identity: &PinnedPrIdentity) -> bool {
    record.repository_id == identity.repository_id
        && record.base_branch == identity.base_branch
        && record.base_sha == identity.base_sha
        && record.head_sha == identity.head_sha
        && record.integration_sha.as_deref() == Some(identity.integration_sha.as_str())
}

fn save_phase(project: &Project, mut record: ZenFlowRecord, phase: ZenFlowPhase) {
    record.phase = phase;
    if let Err(e) = zen_flow::save(&project.name, &record) {
        tracing::warn!(task = %record.task_id, error = %e, "Zen flow record not advanced");
    }
}

/// `pr-create` made PR `pr`'s head the pinned integration commit. CI starts
/// over, so this also takes back an earlier green.
pub(crate) fn note_published(
    project: &Project,
    task_id: &str,
    pr: u64,
    identity: &PinnedPrIdentity,
) {
    match zen_flow::load(&project.name, task_id) {
        Ok(Some(record)) if pins_same_commits(&record, identity) => {
            save_phase(project, record, ZenFlowPhase::Published { pr });
        }
        Ok(_) => tracing::warn!(task = task_id, "PR published under an identity no record pins"),
        Err(e) => tracing::warn!(task = task_id, error = %e, "Zen flow record unreadable"),
    }
}

/// Record `ci-watch`'s verdict for PR `pr`. Anything but green takes back an
/// earlier green, since it's the newer word on that commit.
pub(crate) fn note_ci_verdict(
    project: &Project,
    pr: u64,
    identity: &PinnedPrIdentity,
    verdict: &CiVerdict,
) {
    let record = match zen_flow::find_by_integration(&project.name, &identity.integration_sha) {
        Ok(Some(record)) if pins_same_commits(&record, identity) => record,
        // The release flow watches PRs that have no task, so no match is normal.
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(pr, error = %e, "Zen flow records unreadable");
            return;
        }
    };
    let phase = match verdict {
        CiVerdict::Green => ZenFlowPhase::CiGreen { pr },
        CiVerdict::Red { .. } | CiVerdict::Timeout => ZenFlowPhase::Published { pr },
    };
    save_phase(project, record, phase);
}

/// The flow for `task_id` is over: it landed, or the task left review.
pub(crate) fn finish(project: &Project, task_id: &str) {
    if let Err(e) = zen_flow::forget(&project.name, task_id) {
        tracing::warn!(task = task_id, error = %e, "Zen flow record not cleared");
    }
}

/// Whether `origin_tip` is the integration commit the flow published for the
/// local head `local`. The two have diverged by design, since the integration
/// commit is a squash onto the base, so `push_branch` has nothing to publish
/// and a push could only be rejected as a non-fast-forward.
pub(crate) fn published_for_head(
    project: &Project,
    task_id: &str,
    local: &str,
    origin_tip: &str,
) -> bool {
    match zen_flow::load(&project.name, task_id) {
        Ok(Some(record)) => {
            record.head_sha == local && record.integration_sha.as_deref() == Some(origin_tip)
        }
        _ => false,
    }
}

/// Decide whether `merge` may land `task`, whose open PR is `open_pr`.
///
/// - `Ok(None)`: no Zen flow governs this landing, so merge as usual. Either
///   Zen never probed the task, or Zen is off and a human owns the merge.
/// - `Ok(Some(pin))`: the PR's head is the commit CI graded and the base hasn't
///   moved. Merge with the pin.
/// - `Err(_)`: refuse, and the card stays where it is.
///
/// The base is read here and can't be pinned on the merge itself, since `gh`
/// has no flag for it. A base that moves in the moment between this read and
/// the merge is the one gap left; the head has none.
pub(crate) fn landing_pin(
    project: &Project,
    task: &Issue,
    host: &Host,
    wt: &str,
    open_pr: Option<u64>,
) -> Result<Option<LandingPin>> {
    let Some(record) = zen_flow::load(&project.name, &task.id)? else {
        return Ok(None);
    };
    if shelbi_state::read_state(&project.name)?.zen_mode == ZenModeState::Off {
        return Ok(None);
    }
    let refuse = |detail: String| refusal(&task.id, detail);

    let graded_pr = match record.phase {
        ZenFlowPhase::CiGreen { pr } => pr,
        ZenFlowPhase::Probed | ZenFlowPhase::Published { .. } => {
            return Err(refuse(
                "CI hasn't reported green for the commit the latest probe pinned".to_string(),
            ));
        }
    };
    let Some(integration_sha) = record.integration_sha.clone() else {
        return Err(refuse("the latest probe found no clean integration commit".to_string()));
    };
    let pr = match open_pr {
        Some(pr) if pr == graded_pr => pr,
        Some(pr) => {
            return Err(refuse(format!(
                "CI graded PR #{graded_pr}, but the branch's open PR is #{pr}"
            )));
        }
        None => {
            return Err(refuse(format!(
                "CI graded PR #{graded_pr}, which is no longer open"
            )));
        }
    };

    let (repository, push_target) = lookup_origin_repository_with_push_target(host, wt)?;
    if repository.id != record.repository_id || repository.selector != record.repository {
        return Err(refuse(format!(
            "origin is now {} (id {}), but the probe pinned {} (id {})",
            repository.selector, repository.id, record.repository, record.repository_id
        )));
    }

    let identity = lookup_pr_identity(host, wt, &repository, pr)?;
    if identity.head_repository.id != record.repository_id {
        return Err(refuse(format!(
            "PR #{pr}'s head is in repository {}, not the one the probe pinned",
            identity.head_repository.name_with_owner
        )));
    }
    if identity.base_ref != record.base_branch {
        return Err(refuse(format!(
            "PR #{pr} now targets `{}`, but CI graded it against `{}`",
            identity.base_ref, record.base_branch
        )));
    }
    if identity.head_oid != integration_sha {
        return Err(refuse(format!(
            "PR #{pr}'s head moved after CI went green: CI graded {integration_sha}, and the \
             head is now {}",
            identity.head_oid
        )));
    }
    let base_tip = remote_branch_sha(host, wt, &push_target, &record.base_branch, "before landing")?;
    if base_tip != record.base_sha {
        return Err(refuse(format!(
            "base `{}` moved after CI went green: CI graded against {}, and the base is now \
             {base_tip}",
            record.base_branch, record.base_sha
        )));
    }

    Ok(Some(LandingPin {
        repository: repository.selector,
        integration_sha,
    }))
}

fn refusal(task_id: &str, detail: String) -> Error {
    Error::Other(format!(
        "refusing to land `{task_id}`: {detail}. Nothing was merged. Re-run `shelbi zen probe \
         {task_id}` and restart the pinned PR flow, or merge the PR by hand to override"
    ))
}
