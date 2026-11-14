//! Building the palette's inputs from live state.
//!
//! The registry ([`crate::command`]) is a pure function of a [`CommandModel`].
//! This module builds that model from a client's sidebar model plus a few
//! light on-disk reads (the other projects, the edit targets), and the
//! palette's Projects column from the registered projects, so the TUI and the
//! desktop app list the same entries in the same order for the same state.
//! The reads are small local files; hosts still call these off their UI
//! thread where they can.

use std::collections::HashSet;
use std::path::PathBuf;

use shelbi_palette::{Decoration, DecorationColor};

use crate::command::{CommandModel, EditItem, ProjectItem, ReviewItem, ViewItem, WorkspaceItem};
use crate::exec::EditTarget;
use crate::nav::View;
use crate::overlay::palette::{project_indicator, ProjectEntry};
use crate::view::SidebarModel;

/// The nav glyph for a nav label: the twemoji the sidebar and the palette
/// show (💬 / 📋 / 🚀 / ⚡ / 🖥), or `•` for a label with none.
pub fn nav_glyph(label: &str) -> &'static str {
    match label {
        "Orchestrator" => "💬",
        "Issues" => "📋",
        "Changelog" => "🚀",
        "Activity" => "⚡",
        "Machines" => "🖥",
        "Jobs" => "⏳",
        _ => "•",
    }
}

/// The palette decoration for a nav view: its glyph in its own colors, or
/// `None` (the entry-kind icon) for a label with no glyph.
fn nav_decoration(label: &str) -> Option<Decoration> {
    let glyph = nav_glyph(label);
    (glyph != "•").then(|| Decoration {
        glyph: glyph.to_string(),
        color: DecorationColor::Default,
    })
}

/// Build the [`CommandModel`] for `project` from its sidebar model. Reads the
/// registered projects and checks which edit targets exist on disk.
pub fn build_command_model(
    project: &str,
    sidebar: &SidebarModel,
    other_clients_attached: bool,
) -> CommandModel {
    let other_projects = shelbi_state::list_projects()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.name != project)
        .map(|p| ProjectItem {
            label: p.display_label().to_string(),
            slug: p.name,
        })
        .collect();
    command_model_from(
        project,
        sidebar,
        other_clients_attached,
        other_projects,
        edit_targets(project),
    )
}

/// The pure half of [`build_command_model`]: the model from the sidebar and
/// already-read projects and edit targets.
pub fn command_model_from(
    project: &str,
    sidebar: &SidebarModel,
    other_clients_attached: bool,
    other_projects: Vec<ProjectItem>,
    edit_targets: Vec<EditItem>,
) -> CommandModel {
    let mut views: Vec<ViewItem> = sidebar
        .nav
        .iter()
        .map(|n| ViewItem {
            id: format!("view:{}", n.view.as_view_id()),
            title: n.label.clone(),
            view: n.view.clone(),
            decoration: nav_decoration(&n.label),
        })
        .collect();
    // Activity, Machines and Jobs are palette-only (no sidebar row); add
    // each once.
    for (view, title) in [
        (View::Activity, "Activity"),
        (View::Machines, "Machines"),
        (View::Jobs, crate::jobs::TITLE),
    ] {
        if !views.iter().any(|v| v.view == view) {
            views.push(ViewItem {
                id: format!("view:{}", view.as_view_id()),
                title: title.to_string(),
                decoration: nav_decoration(title),
                view,
            });
        }
    }

    let workspaces = sidebar
        .workspaces
        .iter()
        .map(|w| WorkspaceItem {
            name: w.name.clone(),
            subtitle: w.current_task.clone(),
            decoration: None,
        })
        .collect();

    let reviews = sidebar
        .reviews
        .iter()
        .map(|r| ReviewItem {
            task_id: r.task_id.clone(),
            title: r.title.clone(),
            subtitle: None,
            decoration: None,
        })
        .collect();

    CommandModel {
        project: Some(project.to_string()),
        zen_on: !matches!(sidebar.zen_mode, shelbi_state::ZenModeState::Off),
        zen_shortcut: None,
        views,
        workspaces,
        reviews,
        other_projects,
        edit_targets,
        other_clients_attached,
        multi_window: false,
        // Each client turns these on once it has the launch screen and the
        // Connect to Remote Session dialog.
        launch_screen: false,
        remote_connect: false,
        remote_host: None,
        // Each client fills these from what it has open.
        open_others: Vec::new(),
    }
}

/// Which projects are loaded and which of those have work in progress: what
/// a project's dot shows ([`project_rows`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectLiveness {
    /// The projects recorded open (running in some window or terminal).
    pub open: HashSet<String>,
    /// The loaded projects with an agent at work
    /// ([`shelbi_state::project_is_working`]).
    pub working: HashSet<String>,
}

impl ProjectLiveness {
    /// Read it (blocking: the open records, then the board and the busy
    /// slots' status of each loaded project). `current`, the caller's own
    /// project, counts as loaded even before its open record lands.
    pub fn read(current: Option<&str>) -> Self {
        let open: HashSet<String> = shelbi_state::list_open_projects()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let working = open
            .iter()
            .map(String::as_str)
            .chain(current)
            .filter(|p| shelbi_state::project_is_working(p))
            .map(str::to_string)
            .collect();
        ProjectLiveness { open, working }
    }

    /// `project` is loaded and has work in progress.
    pub fn is_working(&self, project: &str) -> bool {
        self.working.contains(project)
    }
}

/// The palette's Projects column: every registered project, the current one
/// first, each with its dot ([`project_rows`]).
pub fn build_projects_column(current: &str) -> Vec<ProjectEntry> {
    let liveness = ProjectLiveness::read(Some(current));
    let projects = shelbi_state::list_projects()
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.name.clone(), p.display_label().to_string()))
        .collect();
    projects_column_from(current, projects, &liveness)
}

/// The pure half of [`build_projects_column`]: `projects` as `(slug, label)`
/// in list order (most recently launched first).
pub fn projects_column_from(
    current: &str,
    projects: Vec<(String, String)>,
    liveness: &ProjectLiveness,
) -> Vec<ProjectEntry> {
    let mut rows = project_rows(current, projects, liveness);
    // The current project leads; the rest keep list order (a stable sort).
    rows.sort_by_key(|p| p.slug != current);
    rows
}

/// Every project's row in list order, with its indicator: the one rule that
/// decides a project's dot wherever a client lists projects (the palette's
/// Projects column, the desktop's tabs and `+` menu). A project is loaded
/// when `liveness` records it open, or it is `current` (this window has it),
/// and its dot pulses while `liveness` has work in progress there. Being
/// current only sets the row's [`ProjectEntry::current`] (the menu's check),
/// never its dot.
pub fn project_rows(
    current: &str,
    projects: Vec<(String, String)>,
    liveness: &ProjectLiveness,
) -> Vec<ProjectEntry> {
    projects
        .into_iter()
        .map(|(slug, label)| {
            let is_current = slug == current;
            let loaded = is_current || liveness.open.contains(&slug);
            ProjectEntry {
                indicator: project_indicator(loaded, liveness.is_working(&slug)),
                current: is_current,
                label,
                slug,
            }
        })
        .collect()
}

/// The edit targets that exist on disk for `project`, titled like the
/// palette's openers. Missing ones are skipped so no entry is dead.
pub fn edit_targets(project: &str) -> Vec<EditItem> {
    let mut targets = vec![(EditTarget::Project, "Edit Project Settings".to_string())];
    // Per-agent openers, from `agents/`, sorted for stability.
    if let Ok(dir) = shelbi_state::agents_dir(project) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut agents: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            agents.sort();
            for agent in agents {
                let title = format!("Edit {agent} Settings");
                targets.push((EditTarget::Agent(agent), title));
            }
        }
    }
    targets.push((EditTarget::ZenMode, "Edit Zen Mode".to_string()));
    targets.push((EditTarget::Workflows, "Edit Workflows".to_string()));
    targets.push((
        EditTarget::ChangelogTemplate,
        "Edit Changelog Template".to_string(),
    ));
    targets
        .into_iter()
        .filter(|(target, _)| {
            edit_target_path(project, target)
                .map(|p| p.exists())
                .unwrap_or(false)
        })
        .map(|(target, title)| EditItem { target, title })
        .collect()
}

/// The file (or, for [`EditTarget::Workflows`], the directory) an `edit:*`
/// command opens for `project`: the one path both clients hand the user's
/// editor. `None` when the project's directories can't be resolved.
pub fn edit_target_path(project: &str, target: &EditTarget) -> Option<PathBuf> {
    match target {
        EditTarget::Project => project_settings_path(project),
        EditTarget::Agent(agent) => Some(
            shelbi_state::agents_dir(project)
                .ok()?
                .join(agent)
                .join("instructions.md"),
        ),
        EditTarget::ZenMode => shelbi_state::zenmode_path(project).ok(),
        EditTarget::Workflows => shelbi_state::workflows_dir(project).ok(),
        EditTarget::ChangelogTemplate => shelbi_state::changelog_template_path(project).ok(),
    }
}

/// The project settings file: an in-repo `project.yaml` over the global
/// `projects/<name>.yaml`.
fn project_settings_path(project: &str) -> Option<PathBuf> {
    if let Ok(dir) = shelbi_state::config_project_dir(project) {
        let in_repo = dir.join("project.yaml");
        if in_repo.exists() {
            return Some(in_repo);
        }
    }
    Some(shelbi_state::projects_dir().ok()?.join(format!("{project}.yaml")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandRegistry;
    use crate::overlay::palette::ProjectIndicator;
    use crate::view::{NavItem, ReviewRow, ReviewState, WorkspaceBadge, WorkspaceRow};

    pub(crate) fn sidebar() -> SidebarModel {
        let mut s = SidebarModel::skeleton("alpha");
        s.nav = vec![
            NavItem {
                label: "Orchestrator".into(),
                view: View::Session("orch".into()),
            },
            NavItem {
                label: "Issues".into(),
                view: View::Issues,
            },
            NavItem {
                label: "Changelog".into(),
                view: View::Changelog,
            },
        ];
        s.workspaces = vec![WorkspaceRow {
            name: "alpha-1".into(),
            machine: "hub".into(),
            is_remote: false,
            current_task: Some("T-1".into()),
            agent: Some("developer".into()),
            badge: WorkspaceBadge::Working,
        }];
        s.reviews = vec![ReviewRow {
            task_id: "T-9".into(),
            title: "Review me".into(),
            branch: "user/T-9".into(),
            location: None,
            workspace: None,
            state: ReviewState::Pending,
            number: None,
            pr: None,
        }];
        s.board_loading = false;
        s.zen_mode = shelbi_state::ZenModeState::On;
        s
    }

    #[test]
    fn the_model_carries_nav_views_plus_machines_workspaces_and_reviews() {
        let m = command_model_from("alpha", &sidebar(), false, Vec::new(), Vec::new());
        assert!(m.zen_on);
        let ids: Vec<&str> = m.views.iter().map(|v| v.id.as_str()).collect();
        // Activity, Machines and Jobs follow the sidebar's nav as
        // palette-only views.
        assert_eq!(
            ids,
            ["view:orch", "view:tasks", "view:changelog", "view:activity", "view:machines", "view:jobs"]
        );
        assert_eq!(m.views[5].title, "Jobs");
        assert_eq!(m.views[5].view, View::Jobs);
        assert_eq!(m.views[2].decoration.as_ref().unwrap().glyph, "🚀");
        assert_eq!(m.views[3].title, "Activity");
        assert_eq!(m.views[3].decoration.as_ref().unwrap().glyph, "⚡");
        assert_eq!(m.views[0].decoration.as_ref().unwrap().glyph, "💬");
        assert_eq!(m.workspaces[0].subtitle.as_deref(), Some("T-1"));
        assert_eq!(m.reviews[0].task_id, "T-9");
    }

    #[test]
    fn palette_only_views_are_not_duplicated_when_the_nav_has_them() {
        let mut s = sidebar();
        s.nav.push(NavItem {
            label: "Machines".into(),
            view: View::Machines,
        });
        s.nav.push(NavItem {
            label: "Activity".into(),
            view: View::Activity,
        });
        let m = command_model_from("alpha", &s, false, Vec::new(), Vec::new());
        assert_eq!(m.views.iter().filter(|v| v.view == View::Machines).count(), 1);
        assert_eq!(m.views.iter().filter(|v| v.view == View::Activity).count(), 1);
        assert_eq!(m.views.iter().filter(|v| v.view == View::Jobs).count(), 1);
    }

    /// The palette's whole default list for a fixture state, in order: the
    /// order both clients show.
    #[test]
    fn the_registry_lists_a_fixture_state_in_palette_order() {
        let m = command_model_from(
            "alpha",
            &sidebar(),
            false,
            vec![ProjectItem {
                slug: "beta".into(),
                label: "Beta".into(),
            }],
            Vec::new(),
        );
        let blank: Vec<String> = shelbi_palette::search(&CommandRegistry::new().entries(&m), "")
            .into_iter()
            .map(|(e, _)| e.id)
            .collect();
        assert_eq!(
            blank,
            [
                "view:orch",
                "view:tasks",
                "view:changelog",
                "view:activity",
                "view:machines",
                "view:jobs",
                "action:toggle-zen",
                "review:T-9",
                "action:new-issue",
                "action:error-log",
                "action:settings",
                "action:send-feedback",
                "action:detach",
                "action:quit-project",
                "action:quit-shelbi",
            ]
        );
        let typed: Vec<String> = shelbi_palette::search(&CommandRegistry::new().entries(&m), "beta")
            .into_iter()
            .map(|(e, _)| e.id)
            .collect();
        assert_eq!(typed, ["action:switch-project:beta"]);
        // Typing "Jobs" offers the Jobs screen first.
        let jobs = shelbi_palette::search(&CommandRegistry::new().entries(&m), "Jobs");
        assert_eq!(jobs.first().map(|(e, _)| e.id.as_str()), Some("view:jobs"));
    }

    fn liveness(open: &[&str], working: &[&str]) -> ProjectLiveness {
        ProjectLiveness {
            open: open.iter().map(|p| p.to_string()).collect(),
            working: working.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn rows_of(rows: &[ProjectEntry]) -> Vec<(&str, ProjectIndicator, bool)> {
        rows.iter().map(|r| (r.slug.as_str(), r.indicator, r.current)).collect()
    }

    fn three() -> Vec<(String, String)> {
        vec![
            ("web".into(), "Web".into()),
            ("api".into(), "API".into()),
            ("alpha".into(), "Alpha".into()),
        ]
    }

    #[test]
    fn the_projects_column_leads_with_the_current_project() {
        let rows = projects_column_from("alpha", three(), &liveness(&["web"], &[]));
        assert_eq!(
            rows_of(&rows),
            [
                ("alpha", ProjectIndicator::LoadedIdle, true),
                ("web", ProjectIndicator::LoadedIdle, false),
                ("api", ProjectIndicator::Unloaded, false),
            ]
        );
    }

    #[test]
    fn project_rows_keep_list_order() {
        let rows = project_rows("alpha", three(), &liveness(&["web"], &["web"]));
        assert_eq!(
            rows_of(&rows),
            [
                ("web", ProjectIndicator::Working, false),
                ("api", ProjectIndicator::Unloaded, false),
                ("alpha", ProjectIndicator::LoadedIdle, true),
            ]
        );
    }

    /// The dot follows loaded and working only: the current project is the
    /// check, never the dot, and a working flag on a project that isn't
    /// loaded is stale.
    #[test]
    fn the_dot_follows_loaded_and_working_not_the_current_project() {
        let cases = [
            // (open, working, current, expected for "web")
            (false, false, false, ProjectIndicator::Unloaded),
            (false, true, false, ProjectIndicator::Unloaded),
            (true, false, false, ProjectIndicator::LoadedIdle),
            (true, true, false, ProjectIndicator::Working),
            // Current: loaded by this window, its dot by its work alone.
            (false, false, true, ProjectIndicator::LoadedIdle),
            (true, false, true, ProjectIndicator::LoadedIdle),
            (true, true, true, ProjectIndicator::Working),
            (false, true, true, ProjectIndicator::Working),
        ];
        for (open, working, current, expected) in cases {
            let live = liveness(if open { &["web"] } else { &[] }, if working { &["web"] } else { &[] });
            let rows = project_rows(if current { "web" } else { "other" }, vec![("web".into(), "Web".into())], &live);
            assert_eq!(rows[0].indicator, expected, "open={open} working={working} current={current}");
            assert_eq!(rows[0].current, current);
        }
    }

    #[test]
    fn nav_glyphs_match_the_sidebar() {
        assert_eq!(nav_glyph("Issues"), "📋");
        assert_eq!(nav_glyph("Changelog"), "🚀");
        assert_eq!(nav_glyph("Machines"), "🖥");
        assert_eq!(nav_glyph("Other"), "•");
    }
}
