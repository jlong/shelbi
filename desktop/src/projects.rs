//! The project catalog and opening a project.
//!
//! The catalog is the TUI switcher's source, `shelbi_state::list_projects`
//! (most recently launched first): this machine's projects. With each, its
//! config-only sidebar skeleton (its source's `config_skeleton`: its label and
//! declared workspace rows with a loading badge). Both are read on a
//! background thread and kept in memory, so a project switch paints the
//! target's chrome on its very first frame without touching disk: the
//! prefetched skeleton when there is one, else the zero-IO
//! [`SidebarModel::skeleton`]. Data cascades in from the window's feed. Each
//! project's saved [`ProjectViewState`] (where it was left) is prefetched the
//! same way, so switching back to a project restores its view without a disk
//! read on the switch.
//!
//! With them, which projects are open and which of those have work in
//! progress ([`ProjectLiveness`]): every project's dot. A full refresh reads
//! it with the rest; [`Catalog::watch_changes`] re-reads just it whenever the
//! daemon reports a board or workspace change in any project, so a project
//! that starts or stops working repaints its tab and menu row without a
//! timer.
//!
//! Opening a project brings up its orchestrator and records it open (the
//! record the daemon's per-project poller watches), the way the TUI's startup
//! does, through the CLI bootstrap found. See [`open_project`].

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use gpui::Global;
use shelbi_app::command_model::ProjectLiveness;
use shelbi_app::source::{ProjectKey, ProjectSource};
use shelbi_app::view::SidebarModel;

use crate::platform::env::Env;
use crate::window_state::ProjectViewState;

/// How long [`Catalog::watch_changes`] lets a burst of changes settle before
/// it re-reads.
const LIVENESS_SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

/// One project the switcher lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    /// The project id (its `projects/<name>.yaml` stem).
    pub name: String,
    /// What to show: the display name, else the id.
    pub label: String,
}

#[derive(Default)]
struct CatalogInner {
    projects: Vec<ProjectEntry>,
    /// The projects recorded open (running in some window or terminal), and
    /// those with work in progress, as of the last refresh.
    liveness: ProjectLiveness,
    /// The open set has been read (a refresh landed, or a test set it): the
    /// tabs follow it only then.
    open_known: bool,
    skeletons: std::collections::HashMap<ProjectKey, SidebarModel>,
    views: std::collections::HashMap<ProjectKey, ProjectViewState>,
    /// Projects whose view state this process has set: a refresh never
    /// replaces those with what it read (the write may not have landed).
    remembered: std::collections::HashSet<ProjectKey>,
}

/// The app-wide project catalog, installed as a gpui global. Cheap to clone;
/// reads never block on disk (a refresh runs on its own thread and swaps the
/// result in).
#[derive(Clone, Default)]
pub struct Catalog {
    inner: Arc<Mutex<CatalogInner>>,
}

impl Global for Catalog {}

impl Catalog {
    /// The projects, most recently launched first. Empty until the first
    /// refresh lands.
    pub fn projects(&self) -> Vec<ProjectEntry> {
        self.lock().projects.clone()
    }

    /// The projects as the switcher lists them, in the same order, each with
    /// its dot: `shelbi_app::command_model::project_rows`, the rule the
    /// palette's Projects column uses too (`current` is this window's).
    pub fn rows(&self, current: &str) -> Vec<shelbi_app::overlay::palette::ProjectEntry> {
        let inner = self.lock();
        let projects = inner
            .projects
            .iter()
            .map(|p| (p.name.clone(), p.label.clone()))
            .collect();
        shelbi_app::command_model::project_rows(current, projects, &inner.liveness)
    }

    /// Whether `project` has work in progress (a workspace's agent is
    /// working), as of the last read: its tab's dot pulses.
    pub fn is_working(&self, project: &str) -> bool {
        self.lock().liveness.is_working(project)
    }

    /// The projects recorded open, sorted by name, or `None` before the
    /// first refresh lands (the tabs then drop nothing).
    pub fn open_projects(&self) -> Option<Vec<String>> {
        let inner = self.lock();
        inner.open_known.then(|| {
            let mut open: Vec<String> = inner.liveness.open.iter().cloned().collect();
            open.sort();
            open
        })
    }

    /// Record `project` open (`true`) or closed, ahead of the next refresh:
    /// this window just brought it up or quit it, and a tab must neither
    /// come back nor go meanwhile.
    pub fn note_open(&self, project: &str, open: bool) {
        let mut inner = self.lock();
        if open {
            inner.liveness.open.insert(project.to_string());
        } else {
            inner.liveness.open.remove(project);
            inner.liveness.working.remove(project);
        }
    }

    /// The sidebar a window for `project` paints on its first frame, from
    /// memory only: its prefetched config skeleton, else the bare skeleton.
    pub fn first_frame_sidebar(&self, project: &ProjectKey) -> SidebarModel {
        // `try_lock`: a refresh holds the lock only for its final swap, and a
        // first frame must never wait on it.
        self.inner
            .try_lock()
            .ok()
            .and_then(|inner| inner.skeletons.get(project).cloned())
            .unwrap_or_else(|| SidebarModel::skeleton(&project.name))
    }

    /// Where `project` was left, from memory only (the default when it was
    /// never left, or the catalog hasn't loaded).
    pub fn view_state(&self, project: &ProjectKey) -> ProjectViewState {
        self.inner
            .try_lock()
            .ok()
            .and_then(|inner| inner.views.get(project).cloned())
            .unwrap_or_default()
    }

    /// Remember where `project` was left (the caller saves it to disk).
    pub fn remember_view(&self, project: &ProjectKey, state: ProjectViewState) {
        let mut inner = self.lock();
        inner.views.insert(project.clone(), state);
        inner.remembered.insert(project.clone());
    }

    /// Start `project` over: no view state from a project that had its id
    /// before (a project just created opens on its Orchestrator), and a
    /// refresh never brings one back from disk.
    pub fn forget_view(&self, project: &ProjectKey) {
        self.remember_view(project, ProjectViewState::default());
    }

    /// Re-read the catalog on a background thread, then call `done` there.
    pub fn refresh(&self, done: impl FnOnce() + Send + 'static) {
        let catalog = self.clone();
        let spawned = std::thread::Builder::new()
            .name("shelbi-desktop-projects".into())
            .spawn(move || {
                catalog.reload();
                done();
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not refresh the project list");
        }
    }

    /// Re-read the catalog here and now (blocking: run it off the UI
    /// thread).
    pub fn reload(&self) {
        let summaries = shelbi_state::list_projects().unwrap_or_default();
        let liveness = ProjectLiveness::read(None);
        let projects: Vec<ProjectEntry> = summaries
            .iter()
            .map(|p| ProjectEntry {
                name: p.name.clone(),
                label: p.display_label().to_string(),
            })
            .collect();
        let skeletons = projects
            .iter()
            .filter_map(|p| {
                let source = shelbi_orchestrator::source::LocalSource::new(&p.name);
                source.config_skeleton().map(|m| (source.key().clone(), m))
            })
            .collect();
        let views: Vec<(ProjectKey, ProjectViewState)> = projects
            .iter()
            .map(|p| {
                let key = ProjectKey::local(&p.name);
                let view = ProjectViewState::load(&key);
                (key, view)
            })
            .collect();
        let mut guard = self.lock();
        guard.projects = projects;
        guard.liveness = liveness;
        guard.open_known = true;
        guard.skeletons = skeletons;
        for (name, view) in views {
            if !guard.remembered.contains(&name) {
                guard.views.insert(name, view);
            }
        }
    }

    /// Re-read which projects are open and working (blocking: run it off
    /// the UI thread). Whether that changed what the catalog had.
    pub fn reload_liveness(&self) -> bool {
        let liveness = ProjectLiveness::read(None);
        let mut inner = self.lock();
        let changed = !inner.open_known || inner.liveness != liveness;
        inner.liveness = liveness;
        inner.open_known = true;
        changed
    }

    /// Follow every project's board and workspace changes (the daemon's
    /// change stream, all projects): after each burst, re-read the liveness
    /// on the watcher's thread and call `changed` there when it moved. Runs
    /// for the app's lifetime; reconnects on its own while the daemon is
    /// down.
    pub fn watch_changes(&self, changed: impl Fn() + Send + 'static) {
        let catalog = self.clone();
        let spawned = std::thread::Builder::new()
            .name("shelbi-desktop-liveness".into())
            .spawn(move || {
                let (_subscription, wakes) = shelbi_app::changes::spawn_all();
                while wakes.recv().is_ok() {
                    // One read per burst: a dispatch moves a card and starts a
                    // pane at once.
                    while wakes.recv_timeout(LIVENESS_SETTLE).is_ok() {}
                    if catalog.reload_liveness() {
                        changed();
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not watch the projects' work");
        }
    }

    /// Record `open` as the projects open elsewhere (a test's or a visual
    /// check's catalog, which never refreshes).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn set_open(&self, open: &[&str]) {
        let mut inner = self.lock();
        inner.liveness.open = open.iter().map(|p| p.to_string()).collect();
        inner.open_known = true;
    }

    /// Record `working` as the projects with work in progress (a test's or a
    /// visual check's catalog).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn set_working(&self, working: &[&str]) {
        self.lock().liveness.working = working.iter().map(|p| p.to_string()).collect();
    }

    /// `project`'s label (its display name), as of the last refresh.
    pub fn label_of(&self, project: &str) -> Option<String> {
        self.lock()
            .projects
            .iter()
            .find(|p| p.name == project)
            .map(|p| p.label.clone())
    }

    /// Whether `project` is in the catalog (as of its last refresh).
    pub fn lists(&self, project: &str) -> bool {
        self.lock().projects.iter().any(|p| p.name == project)
    }

    #[cfg(any(test, feature = "visual-check"))]
    pub fn with(projects: Vec<ProjectEntry>, skeletons: Vec<SidebarModel>) -> Catalog {
        let catalog = Catalog::default();
        {
            let mut inner = catalog.lock();
            inner.skeletons = skeletons
                .into_iter()
                .zip(projects.iter())
                .map(|(m, p)| (ProjectKey::local(&p.name), m))
                .collect();
            inner.projects = projects;
        }
        catalog
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CatalogInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The project named on the command line (the first argument), else by
/// `$SHELBI_PROJECT`.
pub fn named_project() -> Option<String> {
    std::env::args()
        .nth(1)
        .filter(|a| !a.starts_with('-'))
        .or_else(|| std::env::var("SHELBI_PROJECT").ok())
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// The registered projects' names (blocking: a directory read).
pub fn project_names() -> std::collections::HashSet<String> {
    shelbi_state::list_projects()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.name)
        .collect()
}

/// The project a fresh launch opens, picked the way a terminal user would land
/// on one:
///
/// 1. The first argument, else `$SHELBI_PROJECT`.
/// 2. The project whose root contains the working directory (`cargo run` from
///    a checkout lands on that checkout's project).
/// 3. The first project that is open (has a running session).
/// 4. The most recently launched project.
///
/// Reads disk, so it runs once, before the first window opens.
pub fn launch_project() -> Option<String> {
    named_project()
        .or_else(|| {
            let cwd = std::env::current_dir().ok()?;
            shelbi_state::resolve_project_for_cwd(&cwd).ok().flatten()
        })
        .or_else(|| shelbi_state::list_open_projects().ok()?.into_iter().next())
        .or_else(|| {
            shelbi_state::list_projects()
                .ok()?
                .into_iter()
                .next()
                .map(|p| p.name)
        })
}

/// Open `project`: bring up its orchestrator and record the project open, by
/// running `<cli> --project <name> orchestrate` under the login environment
/// bootstrap resolved: the work the TUI's startup does in process
/// (`ensure_daemon_running` + `ensure_dashboard`). Idempotent: an
/// already-running orchestrator is left alone.
///
/// Blocking (it can wait on an agent launch): run it on a background thread.
/// Returns a user-facing message on failure.
///
/// The CLI does it rather than this process because spawning a session in
/// process re-executes `current_exe()`, which here is the app.
pub fn open_project(project: &str, cli: &Path, env: &Env) -> Result<(), String> {
    let output = Command::new(cli)
        .args(["--project", project, "orchestrate"])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run {}: {e}", cli.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        Err(format!("couldn't bring up the orchestrator: {}", last.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::view::{WorkspaceBadge, WorkspaceRow};

    #[test]
    fn first_frame_prefers_the_prefetched_skeleton() {
        let mut skeleton = SidebarModel::skeleton("Web App");
        skeleton.board_loading = false;
        skeleton.workspaces = vec![WorkspaceRow {
            name: "alpha".into(),
            machine: "hub".into(),
            is_remote: false,
            current_task: None,
            agent: None,
            badge: WorkspaceBadge::Loading,
        }];
        let catalog = Catalog::with(
            vec![ProjectEntry {
                name: "web".into(),
                label: "Web App".into(),
            }],
            vec![skeleton.clone()],
        );
        assert_eq!(catalog.first_frame_sidebar(&ProjectKey::local("web")), skeleton);
        // A remote project of the same name is another project.
        let remote = catalog.first_frame_sidebar(&ProjectKey::remote("devbox", "web"));
        assert_eq!(remote.project_label, "web");
        assert!(remote.board_loading);
        // An unknown project paints the bare skeleton: its own name, loading.
        let bare = catalog.first_frame_sidebar(&ProjectKey::local("api"));
        assert_eq!(bare.project_label, "api");
        assert!(bare.board_loading);
    }

    #[test]
    fn rows_keep_the_switchers_order_with_each_projects_dot() {
        use shelbi_app::overlay::palette::ProjectIndicator;
        let entry = |name: &str| ProjectEntry {
            name: name.into(),
            label: name.to_uppercase(),
        };
        let catalog = Catalog::with(vec![entry("web"), entry("api"), entry("shelbi")], Vec::new());
        catalog.set_open(&["web", "shelbi"]);
        catalog.set_working(&["web"]);
        let rows: Vec<(String, ProjectIndicator, bool)> = catalog
            .rows("shelbi")
            .into_iter()
            .map(|r| (r.label, r.indicator, r.current))
            .collect();
        assert_eq!(
            rows,
            [
                ("WEB".to_string(), ProjectIndicator::Working, false),
                ("API".to_string(), ProjectIndicator::Unloaded, false),
                // The current project wears the check, its dot by its work.
                ("SHELBI".to_string(), ProjectIndicator::LoadedIdle, true),
            ]
        );
    }

    #[test]
    fn a_remembered_view_state_is_served_from_memory() {
        let catalog = Catalog::default();
        let web = ProjectKey::local("web");
        assert_eq!(catalog.view_state(&web), ProjectViewState::default());
        let state = ProjectViewState {
            view: Some("tasks".into()),
            ..Default::default()
        };
        catalog.remember_view(&web, state.clone());
        assert_eq!(catalog.view_state(&web), state);
        assert_eq!(
            catalog.view_state(&ProjectKey::remote("devbox", "web")),
            ProjectViewState::default(),
            "a remote project of the same name has its own"
        );
        // A project created under the id starts over.
        catalog.forget_view(&web);
        assert_eq!(catalog.view_state(&web), ProjectViewState::default());
        assert!(catalog.inner.lock().unwrap().remembered.contains(&web), "a refresh keeps it forgotten");
    }

    #[test]
    fn the_open_set_is_unknown_until_read_and_follows_notes() {
        let catalog = Catalog::default();
        assert_eq!(catalog.open_projects(), None, "nothing read yet");
        catalog.set_open(&["web", "api"]);
        assert_eq!(catalog.open_projects(), Some(vec!["api".to_string(), "web".to_string()]));
        catalog.note_open("api", false);
        catalog.note_open("docs", true);
        assert_eq!(catalog.open_projects(), Some(vec!["docs".to_string(), "web".to_string()]));
    }

    #[test]
    fn the_working_flag_follows_the_daemons_state_and_closing_clears_it() {
        let catalog = Catalog::default();
        catalog.set_open(&["web", "api"]);
        catalog.set_working(&["web"]);
        assert!(catalog.is_working("web"));
        assert!(!catalog.is_working("api"));
        catalog.note_open("web", false);
        assert!(!catalog.is_working("web"), "a closed project isn't working");
    }

    /// The per-project flag is read from disk for every open project, not
    /// only a window's own: a project another terminal has open, whose dev
    /// slot holds an active card its agent is working, reads as working, and
    /// stops when the agent goes idle.
    #[test]
    fn reload_liveness_reads_every_open_projects_work() {
        struct Home {
            dir: std::path::PathBuf,
            prev: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for Home {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var("SHELBI_HOME", v),
                    None => std::env::remove_var("SHELBI_HOME"),
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
        let _lock = crate::test_env::lock();
        let dir = std::env::temp_dir().join(format!("shb-liveness-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _home = Home { dir: dir.clone(), prev: std::env::var_os("SHELBI_HOME"), _lock };
        std::env::set_var("SHELBI_HOME", &dir);
        let projects = shelbi_state::projects_dir().unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        let yaml = |name: &str| {
            let repo = dir.join("repos").join(name);
            std::fs::create_dir_all(&repo).unwrap();
            format!(
                "name: {name}\nrepo: {r}\ndefault_branch: main\nmachines:\n  - {{ name: hub, kind: local, work_dir: {r} }}\norchestrator: {{ runner: claude }}\nagent_runners:\n  claude: {{ command: claude, flags: [] }}\nworkspaces:\n  - {{ name: alpha, machine: hub }}\n",
                r = repo.display()
            )
        };
        for name in ["web", "api"] {
            std::fs::write(projects.join(format!("{name}.yaml")), yaml(name)).unwrap();
            shelbi_state::set_project_open(name, true).unwrap();
        }
        let now = chrono::Utc::now();
        let mut task = shelbi_core::Issue {
            id: "t-1".into(),
            title: "Work".into(),
            column: shelbi_core::Column::in_progress(),
            priority: 0,
            assigned_to: Some("alpha".into()),
            workflow: None,
            branch: None,
            pr: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: Default::default(),
        };
        shelbi_state::save_task("web", &task, "").unwrap();
        let status = |state| shelbi_state::WorkspaceStatus {
            workspace: "alpha".into(),
            current_task: Some("t-1".into()),
            state,
            last_transition: now,
            last_seen: now,
        };
        shelbi_state::save_workspace_status("web", &status(shelbi_state::WorkspaceState::Working)).unwrap();

        let catalog = Catalog::default();
        assert!(catalog.reload_liveness(), "the first read is a change");
        assert!(catalog.is_working("web"));
        assert!(!catalog.is_working("api"), "nothing in progress there");
        assert!(!catalog.reload_liveness(), "nothing moved");

        shelbi_state::save_workspace_status("web", &status(shelbi_state::WorkspaceState::AwaitingInput))
            .unwrap();
        assert!(catalog.reload_liveness());
        assert!(!catalog.is_working("web"), "its agent went idle");

        // Back to work, then the card leaves the active column.
        shelbi_state::save_workspace_status("web", &status(shelbi_state::WorkspaceState::Working)).unwrap();
        assert!(catalog.reload_liveness());
        task.column = shelbi_core::Column::review();
        shelbi_state::save_task("web", &task, "").unwrap();
        assert!(catalog.reload_liveness());
        assert!(!catalog.is_working("web"), "a handed-off card is no work in progress");
    }

    #[test]
    fn lists_answers_from_the_last_refresh() {
        let catalog = Catalog::with(
            vec![ProjectEntry {
                name: "web".into(),
                label: "Web App".into(),
            }],
            vec![SidebarModel::skeleton("Web App")],
        );
        assert!(catalog.lists("web"));
        assert!(!catalog.lists("api"));
    }
}
