//! `shelbi popup` — open the palette as a tmux display-popup overlay.
//!
//! Detects the active shelbi session from tmux env, then launches a popup
//! containing `shelbi __palette <project>`. Bound globally to Ctrl+P by
//! `ensure_dashboard` so it works from any pane in any shelbi session.

use anyhow::{anyhow, Result};

pub fn run() -> Result<()> {
    // Detect the current tmux session via display-message.
    let session = tmux_capture(&["display-message", "-p", "#{session_name}"])
        .map_err(|e| anyhow!(e))?;
    let session = session.trim();

    let Some(project) = session.strip_prefix("shelbi-") else {
        // We're in a non-shelbi session — show a friendly message.
        let _ = std::process::Command::new("tmux")
            .args([
                "display-message",
                &format!("[shelbi] not in a shelbi session (current: {session})"),
            ])
            .status();
        return Ok(());
    };

    let bin = std::env::current_exe()?.to_string_lossy().into_owned();
    let cmd = format!(
        "{bin_q} __palette {proj_q}",
        bin_q = shelbi_agent::shell_escape(&bin),
        proj_q = shelbi_agent::shell_escape(project),
    );

    let _ = std::process::Command::new("tmux")
        .args(["display-popup", "-E", "-w", "70%", "-h", "60%", &cmd])
        .status()?;
    Ok(())
}

fn tmux_capture(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("tmux").args(args).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "tmux {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
