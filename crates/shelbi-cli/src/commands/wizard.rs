use anyhow::{Context, Result};

use crate::wizard;

/// What the wizard did. The entry-point uses this to decide whether to
/// boot a TUI, print a hint, or exit silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardOutcome {
    /// All requested phases ran to completion (each phase is independently
    /// idempotent, so "completion" includes "skipped because already done").
    Completed,
    /// User chose q or pressed Ctrl-C / Esc. Confirmation-card cancellation
    /// leaves no project state on disk.
    Cancelled,
}

/// Run the wizard. When `first_run` is true (no `~/.shelbi/` existed
/// before this invocation), the banner + tagline is printed once at the
/// very top, before any prompt. Re-entries (`shelbi wizard` after the
/// home dir exists) pass `false`. `shelbi project add` enters the shared
/// one-project flow below without the idempotence guard.
pub fn run(first_run: bool) -> Result<WizardOutcome> {
    if first_run {
        wizard::print_banner();
    }
    if has_any_project_registration()? {
        return Ok(WizardOutcome::Completed);
    }

    let outcome = run_one_project()?;
    let command_outcome = wizard_outcome_for_setup(&outcome);
    launch_setup_outcome(outcome, |name| {
        shelbi_tui::run_main(name).context("launching shelbi")
    })?;
    Ok(command_outcome)
}

fn wizard_outcome_for_setup(outcome: &wizard::SetupOutcome) -> WizardOutcome {
    match outcome {
        wizard::SetupOutcome::Created(_) => WizardOutcome::Completed,
        wizard::SetupOutcome::Quit => WizardOutcome::Cancelled,
    }
}

fn has_any_project_registration() -> Result<bool> {
    let projects = shelbi_state::projects_dir().map_err(|error| anyhow::anyhow!(error))?;
    if !projects.exists() {
        return Ok(false);
    }
    for entry in
        std::fs::read_dir(&projects).with_context(|| format!("reading {}", projects.display()))?
    {
        let path = entry
            .with_context(|| format!("reading an entry in {}", projects.display()))?
            .path();
        if (path.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("yaml"))
            || (path.is_dir() && path.join("local.yaml").is_file())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run the shared one-project setup flow, translating prompt cancellation
/// into the same write-free quit outcome as an explicit `q` on the plan card.
/// Callers use the created project name to launch the same dashboard from
/// bare `shelbi` and `shelbi project add`.
pub fn run_one_project() -> Result<wizard::SetupOutcome> {
    match wizard::setup_one_project() {
        Ok(outcome) => Ok(outcome),
        Err(e) if is_cancel(&e) => Ok(wizard::SetupOutcome::Quit),
        Err(e) => Err(e),
    }
}

/// Run setup and launch exactly when it created a project. Shared by bare
/// `shelbi` and `shelbi project add` so their Enter/q behavior cannot drift.
pub fn run_one_project_and_launch() -> Result<()> {
    launch_setup_outcome(run_one_project()?, |name| {
        shelbi_tui::run_main(name).context("launching shelbi")
    })
}

fn launch_setup_outcome<F>(outcome: wizard::SetupOutcome, launch: F) -> Result<()>
where
    F: FnOnce(&str) -> Result<()>,
{
    match outcome {
        wizard::SetupOutcome::Created(name) => launch(&name),
        wizard::SetupOutcome::Quit => Ok(()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use tempfile::TempDir;

    #[test]
    fn created_launches_once_and_quit_does_not_launch() {
        let mut launched = Vec::new();
        launch_setup_outcome(wizard::SetupOutcome::Created("shaft".into()), |name| {
            launched.push(name.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(launched, vec!["shaft"]);

        launch_setup_outcome(wizard::SetupOutcome::Quit, |_| {
            panic!("Quit must not launch the dashboard")
        })
        .unwrap();

        assert_eq!(
            wizard_outcome_for_setup(&wizard::SetupOutcome::Created("shaft".into())),
            WizardOutcome::Completed
        );
        assert_eq!(
            wizard_outcome_for_setup(&wizard::SetupOutcome::Quit),
            WizardOutcome::Cancelled
        );
    }

    #[test]
    fn idempotence_guard_recognizes_flat_and_split_registrations() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        assert!(!has_any_project_registration().unwrap());
        std::fs::create_dir_all(home.join("projects/split")).unwrap();
        std::fs::write(home.join("projects/split/local.yaml"), "repo: /tmp/split\n").unwrap();
        assert!(has_any_project_registration().unwrap());

        std::fs::remove_dir_all(home.join("projects/split")).unwrap();
        std::fs::write(home.join("projects/flat.yaml"), "name: flat\n").unwrap();
        assert!(has_any_project_registration().unwrap());
    }
}
