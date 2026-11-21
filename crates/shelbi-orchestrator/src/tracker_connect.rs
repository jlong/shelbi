//! Connect an existing project to a tracker, switch it to another, or
//! disconnect it (back to the local file system): [`apply_tracker_connection`].
//!
//! `shelbi tracker linear connect` / `shelbi tracker disconnect` call it in
//! process; Settings sends a `ConnectTracker` app request to the project's
//! daemon, which calls it there (`crate::far`). In order it:
//!
//! 1. refuses to switch boards while the current one has issues the new one
//!    doesn't hold (they'd be left behind, unseen), pointing at `shelbi
//!    issue-store migrate`, unless [`ConnectTracker::force`]. Issues already
//!    moved there (matched by shelbi id, as the migration matches them) don't
//!    count: after [`move_issues`] the switch needs no force;
//! 2. runs the new tracker's preflight (`gh` for GitHub,
//!    [`crate::project_create::linear::linear_preflight`] for Linear), so a
//!    connection that can't work is never written. For Linear that includes
//!    planning the status sync onto the team ([`ConnectEnv::plan_statuses`]):
//!    a workflow status no team status can take refuses the connection, since
//!    the project wouldn't load after it;
//! 3. writes the `issue_tracker:` block whole
//!    ([`shelbi_state::tracker_config::write_issue_tracker`]), so the
//!    project keeps exactly one external tracker block (none for the file
//!    system);
//! 4. for Linear, caches the resolved filter ids
//!    (`projects/<name>/linear.json`) and syncs the team's workflow states
//!    into the status catalog through [`ConnectEnv::sync_statuses`]
//!    ([`sync_linear_statuses`]), mapping the project's workflows onto the
//!    synced statuses by category so the project loads right after.
//!
//! Every IO step goes through [`ConnectEnv`], so tests run the whole
//! operation with canned answers.

use shelbi_app::tracker_connect::{
    ConnectError, ConnectTracker, Connected, ImportPreview, MoveFailed, Moved, MovedIssue, PreviewIssue, StatusSync,
    TrackerChoice,
};
use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig, LinearConnection};
use shelbi_state::linear_client::{self, ResolvedLinearFilters};

use crate::project_create::{issues, linear};

/// The IO [`apply_tracker_connection_with`] needs.
pub trait ConnectEnv {
    /// How many issues `project`'s board on `tracker` holds (open ones, for
    /// a remote board).
    fn issue_count(&mut self, project: &str, tracker: &IssueTrackerConfig) -> Result<usize, String>;
    /// How many of `current`'s issues the `new` board already holds
    /// (matched by shelbi id), so a switch doesn't leave them behind. Asked
    /// only when `current` has issues; the rest of [`Self::issue_count`]
    /// counts as left behind, including any the migration can't read.
    fn already_moved(&mut self, project: &str, current: &IssueTrackerConfig, new: &IssueTrackerConfig) -> Result<usize, String>;
    /// The `gh` preflight for `repo`: the fix to show when it fails.
    fn github_preflight(&mut self, repo: &str) -> Result<(), String>;
    /// The Linear preflight with `project`'s key: what `conn` resolved to,
    /// or the fix to show.
    fn linear_preflight(&mut self, project: &str, conn: &LinearConnection) -> Result<ResolvedLinearFilters, String>;
    /// Plan syncing the Linear team's states into `project`'s catalog and
    /// mapping its workflows onto them, writing nothing: why the connection
    /// must be refused (a workflow status no team status can take), if so.
    fn plan_statuses(&mut self, project: &str, filters: &ResolvedLinearFilters) -> Result<(), String>;
    /// Sync the Linear team's workflow states into `project`'s status
    /// catalog, and map its workflows onto them.
    fn sync_statuses(&mut self, project: &str, filters: &ResolvedLinearFilters) -> StatusSync;
}

/// The real IO: the project's store, `gh`, Linear.
pub struct LiveEnv;

impl ConnectEnv for LiveEnv {
    fn issue_count(&mut self, project: &str, tracker: &IssueTrackerConfig) -> Result<usize, String> {
        if tracker.backend == IssueTrackerBackend::FileSystem {
            return Ok(issues::file_system_board_issue_count(project));
        }
        let store = shelbi_state::issue_store::resolve_issue_store(project, tracker).map_err(|e| e.to_string())?;
        store.list().map(|l| l.len()).map_err(|e| e.to_string())
    }

    fn already_moved(&mut self, project: &str, current: &IssueTrackerConfig, new: &IssueTrackerConfig) -> Result<usize, String> {
        let from = shelbi_state::raw_issue_store(project, current).map_err(|e| e.to_string())?;
        let to = shelbi_state::raw_issue_store(project, new).map_err(|e| e.to_string())?;
        let plan = shelbi_state::plan_issue_migration(from.as_ref(), to.as_ref()).map_err(|e| e.to_string())?;
        Ok(plan.already_present.len())
    }

    fn github_preflight(&mut self, repo: &str) -> Result<(), String> {
        issues::preflight_github(repo)
    }

    fn linear_preflight(&mut self, project: &str, conn: &LinearConnection) -> Result<ResolvedLinearFilters, String> {
        linear::preflight_linear(Some(project), conn)
    }

    fn plan_statuses(&mut self, project: &str, filters: &ResolvedLinearFilters) -> Result<(), String> {
        plan_linear_statuses(project, filters)
    }

    fn sync_statuses(&mut self, project: &str, filters: &ResolvedLinearFilters) -> StatusSync {
        sync_linear_statuses(project, filters)
    }
}

/// Whether `project`'s workflows map onto the statuses of the Linear team
/// `filters` resolved to (`shelbi_state::linear_statuses::plan_connect_to`):
/// the refusal to show when a workflow status has no team status of its
/// category. Reads the team's states; writes nothing.
pub fn plan_linear_statuses(project: &str, filters: &ResolvedLinearFilters) -> Result<(), String> {
    let plan = shelbi_state::linear_statuses::plan_connect_to(project, filters)
        .map_err(|e| format!("couldn't read the Linear team's statuses: {e}"))?;
    match plan.refusal() {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// Sync a Linear team's workflow states into `project`'s status catalog and
/// map its workflows onto them (`shelbi_state::linear_statuses::connect`,
/// run after the connection is written, so it reads the new team). A
/// failure is reported, never fatal: the connection stands and the board
/// refresh re-syncs when the team's states change.
pub fn sync_linear_statuses(project: &str, _filters: &ResolvedLinearFilters) -> StatusSync {
    match shelbi_state::linear_statuses::connect(project) {
        Ok(done) => StatusSync::Synced(status_sync_line(&done)),
        Err(e) => StatusSync::Failed(e.to_string()),
    }
}

/// Settings' tracker service for `project` on this machine: the connection,
/// the Linear pickers' reads (with the project's key) and the status sync,
/// each run in this process. A far project's goes through its daemon
/// ([`shelbi_app::settings::tracker::TrackerService::far`]).
pub fn local_service(project: &str) -> shelbi_app::settings::tracker::TrackerService {
    let (a, b, c, d) = (project.to_string(), project.to_string(), project.to_string(), project.to_string());
    shelbi_app::settings::tracker::TrackerService {
        connect: std::sync::Arc::new(move |req| apply_tracker_connection(&a, &req)),
        linear: std::sync::Arc::new(move |req| linear::answer(Some(&b), req)),
        github: std::sync::Arc::new(crate::project_create::github::answer),
        sync_statuses: std::sync::Arc::new(move || sync_statuses_now(&c)),
        preview: std::sync::Arc::new(move |choice| import_preview(&d, &choice)),
        local_issues: std::sync::Arc::new({
            let project = project.to_string();
            move || Ok(local_issues(&project))
        }),
        move_issues: std::sync::Arc::new({
            let project = project.to_string();
            move |choice| move_issues(&project, &choice)
        }),
    }
}

/// How many issues `project`'s local board holds (Connect to an Issue
/// tracker's move card).
pub fn local_issues(project: &str) -> usize {
    issues::file_system_board_issue_count(project)
}

/// A move this small runs unpaced: its handful of writes is far under any
/// tracker's limits, and the window waits on it.
const UNPACED_MOVE: usize = 10;

/// Move `project`'s current board's issues (its local board, from Connect to
/// an Issue tracker) into `choice` before the switch: what `shelbi
/// issue-store migrate --apply` does, statuses mapped onto a Linear team's
/// and comments copied. Matched by shelbi id, so a re-run skips what's there
/// and a move that stopped resumes. Nothing is switched.
pub fn move_issues(project: &str, choice: &TrackerChoice) -> Result<Moved, MoveFailed> {
    let current = shelbi_state::load_project_tracker(project)
        .map_err(|e| MoveFailed::before_start(format!("project `{project}` could not be loaded: {e}")))?;
    let new = choice.config(&current);
    new.validate().map_err(|e| MoveFailed::before_start(e.to_string()))?;
    let unreadable = |e: shelbi_core::Error| MoveFailed::before_start(format!("couldn't read the issues to move: {e}"));
    let from = shelbi_state::raw_issue_store(project, &current).map_err(unreadable)?;
    let (to, targets): (Box<dyn shelbi_state::IssueStore>, _) = match choice {
        TrackerChoice::Linear(conn) => {
            // The move is the first write of a connection: a team the
            // project's workflows can't be mapped onto (the connection
            // would be refused) is refused here, before any issue moves.
            let filters = linear::preflight_linear(Some(project), conn).map_err(MoveFailed::before_start)?;
            plan_linear_statuses(project, &filters).map_err(MoveFailed::before_start)?;
            // The team's own statuses: each card goes into the one of the
            // same id, else of the same category.
            let store = shelbi_state::LinearStore::new(project, conn.clone());
            let columns = store
                .status_columns()
                .map_err(|e| MoveFailed::before_start(format!("couldn't read the Linear team's statuses: {e}")))?;
            (Box::new(store), Some(columns))
        }
        TrackerChoice::Github { .. } => (shelbi_state::raw_issue_store(project, &new).map_err(unreadable)?, None),
        TrackerChoice::FileSystem => return Err(MoveFailed::before_start("the local board is where they are")),
    };
    move_between(project, from.as_ref(), to.as_ref(), targets.as_deref(), new.backend, &|d| std::thread::sleep(d))
}

/// [`move_issues`] between two stores: plan, then create each card with its
/// comments, reporting what made it when a write fails.
fn move_between(
    project: &str,
    from: &dyn shelbi_state::IssueStore,
    to: &dyn shelbi_state::IssueStore,
    targets: Option<&[(shelbi_core::Column, shelbi_core::StatusCategory)]>,
    target: IssueTrackerBackend,
    sleep: &dyn Fn(std::time::Duration),
) -> Result<Moved, MoveFailed> {
    let plan = shelbi_state::plan_project_migration(project, from, to, targets)
        .map_err(|e| MoveFailed::before_start(format!("couldn't read the issues to move: {e}")))?;
    let pace = if plan.to_migrate.len() <= UNPACED_MOVE {
        std::time::Duration::ZERO
    } else {
        shelbi_state::default_pace(target)
    };
    let named = |id: &str, title: &str| MovedIssue { id: id.to_string(), title: title.to_string() };
    let mut on_created = |_: usize, _: usize, _: &shelbi_core::Issue| {};
    let mut controls = shelbi_state::MigrationControls {
        on_created: &mut on_created,
        pace,
        sleep,
        comments_from: Some(from),
        comments_copied: 0,
    };
    let result = shelbi_state::apply_issue_migration(to, &plan, &mut controls);
    let comments = controls.comments_copied;
    match result {
        Ok(created) => Ok(Moved {
            moved: created.iter().map(|i| named(&i.id, &i.title)).collect(),
            already: plan.already_present.len(),
            comments,
        }),
        Err(shelbi_state::PartialMigration { created, error }) => {
            let made: std::collections::HashSet<&str> = created.iter().map(|i| i.id.as_str()).collect();
            Err(MoveFailed {
                moved: created.iter().map(|i| named(&i.id, &i.title)).collect(),
                already: plan.already_present.len(),
                left: plan
                    .to_migrate
                    .iter()
                    .filter(|tf| !made.contains(tf.task.id.as_str()))
                    .map(|tf| named(&tf.task.id, &tf.task.title))
                    .collect(),
                reason: error.to_string(),
            })
        }
    }
}

/// What a first import of `choice` would bring in for `project` (Connect to
/// an Issue tracker's Review your first import), read with this machine's
/// `gh` or the project's Linear key. Nothing is written.
pub fn import_preview(project: &str, choice: &TrackerChoice) -> Result<ImportPreview, String> {
    match choice {
        TrackerChoice::Github { repo } => crate::project_create::github::import_preview(repo),
        TrackerChoice::Linear(conn) => linear_import_preview(project, conn),
        TrackerChoice::FileSystem => Err("The local board has nothing to import.".into()),
    }
}

fn linear_import_preview(project: &str, conn: &LinearConnection) -> Result<ImportPreview, String> {
    let fail = |e: linear_client::LinearError| format!("Couldn't read the Linear issues: {e}");
    let client = linear_client::LinearClient::for_project(Some(project)).map_err(fail)?.caller("import-preview");
    let filters = linear_client::resolve_filters(&client, conn).map_err(fail)?;
    let viewer = client.viewer().map_err(fail)?;
    let counts = shelbi_state::linear_store::preview::import_counts(&client, &filters, &viewer.id).map_err(fail)?;
    Ok(ImportPreview {
        open: counts.open,
        mine: counts.mine,
        unassigned: counts.unassigned,
        closed: counts.closed,
        closed_capped: counts.closed_capped,
        first: counts.first.into_iter().map(|(key, title)| PreviewIssue { key, title }).collect(),
    })
}

/// Sync `project`'s status catalog from its Linear team now (Settings'
/// "Sync now"): the same sync a connection runs, reported the same way. A
/// project not on Linear has nothing to sync.
pub fn sync_statuses_now(project: &str) -> StatusSync {
    // Read without loading the workflows: a sync is how one naming a status
    // the team lacks gets mapped onto the team's.
    match shelbi_state::load_project_tracker(project) {
        Ok(t) if t.backend == IssueTrackerBackend::Linear => {}
        Ok(_) => return StatusSync::NotNeeded,
        Err(e) => return StatusSync::Failed(e.to_string()),
    }
    match shelbi_state::linear_statuses::connect(project) {
        Ok(done) => StatusSync::Synced(status_sync_line(&done)),
        Err(e) => StatusSync::Failed(e.to_string()),
    }
}

/// `7 statuses from the Linear team (3 changed); mapped 1 workflow onto
/// them (review -> in-review)`.
fn status_sync_line(done: &shelbi_state::linear_statuses::Connected) -> String {
    let sync = &done.sync;
    let n = sync.after.statuses.len();
    let mut line = format!("{n} status{} from the Linear team", if n == 1 { "" } else { "es" });
    if !sync.changes.is_empty() {
        line.push_str(&format!(" ({} changed)", sync.changes.len()));
    }
    if done.rewritten > 0 {
        let mut renames: Vec<String> = Vec::new();
        for r in done.rewrites.iter().filter(|r| r.changes()) {
            for (from, to) in &r.remapped {
                let rename = format!("{from} -> {to}");
                if !renames.contains(&rename) {
                    renames.push(rename);
                }
            }
        }
        line.push_str(&format!(
            "; mapped {} workflow{} onto them",
            done.rewritten,
            if done.rewritten == 1 { "" } else { "s" }
        ));
        if !renames.is_empty() {
            line.push_str(&format!(" ({})", renames.join(", ")));
        }
    }
    if !done.unmappable.is_empty() {
        let names: Vec<String> =
            done.unmappable.iter().map(|u| format!("{}: {}", u.workflow, u.status)).collect();
        line.push_str(&format!(
            "; no Linear status has the category of {} (give one that category, or edit the workflow)",
            names.join(", ")
        ));
    }
    line
}

/// [`apply_tracker_connection_with`] on the real IO.
pub fn apply_tracker_connection(project: &str, req: &ConnectTracker) -> Result<Connected, ConnectError> {
    apply_tracker_connection_with(&mut LiveEnv, project, req)
}

/// The new connection names a different board than `current`: another
/// backend, another GitHub repo, or another Linear team. (A Linear filter
/// change narrows the same team's board, so it isn't a switch.)
pub fn switches_board(current: &IssueTrackerConfig, new: &IssueTrackerConfig) -> bool {
    if current.backend != new.backend {
        return true;
    }
    match current.backend {
        IssueTrackerBackend::Github => {
            let repo = |c: &IssueTrackerConfig| c.github.as_ref().map(|g| g.repo.trim().to_ascii_lowercase());
            repo(current) != repo(new)
        }
        IssueTrackerBackend::Linear => {
            let team = |c: &IssueTrackerConfig| {
                c.linear.as_ref().and_then(|l| l.team_ref().map(str::to_ascii_lowercase))
            };
            team(current) != team(new)
        }
        IssueTrackerBackend::FileSystem | IssueTrackerBackend::Jira => false,
    }
}

/// What a board is called in messages: `GitHub (acme/app)`, `Linear (ENG)`,
/// `the local file system`.
fn board_name(cfg: &IssueTrackerConfig) -> String {
    match cfg.backend {
        IssueTrackerBackend::FileSystem => "the local file system".into(),
        IssueTrackerBackend::Github => {
            format!("GitHub ({})", cfg.github.as_ref().map(|g| g.repo.as_str()).unwrap_or("?"))
        }
        IssueTrackerBackend::Linear => {
            format!("Linear ({})", cfg.linear.as_ref().map(|l| l.team.as_str()).unwrap_or("?"))
        }
        IssueTrackerBackend::Jira => "Jira".into(),
    }
}

/// Connect `project` to `req.choice`: refuse a switch that would leave
/// issues behind (unless forced), preflight the new connection, write it,
/// and for Linear cache its filters and sync its statuses. Nothing is
/// written unless every check passes.
pub fn apply_tracker_connection_with(
    env: &mut dyn ConnectEnv,
    project: &str,
    req: &ConnectTracker,
) -> Result<Connected, ConnectError> {
    // The connection alone, not the workflows: connecting is also how a
    // project whose workflow names a status the team lacks gets mapped
    // onto the team's.
    let current = shelbi_state::load_project_tracker(project)
        .map_err(|e| ConnectError::Failed(format!("project `{project}` could not be loaded: {e}")))?;
    let new = req.config(&current);
    new.validate().map_err(|e| ConnectError::Preflight(e.to_string()))?;

    if switches_board(&current, &new) && !req.force {
        let to = new.backend;
        let args = crate::project_create::issues::migrate_target_args(&new);
        let migrate = format!(
            "Move them first:\n  shelbi issue-store migrate --to {args}           # preview\n  \
             shelbi issue-store migrate --to {args} --apply   # then migrate\n\
             or switch anyway with --force (the issues stay on {}, unseen by this project).",
            board_name(&current)
        );
        match env.issue_count(project, &current) {
            Ok(0) => {}
            // Issues already moved to the new board aren't left behind. The
            // rest are, counted from the cheap local count so a task file the
            // migration skips (malformed) still counts, and a target that
            // can't be read counts them all: the guard fails closed.
            Ok(n) => match n.saturating_sub(env.already_moved(project, &current, &new).unwrap_or(0)) {
                0 => {}
                n => {
                    return Err(ConnectError::NotEmpty {
                        from: current.backend,
                        to,
                        issues: n,
                        message: format!(
                            "project `{project}` has {n} issue{} on {} that {} on {}; switching would leave {} behind. {migrate}",
                            if n == 1 { "" } else { "s" },
                            board_name(&current),
                            if n == 1 { "isn't" } else { "aren't" },
                            board_name(&new),
                            if n == 1 { "it" } else { "them" },
                        ),
                    })
                }
            },
            Err(why) => {
                return Err(ConnectError::Unverified {
                    message: format!(
                        "couldn't check whether {} is empty ({why}), so project `{project}` was not \
                         switched to {}. {migrate}",
                        board_name(&current),
                        board_name(&new),
                    ),
                })
            }
        }
    }

    let (summary, resolved) = match &req.choice {
        TrackerChoice::FileSystem => ("the local file system".to_string(), None),
        TrackerChoice::Github { repo } => {
            env.github_preflight(repo.trim()).map_err(ConnectError::Preflight)?;
            (format!("GitHub {}", repo.trim()), None)
        }
        TrackerChoice::Linear(conn) => {
            let resolved = env.linear_preflight(project, conn).map_err(ConnectError::Preflight)?;
            env.plan_statuses(project, &resolved).map_err(ConnectError::Preflight)?;
            (format!("Linear {}", linear::summary(&resolved)), Some(resolved))
        }
    };

    let file = shelbi_state::tracker_config::write_issue_tracker(project, &new)
        .map_err(|e| ConnectError::Failed(e.to_string()))?;

    // The filter cache belongs to the connection it was resolved for: the
    // new one's replaces it, and leaving Linear drops it.
    let statuses = match (&resolved, &new.linear) {
        (Some(resolved), Some(conn)) => {
            let _ = linear_client::store_filter_cache(project, conn, resolved);
            env.sync_statuses(project, resolved)
        }
        _ => {
            if current.backend == IssueTrackerBackend::Linear {
                let _ = linear_client::invalidate_filter_cache(project);
            }
            StatusSync::NotNeeded
        }
    };

    Ok(Connected {
        backend: new.backend,
        previous: current.backend,
        file: file.display().to_string(),
        summary,
        statuses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::GithubConnection;

    /// Canned answers, and what was asked.
    #[derive(Default)]
    struct FakeEnv {
        issues: Option<Result<usize, String>>,
        /// What `already_moved` answers; `None`: none moved.
        moved: Option<Result<usize, String>>,
        preflight: Option<String>,
        /// What `plan_statuses` refuses with; `None`: the workflows map.
        unmappable: Option<String>,
        counted: Vec<IssueTrackerBackend>,
        planned: Vec<String>,
        synced: Vec<String>,
    }

    impl ConnectEnv for FakeEnv {
        fn issue_count(&mut self, _project: &str, tracker: &IssueTrackerConfig) -> Result<usize, String> {
            self.counted.push(tracker.backend);
            self.issues.clone().unwrap_or(Ok(0))
        }
        fn already_moved(&mut self, _project: &str, _current: &IssueTrackerConfig, _new: &IssueTrackerConfig) -> Result<usize, String> {
            self.moved.clone().unwrap_or(Ok(0))
        }
        fn github_preflight(&mut self, _repo: &str) -> Result<(), String> {
            self.preflight.clone().map_or(Ok(()), Err)
        }
        fn linear_preflight(&mut self, _project: &str, conn: &LinearConnection) -> Result<ResolvedLinearFilters, String> {
            if let Some(why) = &self.preflight {
                return Err(why.clone());
            }
            Ok(ResolvedLinearFilters {
                team_id: "team-eng".into(),
                team_key: conn.team.to_uppercase(),
                team_name: "Engineering".into(),
                project_id: None,
                project_name: None,
                label_id: None,
                label_name: None,
                assignee_id: None,
                assignee_name: None,
            })
        }
        fn plan_statuses(&mut self, _project: &str, filters: &ResolvedLinearFilters) -> Result<(), String> {
            self.planned.push(filters.team_id.clone());
            self.unmappable.clone().map_or(Ok(()), Err)
        }
        fn sync_statuses(&mut self, _project: &str, filters: &ResolvedLinearFilters) -> StatusSync {
            self.synced.push(filters.team_id.clone());
            StatusSync::Synced("7 states".into())
        }
    }

    fn register(tracker: &str) -> std::path::PathBuf {
        let home = std::path::PathBuf::from(std::env::var_os("SHELBI_HOME").unwrap());
        std::fs::create_dir_all(home.join("projects")).unwrap();
        let file = home.join("projects/app.yaml");
        std::fs::write(
            &file,
            format!(
                "# app\nname: app\nrepo: /tmp/app\ndefault_branch: main\n\
orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/app\nworkspaces: []\n{tracker}"
            ),
        )
        .unwrap();
        file
    }

    const GITHUB: &str = "issue_tracker:\n  backend: github\n  github:\n    repo: acme/app\n";

    fn linear(team: &str) -> ConnectTracker {
        ConnectTracker::new(TrackerChoice::Linear(LinearConnection::for_team(team)), false)
    }

    #[test]
    fn connecting_linear_writes_exactly_one_block_caches_filters_and_syncs_statuses() {
        let _home = crate::test_lock::isolated_home();
        let file = register(GITHUB);
        let mut env = FakeEnv::default();
        let done = apply_tracker_connection_with(&mut env, "app", &linear("eng")).unwrap();
        assert_eq!((done.previous, done.backend), (IssueTrackerBackend::Github, IssueTrackerBackend::Linear));
        assert_eq!(done.summary, "Linear ENG (Engineering)");
        assert_eq!(done.statuses, StatusSync::Synced("7 states".into()));
        assert_eq!(env.counted, vec![IssueTrackerBackend::Github], "the old board was checked");
        assert_eq!(env.planned, vec!["team-eng".to_string()], "the sync was planned first");
        assert_eq!(env.synced, vec!["team-eng".to_string()]);

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# app\n"), "comments survive:\n{text}");
        let p = shelbi_state::load_project("app").unwrap();
        assert_eq!(p.issue_tracker.connection_blocks(), vec!["linear"], "only one external block:\n{text}");
        assert_eq!(p.issue_tracker.linear.unwrap().team, "eng");
        let cache = std::fs::read_to_string(linear_client::filter_cache_path("app").unwrap()).unwrap();
        assert!(cache.contains("team-eng"), "{cache}");

        // A filter change on the same team is no switch: no emptiness check.
        let mut env = FakeEnv { issues: Some(Ok(5)), ..FakeEnv::default() };
        let req = ConnectTracker::new(
            TrackerChoice::Linear(LinearConnection { label: Some("Bug".into()), ..LinearConnection::for_team("ENG") }),
            false,
        );
        apply_tracker_connection_with(&mut env, "app", &req).unwrap();
        assert!(env.counted.is_empty());

        // The agent rules ride with the connection, and a later request
        // without them keeps them.
        let rules = shelbi_core::AgentRules { require_review: true, assigned_only: false };
        let mut env = FakeEnv::default();
        apply_tracker_connection_with(&mut env, "app", &req.clone().with_rules(rules)).unwrap();
        assert_eq!(shelbi_state::load_project("app").unwrap().issue_tracker.agent_rules, rules);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("require_review: true") && text.contains("assigned_only: false"), "{text}");
        apply_tracker_connection_with(&mut env, "app", &req).unwrap();
        assert_eq!(shelbi_state::load_project("app").unwrap().issue_tracker.agent_rules, rules);
        apply_tracker_connection_with(&mut env, "app", &req.clone().with_rules(Default::default())).unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("agent_rules"));

        // Disconnecting goes back to no block at all, and drops the cache.
        let mut env = FakeEnv::default();
        let req = ConnectTracker::new(TrackerChoice::FileSystem, false);
        let done = apply_tracker_connection_with(&mut env, "app", &req).unwrap();
        assert_eq!(done.statuses, StatusSync::NotNeeded);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("issue_tracker"));
        assert!(!linear_client::filter_cache_path("app").unwrap().exists());
    }

    #[test]
    fn a_switch_with_issues_is_refused_with_migrate_guidance_unless_forced() {
        let _home = crate::test_lock::isolated_home();
        let file = register(GITHUB);
        let before = std::fs::read_to_string(&file).unwrap();
        let mut env = FakeEnv { issues: Some(Ok(3)), ..FakeEnv::default() };
        let err = apply_tracker_connection_with(&mut env, "app", &linear("ENG")).unwrap_err();
        let ConnectError::NotEmpty { from, to, issues, message } = &err else { panic!("{err:?}") };
        assert_eq!((*from, *to, *issues), (IssueTrackerBackend::Github, IssueTrackerBackend::Linear, 3));
        assert!(message.contains("has 3 issues on GitHub (acme/app) that aren't on Linear (ENG)"), "{message}");
        assert!(message.contains("shelbi issue-store migrate --to linear"), "{message}");
        assert!(message.contains("--force"), "{message}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");

        // Only the issues the new board doesn't hold count; a target that
        // can't be read counts them all.
        let mut env = FakeEnv { issues: Some(Ok(3)), moved: Some(Ok(2)), ..FakeEnv::default() };
        let err = apply_tracker_connection_with(&mut env, "app", &linear("ENG")).unwrap_err();
        assert!(matches!(err, ConnectError::NotEmpty { issues: 1, .. }), "{err:?}");
        let mut env = FakeEnv { issues: Some(Ok(3)), moved: Some(Err("offline".into())), ..FakeEnv::default() };
        let err = apply_tracker_connection_with(&mut env, "app", &linear("ENG")).unwrap_err();
        assert!(matches!(err, ConnectError::NotEmpty { issues: 3, .. }), "{err:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");

        // A board that can't be counted is refused too.
        let mut env = FakeEnv { issues: Some(Err("gh is not signed in".into())), ..FakeEnv::default() };
        let err = apply_tracker_connection_with(&mut env, "app", &linear("ENG")).unwrap_err();
        assert!(matches!(err, ConnectError::Unverified { .. }));
        assert!(err.to_string().contains("gh is not signed in"), "{err}");

        // Every issue moved already: the switch needs no force.
        let mut env = FakeEnv { issues: Some(Ok(3)), moved: Some(Ok(3)), ..FakeEnv::default() };
        apply_tracker_connection_with(&mut env, "app", &linear("ENG")).unwrap();
        assert_eq!(shelbi_state::load_project("app").unwrap().issue_tracker.backend, IssueTrackerBackend::Linear);
        std::fs::write(&file, &before).unwrap();

        // --force switches anyway, without counting.
        let mut env = FakeEnv { issues: Some(Ok(3)), ..FakeEnv::default() };
        let req = ConnectTracker { force: true, ..linear("ENG") };
        apply_tracker_connection_with(&mut env, "app", &req).unwrap();
        assert!(env.counted.is_empty());
        assert_eq!(shelbi_state::load_project("app").unwrap().issue_tracker.backend, IssueTrackerBackend::Linear);
    }

    #[test]
    fn a_move_copies_the_board_with_its_comments_and_a_rerun_skips_what_moved() {
        use shelbi_state::{FileSystemStore, IssueStore, NewIssue};
        let _home = crate::test_lock::isolated_home();
        let home = std::path::PathBuf::from(std::env::var_os("SHELBI_HOME").unwrap());
        for p in ["app", "dest"] {
            std::fs::create_dir_all(home.join("projects").join(p)).unwrap();
        }
        let (from, to) = (FileSystemStore::new("app"), FileSystemStore::new("dest"));
        from.add(NewIssue::new("fix-login", "Fix login", shelbi_core::Column::review(), "body")).unwrap();
        from.add(NewIssue::new("add-tests", "Add tests", shelbi_core::Column::review(), "")).unwrap();
        from.add_comment("fix-login", "Repro attached").unwrap();
        assert_eq!(local_issues("app"), 2);

        let slept = std::cell::Cell::new(0);
        let sleep = |_: std::time::Duration| slept.set(slept.get() + 1);
        let moved = move_between("app", &from, &to, None, IssueTrackerBackend::Github, &sleep).unwrap();
        let mut ids: Vec<_> = moved.moved.iter().map(|i| i.id.as_str()).collect();
        ids.sort();
        assert_eq!((ids, moved.already, moved.comments), (vec!["add-tests", "fix-login"], 0, 1));
        assert_eq!(slept.get(), 0, "a small move isn't paced");
        let copied = to.list_comments("fix-login").unwrap();
        assert!(copied[0].body.contains("Repro attached"), "{copied:?}");
        assert_eq!(to.list().unwrap().len(), 2);

        // Moving again is safe: what's there is skipped, nothing doubles.
        let again = move_between("app", &from, &to, None, IssueTrackerBackend::Github, &sleep).unwrap();
        assert_eq!((again.moved.len(), again.already, again.total()), (0, 2, 2));
        assert_eq!(to.list().unwrap().len(), 2);
        assert_eq!(to.list_comments("fix-login").unwrap().len(), 1);
    }

    #[test]
    fn a_workflow_status_the_team_cant_take_refuses_before_anything_is_written() {
        let _home = crate::test_lock::isolated_home();
        let file = register(GITHUB);
        let before = std::fs::read_to_string(&file).unwrap();
        let mut env = FakeEnv { unmappable: Some("no handoff status in ENG".into()), ..FakeEnv::default() };
        let err = apply_tracker_connection_with(&mut env, "app", &linear("ENG")).unwrap_err();
        assert_eq!(err, ConnectError::Preflight("no handoff status in ENG".into()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");
        assert!(!linear_client::filter_cache_path("app").unwrap().exists(), "no filter cache");
        assert!(env.synced.is_empty(), "no sync");
    }

    #[test]
    fn a_move_onto_a_team_the_workflows_cant_map_onto_is_refused_before_any_issue_moves() {
        use shelbi_state::{FileSystemStore, IssueStore, NewIssue};
        let _home = crate::test_lock::isolated_home();
        let file = register("default_workflow: task\n");
        std::fs::create_dir_all(file.with_extension("")).unwrap();
        shelbi_state::scaffold_project_statuses("app").unwrap();
        shelbi_state::scaffold_project_workflow("app").unwrap();
        FileSystemStore::new("app")
            .add(NewIssue::new("fix-login", "Fix login", shelbi_core::Column::todo(), ""))
            .unwrap();
        let fake = linear_client::FakeLinear::new();
        // Linear's stock team: nothing for the `task` workflow's `review`.
        let mut states: serde_json::Value =
            serde_json::from_str(linear_client::fixture("TeamStates").unwrap()).unwrap();
        states["data"]["team"]["states"]["nodes"].as_array_mut().unwrap().retain(|s| s["name"] != "In Review");
        fake.on("TeamStates", states.to_string());
        let _guard = fake.install();

        let choice = TrackerChoice::Linear(LinearConnection::for_team("ENG"));
        let err = move_issues("app", &choice).unwrap_err();
        assert!(err.moved.is_empty() && err.left.is_empty(), "{err:?}");
        assert!(err.reason.contains("workflow `task`: `review`"), "{}", err.reason);
        assert!(fake.calls_for("IssueCreate").is_empty(), "no issue was created");
        let config = std::fs::read_to_string(&file).unwrap();
        // The connection is refused the same way, nothing written.
        let err = apply_tracker_connection("app", &ConnectTracker::new(choice, true)).unwrap_err();
        assert!(matches!(&err, ConnectError::Preflight(m) if m.contains("`review`")), "{err:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), config);
    }

    #[test]
    fn a_failed_preflight_writes_nothing() {
        let _home = crate::test_lock::isolated_home();
        let file = register("");
        let before = std::fs::read_to_string(&file).unwrap();
        let mut env = FakeEnv { preflight: Some("No Linear team `OPS`".into()), ..FakeEnv::default() };
        let err = apply_tracker_connection_with(&mut env, "app", &linear("OPS")).unwrap_err();
        assert_eq!(err, ConnectError::Preflight("No Linear team `OPS`".into()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

        // A team is required before anything is asked.
        let mut env = FakeEnv::default();
        let err = apply_tracker_connection_with(&mut env, "app", &linear("  ")).unwrap_err();
        assert!(err.to_string().contains("issue_tracker.linear.team is required"), "{err}");
        assert!(env.counted.is_empty());
    }

    #[test]
    fn switching_means_another_backend_repo_or_team() {
        let gh = |repo: &str| IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection { repo: repo.into() }),
            ..IssueTrackerConfig::default()
        };
        let lin = |team: &str| TrackerChoice::Linear(LinearConnection::for_team(team)).config(&IssueTrackerConfig::default());
        assert!(switches_board(&IssueTrackerConfig::default(), &gh("a/b")));
        assert!(switches_board(&gh("a/b"), &gh("a/c")));
        assert!(!switches_board(&gh("a/b"), &gh("A/B")));
        assert!(switches_board(&lin("ENG"), &lin("DES")));
        assert!(!switches_board(&lin("ENG"), &lin("eng")));
        assert!(!switches_board(&IssueTrackerConfig::default(), &IssueTrackerConfig::default()));
    }
}
