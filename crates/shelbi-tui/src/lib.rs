//! ratatui dashboard. Stub for now — full two-pane layout + palette lands in
//! Phase 4. Public entry point is `run()`, called from the binary when shelbi
//! is invoked with no subcommand.

use anyhow::Result;

pub fn run() -> Result<()> {
    println!("shelbi TUI — coming in Phase 4. For now, use the CLI:");
    println!("  shelbi spawn <task-id> --on <machine> --runner <runner> \"<prompt>\"");
    println!("  shelbi list");
    println!("  shelbi --help");
    Ok(())
}
