//! Onboarding wizard.
//!
//! Not a TUI — just a sequence of `inquire` prompts (single-select with
//! arrow keys, free text, y/N). Each phase is idempotent: re-running the
//! wizard skips any phase whose answer is already on disk.
//!
//! Library choice is locked to `inquire`: it shares crossterm with
//! ratatui, has chainable validators, and the `Select` prompt has
//! built-in type-to-filter (used by later phases for the project picker).
//! Do not pull in `dialoguer` alongside.

use std::fmt::Display;

use anyhow::{anyhow, Context, Result};
use inquire::{Confirm, Select, Text};

// `select` and `confirm` are part of the wizard framework — Phase 1
// only consumes `text`, but later phases (project picker, dependency
// confirmations) wire these in directly.
#[allow(dead_code)]
/// Single-select prompt with arrow-key navigation. `options` must be
/// non-empty.
pub fn select<T: Display>(label: &str, options: Vec<T>) -> Result<T> {
    Select::new(label, options)
        .prompt()
        .with_context(|| format!("select prompt `{label}`"))
}

/// Free-text prompt with a default that the user can accept by pressing
/// Enter on an empty line.
pub fn text(label: &str, default: &str) -> Result<String> {
    Text::new(label)
        .with_default(default)
        .prompt()
        .with_context(|| format!("text prompt `{label}`"))
}

#[allow(dead_code)]
/// y/N (or Y/n) prompt. `default` selects which case is upper-cased in
/// the rendered hint.
pub fn confirm(label: &str, default: bool) -> Result<bool> {
    Confirm::new(label)
        .with_default(default)
        .prompt()
        .with_context(|| format!("confirm prompt `{label}`"))
}

/// Phase 1: ask the user what to call their assistant and persist it to
/// `~/.shelbi/shelbi.yaml`. Idempotent — if `assistant_name` is already
/// set, this is a no-op.
pub fn phase_1_assistant_name() -> Result<()> {
    let mut cfg = shelbi_state::load_shelbi_config().map_err(|e| anyhow!(e))?;
    if cfg.assistant_name.is_some() {
        return Ok(());
    }

    let answer = text(
        "What should we call your assistant?",
        shelbi_state::DEFAULT_ASSISTANT_NAME,
    )?;
    let trimmed = answer.trim();
    let name = if trimmed.is_empty() {
        shelbi_state::DEFAULT_ASSISTANT_NAME.to_string()
    } else {
        trimmed.to_string()
    };

    cfg.assistant_name = Some(name);
    shelbi_state::save_shelbi_config(&cfg).map_err(|e| anyhow!(e))?;
    Ok(())
}
