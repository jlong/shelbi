use anyhow::Result;

use crate::wizard;

/// What the wizard did. The entry-point uses this to decide whether to
/// boot a TUI, print a hint, or exit silently.
pub enum WizardOutcome {
    /// All requested phases ran to completion (each phase is independently
    /// idempotent, so "completion" includes "skipped because already done").
    Completed,
    /// User pressed Ctrl-C / Esc out of a prompt. The wizard writes state
    /// only at the end of each phase, so a cancellation leaves no
    /// half-finished project YAML on disk.
    Cancelled,
}

/// Run the wizard. When `first_run` is true (no `~/.shelbi/` existed
/// before this invocation), the banner + tagline is printed once at the
/// very top, before any prompt. Re-entries (`shelbi wizard` after the
/// home dir exists, `shelbi project add`) pass `false`.
pub fn run(first_run: bool) -> Result<WizardOutcome> {
    if first_run {
        wizard::print_banner();
    }
    match wizard::phase_project_setup() {
        Ok(()) => {}
        Err(e) if is_cancel(&e) => return Ok(WizardOutcome::Cancelled),
        Err(e) => return Err(e),
    }
    Ok(WizardOutcome::Completed)
}

/// True if `e` was produced by an `inquire` prompt being cancelled or
/// interrupted (Esc / Ctrl-C). Walks the anyhow source chain because the
/// wizard helpers wrap each `prompt()` call in `.with_context(...)`.
fn is_cancel(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<inquire::error::InquireError>(),
        Some(
            inquire::error::InquireError::OperationCanceled
                | inquire::error::InquireError::OperationInterrupted
        )
    )
}
