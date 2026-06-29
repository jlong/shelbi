//! `shelbi project <subcommand>` — manage projects.
//!
//! Currently only `add`, which runs the same project-setup prompt sequence
//! as initial onboarding (see [`crate::wizard::setup_one_project`]) without
//! the idempotence guard that `phase_2_project_setup` carries.

use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ProjectCmd {
    /// Set up a new project interactively. Walks through the same prompt
    /// sequence as initial onboarding (project name, machines, workspaces,
    /// runners) and writes `~/.shelbi/projects/<name>.yaml`. Does not
    /// launch the TUI on completion.
    Add,
}

pub fn run(cmd: ProjectCmd) -> Result<()> {
    match cmd {
        ProjectCmd::Add => crate::wizard::setup_one_project(),
    }
}
