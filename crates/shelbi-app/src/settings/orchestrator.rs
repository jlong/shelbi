//! The Orchestrator page's shared state (`awareness-setting`): how events
//! reach the project's orchestrator (`orchestrator.awareness:` in the project
//! YAML), picked from two radio options, and the Restart orchestrator button
//! that makes a change take effect.
//!
//! ```text
//!  Orchestrator
//!    Event delivery
//!    (•) Simple heartbeat system (v1)
//!        A periodic heartbeat keeps the orchestrator up to date.
//!    ( ) Awareness system (v2)
//!        Notifies the orchestrator only on board changes, reminders,
//!        finished jobs and overdue tasks.
//!    Applies when the orchestrator restarts.      [ Restart orchestrator ]
//! ```
//!
//! The setting lives beside `default_workflow:` in the project YAML, so the
//! page loads and saves it through the project's
//! [`WorkflowConfig`](shelbi_state::workflow_config::WorkflowConfig): the
//! same hash-checked, comment-preserving plan, sent to the daemon that owns
//! the project (this machine's, or the far machine's for a remote window).
//! Picking an option saves it at once, as the General page's controls do;
//! until the write lands the radio shows the new choice as pending, and a
//! failed write puts it back and says why.
//!
//! The orchestrator reads the mode when it launches, so the page says the
//! change applies on restart and offers Restart orchestrator: the project's
//! daemon asks the orchestrator for its handoff and replaces its session, as
//! `shelbi reload` does, with every worker left running ([`restart`]).
//!
//! Toolkit-free: the TUI's and the desktop app's Orchestrator pages both draw
//! [`OrchestratorForm`] with the labels here.

use shelbi_core::AwarenessMode;
use shelbi_state::workflow_config::WorkflowConfig;

use crate::settings::store::{self, ConfigStore};
use crate::source::ProjectLink;
use crate::state_write::{self, StateWrite};

/// The page's one group's heading.
pub const EVENT_DELIVERY_LABEL: &str = "Event delivery";
/// The note beside the Restart button.
pub const APPLIES_NOTE: &str = "Applies when the orchestrator restarts.";
/// The Restart button.
pub const RESTART_LABEL: &str = "Restart orchestrator";

/// The modes, in the order the radio lists them.
pub const MODES: [AwarenessMode; 2] = AwarenessMode::ALL;

/// A mode's radio label.
pub fn mode_label(mode: AwarenessMode) -> &'static str {
    match mode {
        AwarenessMode::Heartbeat => "Simple heartbeat system (v1)",
        AwarenessMode::Awareness => "Awareness system (v2)",
    }
}

/// A mode's description, under its label.
pub fn mode_description(mode: AwarenessMode) -> &'static str {
    match mode {
        AwarenessMode::Heartbeat => "A periodic heartbeat keeps the orchestrator up to date.",
        AwarenessMode::Awareness => {
            "Notifies the orchestrator only on board changes, reminders, finished jobs and overdue tasks."
        }
    }
}

/// Where the last save of the setting stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SaveStatus {
    #[default]
    Idle,
    Saving,
    Saved,
    Failed(String),
}

/// Where the last Restart orchestrator stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RestartStatus {
    #[default]
    Idle,
    /// The daemon is handing off and replacing the session (it can take as
    /// long as the orchestrator's handoff).
    Restarting,
    Restarted,
    Failed(String),
}

impl RestartStatus {
    /// The line the page shows for it, if any.
    pub fn message(&self) -> Option<String> {
        match self {
            RestartStatus::Idle => None,
            RestartStatus::Restarting => Some("Restarting the orchestrator…".into()),
            RestartStatus::Restarted => Some("Orchestrator restarted.".into()),
            RestartStatus::Failed(e) => Some(format!("Restart failed: {e}")),
        }
    }
}

/// The Orchestrator page's state: the project's config as loaded, the choice
/// being saved, and the restart in flight.
#[derive(Debug, Clone, Default)]
pub struct OrchestratorForm {
    /// The project's config as read; `None` until it is, or while it
    /// doesn't read (then nothing can be saved over it).
    config: Option<WorkflowConfig>,
    unreadable: Option<String>,
    /// The mode being written.
    pending: Option<AwarenessMode>,
    status: SaveStatus,
    restart: RestartStatus,
}

impl OrchestratorForm {
    /// The config was read: `Ok` with it, or why it doesn't read. Ignored
    /// while a save is in flight (its result is newer).
    pub fn load(&mut self, read: Result<WorkflowConfig, String>) {
        if self.pending.is_some() {
            return;
        }
        match read {
            Ok(config) => {
                self.config = Some(config);
                self.unreadable = None;
            }
            Err(e) => {
                self.config = None;
                self.unreadable = Some(e);
            }
        }
    }

    /// Forget everything (another project is shown). A save or restart still
    /// in flight is the client's to drop by its ticket.
    pub fn reset(&mut self) {
        *self = OrchestratorForm::default();
    }

    /// Whether the config has been read (or failed to).
    pub fn loaded(&self) -> bool {
        self.config.is_some() || self.unreadable.is_some()
    }

    /// Whether the config read: the radio has a setting to show.
    pub fn has_config(&self) -> bool {
        self.config.is_some()
    }

    /// Why the config doesn't read, if it doesn't.
    pub fn unreadable(&self) -> Option<&str> {
        self.unreadable.as_deref()
    }

    /// Whether the radio takes a change: the config read, and no save in
    /// flight.
    pub fn editable(&self) -> bool {
        self.config.is_some() && self.pending.is_none()
    }

    /// Whether a save is in flight.
    pub fn saving(&self) -> bool {
        self.pending.is_some()
    }

    /// The mode the radio shows: the one being saved, else the file's.
    pub fn mode(&self) -> AwarenessMode {
        self.pending
            .or_else(|| self.config.as_ref().map(WorkflowConfig::awareness))
            .unwrap_or_default()
    }

    /// Pick `mode`: the config to save ([`save`]), or `None` when it is
    /// already the file's or the form can't save.
    pub fn choose(&mut self, mode: AwarenessMode) -> Option<WorkflowConfig> {
        if !self.editable() || self.mode() == mode {
            return None;
        }
        let next = self.config.as_ref()?.set_awareness(mode);
        self.pending = Some(mode);
        self.status = SaveStatus::Saving;
        Some(next)
    }

    /// The mode `steps` places from the shown one, wrapping (the radio's
    /// arrow keys).
    pub fn step(&mut self, steps: isize) -> Option<WorkflowConfig> {
        let at = MODES.iter().position(|m| *m == self.mode()).unwrap_or(0) as isize;
        self.choose(MODES[(at + steps).rem_euclid(MODES.len() as isize) as usize])
    }

    /// The save landed: on success the form holds the config as now on
    /// disk; on failure the radio goes back to the file's and the status
    /// says why.
    pub fn saved_result(&mut self, result: Result<WorkflowConfig, String>) {
        if self.pending.take().is_none() {
            return;
        }
        match result {
            Ok(config) => {
                self.config = Some(config);
                self.status = SaveStatus::Saved;
            }
            Err(e) => self.status = SaveStatus::Failed(e),
        }
    }

    pub fn status(&self) -> &SaveStatus {
        &self.status
    }

    /// The save's line on the page, if any.
    pub fn status_message(&self) -> Option<String> {
        match &self.status {
            SaveStatus::Idle => None,
            SaveStatus::Saving => Some("Saving…".into()),
            SaveStatus::Saved => Some(match &self.config {
                Some(c) => format!("Saved to {}", crate::settings::summary::display_path(c.project_path())),
                None => "Saved".into(),
            }),
            SaveStatus::Failed(e) => Some(e.clone()),
        }
    }

    /// Start a restart: `false` (nothing to start) while one is in flight.
    pub fn begin_restart(&mut self) -> bool {
        if self.restarting() {
            return false;
        }
        self.restart = RestartStatus::Restarting;
        true
    }

    pub fn restarting(&self) -> bool {
        self.restart == RestartStatus::Restarting
    }

    /// The restart finished.
    pub fn restart_result(&mut self, result: Result<(), String>) {
        self.restart = match result {
            Ok(()) => RestartStatus::Restarted,
            Err(e) => RestartStatus::Failed(e),
        };
    }

    pub fn restart_status(&self) -> &RestartStatus {
        &self.restart
    }

    /// Forget the save and restart lines (a new visit to the page).
    pub fn clear_status(&mut self) {
        if self.pending.is_none() {
            self.status = SaveStatus::Idle;
        }
        if !self.restarting() {
            self.restart = RestartStatus::Idle;
        }
    }
}

/// Write `config` (from [`OrchestratorForm::choose`]) through `store` and read
/// it back. Blocking: run it off the UI thread.
pub fn save(store: &dyn ConfigStore, config: &WorkflowConfig) -> Result<WorkflowConfig, String> {
    store::save_workflows(store, config)
        .map(|saved| saved.config)
        .map_err(|e| e.to_string())
}

/// Restart `project`'s orchestrator through the window's `link` (`None`: this
/// machine's daemon): the daemon hands off and replaces the orchestrator
/// session, workers left running. Blocking: run it off the UI thread.
pub fn restart(project: &str, link: Option<&ProjectLink>) -> Result<(), String> {
    state_write::write(project, link, StateWrite::RestartOrchestrator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_labels_match_the_wireframe() {
        assert_eq!(MODES, [AwarenessMode::Heartbeat, AwarenessMode::Awareness]);
        assert_eq!(mode_label(AwarenessMode::Heartbeat), "Simple heartbeat system (v1)");
        assert_eq!(mode_label(AwarenessMode::Awareness), "Awareness system (v2)");
        assert_eq!(APPLIES_NOTE, "Applies when the orchestrator restarts.");
        assert_eq!(RESTART_LABEL, "Restart orchestrator");
    }

    #[test]
    fn nothing_saves_before_the_config_reads() {
        let mut form = OrchestratorForm::default();
        assert!(!form.loaded());
        assert_eq!(form.mode(), AwarenessMode::Heartbeat);
        assert!(form.choose(AwarenessMode::Awareness).is_none());
        form.load(Err("no project YAML".into()));
        assert!(form.loaded());
        assert_eq!(form.unreadable(), Some("no project YAML"));
        assert!(form.step(1).is_none());
        assert_eq!(form.status(), &SaveStatus::Idle);
    }

    #[test]
    fn picking_a_mode_saves_it_to_the_project_yaml() {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = crate::settings::agents::fixture::Home::new("orch", "orch-page", lock);
        let store = store::Disk::store("orch-page");
        let mut form = OrchestratorForm::default();
        form.load(store.workflows());
        assert!(form.editable());
        assert_eq!(form.mode(), AwarenessMode::Heartbeat);
        assert!(form.choose(AwarenessMode::Heartbeat).is_none(), "already the file's");

        let next = form.step(1).expect("a change to save");
        assert_eq!(form.mode(), AwarenessMode::Awareness, "pending shows at once");
        assert!(!form.editable() && form.status() == &SaveStatus::Saving);
        assert!(form.choose(AwarenessMode::Heartbeat).is_none(), "one save at a time");
        form.load(store.workflows());
        assert_eq!(form.mode(), AwarenessMode::Awareness, "a reload doesn't undo a pending save");
        form.saved_result(save(store.as_ref(), &next));
        assert_eq!(form.status(), &SaveStatus::Saved);
        assert!(form.status_message().unwrap().starts_with("Saved to "));
        assert_eq!(form.mode(), AwarenessMode::Awareness);
        let project = shelbi_state::load_project("orch-page").unwrap();
        assert_eq!(project.orchestrator.awareness, AwarenessMode::Awareness);

        // A failed write puts the radio back on the file's.
        let _ = form.choose(AwarenessMode::Heartbeat).unwrap();
        form.saved_result(Err("changed since it was loaded".into()));
        assert_eq!(form.mode(), AwarenessMode::Awareness);
        assert_eq!(form.status_message().as_deref(), Some("changed since it was loaded"));
        drop(home);
    }

    #[test]
    fn a_restart_runs_one_at_a_time() {
        let mut form = OrchestratorForm::default();
        assert!(form.begin_restart());
        assert!(!form.begin_restart(), "already restarting");
        form.clear_status();
        assert!(form.restarting(), "a restart in flight survives a revisit");
        form.restart_result(Err("handoff refused".into()));
        assert_eq!(form.restart_status().message().as_deref(), Some("Restart failed: handoff refused"));
        assert!(form.begin_restart());
        form.restart_result(Ok(()));
        assert_eq!(form.restart_status(), &RestartStatus::Restarted);
        form.clear_status();
        assert_eq!(form.restart_status(), &RestartStatus::Idle);
    }
}
