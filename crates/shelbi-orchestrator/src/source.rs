//! The local [`ProjectSource`]: every read a client's views make of a project
//! on this machine, answered by the readers they called directly before
//! (`shelbi_app::source` has the seam).
//!
//! It lives here, beside the orchestrator readers it wraps, because
//! `shelbi-app` must not depend on this crate. It is also what a far machine
//! runs to answer a remote window's queries, so a local window and a remote
//! one read a project through the same code.

use std::sync::{Arc, Mutex};

use shelbi_app::activity::{read_activity_window_staged, ActivityMetaCache, ProjectInfo};
use shelbi_app::board::{self, BoardRead, ClosedPage};
use shelbi_app::board_search::{GithubSearch, SearchClient, SearchFailure, SearchPage};
use shelbi_app::changelog::{self, ChangelogData};
use shelbi_app::command::EditItem;
use shelbi_app::session::{ConnectFailure, Connected, Connector, SessionRef};
use shelbi_app::settings::summary::ProjectSummary;
use shelbi_app::settings::workflow_editor::ProjectGit;
use shelbi_app::settings::workspaces::PoolIndex;
use shelbi_app::source::{
    ActivityWindow, ProjectHandle, ProjectKey, ProjectLink, ProjectSource, ReviewOpenTarget, ReviewSlot,
    WorkspaceDetails,
};
use shelbi_app::view::{MachinesModel, SidebarModel};
use shelbi_core::Reachability;
use shelbi_proto::control::ExpectedState;
use shelbi_state::agents_config::AgentsConfig;
use shelbi_state::changelog::ChangelogStamp;
use shelbi_state::keymap::{KeymapDiagnostic, Keymaps};
use shelbi_state::workflow_config::WorkflowConfig;
use shelbi_state::{ErrorLogEntry, ZenModeState};

/// The handle a window on `name` (on this machine) owns: the local source,
/// and the link over this machine's daemon sockets and sessions.
pub fn local_handle(name: &str) -> ProjectHandle {
    ProjectHandle::new(
        Arc::new(LocalSource::new(name)),
        ProjectLink::local(name, Arc::new(LocalSessions)),
    )
}

/// One project on this machine, read from its files.
pub struct LocalSource {
    key: ProjectKey,
    /// The Activity feed's title cache, kept between reads so each resolves
    /// only ids it hasn't seen.
    activity: Mutex<ActivityMetaCache>,
}

impl LocalSource {
    pub fn new(name: impl Into<String>) -> Self {
        LocalSource {
            key: ProjectKey::local(name),
            activity: Mutex::new(ActivityMetaCache::default()),
        }
    }

    fn name(&self) -> &str {
        &self.key.name
    }
}

impl ProjectSource for LocalSource {
    fn key(&self) -> &ProjectKey {
        &self.key
    }

    fn sidebar(&self) -> Option<SidebarModel> {
        crate::sidebar_model::read_sidebar_model(self.name())
    }

    fn config_skeleton(&self) -> Option<SidebarModel> {
        crate::sidebar_model::read_config_skeleton(self.name())
    }

    fn project_exists(&self) -> bool {
        shelbi_state::load_project(self.name()).is_ok()
    }

    fn project_summary(&self) -> Result<ProjectSummary, String> {
        shelbi_app::settings::summary::read_project(self.name())
    }

    fn edit_targets(&self) -> Vec<EditItem> {
        shelbi_app::command_model::edit_targets(self.name())
    }

    fn open_board(&self) -> BoardRead {
        board::read_open_board(self.name())
    }

    fn closed_page(&self) -> Option<ClosedPage> {
        board::read_closed_page(self.name())
    }

    fn more_closed(&self, cursor: &str) -> Result<ClosedPage, String> {
        board::load_more_closed(self.name(), cursor)
    }

    fn search(&self, query: &str, page: u32) -> Result<SearchPage, SearchFailure> {
        GithubSearch.search(self.name(), query, page)
    }

    fn issue_state(&self, id: &str) -> Option<ExpectedState> {
        shelbi_app::exec_daemon::read_expected(self.name(), id)
    }

    fn activity_window(&self, window: usize, early: &mut dyn FnMut(ActivityWindow)) -> ActivityWindow {
        let info = ProjectInfo::read(self.name());
        let read = read_activity_window_staged(self.name(), &self.activity, window, |data| {
            early(ActivityWindow {
                info: info.clone(),
                data,
                more_titles: true,
            })
        });
        ActivityWindow {
            info,
            data: read.data,
            more_titles: read.more_titles,
        }
    }

    fn changelog(&self, unless: Option<&ChangelogStamp>) -> Option<ChangelogData> {
        let stamp = shelbi_state::changelog::changelog_stamp(self.name());
        if unless == Some(&stamp) {
            return None;
        }
        Some(changelog::read_changelog(self.name()))
    }

    fn errors(&self) -> Result<Vec<(ErrorLogEntry, bool)>, String> {
        shelbi_state::read_errors_with_unread(self.name()).map_err(|e| e.to_string())
    }

    fn zen_state(&self) -> Result<ZenModeState, String> {
        shelbi_state::read_state(self.name())
            .map(|s| s.zen_mode)
            .map_err(|e| e.to_string())
    }

    fn workspace_details(&self, ws: &str) -> Result<WorkspaceDetails, String> {
        crate::workspace_session::workspace_details(self.name(), ws).map_err(|e| e.to_string())
    }

    fn review_open_target(&self, task: &str) -> Result<ReviewOpenTarget, String> {
        crate::review_session::review_open_target(self.name(), task).map_err(|e| e.to_string())
    }

    fn review_slots(&self) -> Result<Vec<ReviewSlot>, String> {
        crate::load::review_slots(self.name()).map_err(|e| e.to_string())
    }

    fn machines(&self) -> Result<MachinesModel, String> {
        crate::machines_model::read_machines_model(self.name()).map_err(|e| e.to_string())
    }

    fn machine_reachability(&self, host: &str) -> Reachability {
        crate::machine::probe_host_reachability(host)
    }

    fn workflow_config(&self) -> Result<WorkflowConfig, String> {
        WorkflowConfig::load(self.name()).map_err(|e| e.to_string())
    }

    fn project_git(&self) -> ProjectGit {
        ProjectGit::load(self.name())
    }

    fn agents_config(&self) -> Result<AgentsConfig, String> {
        AgentsConfig::load(self.name()).map_err(|e| e.to_string())
    }

    fn workspaces_config(&self) -> Result<PoolIndex, String> {
        shelbi_app::settings::workspaces::read(self.name())
    }

    fn project_keymap(&self) -> (Keymaps, Vec<KeymapDiagnostic>) {
        shelbi_state::keymap::load_keymaps(Some(self.name()))
    }
}

/// This machine's sessions: [`crate::session_connect`]'s attach with replay
/// (the local sessions directory, or a workspace machine's relay).
pub struct LocalSessions;

impl Connector for LocalSessions {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
        crate::session_connect::connect(project, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_changelog_is_read_again_only_when_its_stamp_moved() {
        use shelbi_state::changelog::{add_entry, NewEntry};
        let _g = crate::test_lock::acquire();
        let home = std::env::temp_dir().join(format!("shelbi-source-cl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        let entry = |title: &str| NewEntry {
            title: title.into(),
            date: shelbi_state::changelog::now(),
            author: "orchestrator".into(),
            issues: Vec::new(),
            prs: Vec::new(),
            urls: Vec::new(),
            description: "Shipped.".into(),
        };

        let source = LocalSource::new("cl");
        add_entry("cl", entry("First")).unwrap();
        let first = source.changelog(None).expect("no stamp to match: read");
        assert_eq!(first.entries.len(), 1);
        assert!(source.changelog(Some(&first.stamp)).is_none(), "nothing changed");
        add_entry("cl", entry("Second")).unwrap();
        let second = source.changelog(Some(&first.stamp)).expect("an entry landed: read again");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(second.entries.len(), 2);
    }

    #[test]
    fn a_local_handle_names_its_project_on_this_machine() {
        let handle = local_handle("alpha");
        assert_eq!(handle.key(), &ProjectKey::local("alpha"));
        assert_eq!(handle.name(), "alpha");
        assert_eq!(handle.link.key(), handle.key());
    }
}
