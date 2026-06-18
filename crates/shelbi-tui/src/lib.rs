//! ratatui dashboard. Stub for now — full two-pane layout + palette lands in
//! Phase 4. Public entry point is `run(session_name)`, called from the binary
//! when shelbi is invoked with no subcommand.

use anyhow::Result;

pub fn run(session_name: &str) -> Result<()> {
    println!("shelbi — loading session `{session_name}`");
    match shelbi_state::load_session(session_name) {
        Ok(s) => {
            println!("✓ {} ({} project{})",
                s.name,
                s.projects.len(),
                if s.projects.len() == 1 { "" } else { "s" }
            );
            for sp in &s.projects {
                println!("  · {} (machines: {})", sp.name, sp.machines.join(", "));
            }
        }
        Err(e) => {
            println!("(no such session — run `shelbi init` to scaffold)");
            tracing::debug!("session load error: {e}");
        }
    }
    println!();
    println!("the TUI dashboard is coming in Phase 4. for now, use the CLI:");
    println!("  shelbi spawn <task-id> --on <machine> --runner <runner> \"<prompt>\"");
    println!("  shelbi list");
    println!("  shelbi --help");
    Ok(())
}
