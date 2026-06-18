use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Workspace / Session

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    #[serde(default)]
    pub projects: Vec<SessionProject>,
    #[serde(default)]
    pub startup: Vec<serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionProject {
    pub name: String,
    #[serde(default)]
    pub machines: Vec<String>,
}

// ---------------------------------------------------------------------------
// Project

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub repo: String,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    pub machines: Vec<Machine>,
    pub orchestrator: OrchestratorSpec,
    pub agent_runners: std::collections::BTreeMap<String, AgentRunnerSpec>,
    #[serde(default)]
    pub editor: Option<String>,
}

fn default_branch() -> String {
    "main".to_string()
}

impl Project {
    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.name == name)
    }

    pub fn runner(&self, name: &str) -> Option<&AgentRunnerSpec> {
        self.agent_runners.get(name)
    }
}

// ---------------------------------------------------------------------------
// Machine / Host

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    pub name: String,
    pub kind: MachineKind,
    pub work_dir: PathBuf,
    /// SSH hostname, required when `kind = ssh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineKind {
    Local,
    Ssh,
}

impl Machine {
    /// Effective host abstraction for shelling out tmux/git/etc.
    pub fn host(&self) -> Host {
        match (&self.kind, &self.host) {
            (MachineKind::Local, _) => Host::Local,
            (MachineKind::Ssh, Some(h)) => Host::Ssh { host: h.clone() },
            (MachineKind::Ssh, None) => Host::Ssh {
                host: self.name.clone(),
            },
        }
    }
}

/// Where a command runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Host {
    Local,
    Ssh { host: String },
}

impl Host {
    pub fn is_local(&self) -> bool {
        matches!(self, Host::Local)
    }
}

// ---------------------------------------------------------------------------
// Agent runner / orchestrator runner

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunnerSpec {
    /// Executable to invoke (e.g. "claude", "codex").
    pub command: String,
    /// Extra flags to append to every invocation.
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSpec {
    /// Name of an agent runner declared in `agent_runners`.
    pub runner: String,
}

// ---------------------------------------------------------------------------
// Worker / Agent state

/// Persistent state for a single worker agent.
///
/// Serialized as YAML frontmatter on disk. The markdown body lives separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub project: String,
    pub machine: String,
    pub runner: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub status: Status,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub tmux: TmuxAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Waiting,
    Done,
    Error,
    Archived,
}

impl Status {
    pub fn glyph(self) -> &'static str {
        match self {
            Status::Queued => "○",
            Status::Running => "●",
            Status::Waiting => "◐",
            Status::Done => "✓",
            Status::Error => "✗",
            Status::Archived => "·",
        }
    }
}

/// A tmux address — `session:window` (we keep pane implicit; one pane per worker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxAddr {
    pub session: String,
    pub window: String,
}

impl TmuxAddr {
    pub fn target(&self) -> String {
        format!("{}:{}", self.session, self.window)
    }
}

// ---------------------------------------------------------------------------
// Agent id validation

/// Validate an agent id: kebab-case alphanumerics, hyphen-separated.
pub fn validate_agent_id(s: &str) -> crate::Result<()> {
    if s.is_empty() {
        return Err(crate::Error::InvalidAgentId(s.to_string()));
    }
    let ok = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let starts_ok = s
        .chars()
        .next()
        .map(|c| c.is_ascii_alphanumeric())
        .unwrap_or(false);
    if !ok || !starts_ok {
        return Err(crate::Error::InvalidAgentId(s.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_validation() {
        assert!(validate_agent_id("fix-login-bug").is_ok());
        assert!(validate_agent_id("fix_login_bug").is_ok());
        assert!(validate_agent_id("abc123").is_ok());
        assert!(validate_agent_id("").is_err());
        assert!(validate_agent_id("-leading-hyphen").is_err());
        assert!(validate_agent_id("has spaces").is_err());
        assert!(validate_agent_id("slash/in/id").is_err());
    }

    #[test]
    fn status_glyphs_unique() {
        let glyphs = [
            Status::Queued.glyph(),
            Status::Running.glyph(),
            Status::Waiting.glyph(),
            Status::Done.glyph(),
            Status::Error.glyph(),
            Status::Archived.glyph(),
        ];
        let unique: std::collections::HashSet<_> = glyphs.iter().collect();
        assert_eq!(unique.len(), glyphs.len());
    }

    #[test]
    fn tmux_target_format() {
        let addr = TmuxAddr {
            session: "shelbi-daily".to_string(),
            window: "w-fix-login".to_string(),
        };
        assert_eq!(addr.target(), "shelbi-daily:w-fix-login");
    }
}
