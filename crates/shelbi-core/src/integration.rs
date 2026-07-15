//! Per-agent integration health mode — the transport tier Shelbi currently
//! has with a given agent's pane.
//!
//! Three tiers, best to worst:
//!
//! - `structured`: native app-server bridge / hook-verified push. The Codex
//!   native bridge holding an active owned thread is the canonical case —
//!   board events enter the conversation through `turn/start` / `turn/steer`.
//! - `conventional`: verified tmux submission plus OSC pane-title hooks — the
//!   ordinary Claude Code workspace contract (send-keys wake, `shelbi:<state>`
//!   markers read back from the pane title).
//! - `degraded`: polling contract only — no push, no verified submission. The
//!   Codex bridge falling back to standalone turn-boundary polling lands here,
//!   as does any runner Shelbi can neither push to nor read hook markers from.
//!
//! This is an observability signal only: nothing in the scheduler branches on
//! it. Its job is to make a silent transport downgrade (e.g. a disengaged
//! native bridge) visible in `shelbi status --full`, `shelbi workspace list`,
//! and the event log rather than something you discover by hand-reading JSON.

use std::fmt;

/// The transport tier for one agent. See the module docs for the meaning of
/// each variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationMode {
    Structured,
    Conventional,
    Degraded,
}

impl IntegrationMode {
    /// The lowercase wire token used in event-log lines and status output.
    pub fn as_str(self) -> &'static str {
        match self {
            IntegrationMode::Structured => "structured",
            IntegrationMode::Conventional => "conventional",
            IntegrationMode::Degraded => "degraded",
        }
    }
}

impl fmt::Display for IntegrationMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The transport tier Shelbi has with a runner along each contract axis — the
/// per-runner capability record the adapter publishes.
///
/// Every field is one of [`IntegrationMode`]'s three tiers. This exists to
/// make the runner's integration profile a single inspectable value (feeding
/// the same surfacing as [`IntegrationMode`]); nothing in the scheduler
/// branches on it. Build one with [`crate::RunnerKind::capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityLadder {
    /// Delivering the initial task context / prompt into the pane.
    pub context_delivery: IntegrationMode,
    /// Waking the agent when a board event arrives.
    pub event_wake: IntegrationMode,
    /// Reading the agent's live state back (pane-title markers, thread state).
    pub state_observation: IntegrationMode,
    /// Pushing hub→workspace messages to the agent.
    pub message_delivery: IntegrationMode,
    /// Restoring prior context after a pane restart.
    pub resume: IntegrationMode,
}
