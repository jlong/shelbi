//! CLI project picker used by the no-arg entry point when more than one
//! project is registered in `~/.shelbi/projects/`. Backed by the same
//! `shelbi_state::list_projects` + nucleo-matcher pair that powers the
//! in-TUI "Switch project" palette action.
//!
//! Behavior:
//! - 0 projects → returns an actionable error pointing at `shelbi init`.
//! - 1 project → auto-selected; no prompt.
//! - 2+ → inquire `Select` with a nucleo filter callback. Final entry
//!   `+ Add a new project` runs the init scaffolding.
//!
//! Inquire's `Scorer` lets the callback both filter (return `None`) and
//! rank (return `Some(score)`). With no query, every entry maps to a
//! recency-baked score so the most-recently-launched project lands at
//! the top; once the user types, ranking switches to the nucleo score,
//! same as the palette. `+ Add a new project` gets a constant
//! always-keep score that's lower than any nucleo hit so it parks below
//! filtered matches.

use anyhow::{anyhow, Context, Result};
use inquire::{
    error::InquireError,
    validator::{ErrorMessage, Validation},
    Select, Text,
};
use nucleo_matcher::Matcher;

use shelbi_palette::score;
use shelbi_state::{list_projects, ProjectSummary};

pub enum PickerOutcome {
    /// User picked an existing project name.
    Existing(String),
    /// User opted to add a new project; the init scaffolding has already
    /// run for this name.
    Created(String),
    /// User cancelled (Esc / Ctrl-C).
    Cancelled,
}

/// Decide what project to load. Errors out only when no projects are
/// registered at all — the caller (entry-point) handles that as
/// "fall through to the wizard hint."
pub fn pick_or_setup() -> Result<PickerOutcome> {
    let projects = list_projects().map_err(|e| anyhow!(e))?;
    if projects.is_empty() {
        return Err(anyhow!(
            "no projects in ~/.shelbi/projects/. Run `shelbi init --project NAME` to scaffold one."
        ));
    }
    if projects.len() == 1 {
        return Ok(PickerOutcome::Existing(
            projects.into_iter().next().unwrap().name,
        ));
    }
    run_picker(projects)
}

#[derive(Clone)]
enum Choice {
    Project(ProjectSummary),
    AddNew,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Choice::Project(p) => {
                let m = if p.machine_count == 1 {
                    "machine"
                } else {
                    "machines"
                };
                let w = if p.workspace_count == 1 {
                    "workspace"
                } else {
                    "workspaces"
                };
                write!(
                    f,
                    "{:<24}  {} {m} · {} {w}",
                    p.name, p.machine_count, p.workspace_count
                )
            }
            Choice::AddNew => f.write_str("+ Add a new project"),
        }
    }
}

fn run_picker(projects: Vec<ProjectSummary>) -> Result<PickerOutcome> {
    let total = projects.len();
    let mut choices: Vec<Choice> = projects.into_iter().map(Choice::Project).collect();
    choices.push(Choice::AddNew);

    // Recency score: list_projects already sorted most-recent first, so
    // index 0 = best. Inquire's scorer treats larger = better, so invert.
    // Keep all recency scores strictly positive so AddNew (assigned -1)
    // sorts last among ties.
    let recency_score = |idx: usize| -> i64 { (total - idx) as i64 };

    let scorer = |query: &str, choice: &Choice, _label: &str, idx: usize| -> Option<i64> {
        if query.is_empty() {
            // No query: surface every entry; recency drives the order.
            return Some(match choice {
                Choice::AddNew => -1,
                Choice::Project(_) => recency_score(idx),
            });
        }
        match choice {
            // AddNew stays visible at every query — sentinel score parks
            // it below any nucleo hit so a typed match always wins focus.
            Choice::AddNew => Some(i64::MIN + 1),
            Choice::Project(p) => {
                let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
                score(&mut matcher, query, &p.name).and_then(|s| {
                    if s == 0 {
                        None
                    } else {
                        Some(s as i64)
                    }
                })
            }
        }
    };

    let result = Select::new("Select project", choices)
        .with_scorer(&scorer)
        .with_help_message("type to fuzzy filter · ↑↓ navigate · Enter select · Esc cancel")
        .prompt();

    match result {
        Ok(Choice::Project(p)) => Ok(PickerOutcome::Existing(p.name)),
        Ok(Choice::AddNew) => add_new_project(),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Ok(PickerOutcome::Cancelled)
        }
        Err(e) => Err(anyhow!(e)),
    }
}

fn add_new_project() -> Result<PickerOutcome> {
    let validator = |s: &_| -> std::result::Result<Validation, inquire::CustomUserError> {
        if shelbi_core::validate_agent_id(s).is_ok() {
            Ok(Validation::Valid)
        } else {
            Ok(Validation::Invalid(ErrorMessage::Custom(
                "project name must be kebab/snake-case alphanumeric (e.g. `my-app`)".into(),
            )))
        }
    };
    let name = Text::new("Project name:")
        .with_validator(validator)
        .with_help_message(
            "creates ~/.shelbi/projects/<name>.yaml using the current directory as work_dir",
        )
        .prompt();
    match name {
        Ok(name) => {
            crate::commands::init::run(crate::commands::init::Args {
                project: Some(name.clone()),
                root: None,
                mode: None,
                pick_up: false,
            })
            .context("running init for new project")?;
            Ok(PickerOutcome::Created(name))
        }
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Ok(PickerOutcome::Cancelled)
        }
        Err(e) => Err(anyhow!(e)),
    }
}
