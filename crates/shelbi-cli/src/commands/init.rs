use anyhow::{anyhow, Result};

pub fn run() -> Result<()> {
    let home = shelbi_state::shelbi_home().map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&home).map_err(|e| anyhow!(e))?;

    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    let sessions_dir = shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&projects_dir).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&sessions_dir).map_err(|e| anyhow!(e))?;

    // If no sessions exist yet, drop a `default` one in.
    let default_session = sessions_dir.join("default.yaml");
    if !default_session.exists() {
        std::fs::write(
            &default_session,
            "name: default\nprojects: []\nstartup: []\n",
        )?;
    }

    println!("✓ scaffolded {}", home.display());
    println!();
    println!("created:");
    println!("  {}", projects_dir.display());
    println!("  {}", sessions_dir.display());
    if default_session.exists() {
        println!("  {} (empty workspace)", default_session.display());
    }
    println!();
    println!("next:");
    println!("  1. drop a project YAML at ~/.shelbi/projects/<name>.yaml");
    println!("     (see examples/myapp.yaml in the shelbi repo for a template)");
    println!("  2. reference it from ~/.shelbi/sessions/default.yaml");
    println!("  3. cd into your repo and run:");
    println!("       echo NAME > .shelbi/project");
    println!("     so `shelbi spawn ...` knows which project you mean.");
    Ok(())
}
