//! Orchestrator agent bootstrap.
//!
//! The orchestrator is just another agent CLI (e.g. `claude`) running in
//! window 1 of the shelbi tmux session, with two affordances:
//!
//! 1. The `shelbi` binary on PATH, used as its tool surface.
//! 2. A generated system-prompt fragment (default + optional per-project
//!    `ORCHESTRATOR.md` override) that teaches it the CLI.
//!
//! This crate is a stub for v1 — full integration lands in Phase 3.

pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("default_orchestrator.md");

/// Resolve the active orchestrator system prompt for a project: per-project
/// override (`ORCHESTRATOR.md`) if present, else the bundled default.
pub fn system_prompt(project: &str) -> shelbi_core::Result<String> {
    let path = shelbi_state::project_dir(project)?.join("ORCHESTRATOR.md");
    if path.exists() {
        Ok(std::fs::read_to_string(&path).map_err(shelbi_core::Error::Io)?)
    } else {
        Ok(DEFAULT_SYSTEM_PROMPT.to_string())
    }
}
