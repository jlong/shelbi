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
