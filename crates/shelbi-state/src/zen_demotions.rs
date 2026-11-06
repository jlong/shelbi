//! The user's "leave this card alone" signal to Zen: a card the user moved by
//! hand from a ready status back to the backlog stays out of the Zen scan until
//! the user moves it out of the backlog again.
//!
//! The demotion lives in the project's `state.json`
//! ([`State::zen_demotions`](crate::State::zen_demotions)), written at the moment
//! of the move. It used to be re-derived from `events.log` on every scan, which
//! tied a permanent decision to a size-rotated, hub-global log: rotation expired
//! it, and the same id in another project shared it.
//!
//! "By hand" means a `user:`-prefixed move reason (`user:tui`, `user:app`,
//! `user:cli`). Zen's own moves carry `orchestrator:` / `zen:` reasons, so they
//! neither set nor clear a demotion. Statuses are matched by category through
//! `statuses.yaml`, so a custom ready or backlog status behaves like the stock
//! `todo` and `backlog`.

use std::collections::HashSet;

use shelbi_core::{Column, Result, StatusCategory};

use crate::event_log::resolve_move_categories;
use crate::{read_state, update_state};

/// Apply a landed `from -> to` move to `task_id`'s demotion. Call it for every
/// move that carries a caller-supplied reason, after the move is on disk. A move
/// that neither demotes nor re-promotes doesn't touch `state.json`.
pub fn record_zen_user_move(
    project: &str,
    task_id: &str,
    from: &Column,
    to: &Column,
    reason: &str,
) -> Result<()> {
    // Checked first so the many non-user moves skip the catalog read.
    if !reason.starts_with("user:") {
        return Ok(());
    }
    match resolve_move_categories(project, from, to) {
        (StatusCategory::Ready, StatusCategory::Backlog) => update_state(project, |state| {
            state
                .zen_demotions
                .insert(task_id.to_string(), chrono::Utc::now());
            Ok(())
        }),
        (StatusCategory::Backlog, to) if to != StatusCategory::Backlog => {
            clear_zen_demotion(project, task_id)
        }
        _ => Ok(()),
    }
}

/// Forget `task_id`'s demotion, if it has one. The issue-delete path calls this
/// so a later card that reuses the id doesn't inherit it.
pub fn clear_zen_demotion(project: &str, task_id: &str) -> Result<()> {
    update_state(project, |state| {
        state.zen_demotions.remove(task_id);
        Ok(())
    })
}

/// The task ids Zen must leave in the backlog for `project`.
pub fn zen_demoted_task_ids(project: &str) -> Result<HashSet<String>> {
    Ok(read_state(project)?.zen_demotions.into_keys().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;

    struct Home {
        path: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        fn new(tag: &str) -> Self {
            let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let path = std::env::temp_dir().join(format!(
                "shelbi-zen-demotions-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("projects/demo")).unwrap();
            std::fs::create_dir_all(path.join("projects/other")).unwrap();
            std::env::set_var("SHELBI_HOME", &path);
            Self { path, _lock: lock }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn demoted(project: &str) -> Vec<String> {
        let mut ids: Vec<_> = zen_demoted_task_ids(project).unwrap().into_iter().collect();
        ids.sort();
        ids
    }

    #[test]
    fn a_user_move_from_ready_to_backlog_demotes_and_a_user_move_out_clears() {
        let _home = Home::new("cycle");
        for reason in ["user:tui", "user:app", "user:cli"] {
            record_zen_user_move("demo", "t", &Column::todo(), &Column::backlog(), reason).unwrap();
            assert_eq!(demoted("demo"), ["t"], "{reason} should demote");
            record_zen_user_move("demo", "t", &Column::backlog(), &Column::todo(), reason).unwrap();
            assert!(demoted("demo").is_empty(), "{reason} should clear");
        }
    }

    #[test]
    fn starting_a_demoted_card_by_hand_clears_it() {
        let _home = Home::new("start");
        record_zen_user_move("demo", "t", &Column::todo(), &Column::backlog(), "user:tui").unwrap();
        record_zen_user_move(
            "demo",
            "t",
            &Column::backlog(),
            &Column::in_progress(),
            "user:cli:start",
        )
        .unwrap();
        assert!(demoted("demo").is_empty());
    }

    #[test]
    fn moves_without_a_user_reason_neither_demote_nor_clear() {
        let _home = Home::new("nonuser");
        record_zen_user_move("demo", "t", &Column::todo(), &Column::backlog(), "zen:rollback")
            .unwrap();
        assert!(demoted("demo").is_empty(), "Zen's own rollback isn't the user");

        record_zen_user_move("demo", "t", &Column::todo(), &Column::backlog(), "user:tui").unwrap();
        record_zen_user_move(
            "demo",
            "t",
            &Column::backlog(),
            &Column::todo(),
            "orchestrator:zen-promote category=1",
        )
        .unwrap();
        assert_eq!(demoted("demo"), ["t"], "Zen can't clear the user's demotion");
    }

    #[test]
    fn other_user_moves_leave_the_demotion_as_it_was() {
        let _home = Home::new("other-edges");
        // Into the backlog from past the ready stage: not the todo -> backlog
        // decision the guarantee covers.
        record_zen_user_move(
            "demo",
            "a",
            &Column::in_progress(),
            &Column::backlog(),
            "user:tui",
        )
        .unwrap();
        assert!(demoted("demo").is_empty());
        // A later hop that doesn't leave the backlog keeps it.
        record_zen_user_move("demo", "b", &Column::todo(), &Column::backlog(), "user:tui").unwrap();
        record_zen_user_move("demo", "b", &Column::todo(), &Column::in_progress(), "user:tui")
            .unwrap();
        assert_eq!(demoted("demo"), ["b"]);
    }

    #[test]
    fn a_demotion_belongs_to_one_project() {
        let _home = Home::new("scope");
        record_zen_user_move("demo", "t", &Column::todo(), &Column::backlog(), "user:tui").unwrap();
        assert_eq!(demoted("demo"), ["t"]);
        assert!(demoted("other").is_empty());
    }

    #[test]
    fn custom_statuses_match_by_category() {
        let _home = Home::new("custom");
        let mut catalog = crate::load_project_statuses("demo").unwrap();
        for (id, category) in [
            ("icebox", StatusCategory::Backlog),
            ("queued", StatusCategory::Ready),
        ] {
            catalog.statuses.push(shelbi_core::ProjectStatus {
                id: id.into(),
                name: id.into(),
                category,
            });
        }
        crate::save_project_statuses("demo", &catalog).unwrap();
        let icebox = Column::from_status_id("icebox");
        let queued = Column::from_status_id("queued");
        record_zen_user_move("demo", "t", &queued, &icebox, "user:app").unwrap();
        assert_eq!(demoted("demo"), ["t"]);
        record_zen_user_move("demo", "t", &icebox, &queued, "user:app").unwrap();
        assert!(demoted("demo").is_empty());
    }

    #[test]
    fn clear_forgets_a_deleted_card() {
        let _home = Home::new("clear");
        record_zen_user_move("demo", "t", &Column::todo(), &Column::backlog(), "user:tui").unwrap();
        clear_zen_demotion("demo", "t").unwrap();
        assert!(demoted("demo").is_empty());
    }

    #[test]
    fn an_empty_map_leaves_no_key_in_state_json() {
        let _home = Home::new("bytes");
        let body = serde_json::to_string(&crate::State::default()).unwrap();
        assert!(!body.contains("zen_demotions"), "{body}");
    }
}
