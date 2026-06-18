use anyhow::{anyhow, Result};

pub fn run() -> Result<()> {
    let home = shelbi_state::shelbi_home().map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&home).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&shelbi_state::projects_dir().map_err(|e| anyhow!(e))?)
        .map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?)
        .map_err(|e| anyhow!(e))?;
    println!("✓ scaffolded {}", home.display());
    println!();
    println!("next: drop a project YAML at ~/.shelbi/projects/<name>.yaml");
    println!("      see examples/myapp.yaml in the shelbi repo for a template");
    Ok(())
}
