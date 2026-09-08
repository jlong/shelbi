//! Backend-to-backend issue migration: copy a project's whole board from one
//! [`IssueStore`] to another (today `file_system` ⇄ `github`), matched on the
//! stable shelbi id so a re-run never duplicates an already-migrated card.
//!
//! Both live backends key every issue by the same durable id — the on-disk
//! task filename for `file_system`, the `shelbi:id/<slug>` label for `github` —
//! so "has this card already been migrated?" is just "does the target already
//! hold an issue with this id?". [`plan_issue_migration`] answers that for the
//! whole board with a single `list()` on each side, partitioning the source
//! into the cards to create and the ids to skip. [`apply_issue_migration`]
//! then `add`s the to-create set into the target.
//!
//! The plan is ordered so an issue's in-set dependencies are always created
//! before it — the `file_system` backend rejects an `add` whose `depends_on`
//! names an id not yet on the board, so a naive column-order pass would fail
//! the moment a card depends on one that sorts after it. Dependencies that
//! already live in the target (skipped, or migrated on a prior run) are
//! satisfied the instant we start.
//!
//! Idempotency comes entirely from the id match: re-running against a target
//! that already holds every card produces an empty plan, and apply is a no-op.
//! `dry-run` is simply "plan, then don't apply" — the command layer prints the
//! plan and stops.
//!
//! Scope: this carries the durable issue definition (title, body, status,
//! priority, workflow, branch, deps, machine hint, zen/launch overrides, and
//! free-form params) — everything [`NewIssue`] can express. Comments are *not*
//! migrated: they carry no per-comment anchor, so a re-run could not tell an
//! already-copied comment from a new one without a second bookkeeping channel,
//! which would break the "re-run is a clean no-op" guarantee. Ephemeral local
//! routing (`assigned_to`) is intentionally dropped, exactly as it is on any
//! other cross-backend write.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use shelbi_core::{Error, Issue, Result};

use crate::{IssueFile, IssueStore, NewIssue};

/// The computed effect of migrating from one store to another: which source
/// cards will be created in the target, and which are already there.
#[derive(Debug, Clone, Default)]
pub struct IssueMigrationPlan {
    /// Source issues absent from the target, in an order where every in-set
    /// dependency precedes the card that needs it. These are `add`ed on apply.
    pub to_migrate: Vec<IssueFile>,
    /// Source issue ids already present in the target (matched on the shelbi id
    /// anchor), left untouched. A non-empty list on a fresh migration means the
    /// target was partially populated; on a re-run it is the whole board.
    pub already_present: Vec<String>,
}

impl IssueMigrationPlan {
    /// True when there is nothing to create — the target already holds every
    /// source card. Apply is a no-op in this state.
    pub fn is_empty(&self) -> bool {
        self.to_migrate.is_empty()
    }
}

/// Compute (without writing anything) what migrating every issue from `from`
/// into `to` would do. Reads the full board on each side once and matches on
/// the durable shelbi id: a source card whose id the target already holds is
/// skipped, everything else is queued for creation in dependency-safe order.
///
/// This is the read half the `--dry-run` path stops at; the write half is
/// [`apply_issue_migration`].
pub fn plan_issue_migration(
    from: &dyn IssueStore,
    to: &dyn IssueStore,
) -> Result<IssueMigrationPlan> {
    let source = from.list()?;
    let present: HashSet<String> = to.list()?.into_iter().map(|tf| tf.task.id).collect();

    let mut to_migrate = Vec::new();
    let mut already_present = Vec::new();
    for tf in source {
        if present.contains(&tf.task.id) {
            already_present.push(tf.task.id);
        } else {
            to_migrate.push(tf);
        }
    }
    order_by_dependencies(&mut to_migrate);
    Ok(IssueMigrationPlan {
        to_migrate,
        already_present,
    })
}

/// A mid-run abort: the issues that *were* created (durable on the target)
/// before an `add` failed, plus the error that stopped the run. Returned as the
/// `Err` half of [`apply_issue_migration`] so the caller can report the partial
/// progress — the created cards, the error, and that a re-run resumes from the
/// failure — instead of discarding everything the loop had already achieved.
///
/// The resume property is real: [`plan_issue_migration`] matches on id, so a
/// corrected re-run skips every card in `created` and creates only the rest.
#[derive(Debug)]
pub struct PartialMigration {
    /// Issues created before the failure, in creation order (already reported
    /// through [`MigrationControls::on_created`] as they landed).
    pub created: Vec<Issue>,
    /// The error that aborted the run.
    pub error: Error,
}

/// Per-run controls for [`apply_issue_migration`]: how to report progress as it
/// happens, and how hard to pace the writes.
///
/// Progress is reported *from inside the loop* (not from the returned vec after
/// the fact), so a multi-minute bulk run streams a line per creation instead of
/// going silent. Pacing throttles the writes to stay under GitHub's secondary
/// content-creation limit (no more than 80/min and 500/hr) rather than
/// sprinting into it and relying on backoff to recover.
pub struct MigrationControls<'a> {
    /// Invoked as each issue lands, with `(index, total, issue)` where `index`
    /// is 1-based — the hook the CLI turns into a `[137/588] ✓ migrated <id>`
    /// line with a running count.
    pub on_created: &'a mut dyn FnMut(usize, usize, &Issue),
    /// Minimum spacing between successive `add` calls. Zero disables pacing (the
    /// right choice for the filesystem target, which has no rate limit). The
    /// delay lands *before* each create after the first, so the run's cadence
    /// stays under the secondary limit.
    pub pace: Duration,
    /// Sleep primitive, injected so tests pace without waiting in real time.
    /// Production passes [`std::thread::sleep`].
    pub sleep: &'a dyn Fn(Duration),
}

impl<'a> MigrationControls<'a> {
    /// Controls that report nothing and pace at zero — the whole-run no-op used
    /// by callers (and tests) that only want the created issues back.
    pub fn silent(on_created: &'a mut dyn FnMut(usize, usize, &Issue)) -> Self {
        Self {
            on_created,
            pace: Duration::ZERO,
            sleep: &|_| {},
        }
    }
}

/// Execute `plan` against the target store, creating each queued issue in order
/// and reporting each creation through `controls` as it lands. Returns the
/// created issues on success.
///
/// On the first `add` error the run aborts, but the cards created before it stay
/// put — so the error is returned as [`PartialMigration`] carrying *both* the
/// created issues and the failure, never discarding the progress. Because
/// migration matches on id, a corrected re-run resumes exactly where it left
/// off (see [`plan_issue_migration`]); the caller uses the partial to tell the
/// operator that.
pub fn apply_issue_migration(
    to: &dyn IssueStore,
    plan: &IssueMigrationPlan,
    controls: &mut MigrationControls<'_>,
) -> std::result::Result<Vec<Issue>, PartialMigration> {
    let total = plan.to_migrate.len();
    let mut created = Vec::with_capacity(total);
    for (i, tf) in plan.to_migrate.iter().enumerate() {
        // Pace before every create after the first, so a bulk run stays under
        // the secondary content-creation limit instead of provoking it.
        if i > 0 && !controls.pace.is_zero() {
            (controls.sleep)(controls.pace);
        }
        match to.add(new_issue_from(tf)) {
            Ok(issue) => {
                (controls.on_created)(i + 1, total, &issue);
                created.push(issue);
            }
            Err(error) => return Err(PartialMigration { created, error }),
        }
    }
    Ok(created)
}

/// Build the creation spec for a source card, carrying every durable field the
/// target backend can persist. `priority` is pinned to the source position so
/// the target column keeps the same order rather than re-appending; ephemeral
/// `assigned_to` is dropped (never a cross-backend field).
fn new_issue_from(tf: &IssueFile) -> NewIssue {
    NewIssue {
        id: tf.task.id.clone(),
        title: tf.task.title.clone(),
        column: tf.task.column.clone(),
        body: tf.body.clone(),
        workflow: tf.task.workflow.clone(),
        branch: tf.task.branch.clone(),
        depends_on: tf.task.depends_on.clone(),
        prefers_machine: tf.task.prefers_machine.clone(),
        zen: tf.task.zen.clone(),
        launch: tf.task.launch.clone(),
        params: tf.task.params.clone(),
        priority: Some(tf.task.priority),
    }
}

/// Stable topological sort so every card is preceded by the cards it depends
/// on *that are also in this set*. Deps pointing outside the set (already in
/// the target) impose no constraint — they exist before the first create.
/// Ties are broken by the incoming (source) order, so within a dependency
/// level the board's column-then-priority order is preserved. The source board
/// is acyclic (enforced at creation), but a leftover-append guards against a
/// pathological cycle rather than looping forever.
fn order_by_dependencies(items: &mut Vec<IssueFile>) {
    let n = items.len();
    if n < 2 {
        return;
    }
    let index_of: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(i, tf)| (tf.task.id.as_str(), i))
        .collect();

    // In-set prerequisite indices for each node, and the count still unmet.
    let mut prereqs: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut unmet = vec![0usize; n];
    for (i, tf) in items.iter().enumerate() {
        for d in &tf.task.depends_on {
            if let Some(&j) = index_of.get(d.as_str()) {
                if j != i {
                    prereqs[i].push(j);
                    unmet[i] += 1;
                }
            }
        }
    }

    let mut emitted = vec![false; n];
    let mut order = Vec::with_capacity(n);
    // Kahn's algorithm, always taking the lowest source index among ready
    // nodes so the original order is the tie-break. O(n^2) — boards are small.
    for _ in 0..n {
        let Some(next) = (0..n).find(|&i| !emitted[i] && unmet[i] == 0) else {
            break; // cycle: fall through to the leftover-append below
        };
        emitted[next] = true;
        order.push(next);
        for (k, pre) in prereqs.iter().enumerate() {
            if !emitted[k] && pre.contains(&next) {
                unmet[k] -= 1;
            }
        }
    }
    // Defensive: append anything a cycle stranded, preserving source order.
    for (i, done) in emitted.iter().enumerate() {
        if !done {
            order.push(i);
        }
    }

    let mut slots: Vec<Option<IssueFile>> = items.drain(..).map(Some).collect();
    *items = order.into_iter().map(|i| slots[i].take().unwrap()).collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use crate::FileSystemStore;
    use shelbi_core::Column;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-issue-migrate-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn spec(id: &str, column: Column) -> NewIssue {
        NewIssue::new(id, id.replace('-', " "), column, format!("# {id}\n\nbody\n"))
    }

    fn ids(plan: &IssueMigrationPlan) -> Vec<String> {
        plan.to_migrate.iter().map(|tf| tf.task.id.clone()).collect()
    }

    /// Run `apply` with a no-op progress hook and no pacing — the shape most
    /// tests want (they only care about the created issues / the error).
    fn run_apply(
        to: &dyn IssueStore,
        plan: &IssueMigrationPlan,
    ) -> std::result::Result<Vec<Issue>, PartialMigration> {
        let mut noop = |_: usize, _: usize, _: &Issue| {};
        let mut controls = MigrationControls::silent(&mut noop);
        apply_issue_migration(to, plan, &mut controls)
    }

    #[test]
    fn migrates_every_card_then_reruns_as_noop() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let from = FileSystemStore::new("src");
        let to = FileSystemStore::new("dst");
        from.add(spec("a", Column::todo())).unwrap();
        from.add(spec("b", Column::in_progress())).unwrap();
        from.add(spec("c", Column::todo())).unwrap();

        // First pass creates one target card per source card, anchored by id.
        let plan = plan_issue_migration(&from, &to).unwrap();
        assert_eq!(plan.to_migrate.len(), 3);
        assert!(plan.already_present.is_empty());
        let created = run_apply(&to, &plan).unwrap();
        assert_eq!(created.len(), 3);

        let mut migrated: Vec<String> = to.list().unwrap().into_iter().map(|tf| tf.task.id).collect();
        migrated.sort();
        assert_eq!(migrated, vec!["a", "b", "c"]);
        // Status is carried across, not reset to the default column.
        assert_eq!(to.get("b").unwrap().unwrap().task.column, Column::in_progress());
        // Body survives the round-trip.
        assert_eq!(to.get("a").unwrap().unwrap().body, "# a\n\nbody\n");

        // Re-running is a clean no-op: everything already present, nothing new.
        let replan = plan_issue_migration(&from, &to).unwrap();
        assert!(replan.is_empty());
        assert_eq!(replan.already_present.len(), 3);
        assert!(run_apply(&to, &replan).unwrap().is_empty());
        assert_eq!(to.list().unwrap().len(), 3);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn partially_migrated_target_migrates_only_the_gap() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let from = FileSystemStore::new("src");
        let to = FileSystemStore::new("dst");
        from.add(spec("a", Column::todo())).unwrap();
        from.add(spec("b", Column::todo())).unwrap();
        // `a` already lives in the target (a prior interrupted run).
        to.add(spec("a", Column::todo())).unwrap();

        let plan = plan_issue_migration(&from, &to).unwrap();
        assert_eq!(ids(&plan), vec!["b"]);
        assert_eq!(plan.already_present, vec!["a"]);
        run_apply(&to, &plan).unwrap();

        let mut migrated: Vec<String> = to.list().unwrap().into_iter().map(|tf| tf.task.id).collect();
        migrated.sort();
        assert_eq!(migrated, vec!["a", "b"]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn dry_run_plan_writes_nothing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let from = FileSystemStore::new("src");
        let to = FileSystemStore::new("dst");
        from.add(spec("a", Column::todo())).unwrap();

        // Planning is read-only: the target is still empty afterward.
        let plan = plan_issue_migration(&from, &to).unwrap();
        assert_eq!(ids(&plan), vec!["a"]);
        assert!(to.list().unwrap().is_empty());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn dependencies_are_created_before_their_dependents() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let from = FileSystemStore::new("src");
        let to = FileSystemStore::new("dst");
        // `a` depends on `b`; `b` depends on `c`. Add them so the source board
        // lists `a` before its prerequisites within the same column.
        from.add(spec("c", Column::todo())).unwrap();
        from.add(spec("b", Column::todo())).unwrap();
        {
            let mut s = spec("a", Column::todo());
            s.depends_on = vec!["b".into()];
            from.add(s).unwrap();
        }
        {
            let mut s = spec("b2", Column::todo());
            s.depends_on = vec!["c".into()];
            // Move b2's dep edge onto b's frontmatter after the fact would be
            // complex; instead assert the ordering property below directly.
            from.add(s).unwrap();
        }

        let plan = plan_issue_migration(&from, &to).unwrap();
        let order = ids(&plan);
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("b") < pos("a"), "b must precede its dependent a: {order:?}");
        assert!(pos("c") < pos("b2"), "c must precede its dependent b2: {order:?}");

        // Apply must succeed — the file_system backend would reject an add whose
        // dep is not yet on the board, so a wrong order surfaces here.
        run_apply(&to, &plan).unwrap();
        assert_eq!(to.list().unwrap().len(), 4);
        assert_eq!(to.get("a").unwrap().unwrap().task.depends_on, vec!["b".to_string()]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn progress_is_reported_per_issue_with_a_running_count() {
        // Each creation must be announced as it lands, with a 1-based index and
        // the total — the streamed `[i/total]` the CLI renders.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let from = FileSystemStore::new("src");
        let to = FileSystemStore::new("dst");
        from.add(spec("a", Column::todo())).unwrap();
        from.add(spec("b", Column::todo())).unwrap();
        from.add(spec("c", Column::todo())).unwrap();

        let plan = plan_issue_migration(&from, &to).unwrap();
        let mut seen: Vec<(usize, usize, String)> = Vec::new();
        {
            let mut on_created =
                |i: usize, total: usize, issue: &Issue| seen.push((i, total, issue.id.clone()));
            let mut controls = MigrationControls::silent(&mut on_created);
            apply_issue_migration(&to, &plan, &mut controls).unwrap();
        }
        // Reported in creation order, 1-based, with the full total each time.
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].0, 1);
        assert_eq!(seen[2].0, 3);
        assert!(seen.iter().all(|(_, total, _)| *total == 3));
    }

    #[test]
    fn pacing_sleeps_between_creates_but_not_before_the_first() {
        // With pacing on, the delay lands once per create *after* the first, so a
        // 3-card run sleeps twice. The sleeper is injected, so nothing waits.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let from = FileSystemStore::new("src");
        let to = FileSystemStore::new("dst");
        from.add(spec("a", Column::todo())).unwrap();
        from.add(spec("b", Column::todo())).unwrap();
        from.add(spec("c", Column::todo())).unwrap();

        let plan = plan_issue_migration(&from, &to).unwrap();
        let sleeps = std::cell::RefCell::new(Vec::<Duration>::new());
        {
            let mut noop = |_: usize, _: usize, _: &Issue| {};
            let sleep = |d: Duration| sleeps.borrow_mut().push(d);
            let mut controls = MigrationControls {
                on_created: &mut noop,
                pace: Duration::from_secs(7),
                sleep: &sleep,
            };
            apply_issue_migration(&to, &plan, &mut controls).unwrap();
        }
        let recorded = sleeps.into_inner();
        assert_eq!(recorded.len(), 2, "3 creates → 2 inter-request waits");
        assert!(recorded.iter().all(|d| *d == Duration::from_secs(7)));
    }

    #[test]
    fn partial_failure_returns_created_progress_and_a_re_run_finishes_the_rest() {
        // A store that creates the first two cards then fails on the third. Apply
        // returns the two created (not discards them) plus the error; a corrected
        // re-run then creates only the remainder and skips the rest by id —
        // the resume contract the docstring promises.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let from = FileSystemStore::new("src");
        for id in ["a", "b", "c", "d"] {
            from.add(spec(id, Column::todo())).unwrap();
        }

        // First run: the target store fails after two successful adds.
        let failing = FailingStore::new("dst", 2);
        let plan = plan_issue_migration(&from, &failing).unwrap();
        assert_eq!(plan.to_migrate.len(), 4);
        let partial = run_apply(&failing, &plan).unwrap_err();
        assert_eq!(partial.created.len(), 2, "the two pre-failure creates are reported");

        // The two created cards are durable on the board despite the abort.
        let dst = FileSystemStore::new("dst");
        assert_eq!(dst.list().unwrap().len(), 2);

        // Re-run against the real board: only the gap is planned, the created two
        // are already present, and apply finishes the board.
        let replan = plan_issue_migration(&from, &dst).unwrap();
        assert_eq!(replan.to_migrate.len(), 2, "only the remainder is re-created");
        assert_eq!(replan.already_present.len(), 2);
        run_apply(&dst, &replan).unwrap();

        let mut migrated: Vec<String> =
            dst.list().unwrap().into_iter().map(|tf| tf.task.id).collect();
        migrated.sort();
        assert_eq!(migrated, vec!["a", "b", "c", "d"]);

        std::env::remove_var("SHELBI_HOME");
    }

    /// A target store that delegates to a real [`FileSystemStore`] but fails its
    /// `add` after `fail_after` successful creations — the seam for exercising a
    /// mid-run abort without a real GitHub outage. Only the methods `plan`/`apply`
    /// touch (`list`, `add`) are meaningful; the rest are unreachable in these
    /// tests.
    struct FailingStore {
        inner: FileSystemStore,
        fail_after: usize,
        created: std::sync::atomic::AtomicUsize,
    }

    impl FailingStore {
        fn new(project: &str, fail_after: usize) -> Self {
            Self {
                inner: FileSystemStore::new(project),
                fail_after,
                created: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl IssueStore for FailingStore {
        fn list(&self) -> Result<Vec<IssueFile>> {
            self.inner.list()
        }
        fn add(&self, spec: NewIssue) -> Result<Issue> {
            if self.created.load(std::sync::atomic::Ordering::SeqCst) >= self.fail_after {
                return Err(Error::Other("simulated GitHub outage".into()));
            }
            let issue = self.inner.add(spec)?;
            self.created.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(issue)
        }
        fn list_in_status(&self, status: &crate::Column) -> Result<Vec<IssueFile>> {
            self.inner.list_in_status(status)
        }
        fn get(&self, id: &str) -> Result<Option<IssueFile>> {
            self.inner.get(id)
        }
        fn move_status(
            &self,
            _id: &str,
            _to: &crate::Column,
            _reason: &str,
        ) -> Result<Option<crate::StatusMove>> {
            unimplemented!("not used by migration")
        }
        fn set_priority(&self, _id: &str, _pos: crate::PrioMove) -> Result<()> {
            unimplemented!("not used by migration")
        }
        fn set_fields(&self, _id: &str, _fields: crate::IssueFields) -> Result<()> {
            unimplemented!("not used by migration")
        }
        fn cancel(&self, _id: &str, _reason: &str) -> Result<Option<crate::StatusMove>> {
            unimplemented!("not used by migration")
        }
        fn move_status_and_unassign(
            &self,
            _id: &str,
            _to: &crate::Column,
            _reason: &str,
        ) -> Result<Option<crate::StatusMove>> {
            unimplemented!("not used by migration")
        }
        fn delete(&self, _id: &str) -> Result<()> {
            unimplemented!("not used by migration")
        }
        fn renumber(&self, _status: &crate::Column) -> Result<()> {
            unimplemented!("not used by migration")
        }
        fn park_review(&self, _id: &str) -> Result<Option<String>> {
            unimplemented!("not used by migration")
        }
        fn clear_parked(&self, _id: &str) -> Result<()> {
            unimplemented!("not used by migration")
        }
        fn reject_review(
            &self,
            _id: &str,
            _ready: &crate::Column,
            _reason: &str,
            _date: &str,
        ) -> Result<Option<crate::StatusMove>> {
            unimplemented!("not used by migration")
        }
        fn poll_changes(
            &self,
            _since: &crate::Cursor,
        ) -> Result<(Vec<crate::IssueChange>, crate::Cursor)> {
            unimplemented!("not used by migration")
        }
        fn list_comments(&self, _id: &str) -> Result<Vec<crate::IssueComment>> {
            unimplemented!("not used by migration")
        }
        fn add_comment(&self, _id: &str, _body: &str) -> Result<crate::IssueComment> {
            unimplemented!("not used by migration")
        }
    }
}
