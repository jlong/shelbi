//! Connecting an existing project to a tracker, or switching it: the
//! request every front end makes and its answer. The operation itself is
//! `shelbi_orchestrator::tracker_connect::apply_tracker_connection`; the CLI
//! (`shelbi tracker linear connect`, `shelbi tracker disconnect`) calls it
//! in process, and Settings sends a [`crate::far::Request::ConnectTracker`]
//! to the project's daemon (this machine's or the far one), which runs it
//! where the project lives.
//!
//! **One external integration per project**: a connection replaces the
//! project's whole `issue_tracker:` block, so it ends up with GitHub, Linear,
//! or neither (the local file system), never two.

use serde::{Deserialize, Serialize};
use shelbi_core::{GithubConnection, IssueTrackerBackend, IssueTrackerConfig, LinearConnection};

/// The board to connect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerChoice {
    /// The local markdown board (disconnects any external tracker).
    FileSystem,
    Github { repo: String },
    Linear(LinearConnection),
}

impl TrackerChoice {
    pub fn backend(&self) -> IssueTrackerBackend {
        match self {
            TrackerChoice::FileSystem => IssueTrackerBackend::FileSystem,
            TrackerChoice::Github { .. } => IssueTrackerBackend::Github,
            TrackerChoice::Linear(_) => IssueTrackerBackend::Linear,
        }
    }

    /// The `issue_tracker:` block this choice writes over `current`: only
    /// this choice's connection block, with `current`'s refresh cadence and
    /// budget kept (they aren't a connection).
    pub fn config(&self, current: &IssueTrackerConfig) -> IssueTrackerConfig {
        let mut cfg = IssueTrackerConfig {
            backend: self.backend(),
            refresh_secs: current.refresh_secs,
            budget: current.budget.clone(),
            ..IssueTrackerConfig::default()
        };
        match self {
            TrackerChoice::FileSystem => {}
            TrackerChoice::Github { repo } => cfg.github = Some(GithubConnection { repo: repo.trim().to_string() }),
            TrackerChoice::Linear(conn) => cfg.linear = Some(conn.clone()),
        }
        cfg
    }

    /// The choice a config is connected to now.
    pub fn from_config(cfg: &IssueTrackerConfig) -> Self {
        match cfg.backend {
            IssueTrackerBackend::Github => TrackerChoice::Github {
                repo: cfg.github.as_ref().map(|g| g.repo.clone()).unwrap_or_default(),
            },
            IssueTrackerBackend::Linear => TrackerChoice::Linear(cfg.linear.clone().unwrap_or_default()),
            IssueTrackerBackend::FileSystem | IssueTrackerBackend::Jira => TrackerChoice::FileSystem,
        }
    }
}

/// Connect (or switch, or disconnect) a project's tracker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectTracker {
    pub choice: TrackerChoice,
    /// Switch even though the current board has issues (they stay where
    /// they are, unseen by the project).
    #[serde(default)]
    pub force: bool,
}

/// What became of the Linear status catalog after a Linear connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSync {
    /// Not a Linear connection: nothing to sync.
    NotNeeded,
    /// The team's workflow states were synced into the project's catalog
    /// (a line saying what changed).
    Synced(String),
    /// The sync failed (the connection still stands): why.
    Failed(String),
}

/// A connection that was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connected {
    pub backend: IssueTrackerBackend,
    /// The backend before.
    pub previous: IssueTrackerBackend,
    /// The project YAML written.
    pub file: String,
    /// What the connection resolved to, one line (`Linear: ENG
    /// (Engineering) · label Bug`).
    pub summary: String,
    pub statuses: StatusSync,
}

/// Why a connection wasn't written. Nothing changed in any of these cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectError {
    /// The current board has issues, and switching would leave them behind.
    /// `message` says how to migrate them, or to force the switch.
    NotEmpty {
        from: IssueTrackerBackend,
        to: IssueTrackerBackend,
        issues: usize,
        message: String,
    },
    /// Whether the current board is empty couldn't be checked.
    Unverified { message: String },
    /// The new connection didn't pass its preflight: the fix.
    Preflight(String),
    /// Anything else (the project doesn't load, the write failed).
    Failed(String),
}

impl ConnectError {
    pub fn message(&self) -> &str {
        match self {
            ConnectError::NotEmpty { message, .. } | ConnectError::Unverified { message } => message,
            ConnectError::Preflight(m) | ConnectError::Failed(m) => m,
        }
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ConnectError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_writes_only_its_own_block_and_keeps_the_cadence() {
        let current = IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection { repo: "acme/app".into() }),
            refresh_secs: Some(90),
            ..IssueTrackerConfig::default()
        };
        let linear = TrackerChoice::Linear(LinearConnection::for_team("ENG")).config(&current);
        assert_eq!(linear.connection_blocks(), vec!["linear"]);
        assert_eq!(linear.refresh_secs, Some(90));
        linear.validate().unwrap();

        let fs = TrackerChoice::FileSystem.config(&IssueTrackerConfig::default());
        assert!(fs.is_default(), "the file system writes no block");
        assert_eq!(TrackerChoice::from_config(&current), TrackerChoice::Github { repo: "acme/app".into() });
        assert_eq!(TrackerChoice::from_config(&linear), TrackerChoice::Linear(LinearConnection::for_team("ENG")));
    }

    #[test]
    fn requests_round_trip_as_json() {
        let req = ConnectTracker { choice: TrackerChoice::Linear(LinearConnection::for_team("ENG")), force: true };
        let back: ConnectTracker = serde_json::from_value(serde_json::to_value(&req).unwrap()).unwrap();
        assert_eq!(back, req);
        let err = ConnectError::Preflight("no team".into());
        assert_eq!(err.to_string(), "no team");
    }
}
