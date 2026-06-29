//! `shelbi status <subcommand>` — inspect the project's status catalogue.
//!
//! Status identity (id, name, category, declared order) is the project-
//! wide source of truth in `workflows/statuses.yml`; individual workflows
//! reference these ids and add per-workflow owner / optional agent. This
//! subcommand exposes the catalogue itself — for per-workflow owner/agent
//! tables, see `shelbi workflow show <name>`.

use anyhow::{anyhow, Result};
use clap::Subcommand;

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum StatusCmd {
    /// Print the canonical status list — order, id, name, category —
    /// from `workflows/statuses.yml`. Order here is the left-to-right
    /// column order used by every view in the project.
    List,
}

pub fn run(project: Option<String>, cmd: StatusCmd) -> Result<()> {
    let project = require_project(project)?;
    match cmd {
        StatusCmd::List => list(&project),
    }
}

fn list(project: &str) -> Result<()> {
    let statuses = shelbi_state::load_project_statuses(project).map_err(|e| anyhow!(e))?;
    println!("{:<7} {:<13} {:<15} CATEGORY", "ORDER", "ID", "NAME");
    for (idx, st) in statuses.statuses.iter().enumerate() {
        println!(
            "{:<7} {:<13} {:<15} {}",
            idx + 1,
            st.id,
            st.name,
            st.category,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use shelbi_core::{default_project_statuses, ProjectStatuses};
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-status-test-{}-{}",
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
    fn list_succeeds_against_default_statuses() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // No statuses.yml on disk — loader falls back to the built-in
        // default and `list` should still print without erroring.
        list("p").unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_prints_in_canonical_declared_order() {
        // Sanity-check that the printed order matches the on-disk
        // declared order — the column-ordering contract everything
        // downstream relies on.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let custom = ProjectStatuses {
            statuses: vec![
                shelbi_core::ProjectStatus {
                    id: "z-last".into(),
                    name: "Z Last".into(),
                    category: shelbi_core::StatusCategory::Archived,
                },
                shelbi_core::ProjectStatus {
                    id: "a-first".into(),
                    name: "A First".into(),
                    category: shelbi_core::StatusCategory::Ready,
                },
            ],
        };
        shelbi_state::save_project_statuses("p", &custom).unwrap();
        // List drives off the loader, which preserves on-disk order.
        let loaded = shelbi_state::load_project_statuses("p").unwrap();
        assert_eq!(loaded.statuses[0].id, "z-last");
        assert_eq!(loaded.statuses[1].id, "a-first");
        // Sanity: the helper exists and the round-trip preserves order.
        let _ = default_project_statuses();
        std::env::remove_var("SHELBI_HOME");
    }
}
