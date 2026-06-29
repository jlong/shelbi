use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use shelbi_state::AgentMaterializeOutcome;

use crate::project_root::{resolve_root_for_init, ResolvedProjectRoot};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Override the project name. Defaults to the basename of the
    /// project root (current directory or `--root`).
    #[arg(long)]
    pub project: Option<String>,

    /// Project root directory — the repo `shelbi` will manage. Skips
    /// the interactive "Project root?" prompt. Required when stdin
    /// is not a TTY (CI, piped input).
    #[arg(long)]
    pub root: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let resolved = scaffold_with_prompt(args)?;
    println!();
    println!("next:");
    println!(
        "  1. add machines to ~/.shelbi/projects/{}.yaml if you have remote hubs",
        resolved.name
    );
    println!("  2. add the project to ~/.shelbi/sessions/default.yaml's projects: list");
    println!("  3. spawn your first agent: shelbi spawn TASK --on hub --runner claude \"…\"");
    Ok(())
}

/// `shelbi init` entry point factored so the no-subcommand first-run
/// path can share the same scaffolding without printing the trailing
/// `next:` block (that path is about to launch the TUI, so the hints
/// would just scroll off-screen).
///
/// Resolves the project root (prompting interactively, or honoring
/// `--root` when supplied), then writes the project YAML, the
/// workspace-settings template, the default agent workspaces, and the
/// project-wide statuses catalogue. No `.shelbi/project` marker is
/// dropped — resolution reverse-looks-up the directory against the
/// registered project YAMLs (see [`shelbi_state::resolve_project_for_cwd`]).
pub fn scaffold_with_prompt(args: Args) -> Result<ResolvedProjectRoot> {
    // Hard-fail with a clear, source-tagged error if the shelbi root is
    // unwritable; otherwise materialize the standard layout
    // (projects/, sessions/, agents/, logs/, workspaces/).
    let home = shelbi_state::ensure_root_subdirs().map_err(|e| anyhow!(e))?;

    let sessions_dir = shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?;

    let default_session = sessions_dir.join("default.yaml");
    if !default_session.exists() {
        std::fs::write(
            &default_session,
            "name: default\nprojects: []\nstartup: []\n",
        )?;
    }

    println!("✓ scaffolded {}", home.display());

    if std::io::IsTerminal::is_terminal(&std::io::stdin()) && args.root.is_none() {
        println!();
        println!("shelbi setup — let's get your project configured.");
        println!();
    }

    let cwd = std::env::current_dir()?;
    let resolved = resolve_root_for_init(&cwd, args.root.clone(), args.project.as_deref())?;

    scaffold_project(&resolved)?;
    Ok(resolved)
}

/// Write the project YAML, the workspace-settings template, materialize
/// the default agents, and write the project-wide statuses catalogue.
/// Deliberately does **not** drop a `.shelbi/project` marker: the project
/// tree stays clean and resolution reads the registered YAMLs instead.
///
/// The collision check in [`resolve_root_for_init`] guarantees the YAML
/// path is free at the time we're called. We still guard the write
/// with `exists()` so a race against a concurrent `shelbi init` doesn't
/// blow away another invocation's freshly-written YAML.
fn scaffold_project(resolved: &ResolvedProjectRoot) -> Result<()> {
    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    let yaml_path = projects_dir.join(format!("{}.yaml", resolved.name));

    if yaml_path.exists() {
        println!("(project YAML already exists at {})", yaml_path.display());
        return Ok(());
    }

    let yaml = format!(
        "name: {name}\n\
         repo: \n\
         default_branch: main\n\
         machines:\n\
         \x20\x20- name: hub\n\
         \x20\x20\x20\x20kind: local\n\
         \x20\x20\x20\x20work_dir: {root}\n\
         orchestrator:\n\
         \x20\x20runner: claude\n\
         agent_runners:\n\
         \x20\x20claude: {{ command: claude, flags: [] }}\n\
         \x20\x20codex:  {{ command: codex,  flags: [] }}\n",
        name = resolved.name,
        root = resolved.path.display(),
    );
    std::fs::write(&yaml_path, yaml)?;
    println!("✓ wrote project: {}", yaml_path.display());

    write_workspace_settings_template(&resolved.name)?;

    let outcomes = shelbi_state::materialize_default_agents(&resolved.name)
        .map_err(|e| anyhow!(e))?;
    for outcome in outcomes {
        print_agent_materialize_outcome(&outcome);
    }

    // Materialize `workflows/statuses.yml` so a fresh project ships with
    // the project-wide status catalogue alongside its starter
    // `default.yaml`. `load_project` runs the same migration when the
    // project is opened, but writing it here keeps `shelbi init`'s
    // post-condition self-contained.
    let statuses_path =
        shelbi_state::statuses_path(&resolved.name).map_err(|e| anyhow!(e))?;
    if !statuses_path.exists() {
        shelbi_state::save_project_statuses(
            &resolved.name,
            &shelbi_core::default_project_statuses(),
        )
        .map_err(|e| anyhow!(e))?;
        println!("✓ wrote project statuses: {}", statuses_path.display());
    }
    Ok(())
}

fn write_workspace_settings_template(project: &str) -> Result<()> {
    let template_path = shelbi_state::project_dir(project)
        .map_err(|e| anyhow!(e))?
        .join("workspace-settings.json.template");
    if template_path.exists() {
        println!(
            "(workspace settings template already exists at {})",
            template_path.display()
        );
        return Ok(());
    }
    shelbi_state::ensure_dir(template_path.parent().unwrap()).map_err(|e| anyhow!(e))?;
    std::fs::write(&template_path, shelbi_state::DEFAULT_WORKSPACE_SETTINGS_TEMPLATE)?;
    println!(
        "✓ wrote workspace settings template: {}",
        template_path.display()
    );
    Ok(())
}

/// Stringify a [`shelbi_state::AgentMaterializeOutcome`] for the init /
/// reload report. Same renderer used by both commands so the user sees
/// the same wording for the same outcome regardless of which path
/// touched the agent workspace.
pub(super) fn print_agent_materialize_outcome(outcome: &AgentMaterializeOutcome) {
    match outcome {
        AgentMaterializeOutcome::Created { agent } => {
            println!("✓ created agent workspace: agents/{agent}/");
        }
        AgentMaterializeOutcome::Unchanged { agent } => {
            println!("(agent workspace already exists: agents/{agent}/)");
        }
        AgentMaterializeOutcome::Preserved { agent, first_notice } => {
            if *first_notice {
                println!(
                    "(preserved your custom agents/{agent}/instructions.md — \
                     differs from the bundled default; the project owns the override)"
                );
            } else {
                println!("(preserved your custom agents/{agent}/instructions.md)");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;

    fn fresh_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-init-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn scaffold_writes_yaml_but_no_marker() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let project_root = fresh_dir("repo");
        std::env::set_var("SHELBI_HOME", &home);
        // Materialize projects/, sessions/, … the way `scaffold_with_prompt`
        // does before reaching `scaffold_project`.
        shelbi_state::ensure_root_subdirs().unwrap();

        let resolved = ResolvedProjectRoot {
            path: project_root.clone(),
            name: "myapp".to_string(),
        };
        scaffold_project(&resolved).unwrap();

        // The project YAML lands under the shelbi home, pointing the hub
        // machine at the project root...
        let yaml = home.join("projects/myapp.yaml");
        assert!(yaml.is_file(), "expected project YAML at {}", yaml.display());
        let body = std::fs::read_to_string(&yaml).unwrap();
        assert!(body.contains(&format!("work_dir: {}", project_root.display())));

        // ...but the project tree stays clean — no `.shelbi/project` marker
        // and no `.shelbi` directory created by init.
        assert!(
            !project_root.join(".shelbi/project").exists(),
            "init must not write a .shelbi/project marker"
        );
        assert!(
            !project_root.join(".shelbi").exists(),
            "init must not create a .shelbi directory in the project tree"
        );

        std::env::remove_var("SHELBI_HOME");
    }
}
