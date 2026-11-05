//! A task's pull request: recording it when it opens, and keeping the hub's
//! PR status cache ([`shelbi_state::pr_status`]) current.
//!
//! Everything here is best-effort. The PR itself is what the workflow asked
//! for, so a failed note is logged and never fails the action around it; the
//! next sweep or `open_pr` repairs it.

use shelbi_core::{Error, Host, Issue, IssuePr, PrState, Project};
use shelbi_state::issue_store::IssueFields;
use shelbi_state::pr_status;

use crate::git::{list_recent_prs, locate_hub_workdir, lookup_pr_url};

/// How far back the first sweep on a hub reads: the whole of any done column a
/// user is likely to page through. Later sweeps need only the latest PRs.
const HISTORY_LISTING: u32 = 1000;
const RECENT_LISTING: u32 = 100;

/// Where a PR's URL can be asked for: the repo at `wt` on `host`, in exactly
/// `repository` when the caller resolved it.
pub(crate) struct PrRepo<'a> {
    pub host: &'a Host,
    pub wt: &'a str,
    pub repository: Option<&'a str>,
}

/// Record that PR `number` is `task`'s: the durable number and URL on the task
/// (a new number replaces the old), and its state in the cache. `url` is looked
/// up when the caller doesn't have it. Used when a PR is found for a task in
/// review, where the head hasn't moved.
pub(crate) fn record_found(
    project: &Project,
    repo: &PrRepo<'_>,
    task: &Issue,
    branch: &str,
    number: u64,
    url: Option<&str>,
) {
    let url = url.map(str::to_string).or_else(|| {
        lookup_pr_url(repo.host, repo.wt, repo.repository, number)
            .map_err(|e| tracing::warn!(task = %task.id, pr = number, error = %e, "PR URL lookup failed"))
            .ok()
    });
    record_durable(project, task, IssuePr::new(number, url.clone()));
    if let Err(e) = pr_status::note_state(&project.name, number, Some(branch), url.as_deref(), PrState::Open) {
        tracing::warn!(task = %task.id, pr = number, error = %e, "PR status note failed");
    }
}

/// [`record_found`] for `open_pr` and `zen pr-create`, which opened the PR or
/// moved its head: what CI said of the previous head is forgotten.
pub(crate) fn record_opened(
    project: &Project,
    repo: &PrRepo<'_>,
    task: &Issue,
    branch: &str,
    number: u64,
    url: Option<&str>,
) {
    record_found(project, repo, task, branch, number, url);
    if let Err(e) = pr_status::clear_ci(&project.name, number) {
        tracing::warn!(task = %task.id, pr = number, error = %e, "PR status note failed");
    }
}

fn record_durable(project: &Project, task: &Issue, record: IssuePr) {
    // A record with a URL isn't downgraded by a failed URL lookup.
    let current = task.pr.as_ref();
    if current.is_some_and(|c| c.number == record.number && (c.url == record.url || record.url.is_none())) {
        return;
    }
    let number = record.number;
    let written = shelbi_state::issue_store_for_project(project).and_then(|store| {
        store.set_fields(
            &task.id,
            IssueFields {
                pr: Some(Some(record)),
                ..IssueFields::default()
            },
        )
    });
    if let Err(e) = written {
        tracing::warn!(task = %task.id, pr = number, error = %e, "recording the task's PR failed");
    }
}

/// Note that PR `number` merged or closed. The task's record stays.
pub(crate) fn note_finished(project: &Project, number: u64, head: Option<&str>, state: PrState) {
    if let Err(e) = pr_status::note_state(&project.name, number, head, None, state) {
        tracing::warn!(pr = number, error = %e, "PR status note failed");
    }
}

/// One sweep of the repo's PR listing into the cache: how out-of-band merges
/// and closes reach the task sheet, and how a task that predates PR recording
/// gets its PR (the cache names each PR's head branch; see
/// [`shelbi_state::pr_status::PrStatusCache::resolve`]). A project with no
/// GitHub remote just has nothing to list.
pub(crate) fn refresh(project: &Project) {
    let limit = if pr_status::load(&project.name).history_loaded {
        RECENT_LISTING
    } else {
        HISTORY_LISTING
    };
    let (host, dir) = match locate_hub_workdir(project) {
        Ok(found) => found,
        Err(e) => {
            tracing::debug!(project = %project.name, error = %e, "PR status sweep skipped");
            return;
        }
    };
    let listing = list_recent_prs(&host, &dir.to_string_lossy(), limit);
    apply_listing(&project.name, listing);
}

fn apply_listing(project: &str, listing: shelbi_core::Result<Vec<pr_status::PrEntry>>) {
    let rows = match listing {
        Ok(rows) => rows,
        Err(Error::Command { ref stderr, .. }) if stderr.contains("known GitHub host") => return,
        Err(e) => {
            tracing::debug!(project = %project, error = %e, "PR status sweep failed");
            return;
        }
    };
    let merged = pr_status::update(project, |cache| {
        cache.merge_listing(rows);
        cache.history_loaded = true;
    });
    if let Err(e) = merged {
        tracing::warn!(project = %project, error = %e, "PR status sweep could not write");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transition::fixture_project as bare_project;
    use shelbi_core::Column;
    use shelbi_state::issue_store::{IssueStore, NewIssue};
    use shelbi_state::FileSystemStore;

    const NOWHERE: PrRepo<'static> = PrRepo {
        host: &Host::Local,
        wt: "/nowhere",
        repository: None,
    };

    fn sandbox(f: impl FnOnce(&Project)) {
        let _guard = crate::test_lock::acquire();
        let home = std::env::temp_dir().join(format!("shelbi-pr-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("SHELBI_HOME", &home);
        let project = bare_project();
        std::fs::create_dir_all(shelbi_state::project_dir(&project.name).unwrap()).unwrap();
        f(&project);
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn add_task(project: &Project) -> Issue {
        let store = FileSystemStore::new(&project.name);
        store.add(NewIssue::new("t", "t", Column::review(), "body\n")).unwrap();
        store.get("t").unwrap().unwrap().task
    }

    #[test]
    fn opening_records_the_number_and_url_on_the_task_and_the_state_in_the_cache() {
        sandbox(|project| {
            let task = add_task(project);
            let url = "https://github.com/o/r/pull/7";
            record_opened(project, &NOWHERE, &task, "b", 7, Some(url));

            let stored = FileSystemStore::new(&project.name).get("t").unwrap().unwrap().task;
            // Only the durable half is on the task.
            assert_eq!(stored.pr, Some(IssuePr::new(7, Some(url.to_string()))));
            let cache = pr_status::load(&project.name);
            assert_eq!(cache.prs[&7].state, Some(PrState::Open));
            assert_eq!(cache.prs[&7].head, "b");
            // A read shows both.
            let shown = cache.resolve(&stored).unwrap();
            assert_eq!(shown.state, Some(PrState::Open));
            assert_eq!(shown.url.as_deref(), Some(url));
        });
    }

    #[test]
    fn a_new_pr_replaces_the_record_and_forgets_the_old_heads_ci() {
        sandbox(|project| {
            let task = add_task(project);
            record_opened(project, &NOWHERE, &task, "b", 7, Some("https://h/pull/7"));
            let task = FileSystemStore::new(&project.name).get("t").unwrap().unwrap().task;
            pr_status::note_ci(&project.name, 7, "b", "sha1", Some(shelbi_core::PrCi::Failing)).unwrap();

            // The same PR realigned: the record stands, CI starts over.
            record_opened(project, &NOWHERE, &task, "b", 7, Some("https://h/pull/7"));
            assert_eq!(pr_status::load(&project.name).prs[&7].ci, None);

            record_opened(project, &NOWHERE, &task, "b", 9, Some("https://h/pull/9"));
            let stored = FileSystemStore::new(&project.name).get("t").unwrap().unwrap().task;
            assert_eq!(stored.pr.map(|p| p.number), Some(9));
        });
    }

    #[test]
    fn finding_a_pr_in_review_keeps_what_ci_said() {
        sandbox(|project| {
            let task = add_task(project);
            pr_status::note_ci(&project.name, 7, "b", "sha1", Some(shelbi_core::PrCi::Passing)).unwrap();
            record_found(project, &NOWHERE, &task, "b", 7, Some("https://h/pull/7"));
            assert_eq!(pr_status::load(&project.name).prs[&7].ci, Some(shelbi_core::PrCi::Passing));
            let stored = FileSystemStore::new(&project.name).get("t").unwrap().unwrap().task;
            assert_eq!(stored.pr.map(|p| p.number), Some(7));
        });
    }

    #[test]
    fn a_failed_url_lookup_keeps_a_record_that_has_one() {
        sandbox(|project| {
            let mut task = add_task(project);
            task.pr = Some(IssuePr::new(7, Some("https://h/pull/7".into())));
            FileSystemStore::new(&project.name)
                .set_fields("t", IssueFields { pr: Some(task.pr.clone()), ..IssueFields::default() })
                .unwrap();
            record_durable(project, &task, IssuePr::new(7, None));
            let stored = FileSystemStore::new(&project.name).get("t").unwrap().unwrap().task;
            assert_eq!(stored.pr.and_then(|p| p.url).as_deref(), Some("https://h/pull/7"));
        });
    }

    #[test]
    fn merging_or_closing_keeps_the_record_and_notes_the_state() {
        sandbox(|project| {
            let task = add_task(project);
            record_opened(project, &NOWHERE, &task, "b", 7, Some("https://h/pull/7"));
            note_finished(project, 7, Some("b"), PrState::Merged);
            let stored = FileSystemStore::new(&project.name).get("t").unwrap().unwrap().task;
            let shown = pr_status::load(&project.name).resolve(&stored).unwrap();
            assert_eq!((shown.number, shown.state), (7, Some(PrState::Merged)));
        });
    }

    #[test]
    fn a_sweep_backfills_a_task_with_no_record_from_its_branch() {
        sandbox(|project| {
            let mut task = add_task(project);
            task.branch = Some("jlong/t".into());
            let row = pr_status::PrEntry {
                head: "jlong/t".into(),
                url: Some("https://h/pull/2137".into()),
                state: Some(PrState::Merged),
                created_at: Some(task.created_at + chrono::Duration::hours(1)),
                ..pr_status::PrEntry::new(2137)
            };
            apply_listing(&project.name, Ok(vec![row]));
            let cache = pr_status::load(&project.name);
            assert!(cache.history_loaded);
            let shown = cache.resolve(&task).unwrap();
            assert_eq!((shown.number, shown.state), (2137, Some(PrState::Merged)));
            // Backfill is a read: the stored task isn't rewritten.
            assert_eq!(FileSystemStore::new(&project.name).get("t").unwrap().unwrap().task.pr, None);
        });
    }

    #[test]
    fn a_project_with_no_github_remote_has_nothing_to_sweep() {
        sandbox(|project| {
            apply_listing(
                &project.name,
                Err(Error::Command {
                    cmd: "gh pr list".into(),
                    status: "1".into(),
                    stderr: "none of the git remotes configured for this repository point to a known GitHub host".into(),
                }),
            );
            assert!(!pr_status::load(&project.name).history_loaded);
        });
    }
}
