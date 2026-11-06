//! Move workspace runtime state out of the hub-global directories older
//! releases kept it in.
//!
//! Those releases wrote `~/.shelbi/workspaces/<slot>/` (and, before the rename,
//! `~/.shelbi/workers/<slot>/`), keyed by slot name alone. Slot names are only
//! unique within a project, so a directory has to be attributed to a project
//! before it can move. [`LegacyStateDisposition`] records the answer:
//!
//! * Exactly one registered project declares the slot: it goes there.
//! * Several do: the `current_task` in its `status.yaml` picks the one whose
//!   board holds that task. The name collision this migration exists for means
//!   the row could have been written by any of them, and the task is the only
//!   evidence of which.
//! * Nothing settles it (no project declares the slot, or the evidence is
//!   missing or ambiguous): it is retired to `~/.shelbi/workspaces.migrated/`.
//!   Guessing would hand one project another's row and, worse, another's
//!   review-server pgid. Retiring costs nothing lasting: the poller writes a
//!   fresh status on its next tick. The copy is kept for hand inspection, the
//!   way project migration keeps `<name>.yaml.migrated`.
//!
//! Driven by the config-upgrade pass (`WORKSPACE_STATE_HUB_GLOBAL`), which runs
//! before the daemon starts serving, so the poller never reads a half-moved
//! layout.

use std::fs;
use std::path::{Path, PathBuf};

use shelbi_core::Result;

use crate::{load_project, read_board_with_cfg, shelbi_home, workspaces_dir, WorkspaceStatus};

/// Where state no project can claim is retired, under `~/.shelbi/`.
const RETIRED_DIR: &str = "workspaces.migrated";

/// The hub-global roots, newest name first.
const LEGACY_ROOTS: &[&str] = &["workspaces", "workers"];

/// Suffix on the directory that held a slot's no-restart marker. Nothing writes
/// that marker any more, but an upgrading install can still have the directory.
const SUPERVISION_SUFFIX: &str = ".supervision";

/// Where one legacy directory went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyStateDisposition {
    /// Moved under this project's `workspaces/`.
    Project { project: String, target: PathBuf },
    /// No project could be chosen; kept under `~/.shelbi/workspaces.migrated/`.
    Retired { target: PathBuf },
}

/// Every per-slot directory still under a hub-global root. Empty once migrated.
/// Stray files directly under a root are not state and are ignored.
pub fn legacy_workspace_state_dirs() -> Result<Vec<PathBuf>> {
    let home = shelbi_home()?;
    let mut dirs = Vec::new();
    for root in LEGACY_ROOTS {
        let Ok(entries) = fs::read_dir(home.join(root)) else {
            continue;
        };
        dirs.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() && !p.is_symlink()),
        );
    }
    dirs.sort();
    Ok(dirs)
}

/// Move one directory from [`legacy_workspace_state_dirs`] to wherever its state
/// belongs among `projects`, and drop its hub-global root once that empties.
///
/// A file already at a project-scoped target wins over the legacy copy: only a
/// current release writes there, so it is the newer of the two.
pub fn migrate_legacy_workspace_state_dir(
    dir: &Path,
    projects: &[String],
) -> Result<LegacyStateDisposition> {
    let Some(key) = dir.file_name().and_then(|n| n.to_str()) else {
        return Err(shelbi_core::Error::Other(format!(
            "workspace state directory has no usable name: {}",
            dir.display()
        )));
    };
    let disposition = match owner_of(dir, key, projects) {
        Some(project) => {
            let target = workspaces_dir(&project)?.join(key);
            move_entries(dir, &target, false)?;
            LegacyStateDisposition::Project { project, target }
        }
        None => {
            let target = shelbi_home()?.join(RETIRED_DIR).join(key);
            move_entries(dir, &target, true)?;
            LegacyStateDisposition::Retired { target }
        }
    };
    fs::remove_dir(dir)?;
    if let Some(root) = dir.parent() {
        // Best-effort: anything else still in the root keeps it around, and the
        // sniffer only looks at the directories inside it.
        let _ = fs::remove_dir(root);
    }
    Ok(disposition)
}

/// The project a legacy directory belongs to, per the module docs.
fn owner_of(dir: &Path, key: &str, projects: &[String]) -> Option<String> {
    let slot = key.strip_suffix(SUPERVISION_SUFFIX).unwrap_or(key);
    let mut declaring: Vec<&String> = projects
        .iter()
        .filter(|name| declares_slot(name, slot))
        .collect();
    match declaring.len() {
        0 => None,
        1 => declaring.pop().cloned(),
        _ => {
            let task = status_task(dir)?;
            let mut holding = declaring.into_iter().filter(|name| board_holds(name, &task));
            let owner = holding.next()?;
            holding.next().is_none().then(|| owner.clone())
        }
    }
}

fn declares_slot(project: &str, slot: &str) -> bool {
    load_project(project).is_ok_and(|p| p.workspaces.iter().any(|w| w.name == slot))
}

/// The task a legacy `status.yaml` names. `None` when the file is missing,
/// unreadable, or names no task.
fn status_task(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("status.yaml")).ok()?;
    serde_yaml::from_str::<WorkspaceStatus>(&text)
        .ok()?
        .current_task
}

/// Whether `project`'s board shows `task`. A remote board with no warm index
/// reads as empty, so it just doesn't count as evidence.
fn board_holds(project: &str, task: &str) -> bool {
    let Ok(p) = load_project(project) else {
        return false;
    };
    read_board_with_cfg(project, &p.issue_tracker)
        .map(|board| board.into_issues().iter().any(|i| i.task.id == task))
        .unwrap_or(false)
}

/// Move every entry of `from` into `to`. On a name clash the moved entry
/// replaces the existing one when `replace` is set; otherwise the existing one
/// stays and the moved one is dropped.
fn move_entries(from: &Path, to: &Path, replace: bool) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if !replace && target.exists() {
            if entry.path().is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
            continue;
        }
        fs::rename(entry.path(), target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use crate::{load_workspace_status, read_review_serve_pgid};

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-ws-state-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn register_project(home: &Path, name: &str, slots: &[&str]) {
        let dir = home.join("projects");
        fs::create_dir_all(dir.join(name)).unwrap();
        let workspaces: String = slots
            .iter()
            .map(|s| format!("- {{ name: {s}, machine: hub, runner: claude }}\n"))
            .collect();
        fs::write(
            dir.join(format!("{name}.yaml")),
            format!(
                "name: {name}\nrepo: /tmp/{name}\nmachines:\n- name: hub\n  kind: local\n  work_dir: /tmp/{name}\norchestrator: {{ runner: claude }}\nagent_runners:\n  claude: {{ command: claude, flags: [] }}\nworkspaces:\n{workspaces}"
            ),
        )
        .unwrap();
    }

    fn add_task(project: &str, id: &str) {
        let issue: shelbi_core::Issue = serde_yaml::from_str(&format!(
            "id: {id}\ntitle: {id}\ncolumn: in_progress\npriority: 1\n\
             created_at: 2026-11-05T00:00:00Z\nupdated_at: 2026-11-05T00:00:00Z\n"
        ))
        .unwrap();
        crate::save_task(project, &issue, "body").unwrap();
    }

    fn legacy_slot(home: &Path, root: &str, key: &str, task: Option<&str>) -> PathBuf {
        let dir = home.join(root).join(key);
        fs::create_dir_all(&dir).unwrap();
        let task = task.map(|t| format!("current_task: {t}\n")).unwrap_or_default();
        fs::write(
            dir.join("status.yaml"),
            format!(
                "workspace: {key}\n{task}state: working\nlast_transition: 2026-11-05T00:00:00Z\nlast_seen: 2026-11-05T00:00:00Z\n"
            ),
        )
        .unwrap();
        dir
    }

    fn names(projects: &[&str]) -> Vec<String> {
        projects.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_slot_one_project_declares_moves_to_that_project() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "app", &["alpha"]);
        register_project(&home, "site", &["beta"]);
        let dir = legacy_slot(&home, "workspaces", "alpha", Some("t-1"));
        fs::write(dir.join("review-serve.pgid"), "4242\n").unwrap();

        let got = migrate_legacy_workspace_state_dir(&dir, &names(&["app", "site"])).unwrap();

        assert!(matches!(got, LegacyStateDisposition::Project { ref project, .. } if project == "app"));
        let status = load_workspace_status("app", "alpha").unwrap().unwrap();
        assert_eq!(status.current_task.as_deref(), Some("t-1"));
        assert_eq!(read_review_serve_pgid("app", "alpha").unwrap(), Some(4242));
        assert!(!dir.exists());
        assert!(!home.join("workspaces").exists(), "emptied root is removed");
        assert!(legacy_workspace_state_dirs().unwrap().is_empty());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_shared_slot_name_goes_to_the_project_whose_board_holds_its_task() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "app", &["review"]);
        register_project(&home, "site", &["review"]);
        add_task("site", "site-task");
        let dir = legacy_slot(&home, "workspaces", "review", Some("site-task"));

        let got = migrate_legacy_workspace_state_dir(&dir, &names(&["app", "site"])).unwrap();

        assert!(matches!(got, LegacyStateDisposition::Project { ref project, .. } if project == "site"));
        assert!(load_workspace_status("site", "review").unwrap().is_some());
        assert!(load_workspace_status("app", "review").unwrap().is_none());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_shared_slot_name_with_no_evidence_is_retired_not_guessed() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "app", &["review"]);
        register_project(&home, "site", &["review"]);
        add_task("app", "same-id");
        add_task("site", "same-id");
        let no_task = legacy_slot(&home, "workspaces", "review", None);
        fs::write(no_task.join("review-serve.pgid"), "4242\n").unwrap();

        let got = migrate_legacy_workspace_state_dir(&no_task, &names(&["app", "site"])).unwrap();

        let retired = home.join("workspaces.migrated/review");
        assert_eq!(got, LegacyStateDisposition::Retired { target: retired.clone() });
        assert!(retired.join("status.yaml").exists(), "retired, not deleted");
        for project in ["app", "site"] {
            assert!(load_workspace_status(project, "review").unwrap().is_none());
            assert_eq!(read_review_serve_pgid(project, "review").unwrap(), None);
        }

        // A task both boards hold settles nothing either.
        let both = legacy_slot(&home, "workspaces", "review", Some("same-id"));
        assert!(matches!(
            migrate_legacy_workspace_state_dir(&both, &names(&["app", "site"])).unwrap(),
            LegacyStateDisposition::Retired { .. }
        ));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_slot_nobody_declares_is_retired() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "app", &["alpha"]);
        let dir = legacy_slot(&home, "workspaces", "gone", None);

        let got = migrate_legacy_workspace_state_dir(&dir, &names(&["app"])).unwrap();

        assert!(matches!(got, LegacyStateDisposition::Retired { .. }));
        assert!(home.join("workspaces.migrated/gone/status.yaml").exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_supervision_marker_follows_its_slot() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "app", &["alpha"]);
        let dir = home.join("workspaces/alpha.supervision");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".expected-teardown"), "").unwrap();

        migrate_legacy_workspace_state_dir(&dir, &names(&["app"])).unwrap();

        assert!(home
            .join("projects/app/workspaces/alpha.supervision/.expected-teardown")
            .exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn the_pre_rename_workers_root_migrates_too() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "app", &["alpha"]);
        let dir = home.join("workers/alpha");
        fs::create_dir_all(&dir).unwrap();
        // The legacy `worker:` key still loads after the move.
        fs::write(
            dir.join("status.yaml"),
            "worker: alpha\nstate: working\nlast_transition: 2026-11-05T00:00:00Z\nlast_seen: 2026-11-05T00:00:00Z\n",
        )
        .unwrap();

        assert_eq!(legacy_workspace_state_dirs().unwrap(), vec![dir.clone()]);
        migrate_legacy_workspace_state_dir(&dir, &names(&["app"])).unwrap();

        let status = load_workspace_status("app", "alpha").unwrap().unwrap();
        assert_eq!(status.workspace, "alpha");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_current_file_is_never_clobbered_by_the_legacy_copy() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "app", &["alpha"]);
        let current = crate::workspace_status_path("app", "alpha").unwrap();
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        fs::write(&current, "current").unwrap();
        let dir = legacy_slot(&home, "workspaces", "alpha", None);

        migrate_legacy_workspace_state_dir(&dir, &names(&["app"])).unwrap();

        assert_eq!(fs::read_to_string(&current).unwrap(), "current");
        assert!(!dir.exists(), "the stale copy doesn't linger");

        std::env::remove_var("SHELBI_HOME");
    }
}
