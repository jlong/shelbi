use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use shelbi_state::AgentMaterializeOutcome;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Also scaffold a starter project YAML at ~/.shelbi/projects/<name>.yaml
    /// (using the current directory as the work_dir for a local hub).
    #[arg(long)]
    pub project: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let home = shelbi_state::shelbi_home().map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&home).map_err(|e| anyhow!(e))?;

    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    let sessions_dir = shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&projects_dir).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&sessions_dir).map_err(|e| anyhow!(e))?;

    let default_session = sessions_dir.join("default.yaml");
    if !default_session.exists() {
        std::fs::write(
            &default_session,
            "name: default\nprojects: []\nstartup: []\n",
        )?;
    }

    println!("✓ scaffolded {}", home.display());

    if let Some(name) = args.project.as_deref() {
        let cwd = std::env::current_dir()?;
        let yaml_path = projects_dir.join(format!("{name}.yaml"));
        if yaml_path.exists() {
            println!("(project YAML already exists at {})", yaml_path.display());
        } else {
            let yaml = format!(
                "name: {name}\n\
                 repo: \n\
                 default_branch: main\n\
                 machines:\n\
                 \x20\x20- name: hub\n\
                 \x20\x20\x20\x20kind: local\n\
                 \x20\x20\x20\x20work_dir: {cwd}\n\
                 orchestrator:\n\
                 \x20\x20runner: claude\n\
                 agent_runners:\n\
                 \x20\x20claude: {{ command: claude, flags: [] }}\n\
                 \x20\x20codex:  {{ command: codex,  flags: [] }}\n",
                cwd = cwd.display(),
            );
            std::fs::write(&yaml_path, yaml)?;
            println!("✓ wrote project: {}", yaml_path.display());

            let marker = cwd.join(".shelbi/project");
            shelbi_state::ensure_dir(marker.parent().unwrap())
                .map_err(|e| anyhow!(e))?;
            std::fs::write(&marker, format!("{name}\n"))?;
            println!("✓ wrote project marker: {}", marker.display());

            let template_path = shelbi_state::project_dir(name)
                .map_err(|e| anyhow!(e))?
                .join("worker-settings.json.template");
            if template_path.exists() {
                println!(
                    "(worker settings template already exists at {})",
                    template_path.display()
                );
            } else {
                shelbi_state::ensure_dir(template_path.parent().unwrap())
                    .map_err(|e| anyhow!(e))?;
                std::fs::write(&template_path, shelbi_state::DEFAULT_WORKER_SETTINGS_TEMPLATE)?;
                println!(
                    "✓ wrote worker settings template: {}",
                    template_path.display()
                );
            }

            let outcomes = shelbi_state::materialize_default_agents(name)
                .map_err(|e| anyhow!(e))?;
            for outcome in outcomes {
                print_agent_materialize_outcome(&outcome);
            }
        }
    }

    println!();
    println!("next:");
    if args.project.is_none() {
        println!("  1. drop a project YAML at ~/.shelbi/projects/<name>.yaml");
        println!("     (or rerun: shelbi init --project <name>)");
        println!("  2. reference it from ~/.shelbi/sessions/default.yaml");
        println!("  3. cd into your repo and `echo NAME > .shelbi/project`");
    } else {
        println!("  1. add machines to ~/.shelbi/projects/{}.yaml if you have remote hubs",
            args.project.as_deref().unwrap());
        println!("  2. add the project to ~/.shelbi/sessions/default.yaml's projects: list");
        println!("  3. spawn your first agent: shelbi spawn TASK --on hub --runner claude \"…\"");
    }
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
