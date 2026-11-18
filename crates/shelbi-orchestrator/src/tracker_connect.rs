//! Connect an existing project to a tracker, switch it to another, or
//! disconnect it (back to the local file system): [`apply_tracker_connection`].
//!
//! `shelbi tracker linear connect` / `shelbi tracker disconnect` call it in
//! process; Settings sends a `ConnectTracker` app request to the project's
//! daemon, which calls it there (`crate::far`). In order it:
//!
//! 1. refuses to switch boards while the current one has issues (they'd be
//!    left behind, unseen), pointing at `shelbi issue-store migrate`, unless
//!    [`ConnectTracker::force`];
//! 2. runs the new tracker's preflight (`gh` for GitHub,
//!    [`crate::project_create::linear::linear_preflight`] for Linear), so a
//!    connection that can't work is never written;
//! 3. writes the `issue_tracker:` block whole
//!    ([`shelbi_state::tracker_config::write_issue_tracker`]), so the
//!    project keeps exactly one external tracker block (none for the file
//!    system);
//! 4. for Linear, caches the resolved filter ids
//!    (`projects/<name>/linear.json`) and syncs the team's workflow states
//!    into the status catalog through [`ConnectEnv::sync_statuses`]
//!    ([`sync_linear_statuses`]).
//!
//! Every IO step goes through [`ConnectEnv`], so tests run the whole
//! operation with canned answers.

use shelbi_app::tracker_connect::{ConnectError, ConnectTracker, Connected, StatusSync, TrackerChoice};
use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig, LinearConnection};
use shelbi_state::linear_client::{self, ResolvedLinearFilters};

use crate::project_create::{issues, linear};

/// The IO [`apply_tracker_connection_with`] needs.
pub trait ConnectEnv {
    /// How many issues `project`'s board on `tracker` holds (open ones, for
    /// a remote board).
    fn issue_count(&mut self, project: &str, tracker: &IssueTrackerConfig) -> Result<usize, String>;
    /// The `gh` preflight for `repo`: the fix to show when it fails.
    fn github_preflight(&mut self, repo: &str) -> Result<(), String>;
    /// The Linear preflight with `project`'s key: what `conn` resolved to,
    /// or the fix to show.
    fn linear_preflight(&mut self, project: &str, conn: &LinearConnection) -> Result<ResolvedLinearFilters, String>;
    /// Sync the Linear team's workflow states into `project`'s status
    /// catalog.
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

    fn github_preflight(&mut self, repo: &str) -> Result<(), String> {
        issues::preflight_github(repo)
    }

    fn linear_preflight(&mut self, project: &str, conn: &LinearConnection) -> Result<ResolvedLinearFilters, String> {
        linear::preflight_linear(Some(project), conn)
    }

    fn sync_statuses(&mut self, project: &str, filters: &ResolvedLinearFilters) -> StatusSync {
        sync_linear_statuses(project, filters)
    }
}

/// Sync a Linear team's workflow states into `project`'s status catalog
/// (`shelbi_state::linear_statuses::connect`, run after the connection is
/// written, so it reads the new team). The workflow rewrites it plans onto
/// the synced catalog are offered, not applied: the line says how to
/// apply them. A failure is reported, never fatal: the connection stands
/// and the board refresh re-syncs when the team's states change.
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
    let (a, b, c) = (project.to_string(), project.to_string(), project.to_string());
    shelbi_app::settings::tracker::TrackerService {
        connect: std::sync::Arc::new(move |req| apply_tracker_connection(&a, &req)),
        linear: std::sync::Arc::new(move |req| linear::answer(Some(&b), req)),
        sync_statuses: std::sync::Arc::new(move || sync_statuses_now(&c)),
    }
}

/// Sync `project`'s status catalog from its Linear team now (Settings'
/// "Sync now"): the same sync a connection runs, reported the same way. A
/// project not on Linear has nothing to sync.
pub fn sync_statuses_now(project: &str) -> StatusSync {
    match shelbi_state::load_project(project) {
        Ok(p) if p.issue_tracker.backend == IssueTrackerBackend::Linear => {}
        Ok(_) => return StatusSync::NotNeeded,
        Err(e) => return StatusSync::Failed(e.to_string()),
    }
    match shelbi_state::linear_statuses::connect(project) {
        Ok(done) => StatusSync::Synced(status_sync_line(&done)),
        Err(e) => StatusSync::Failed(e.to_string()),
    }
}

/// `7 statuses from the team (3 changed); 1 workflow can be mapped onto
/// them: shelbi tracker linear sync --rewrite-workflows`.
fn status_sync_line(done: &shelbi_state::linear_statuses::Connected) -> String {
    let sync = &done.sync;
    let n = sync.after.statuses.len();
    let mut line = format!("{n} status{} from the Linear team", if n == 1 { "" } else { "es" });
    if !sync.changes.is_empty() {
        line.push_str(&format!(" ({} changed)", sync.changes.len()));
    }
    let rewrites = done.rewrites.iter().filter(|r| r.changes()).count();
    if rewrites > 0 {
        line.push_str(&format!(
            "; {rewrites} workflow{} can be mapped onto them: `shelbi tracker linear sync --rewrite-workflows`",
            if rewrites == 1 { "" } else { "s" }
        ));
    } else if !sync.broken.is_empty() {
        line.push_str(&format!(
            "; {} workflow reference{} no longer match: `shelbi tracker linear sync --rewrite-workflows`",
            sync.broken.len(),
            if sync.broken.len() == 1 { "" } else { "s" }
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
    let loaded = shelbi_state::load_project(project)
        .map_err(|e| ConnectError::Failed(format!("project `{project}` could not be loaded: {e}")))?;
    let current = loaded.issue_tracker;
    let new = req.choice.config(&current);
    new.validate().map_err(|e| ConnectError::Preflight(e.to_string()))?;

    if switches_board(&current, &new) && !req.force {
        let to = new.backend;
        let migrate = format!(
            "Move them first:\n  shelbi issue-store migrate --to {to} --dry-run   # preview\n  \
             shelbi issue-store migrate --to {to}             # then migrate\n\
             or switch anyway with --force (the issues stay on {}, unseen by this project).",
            board_name(&current)
        );
        match env.issue_count(project, &current) {
            Ok(0) => {}
            Ok(n) => {
                return Err(ConnectError::NotEmpty {
                    from: current.backend,
                    to,
                    issues: n,
                    message: format!(
                        "project `{project}` has {n} issue{} on {}; switching to {} would leave {} behind. {migrate}",
                        if n == 1 { "" } else { "s" },
                        board_name(&current),
                        board_name(&new),
                        if n == 1 { "it" } else { "them" },
                    ),
                })
            }
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
        preflight: Option<String>,
        counted: Vec<IssueTrackerBackend>,
        synced: Vec<String>,
    }

    impl ConnectEnv for FakeEnv {
        fn issue_count(&mut self, _project: &str, tracker: &IssueTrackerConfig) -> Result<usize, String> {
            self.counted.push(tracker.backend);
            self.issues.clone().unwrap_or(Ok(0))
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
        ConnectTracker { choice: TrackerChoice::Linear(LinearConnection::for_team(team)), force: false }
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
        let req = ConnectTracker {
            choice: TrackerChoice::Linear(LinearConnection { label: Some("Bug".into()), ..LinearConnection::for_team("ENG") }),
            force: false,
        };
        apply_tracker_connection_with(&mut env, "app", &req).unwrap();
        assert!(env.counted.is_empty());

        // Disconnecting goes back to no block at all, and drops the cache.
        let mut env = FakeEnv::default();
        let req = ConnectTracker { choice: TrackerChoice::FileSystem, force: false };
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
        assert!(message.contains("has 3 issues on GitHub (acme/app)"), "{message}");
        assert!(message.contains("shelbi issue-store migrate --to linear"), "{message}");
        assert!(message.contains("--force"), "{message}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");

        // A board that can't be counted is refused too.
        let mut env = FakeEnv { issues: Some(Err("gh is not signed in".into())), ..FakeEnv::default() };
        let err = apply_tracker_connection_with(&mut env, "app", &linear("ENG")).unwrap_err();
        assert!(matches!(err, ConnectError::Unverified { .. }));
        assert!(err.to_string().contains("gh is not signed in"), "{err}");

        // --force switches anyway, without counting.
        let mut env = FakeEnv { issues: Some(Ok(3)), ..FakeEnv::default() };
        let req = ConnectTracker { force: true, ..linear("ENG") };
        apply_tracker_connection_with(&mut env, "app", &req).unwrap();
        assert!(env.counted.is_empty());
        assert_eq!(shelbi_state::load_project("app").unwrap().issue_tracker.backend, IssueTrackerBackend::Linear);
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
