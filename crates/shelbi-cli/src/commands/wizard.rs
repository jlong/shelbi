use anyhow::Result;

use crate::wizard;

pub fn run() -> Result<()> {
    wizard::phase_1_assistant_name()?;
    let cfg = shelbi_state::load_shelbi_config()
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("✓ assistant: {}", cfg.assistant_name());
    Ok(())
}
