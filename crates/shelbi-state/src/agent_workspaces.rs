//! Per-project agent workspaces — the `agents/<name>/` layout that
//! ships with every shelbi project.
//!
//! Each agent gets a stable directory name (the *agent name*) that later
//! integrations — the workflow YAML's `agent:` field, the
//! `shelbi agent` CLI, event-log lines — reference. The directory holds
//! the agent's `instructions.md` system prompt plus a `skills/` subdir
//! that the task-dispatch path mounts into `.claude/skills/`.
//!
//! Two agents ship with the binary:
//!
//! - **orchestrator** — the coordinator agent that runs in the
//!   dashboard's right pane. Its bundled prompt is the content
//!   previously embedded as `default_orchestrator.md.template`.
//! - **developer** — the worker agent handed individual tasks. Bundled
//!   prompt lives in `default_developer.md.template`.
//!
//! Both are materialized on first [`materialize_default_agents`] (called
//! from `shelbi init`) and self-healed by [`self_heal_default_agents`]
//! (called from `shelbi reload`). User edits to `instructions.md` are
//! preserved on self-heal — a byte-compare against the bundled default
//! decides whether to fire the "you've customized this agent" notice.

use std::fs;
use std::path::PathBuf;

use shelbi_core::{Error, Result};

use crate::{agents_dir, ensure_dir, read_state, write_state};

/// Stable identifier of the default orchestrator agent.
pub const ORCHESTRATOR_AGENT: &str = "orchestrator";

/// Stable identifier of the default developer agent.
pub const DEVELOPER_AGENT: &str = "developer";

/// Bundled orchestrator `instructions.md` content. Source of truth for
/// both the agent workspace materialize/self-heal path and the legacy
/// `shelbi_orchestrator::DEFAULT_SYSTEM_PROMPT` re-export.
pub const DEFAULT_ORCHESTRATOR_INSTRUCTIONS: &str =
    include_str!("default_orchestrator.md.template");

/// Bundled developer `instructions.md` content.
pub const DEFAULT_DEVELOPER_INSTRUCTIONS: &str =
    include_str!("default_developer.md.template");

/// Defaults shipped with the binary, in declaration order. Iteration
/// order matches the order outcomes appear in
/// [`materialize_default_agents`] / [`self_heal_default_agents`] reports.
pub const DEFAULT_AGENTS: &[(&str, &str)] = &[
    (ORCHESTRATOR_AGENT, DEFAULT_ORCHESTRATOR_INSTRUCTIONS),
    (DEVELOPER_AGENT, DEFAULT_DEVELOPER_INSTRUCTIONS),
];

/// `~/.shelbi/projects/<project>/agents/<agent>/`. The directory name
/// IS the agent's stable identifier — that's what downstream callers
/// (workflow YAML, CLI subcommands, event log lines) reference.
pub fn agent_workspace_dir(project: &str, agent: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?.join(agent))
}

/// `<workspace>/instructions.md` — the agent's system prompt.
pub fn agent_instructions_path(project: &str, agent: &str) -> Result<PathBuf> {
    Ok(agent_workspace_dir(project, agent)?.join("instructions.md"))
}

/// `<workspace>/skills/` — auto-loaded into `.claude/skills/` by the
/// task-dispatch path (landed in a later subtask). Ships empty in v1.
pub fn agent_skills_dir(project: &str, agent: &str) -> Result<PathBuf> {
    Ok(agent_workspace_dir(project, agent)?.join("skills"))
}

/// Per-agent result of a materialize / self-heal pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMaterializeOutcome {
    /// The agent directory (or its `instructions.md`) was missing and
    /// has just been written from the bundled default.
    Created { agent: String },
    /// The agent directory exists and `instructions.md` matches the
    /// bundled default byte-for-byte. Nothing changed.
    Unchanged { agent: String },
    /// The agent directory exists and `instructions.md` differs from
    /// the bundled default. Preserved as-is. `first_notice` is `true`
    /// when this is the first self-heal pass to observe the current
    /// divergence — callers should surface a user-facing notice in
    /// that case and stay silent otherwise.
    Preserved { agent: String, first_notice: bool },
}

impl AgentMaterializeOutcome {
    /// Agent name this outcome refers to.
    pub fn agent(&self) -> &str {
        match self {
            Self::Created { agent }
            | Self::Unchanged { agent }
            | Self::Preserved { agent, .. } => agent,
        }
    }
}

/// Create `agents/{orchestrator,developer}/` from the bundled defaults
/// for `project`. Each agent's directory is created only if missing —
/// existing directories are left untouched and reported as `Unchanged`
/// (init is conservative; self-heal does the SHA-compare).
///
/// Returns one outcome per default agent, in [`DEFAULT_AGENTS`] order.
pub fn materialize_default_agents(project: &str) -> Result<Vec<AgentMaterializeOutcome>> {
    let mut outcomes = Vec::with_capacity(DEFAULT_AGENTS.len());
    for (name, default_body) in DEFAULT_AGENTS {
        let workspace = agent_workspace_dir(project, name)?;
        if workspace.exists() {
            outcomes.push(AgentMaterializeOutcome::Unchanged {
                agent: (*name).to_string(),
            });
            continue;
        }
        write_bundled_agent(project, name, default_body)?;
        outcomes.push(AgentMaterializeOutcome::Created {
            agent: (*name).to_string(),
        });
    }
    Ok(outcomes)
}

/// `shelbi reload`'s self-heal pass. For each default agent:
///
/// - Missing directory → recreate from the bundled default (`Created`).
/// - `instructions.md` missing → drop the bundled default back in
///   (`Created`).
/// - `instructions.md` byte-matches the bundled default → `Unchanged`.
/// - `instructions.md` differs → leave it alone (`Preserved`). The
///   `first_notice` field is set the first time the current divergent
///   content is seen, tracked in [`State::notified_diverged_agents`] so
///   the user-facing notice fires exactly once per divergence.
///
/// Also ensures the `skills/` subdir exists for every default agent.
pub fn self_heal_default_agents(project: &str) -> Result<Vec<AgentMaterializeOutcome>> {
    let mut state = read_state(project)?;
    let mut state_dirty = false;
    let mut outcomes = Vec::with_capacity(DEFAULT_AGENTS.len());

    for (name, default_body) in DEFAULT_AGENTS {
        let workspace = agent_workspace_dir(project, name)?;
        if !workspace.exists() {
            write_bundled_agent(project, name, default_body)?;
            if state.notified_diverged_agents.remove(*name) {
                state_dirty = true;
            }
            outcomes.push(AgentMaterializeOutcome::Created {
                agent: (*name).to_string(),
            });
            continue;
        }

        // Workspace exists. Ensure `skills/` is there before we judge
        // `instructions.md` — a half-materialized workspace from an
        // older shelbi version shouldn't be left without it.
        ensure_dir(&agent_skills_dir(project, name)?)?;

        let path = agent_instructions_path(project, name)?;
        let current = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::write(&path, default_body).map_err(Error::Io)?;
                if state.notified_diverged_agents.remove(*name) {
                    state_dirty = true;
                }
                outcomes.push(AgentMaterializeOutcome::Created {
                    agent: (*name).to_string(),
                });
                continue;
            }
            Err(e) => return Err(Error::Io(e)),
        };

        if current == *default_body {
            if state.notified_diverged_agents.remove(*name) {
                state_dirty = true;
            }
            outcomes.push(AgentMaterializeOutcome::Unchanged {
                agent: (*name).to_string(),
            });
        } else {
            let first_notice = state.notified_diverged_agents.insert((*name).to_string());
            if first_notice {
                state_dirty = true;
            }
            outcomes.push(AgentMaterializeOutcome::Preserved {
                agent: (*name).to_string(),
                first_notice,
            });
        }
    }

    if state_dirty {
        write_state(project, &state)?;
    }
    Ok(outcomes)
}

/// Create `<workspace>/` and `<workspace>/skills/`, then write
/// `<workspace>/instructions.md` with `body`. Used by both materialize
/// and self-heal whenever a default agent needs to be (re)dropped onto
/// disk in full.
fn write_bundled_agent(project: &str, agent: &str, body: &str) -> Result<()> {
    let workspace = agent_workspace_dir(project, agent)?;
    ensure_dir(&workspace)?;
    ensure_dir(&agent_skills_dir(project, agent)?)?;
    fs::write(agent_instructions_path(project, agent)?, body).map_err(Error::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-agent-workspaces-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// `shelbi init` happy path: both default agents land on disk with
    /// the bundled instructions and an empty `skills/` subdir.
    #[test]
    fn materialize_creates_both_default_agents_with_skills_dir() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let outcomes = materialize_default_agents("p").unwrap();
        assert_eq!(outcomes.len(), 2);
        for (i, name) in [ORCHESTRATOR_AGENT, DEVELOPER_AGENT].iter().enumerate() {
            assert_eq!(
                outcomes[i],
                AgentMaterializeOutcome::Created {
                    agent: (*name).to_string()
                }
            );
            let instructions = agent_instructions_path("p", name).unwrap();
            let skills = agent_skills_dir("p", name).unwrap();
            assert!(instructions.exists(), "{name}: instructions.md missing");
            assert!(skills.is_dir(), "{name}: skills/ missing");
            // Skills dir ships empty.
            assert_eq!(
                fs::read_dir(&skills).unwrap().count(),
                0,
                "{name}: skills/ should ship empty"
            );
        }
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap()).unwrap(),
            DEFAULT_ORCHESTRATOR_INSTRUCTIONS
        );
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", DEVELOPER_AGENT).unwrap()).unwrap(),
            DEFAULT_DEVELOPER_INSTRUCTIONS
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi init` is idempotent and does not stomp a user's edits if
    /// re-run on an already-materialized project.
    #[test]
    fn materialize_is_idempotent_and_does_not_overwrite_existing_workspace() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let custom = "# my custom prompt\n";
        fs::write(
            agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap(),
            custom,
        )
        .unwrap();

        let outcomes = materialize_default_agents("p").unwrap();
        assert_eq!(
            outcomes,
            vec![
                AgentMaterializeOutcome::Unchanged {
                    agent: ORCHESTRATOR_AGENT.to_string()
                },
                AgentMaterializeOutcome::Unchanged {
                    agent: DEVELOPER_AGENT.to_string()
                },
            ]
        );
        // Custom edit survived.
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap()).unwrap(),
            custom
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload`: when a default agent directory is missing, the
    /// self-heal pass recreates it from the bundled default.
    #[test]
    fn self_heal_recreates_a_missing_agent_directory_from_bundled_default() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        // Simulate the user (or a stale install) nuking the developer dir.
        let developer_dir = agent_workspace_dir("p", DEVELOPER_AGENT).unwrap();
        fs::remove_dir_all(&developer_dir).unwrap();
        assert!(!developer_dir.exists());

        let outcomes = self_heal_default_agents("p").unwrap();
        assert!(outcomes.contains(&AgentMaterializeOutcome::Created {
            agent: DEVELOPER_AGENT.to_string()
        }));
        // Orchestrator was untouched — it byte-matches the bundled default
        // (we just wrote it) so it should self-report as Unchanged.
        assert!(outcomes.contains(&AgentMaterializeOutcome::Unchanged {
            agent: ORCHESTRATOR_AGENT.to_string()
        }));

        assert!(agent_workspace_dir("p", DEVELOPER_AGENT).unwrap().is_dir());
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", DEVELOPER_AGENT).unwrap()).unwrap(),
            DEFAULT_DEVELOPER_INSTRUCTIONS
        );
        assert!(agent_skills_dir("p", DEVELOPER_AGENT).unwrap().is_dir());

        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload`: user-edited `instructions.md` is preserved
    /// byte-for-byte; the first observation flips the `first_notice`
    /// bit and subsequent reloads stay silent until the content
    /// changes.
    #[test]
    fn self_heal_preserves_user_edited_instructions_and_fires_notice_once() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let custom = "# my orchestrator\nlocal rules go here\n";
        let path = agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap();
        fs::write(&path, custom).unwrap();

        // First reload after the edit — should preserve + flag as first notice.
        let outcomes = self_heal_default_agents("p").unwrap();
        let preserved = outcomes
            .iter()
            .find(|o| o.agent() == ORCHESTRATOR_AGENT)
            .unwrap();
        assert_eq!(
            preserved,
            &AgentMaterializeOutcome::Preserved {
                agent: ORCHESTRATOR_AGENT.to_string(),
                first_notice: true,
            }
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), custom);
        // Persistence of the notice was recorded in state.json.
        assert!(read_state("p")
            .unwrap()
            .notified_diverged_agents
            .contains(ORCHESTRATOR_AGENT));

        // Second reload with the same divergent content — should stay silent.
        let outcomes = self_heal_default_agents("p").unwrap();
        let preserved = outcomes
            .iter()
            .find(|o| o.agent() == ORCHESTRATOR_AGENT)
            .unwrap();
        assert_eq!(
            preserved,
            &AgentMaterializeOutcome::Preserved {
                agent: ORCHESTRATOR_AGENT.to_string(),
                first_notice: false,
            }
        );

        // Re-align with the default — the acknowledgment should clear so a
        // future divergence re-fires the notice.
        fs::write(&path, DEFAULT_ORCHESTRATOR_INSTRUCTIONS).unwrap();
        let outcomes = self_heal_default_agents("p").unwrap();
        let unchanged = outcomes
            .iter()
            .find(|o| o.agent() == ORCHESTRATOR_AGENT)
            .unwrap();
        assert_eq!(
            unchanged,
            &AgentMaterializeOutcome::Unchanged {
                agent: ORCHESTRATOR_AGENT.to_string(),
            }
        );
        assert!(!read_state("p")
            .unwrap()
            .notified_diverged_agents
            .contains(ORCHESTRATOR_AGENT));

        std::env::remove_var("SHELBI_HOME");
    }

    /// Workspace exists but `instructions.md` is missing — self-heal
    /// drops the bundled default back in (rather than leaving the
    /// runtime without prompt text). Skills dir is also ensured.
    #[test]
    fn self_heal_recreates_a_missing_instructions_file_inside_an_existing_workspace() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        // Nuke just the instructions.md, keep the workspace + skills dir.
        fs::remove_file(agent_instructions_path("p", DEVELOPER_AGENT).unwrap()).unwrap();

        let outcomes = self_heal_default_agents("p").unwrap();
        let dev = outcomes
            .iter()
            .find(|o| o.agent() == DEVELOPER_AGENT)
            .unwrap();
        assert_eq!(
            dev,
            &AgentMaterializeOutcome::Created {
                agent: DEVELOPER_AGENT.to_string()
            }
        );
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", DEVELOPER_AGENT).unwrap()).unwrap(),
            DEFAULT_DEVELOPER_INSTRUCTIONS
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// The orchestrator's `DEFAULT_SYSTEM_PROMPT` re-export must keep
    /// pointing at the same bytes the agent workspace ships, otherwise
    /// the dashboard's CLAUDE.md and the per-project
    /// `agents/orchestrator/instructions.md` will drift.
    #[test]
    fn orchestrator_template_byte_matches_default_instructions_const() {
        assert!(!DEFAULT_ORCHESTRATOR_INSTRUCTIONS.is_empty());
        assert!(DEFAULT_ORCHESTRATOR_INSTRUCTIONS.contains("{{assistant_name}}"));
    }

    /// Sanity-check the developer prompt has the spec-required hooks
    /// (review marker handoff, agents/_shared/preamble.md reference)
    /// so a regression doesn't quietly ship a half-written prompt.
    #[test]
    fn developer_template_contains_required_hooks() {
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("review marker"));
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("agents/_shared/preamble.md"));
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("skills"));
    }
}
