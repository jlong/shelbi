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

pub mod flow;

use serde::{Deserialize, Serialize};
use shelbi_core::{AgentRules, GithubConnection, IssueTrackerBackend, IssueTrackerConfig, LinearConnection};

/// The agent rules' copy (04-agent-rules), shared by the connection flow and
/// Settings ▸ Properties ▸ Issue tracker.
pub mod rules_copy {
    /// "Always require human review before beginning work"
    /// ([`shelbi_core::AgentRules::require_review`]).
    pub const REVIEW_TITLE: &str = "Always require human review before beginning work";
    pub const REVIEW_HELP: &str =
        "You must move them out of the backlog status before work begins. Even in Zen mode.";
    /// "Only work on issues assigned to me"
    /// ([`shelbi_core::AgentRules::assigned_only`]); [`assigned_title`] adds
    /// the account.
    pub const ASSIGNED_TITLE: &str = "Only work on issues assigned to me";
    pub const ASSIGNED_HELP: &str = "Issues must be assigned to your user before work begins.";

    /// "Only work on issues assigned to me (ada)", or without the account
    /// while it isn't known.
    pub fn assigned_title(account: Option<&str>) -> String {
        match account.map(str::trim).filter(|a| !a.is_empty()) {
            Some(account) => format!("{ASSIGNED_TITLE} ({account})"),
            None => ASSIGNED_TITLE.to_string(),
        }
    }
}

/// Who the connected account is on GitHub, for "assigned to me (<account>)"
/// in Settings: the host runs [`crate::github_setup::GithubRequest::Account`]
/// once and answers. (The connection flow reads it through its
/// [`crate::github_setup::GithubChoice`]; Linear's is the key's viewer,
/// [`crate::linear_setup::LinearChoice::account`].)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GithubLogin {
    #[default]
    NotAsked,
    Asking,
    Known(String),
    /// `gh` isn't signed in (or couldn't be run): the title goes without.
    Failed(String),
}

impl GithubLogin {
    /// Mark it asked, returning whether the host should ask now.
    pub fn take_due(&mut self) -> bool {
        if *self == GithubLogin::NotAsked {
            *self = GithubLogin::Asking;
            return true;
        }
        false
    }

    pub fn answer(&mut self, result: Result<String, String>) {
        *self = match result {
            Ok(login) if !login.trim().is_empty() => GithubLogin::Known(login.trim().to_string()),
            Ok(_) => GithubLogin::Failed("no GitHub login".into()),
            Err(why) => GithubLogin::Failed(why),
        };
    }

    /// Answer from a [`crate::github_setup::GithubAnswer::Account`] (any
    /// other answer is ignored).
    pub fn answer_account(&mut self, answer: crate::github_setup::GithubAnswer) {
        if let crate::github_setup::GithubAnswer::Account(result) = answer {
            self.answer(result.map(|a| a.login).map_err(|e| format!("{e:?}")));
        }
    }

    pub fn login(&self) -> Option<&str> {
        match self {
            GithubLogin::Known(login) => Some(login),
            _ => None,
        }
    }
}

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
    /// this choice's connection block, with `current`'s refresh cadence,
    /// budget and agent rules kept (they aren't a connection).
    pub fn config(&self, current: &IssueTrackerConfig) -> IssueTrackerConfig {
        let mut cfg = IssueTrackerConfig {
            backend: self.backend(),
            refresh_secs: current.refresh_secs,
            budget: current.budget.clone(),
            agent_rules: current.agent_rules,
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
    /// The agent rules to write with the connection (the flow's agent-rules
    /// step, Settings' toggles). `None` keeps the project's current rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<AgentRules>,
}

impl ConnectTracker {
    /// Connect to `choice`, keeping the project's agent rules.
    pub fn new(choice: TrackerChoice, force: bool) -> Self {
        ConnectTracker { choice, force, rules: None }
    }

    /// Also write `rules`.
    pub fn with_rules(mut self, rules: AgentRules) -> Self {
        self.rules = Some(rules);
        self
    }

    /// The `issue_tracker:` block this request writes over `current`: the
    /// choice's ([`TrackerChoice::config`]) with the requested rules, if any.
    pub fn config(&self, current: &IssueTrackerConfig) -> IssueTrackerConfig {
        let mut cfg = self.choice.config(current);
        if let Some(rules) = self.rules {
            cfg.agent_rules = rules;
        }
        cfg
    }
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
    fn a_reconnect_keeps_the_agent_rules_unless_the_request_sets_them() {
        let rules = AgentRules { require_review: true, assigned_only: false };
        let current = IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection { repo: "acme/app".into() }),
            agent_rules: rules,
            ..IssueTrackerConfig::default()
        };
        let linear = TrackerChoice::Linear(LinearConnection::for_team("ENG"));
        assert_eq!(linear.config(&current).agent_rules, rules, "a switch keeps them");
        let req = ConnectTracker::new(linear, false);
        assert_eq!(req.config(&current).agent_rules, rules);
        let req = req.with_rules(AgentRules::default());
        assert_eq!(req.config(&current).agent_rules, AgentRules::default(), "the request's win");
    }

    #[test]
    fn requests_round_trip_as_json() {
        let req = ConnectTracker::new(TrackerChoice::Linear(LinearConnection::for_team("ENG")), true);
        let back: ConnectTracker = serde_json::from_value(serde_json::to_value(&req).unwrap()).unwrap();
        assert_eq!(back, req);
        // The rules ride along, and a request from before they existed reads
        // as "keep the project's".
        let ruled = req.clone().with_rules(AgentRules { require_review: true, assigned_only: false });
        let back: ConnectTracker = serde_json::from_value(serde_json::to_value(&ruled).unwrap()).unwrap();
        assert_eq!(back, ruled);
        let old: ConnectTracker =
            serde_json::from_str(r#"{"choice":"file_system","force":false}"#).unwrap();
        assert_eq!(old.rules, None);
    }

    #[test]
    fn the_assigned_rule_names_the_account_once_known() {
        assert_eq!(rules_copy::assigned_title(None), "Only work on issues assigned to me");
        assert_eq!(rules_copy::assigned_title(Some(" ")), "Only work on issues assigned to me");
        assert_eq!(rules_copy::assigned_title(Some("ada")), "Only work on issues assigned to me (ada)");
        let mut login = GithubLogin::default();
        assert!(login.take_due());
        assert!(!login.take_due(), "asked once");
        login.answer(Ok("octocat\n".into()));
        assert_eq!(login.login(), Some("octocat"));
        login.answer(Err("gh is not signed in".into()));
        assert_eq!(login.login(), None);
        let err = ConnectError::Preflight("no team".into());
        assert_eq!(err.to_string(), "no team");
    }
}
