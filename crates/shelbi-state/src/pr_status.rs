//! The hub's local cache of where each pull request stands.
//!
//! A task durably records which PR is its own ([`Issue::pr`]: number and URL,
//! written once when the PR opens). Everything that keeps changing about that
//! PR lives here instead, in `<project>/pr-status.json`:
//!
//! - **State and CI.** The hub poller's sweeps and the workflow actions note
//!   them as they learn them, and every board read folds them in ([`fold`]). A
//!   GitHub issue's `updated_at` moves on any write and orders the done column,
//!   so storing these on the issue would spend a write per CI result and
//!   reshuffle Done whenever a PR's state settled after its issue closed.
//! - **Backfill.** A task that predates PR recording has no record, but the PR
//!   listing here names each PR's head branch, so a read finds the task's PR
//!   from its branch ([`PrStatusCache::resolve`]) without rewriting history.
//!
//! The file is a cache, never the record: a missing or corrupt one reads as
//! empty, and the next sweep rebuilds it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::{Issue, IssuePr, PrCi, PrState, Result};

use crate::IssueFile;

const FILE: &str = "pr-status.json";
const LOCK_FILE: &str = "pr-status.lock";

/// One pull request as last seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrEntry {
    pub number: u64,
    /// The PR's head branch. Empty when noted by number alone.
    #[serde(default)]
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PrState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<PrCi>,
    /// The commit [`ci`](Self::ci) graded, so a new push is told from a
    /// momentary lack of verdict on the same one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

impl PrEntry {
    pub fn new(number: u64) -> PrEntry {
        PrEntry {
            number,
            head: String::new(),
            url: None,
            state: None,
            ci: None,
            ci_head: None,
            created_at: None,
        }
    }
}

/// `pr-status.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStatusCache {
    #[serde(default)]
    pub prs: BTreeMap<u64, PrEntry>,
    /// Whether a sweep has read the long history listing yet; later sweeps
    /// only need the most recent PRs.
    #[serde(default)]
    pub history_loaded: bool,
}

impl PrStatusCache {
    /// Task `task`'s PR with what's known about it: its record with the state
    /// and CI filled in, or for a task with no record, the PR found from its
    /// branch. `None` when it has neither.
    ///
    /// A PR found from a branch must be open or merged (a closed one was
    /// abandoned, and says nothing about this task) and no older than the task:
    /// a branch name can be reused, and an old PR on it isn't this task's.
    /// Open wins, then the newest.
    pub fn resolve(&self, task: &Issue) -> Option<IssuePr> {
        let Some(record) = &task.pr else {
            return self.found_for_branch(task).map(entry_pr);
        };
        let Some(entry) = self.prs.get(&record.number) else {
            return Some(record.clone());
        };
        Some(IssuePr {
            number: record.number,
            url: record.url.clone().or_else(|| entry.url.clone()),
            state: entry.state.or(record.state),
            ci: entry.ci.or(record.ci),
        })
    }

    fn found_for_branch(&self, task: &Issue) -> Option<&PrEntry> {
        let branch = task.branch.as_deref().filter(|b| !b.is_empty())?;
        self.prs
            .values()
            .filter(|e| e.head == branch)
            .filter(|e| e.url.is_some())
            .filter(|e| matches!(e.state, Some(PrState::Open | PrState::Merged)))
            .filter(|e| e.created_at.is_none_or(|at| at >= task.created_at))
            .max_by_key(|e| (e.state == Some(PrState::Open), e.number))
    }

    /// Fold a fresh listing in. A listed PR's own fields win; the CI noted for
    /// it stays, since a listing says nothing about checks.
    pub fn merge_listing(&mut self, listing: Vec<PrEntry>) {
        for mut row in listing {
            if let Some(known) = self.prs.get(&row.number) {
                row.ci = known.ci;
                row.ci_head = known.ci_head.clone();
                row.url = row.url.or_else(|| known.url.clone());
                row.created_at = row.created_at.or(known.created_at);
                if row.head.is_empty() {
                    row.head = known.head.clone();
                }
            }
            self.prs.insert(row.number, row);
        }
    }

    fn entry(&mut self, number: u64) -> &mut PrEntry {
        self.prs.entry(number).or_insert_with(|| PrEntry::new(number))
    }
}

fn entry_pr(entry: &PrEntry) -> IssuePr {
    IssuePr {
        number: entry.number,
        url: entry.url.clone(),
        state: entry.state,
        ci: entry.ci,
    }
}

fn path(project: &str) -> Result<PathBuf> {
    Ok(crate::project_dir(project)?.join(FILE))
}

/// The cache for `project`; empty when it is missing or unreadable.
pub fn load(project: &str) -> PrStatusCache {
    path(project)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Read-modify-write under the cache's own lock (several processes note PRs:
/// the daemon's poller, a CLI action). Writes only when `f` changed something,
/// since a sweep notes the same state every tick.
pub fn update(project: &str, f: impl FnOnce(&mut PrStatusCache)) -> Result<()> {
    let dir = crate::project_dir(project)?;
    crate::ensure_dir(&dir)?;
    let _lock = crate::acquire_file_lock(&dir.join(LOCK_FILE))?;
    let before = load(project);
    let mut after = before.clone();
    f(&mut after);
    if after == before {
        return Ok(());
    }
    let bytes = serde_json::to_vec(&after)
        .map_err(|e| shelbi_core::Error::Other(format!("serialize {FILE}: {e}")))?;
    crate::atomic_write(&path(project)?, &bytes)
}

/// Note a PR's state, and its URL and head branch when the caller knows them.
pub fn note_state(
    project: &str,
    number: u64,
    head: Option<&str>,
    url: Option<&str>,
    state: PrState,
) -> Result<()> {
    update(project, |cache| {
        let entry = cache.entry(number);
        entry.state = Some(state);
        if let Some(head) = head {
            entry.head = head.to_string();
        }
        if let Some(url) = url {
            entry.url = Some(url.to_string());
        }
    })
}

/// Note the CI verdict for commit `head_sha` of an open PR: a verdict replaces
/// what was known; no verdict (`None`, still running) shows as pending only
/// once the head has moved, so a settled result doesn't flicker while the same
/// commit is regraded.
pub fn note_ci(
    project: &str,
    number: u64,
    head: &str,
    head_sha: &str,
    verdict: Option<PrCi>,
) -> Result<()> {
    update(project, |cache| {
        let entry = cache.entry(number);
        entry.state = Some(PrState::Open);
        if !head.is_empty() {
            entry.head = head.to_string();
        }
        let same_head = entry.ci_head.as_deref() == Some(head_sha);
        match verdict {
            Some(ci) => entry.ci = Some(ci),
            None if same_head && entry.ci.is_some() => {}
            None => entry.ci = Some(PrCi::Pending),
        }
        entry.ci_head = Some(head_sha.to_string());
    })
}

/// Forget the CI known for a PR: its next head hasn't been graded.
pub fn clear_ci(project: &str, number: u64) -> Result<()> {
    update(project, |cache| {
        if let Some(entry) = cache.prs.get_mut(&number) {
            entry.ci = None;
            entry.ci_head = None;
        }
    })
}

/// Fill in `board`'s PRs from the cache ([`PrStatusCache::resolve`]). What
/// comes out is for showing: the store's copy of each task is untouched.
pub fn fold(project: &str, board: &mut [IssueFile]) {
    let cache = load(project);
    fold_with(&cache, board);
}

/// [`fold`] with the cache in hand.
pub fn fold_with(cache: &PrStatusCache, board: &mut [IssueFile]) {
    for tf in board {
        tf.task.pr = cache.resolve(&tf.task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use shelbi_core::Column;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, 0, 0, 0).unwrap()
    }

    fn task(branch: Option<&str>, pr: Option<IssuePr>) -> Issue {
        Issue {
            id: "t".into(),
            title: "t".into(),
            column: Column::done(),
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: branch.map(str::to_string),
            pr,
            depends_on: vec![],
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: at(10),
            updated_at: at(10),
            params: Default::default(),
        }
    }

    fn row(number: u64, head: &str, state: PrState, created: u32) -> PrEntry {
        PrEntry {
            head: head.into(),
            url: Some(format!("https://github.com/o/r/pull/{number}")),
            state: Some(state),
            created_at: Some(at(created)),
            ..PrEntry::new(number)
        }
    }

    fn cache(rows: Vec<PrEntry>) -> PrStatusCache {
        let mut c = PrStatusCache::default();
        c.merge_listing(rows);
        c
    }

    #[test]
    fn a_record_gets_its_state_and_ci_from_the_cache() {
        let mut c = cache(vec![row(7, "b", PrState::Merged, 11)]);
        c.entry(7).ci = Some(PrCi::Passing);
        let pr = c
            .resolve(&task(Some("b"), Some(IssuePr::new(7, Some("https://x/pull/7".into())))))
            .unwrap();
        assert_eq!(pr.url.as_deref(), Some("https://x/pull/7"));
        assert_eq!(pr.state, Some(PrState::Merged));
        assert_eq!(pr.ci, Some(PrCi::Passing));
    }

    #[test]
    fn a_record_the_cache_has_not_seen_shows_as_recorded() {
        let record = IssuePr::new(7, Some("https://x/pull/7".into()));
        let pr = PrStatusCache::default().resolve(&task(None, Some(record.clone())));
        assert_eq!(pr, Some(record));
    }

    #[test]
    fn a_hand_written_number_gets_its_url_from_the_cache() {
        let c = cache(vec![row(7, "b", PrState::Open, 11)]);
        let pr = c.resolve(&task(None, Some(IssuePr::new(7, None)))).unwrap();
        assert_eq!(pr.url.as_deref(), Some("https://github.com/o/r/pull/7"));
    }

    #[test]
    fn backfill_finds_an_open_or_merged_pr_from_the_branch() {
        let c = cache(vec![row(7, "b", PrState::Merged, 11)]);
        let pr = c.resolve(&task(Some("b"), None)).unwrap();
        assert_eq!(pr.number, 7);
        assert_eq!(pr.state, Some(PrState::Merged));
        assert_eq!(c.resolve(&task(Some("other"), None)), None);
        assert_eq!(c.resolve(&task(None, None)), None);
    }

    #[test]
    fn backfill_prefers_open_then_the_newest() {
        let c = cache(vec![
            row(5, "b", PrState::Merged, 11),
            row(6, "b", PrState::Merged, 12),
            row(4, "b", PrState::Open, 11),
        ]);
        assert_eq!(c.resolve(&task(Some("b"), None)).unwrap().number, 4);
        let c = cache(vec![row(5, "b", PrState::Merged, 11), row(6, "b", PrState::Merged, 12)]);
        assert_eq!(c.resolve(&task(Some("b"), None)).unwrap().number, 6);
    }

    #[test]
    fn backfill_ignores_a_closed_pr_and_one_older_than_the_task() {
        let c = cache(vec![row(5, "b", PrState::Closed, 11)]);
        assert_eq!(c.resolve(&task(Some("b"), None)), None);
        // The branch name was used by an earlier task.
        let c = cache(vec![row(5, "b", PrState::Merged, 2)]);
        assert_eq!(c.resolve(&task(Some("b"), None)), None);
    }

    #[test]
    fn a_closed_or_merged_pr_keeps_its_record() {
        let c = cache(vec![row(7, "b", PrState::Closed, 11)]);
        let pr = c.resolve(&task(Some("b"), Some(IssuePr::new(7, None)))).unwrap();
        assert_eq!((pr.number, pr.state), (7, Some(PrState::Closed)));
    }

    #[test]
    fn a_listing_keeps_the_ci_noted_for_a_pr() {
        let mut c = cache(vec![row(7, "b", PrState::Open, 11)]);
        c.entry(7).ci = Some(PrCi::Failing);
        c.merge_listing(vec![row(7, "b", PrState::Merged, 11)]);
        assert_eq!(c.prs[&7].state, Some(PrState::Merged));
        assert_eq!(c.prs[&7].ci, Some(PrCi::Failing));
    }

    #[test]
    fn folding_replaces_each_card_pr() {
        let c = cache(vec![row(7, "b", PrState::Open, 11)]);
        let mut board = vec![IssueFile {
            task: task(Some("b"), None),
            body: String::new(),
            tracker_assignees: vec![],
        }];
        fold_with(&c, &mut board);
        assert_eq!(board[0].task.pr.as_ref().map(|p| p.number), Some(7));
    }

    mod on_disk {
        use super::*;
        use crate::test_lock::LOCK;

        fn with_home(f: impl FnOnce()) {
            let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-pr-status-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&home);
            std::env::set_var("SHELBI_HOME", &home);
            f();
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&home);
        }

        #[test]
        fn notes_round_trip_and_a_missing_file_reads_empty() {
            with_home(|| {
                assert_eq!(load("p"), PrStatusCache::default());
                note_state("p", 7, Some("b"), Some("https://x/pull/7"), PrState::Open).unwrap();
                note_ci("p", 7, "b", "sha1", Some(PrCi::Passing)).unwrap();
                let entry = load("p").prs[&7].clone();
                assert_eq!(entry.head, "b");
                assert_eq!(entry.state, Some(PrState::Open));
                assert_eq!(entry.ci, Some(PrCi::Passing));
            });
        }

        #[test]
        fn no_verdict_keeps_the_result_for_the_same_head_and_clears_it_for_a_new_one() {
            with_home(|| {
                note_ci("p", 7, "b", "sha1", Some(PrCi::Passing)).unwrap();
                note_ci("p", 7, "b", "sha1", None).unwrap();
                assert_eq!(load("p").prs[&7].ci, Some(PrCi::Passing));
                note_ci("p", 7, "b", "sha2", None).unwrap();
                assert_eq!(load("p").prs[&7].ci, Some(PrCi::Pending));
            });
        }

        #[test]
        fn a_corrupt_file_reads_empty_and_the_next_note_replaces_it() {
            with_home(|| {
                crate::ensure_dir(&crate::project_dir("p").unwrap()).unwrap();
                std::fs::write(path("p").unwrap(), "{not json").unwrap();
                assert_eq!(load("p"), PrStatusCache::default());
                note_state("p", 7, None, None, PrState::Merged).unwrap();
                assert_eq!(load("p").prs[&7].state, Some(PrState::Merged));
            });
        }
    }
}
