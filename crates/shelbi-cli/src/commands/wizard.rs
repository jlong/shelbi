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

pub fn run() -> Result<WizardOutcome> {
    match wizard::phase_1_assistant_name() {
        Ok(()) => {}
        Err(e) if is_cancel(&e) => return Ok(WizardOutcome::Cancelled),
        Err(e) => return Err(e),
    }
    let cfg = shelbi_state::load_shelbi_config()
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("✓ assistant: {}", cfg.assistant_name());

    match wizard::phase_2_project_setup() {
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
