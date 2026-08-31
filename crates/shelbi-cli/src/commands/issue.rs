//! `shelbi issue <subcommand>` — operations on the project's issues that are
//! backend-agnostic, routed through the [`IssueStore`] seam so they work the
//! same whether the board lives on disk (`file_system`) or as GitHub issues
//! (`github`).
//!
//! Today the only subcommand is `comment`, which posts a comment onto an issue
//! (plan Decision D4 — comments are first-class). The Kanban-management verbs
//! still live under `shelbi task`; the broader `task → issue` rename (D5) is a
//! separate, tracked change.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use shelbi_state::IssueStore;

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum IssueCmd {
    /// Post a comment on an issue. Works against whichever issue-tracker
    /// backend the project is configured for.
    Comment {
        /// The issue id (the stable shelbi slug).
        id: String,
        /// The comment text.
        text: String,
    },
}

pub fn run(project_opt: Option<String>, cmd: IssueCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    // Board mutations against a stale daemon (old binary kept running across an
    // upgrade) produce undiagnosable io errors — refuse up front, matching the
    // `shelbi task` mutation gate.
    super::hub_version::ensure_daemon_matches_for_mutation()?;
    match cmd {
        IssueCmd::Comment { id, text } => comment(&project, &id, &text),
    }
}

fn comment(project: &str, id: &str, text: &str) -> Result<()> {
    let store = resolve_store(project)?;
    let posted = store.add_comment(id, text).map_err(|e| anyhow!(e))?;
    println!("✓ commented on {id} (comment {})", posted.id);
    Ok(())
}

/// Resolve the project's configured issue-tracker backend into a live store.
/// The project YAML's `issue_tracker` block selects and validates the backend
/// (`file_system` by default, `github` for issues in a repo).
fn resolve_store(project: &str) -> Result<Box<dyn IssueStore>> {
    let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    shelbi_state::resolve_issue_store(project, &project_yaml.issue_tracker).map_err(|e| anyhow!(e))
}
