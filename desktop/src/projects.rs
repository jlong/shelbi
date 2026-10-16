//! The project catalog and opening a project.
//!
//! The catalog is the TUI switcher's source, `shelbi_state::list_projects`
//! (most recently launched first), plus each project's config-only sidebar
//! skeleton (`shelbi_orchestrator::sidebar_model::read_config_skeleton`: its
//! label and declared workspace rows with a loading badge). Both are read on a
//! background thread and kept in memory, so a project switch paints the
//! target's chrome on its very first frame without touching disk: the
//! prefetched skeleton when there is one, else the zero-IO
//! [`SidebarModel::skeleton`]. Data cascades in from the window's feed. Each
//! project's saved [`ProjectViewState`] (where it was left) is prefetched the
//! same way, so switching back to a project restores its view without a disk
//! read on the switch.
//!
//! Opening a project brings up its orchestrator and records it open (the
//! record the daemon's per-project poller watches), the way the TUI's startup
//! does, through the CLI bootstrap found. See [`open_project`].

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use gpui::Global;
use shelbi_app::view::SidebarModel;

use crate::platform::env::Env;
use crate::window_state::ProjectViewState;

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
    skeletons: std::collections::HashMap<String, SidebarModel>,
    views: std::collections::HashMap<String, ProjectViewState>,
    /// Projects whose view state this process has set: a refresh never
    /// replaces those with what it read (the write may not have landed).
    remembered: std::collections::HashSet<String>,
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

    /// The sidebar a window for `project` paints on its first frame, from
    /// memory only: its prefetched config skeleton, else the bare skeleton.
    pub fn first_frame_sidebar(&self, project: &str) -> SidebarModel {
        // `try_lock`: a refresh holds the lock only for its final swap, and a
        // first frame must never wait on it.
        self.inner
            .try_lock()
            .ok()
            .and_then(|inner| inner.skeletons.get(project).cloned())
            .unwrap_or_else(|| SidebarModel::skeleton(project))
    }

    /// Where `project` was left, from memory only (the default when it was
    /// never left, or the catalog hasn't loaded).
    pub fn view_state(&self, project: &str) -> ProjectViewState {
        self.inner
            .try_lock()
            .ok()
            .and_then(|inner| inner.views.get(project).cloned())
            .unwrap_or_default()
    }

    /// Remember where `project` was left (the caller saves it to disk).
    pub fn remember_view(&self, project: &str, state: ProjectViewState) {
        let mut inner = self.lock();
        inner.views.insert(project.to_string(), state);
        inner.remembered.insert(project.to_string());
    }

    /// Re-read the catalog on a background thread, then call `done` there.
    pub fn refresh(&self, done: impl FnOnce() + Send + 'static) {
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new()
            .name("shelbi-desktop-projects".into())
            .spawn(move || {
                let summaries = shelbi_state::list_projects().unwrap_or_default();
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
                        shelbi_orchestrator::sidebar_model::read_config_skeleton(&p.name)
                            .map(|m| (p.name.clone(), m))
                    })
                    .collect();
                let views: Vec<(String, ProjectViewState)> = projects
                    .iter()
                    .map(|p| (p.name.clone(), ProjectViewState::load(&p.name)))
                    .collect();
                {
                    let mut guard = inner.lock().unwrap_or_else(|p| p.into_inner());
                    guard.projects = projects;
                    guard.skeletons = skeletons;
                    for (name, view) in views {
                        if !guard.remembered.contains(&name) {
                            guard.views.insert(name, view);
                        }
                    }
                }
                done();
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not refresh the project list");
        }
    }

    #[cfg(any(test, feature = "visual-check"))]
    pub fn with(projects: Vec<ProjectEntry>, skeletons: Vec<SidebarModel>) -> Catalog {
        let catalog = Catalog::default();
        {
            let mut inner = catalog.lock();
            inner.skeletons = skeletons
                .into_iter()
                .zip(projects.iter())
                .map(|(m, p)| (p.name.clone(), m))
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
        assert_eq!(catalog.first_frame_sidebar("web"), skeleton);
        // An unknown project paints the bare skeleton: its own name, loading.
        let bare = catalog.first_frame_sidebar("api");
        assert_eq!(bare.project_label, "api");
        assert!(bare.board_loading);
    }

    #[test]
    fn a_remembered_view_state_is_served_from_memory() {
        let catalog = Catalog::default();
        assert_eq!(catalog.view_state("web"), ProjectViewState::default());
        let state = ProjectViewState {
            view: Some("tasks".into()),
            ..Default::default()
        };
        catalog.remember_view("web", state.clone());
        assert_eq!(catalog.view_state("web"), state);
    }
}
