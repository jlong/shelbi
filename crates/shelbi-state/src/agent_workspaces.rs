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

use crate::{agents_dir, ensure_dir, load_shelbi_config, project_dir, read_state, write_state};

/// Stable identifier of the default orchestrator agent.
pub const ORCHESTRATOR_AGENT: &str = "orchestrator";

/// Stable identifier of the default developer agent.
pub const DEVELOPER_AGENT: &str = "developer";

/// Reserved subdirectory name under `agents/` that holds the per-project
/// shared preamble — project-wide context prepended to every agent's
/// `instructions.md` at dispatch time. NOT an agent itself; the
/// [`list_agents`] / `shelbi agent new` paths skip names starting with
/// `_` so this stays out of the agent list.
pub const SHARED_AGENT_DIR: &str = "_shared";

/// File name of the project-wide preamble that gets prepended to every
/// dispatched agent's `instructions.md`. Optional — absent file means
/// agents see their own instructions verbatim.
pub const SHARED_PREAMBLE_FILE: &str = "preamble.md";

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

/// `~/.shelbi/projects/<project>/agents/_shared/preamble.md` — the
/// optional, per-project shared preamble that gets prepended to every
/// dispatched agent's `instructions.md`.
pub fn agent_shared_preamble_path(project: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?
        .join(SHARED_AGENT_DIR)
        .join(SHARED_PREAMBLE_FILE))
}

/// Read the per-project shared preamble if it exists, otherwise return
/// `None`. The preamble is optional — a missing file is a normal state
/// (not every project has shared context to inject), not an error.
pub fn load_shared_preamble(project: &str) -> Result<Option<String>> {
    let path = agent_shared_preamble_path(project)?;
    match fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Build the system prompt for `agent` in `project` by composing
/// `agents/_shared/preamble.md` (if present) with the agent's own
/// `instructions.md`. The two halves are joined with a single blank line
/// so the agent's first H1 doesn't collide with the preamble's tail.
///
/// `{{assistant_name}}` is substituted in the composed body using the
/// user's chosen assistant name from `~/.shelbi/shelbi.yaml` (falling
/// back to [`crate::DEFAULT_ASSISTANT_NAME`] when the wizard hasn't
/// run). Agent prompts that don't reference the placeholder are
/// unaffected — `String::replace` on a no-match is free.
pub fn compose_agent_prompt(project: &str, agent: &str) -> Result<String> {
    let instructions_path = agent_instructions_path(project, agent)?;
    let instructions = fs::read_to_string(&instructions_path).map_err(|e| {
        Error::Other(format!(
            "agent `{agent}` instructions.md unreadable at {}: {e}",
            instructions_path.display(),
        ))
    })?;
    let preamble = load_shared_preamble(project)?;
    let composed = match preamble {
        Some(p) => {
            let mut out = String::with_capacity(p.len() + instructions.len() + 2);
            out.push_str(&p);
            if !p.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(&instructions);
            out
        }
        None => instructions,
    };
    let cfg = load_shelbi_config()?;
    Ok(composed.replace("{{assistant_name}}", cfg.assistant_name()))
}

/// Path to the legacy orchestrator `CLAUDE.md` file that pre-shelbi
/// versions wrote at `~/.shelbi/projects/<project>/CLAUDE.md`. Kept as a
/// helper so the migration-hint code path and a future cleanup utility
/// agree on the location.
pub fn legacy_claude_md_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("CLAUDE.md"))
}

/// Emit a one-time hint when a legacy `CLAUDE.md` is still present at
/// the project root. The orchestrator no longer reads it on dispatch —
/// this nudges the user toward `agents/_shared/preamble.md`
/// (project-wide) and `agents/orchestrator/instructions.md`
/// (orchestrator-specific overrides) and to delete `CLAUDE.md` once the
/// migration is done.
///
/// Idempotent: the per-project [`State::claude_md_migration_hinted`]
/// flag gates emission so multiple workspace dispatches inside the same
/// orchestrator session only see the hint once. Reset the flag with
/// [`reset_claude_md_migration_hint`] at orchestrator startup.
///
/// Routed through `tracing::warn!` (not `eprintln!`) so TUI subcommands
/// — which init tracing with a file writer at `~/.shelbi/logs/tui.log`
/// — keep the hint off the alt-screen pane. CLI invocations from a
/// real shell still surface it on stderr via the default writer.
///
/// Best-effort — IO failures from `read_state`/`write_state` are
/// returned to the caller so the spawn path can decide whether to bail
/// or proceed. In practice every caller treats the hint as advisory and
/// uses `let _ = …` to ignore the result.
pub fn maybe_emit_claude_md_migration_hint(project: &str) -> Result<()> {
    let mut state = read_state(project)?;
    if state.claude_md_migration_hinted {
        return Ok(());
    }
    let path = legacy_claude_md_path(project)?;
    if !path.exists() {
        return Ok(());
    }
    tracing::warn!(
        project,
        claude_md = %path.display(),
        "shelbi: CLAUDE.md detected at {} but no longer read.\n  \
         → project-wide context belongs in agents/_shared/preamble.md\n  \
         → orchestrator-specific overrides belong in agents/orchestrator/instructions.md\n  \
         → remove CLAUDE.md when migration is complete.",
        path.display(),
    );
    state.claude_md_migration_hinted = true;
    write_state(project, &state)
}

/// Clear the per-project "migration hint already fired" flag so the
/// next [`maybe_emit_claude_md_migration_hint`] call (in this
/// orchestrator session) re-checks the disk and emits if applicable.
/// Called from `__zen-orch-start` so each new orchestrator session
/// starts with a clean slate — the v1 deprecation guidepost should
/// surface once per session regardless of where the first dispatch
/// originates.
pub fn reset_claude_md_migration_hint(project: &str) -> Result<()> {
    let mut state = read_state(project)?;
    if !state.claude_md_migration_hinted {
        return Ok(());
    }
    state.claude_md_migration_hinted = false;
    write_state(project, &state)
}

/// Names of every agent under `~/.shelbi/projects/<project>/agents/`,
/// sorted ascending. A "name" is the immediate child directory's basename;
/// non-directories (stray files in the agents/ dir) and hidden entries
/// (anything starting with `.`) are skipped. Returns an empty vec when
/// the agents directory doesn't exist yet — that's the legitimate state
/// for a fresh project that hasn't run `shelbi init --project`.
pub fn list_agents(project: &str) -> Result<Vec<String>> {
    let dir = agents_dir(project)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry.map_err(Error::Io)?;
        if !entry.file_type().map_err(Error::Io)?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        // `_shared` (the preamble dir mentioned in default_developer.md)
        // isn't an agent — skip it so it doesn't pollute `agent list`.
        if name.starts_with('_') {
            continue;
        }
        out.push(name.to_string());
    }
    out.sort();
    Ok(out)
}

/// Bundled `instructions.md` body for the named default agent, or `None`
/// if `name` isn't a shipped default. Used to decide whether the agent's
/// on-disk `instructions.md` is `CUSTOMIZED` for the `shelbi agent list`
/// report.
pub fn default_agent_body(name: &str) -> Option<&'static str> {
    DEFAULT_AGENTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, body)| *body)
}

/// True iff `name` is a shipped default agent (currently `orchestrator`
/// or `developer`). Convenience over [`default_agent_body`] for callers
/// that only care about presence, not the bundled body.
pub fn is_default_agent(name: &str) -> bool {
    default_agent_body(name).is_some()
}

/// Count of `*.md` files immediately under `<workspace>/skills/`. Returns
/// 0 when the directory doesn't exist (an agent without a skills/ subdir
/// is treated as having zero skills, not as an error). Non-recursive —
/// only immediate children are counted.
pub fn count_agent_skills(project: &str, agent: &str) -> Result<usize> {
    let dir = agent_skills_dir(project, agent)?;
    if !dir.exists() {
        return Ok(0);
    }
    let mut n = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry.map_err(Error::Io)?;
        if !entry.file_type().map_err(Error::Io)?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        if std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            == Some("md")
        {
            n += 1;
        }
    }
    Ok(n)
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
    /// the dashboard's deployed `.claude/agent-instructions.md` and
    /// the per-project `agents/orchestrator/instructions.md` will
    /// drift.
    #[test]
    fn orchestrator_template_byte_matches_default_instructions_const() {
        assert!(!DEFAULT_ORCHESTRATOR_INSTRUCTIONS.is_empty());
        assert!(DEFAULT_ORCHESTRATOR_INSTRUCTIONS.contains("{{assistant_name}}"));
    }

    /// Sanity-check the developer prompt has the spec-required hooks
    /// (review marker handoff, agents/_shared/preamble.md reference,
    /// the Phase 5 socket-emit paragraph) so a regression doesn't
    /// quietly ship a half-written prompt.
    #[test]
    fn developer_template_contains_required_hooks() {
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("review marker"));
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("agents/_shared/preamble.md"));
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("skills"));
        // Phase 5: the same socket-emit paragraph covers hub and remote
        // workers — the only thing that differs between them is the
        // path `$SHELBI_HUB_SOCK` resolves to. All three tool variants
        // must be named so the agent can fall back gracefully when one
        // isn't on PATH.
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("$SHELBI_HUB_SOCK"));
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("nc -U"));
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("socat"));
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("python3"));
        // Retry-once-then-continue is the spec's loss-handling rule —
        // pin it so a future copy edit can't quietly drop the policy.
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("retry once"));
    }

    #[test]
    fn list_agents_returns_empty_when_directory_missing() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert!(list_agents("p").unwrap().is_empty());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_agents_returns_subdirs_sorted_and_skips_files_and_reserved_prefixes() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = crate::agents_dir("p").unwrap();
        fs::create_dir_all(dir.join("zeta")).unwrap();
        fs::create_dir_all(dir.join("alpha")).unwrap();
        fs::create_dir_all(dir.join(".hidden")).unwrap();
        fs::create_dir_all(dir.join("_shared")).unwrap();
        // Stray file at the top of agents/ — must not appear in the listing.
        fs::write(dir.join("README.md"), "ignore me").unwrap();

        let got = list_agents("p").unwrap();
        assert_eq!(got, vec!["alpha".to_string(), "zeta".to_string()]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn count_agent_skills_counts_md_files_only_non_recursively() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let skills = agent_skills_dir("p", DEVELOPER_AGENT).unwrap();
        fs::write(skills.join("a.md"), "x").unwrap();
        fs::write(skills.join("b.md"), "x").unwrap();
        // .txt should be ignored.
        fs::write(skills.join("c.txt"), "x").unwrap();
        // Hidden file ignored.
        fs::write(skills.join(".swp"), "x").unwrap();
        // Subdirectory's contents are NOT counted (non-recursive contract).
        fs::create_dir_all(skills.join("nested")).unwrap();
        fs::write(skills.join("nested/deep.md"), "x").unwrap();

        assert_eq!(count_agent_skills("p", DEVELOPER_AGENT).unwrap(), 2);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn count_agent_skills_zero_when_skills_dir_absent() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert_eq!(count_agent_skills("p", "ghost").unwrap(), 0);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn is_default_agent_only_recognizes_bundled_names() {
        assert!(is_default_agent(ORCHESTRATOR_AGENT));
        assert!(is_default_agent(DEVELOPER_AGENT));
        assert!(!is_default_agent("qa"));
        assert!(!is_default_agent(""));
    }

    #[test]
    fn default_agent_body_matches_const_for_bundled_defaults() {
        assert_eq!(
            default_agent_body(ORCHESTRATOR_AGENT),
            Some(DEFAULT_ORCHESTRATOR_INSTRUCTIONS),
        );
        assert_eq!(
            default_agent_body(DEVELOPER_AGENT),
            Some(DEFAULT_DEVELOPER_INSTRUCTIONS),
        );
        assert_eq!(default_agent_body("qa"), None);
    }

    /// Acceptance criterion (b): missing `agents/_shared/preamble.md` is
    /// a no-op — the agent's own `instructions.md` flows through
    /// verbatim (modulo `{{assistant_name}}` substitution).
    #[test]
    fn compose_agent_prompt_returns_just_instructions_when_preamble_absent() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let workspace = agent_workspace_dir("p", "developer").unwrap();
        ensure_dir(&workspace).unwrap();
        fs::write(
            agent_instructions_path("p", "developer").unwrap(),
            "# Developer\nbody\n",
        )
        .unwrap();
        // Sanity: no _shared/ dir on disk.
        assert!(!agent_shared_preamble_path("p").unwrap().exists());

        let composed = compose_agent_prompt("p", "developer").unwrap();
        assert_eq!(composed, "# Developer\nbody\n");
        std::env::remove_var("SHELBI_HOME");
    }

    /// Acceptance criterion (a): when `agents/_shared/preamble.md`
    /// exists, its contents land before the agent's `instructions.md`
    /// with a single blank line between them so a heading at the top
    /// of `instructions.md` doesn't collide with the preamble's tail.
    #[test]
    fn compose_agent_prompt_prepends_preamble_with_blank_separator() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let workspace = agent_workspace_dir("p", "developer").unwrap();
        ensure_dir(&workspace).unwrap();
        fs::write(
            agent_instructions_path("p", "developer").unwrap(),
            "# Developer\nbody\n",
        )
        .unwrap();
        let preamble_path = agent_shared_preamble_path("p").unwrap();
        ensure_dir(preamble_path.parent().unwrap()).unwrap();
        fs::write(&preamble_path, "project monorepo overview\n").unwrap();

        let composed = compose_agent_prompt("p", "developer").unwrap();
        assert_eq!(
            composed, "project monorepo overview\n\n# Developer\nbody\n",
            "preamble must come first, blank line separator, then instructions"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// Preamble that doesn't end with a newline still gets a clean
    /// blank-line separator before the agent body. Otherwise the
    /// composed text would read `preamble# Developer` and the agent's
    /// heading would be mis-rendered.
    #[test]
    fn compose_agent_prompt_normalizes_preamble_missing_trailing_newline() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let workspace = agent_workspace_dir("p", "developer").unwrap();
        ensure_dir(&workspace).unwrap();
        fs::write(
            agent_instructions_path("p", "developer").unwrap(),
            "# Developer\n",
        )
        .unwrap();
        let preamble_path = agent_shared_preamble_path("p").unwrap();
        ensure_dir(preamble_path.parent().unwrap()).unwrap();
        // NB: no trailing \n in the preamble body.
        fs::write(&preamble_path, "preamble").unwrap();

        let composed = compose_agent_prompt("p", "developer").unwrap();
        assert_eq!(composed, "preamble\n\n# Developer\n");
        std::env::remove_var("SHELBI_HOME");
    }

    /// The `{{assistant_name}}` placeholder used by the orchestrator
    /// template still gets substituted when the prompt flows through
    /// the compose pipeline (formerly `system_prompt()` did it).
    #[test]
    fn compose_agent_prompt_substitutes_assistant_name_placeholder() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let workspace = agent_workspace_dir("p", ORCHESTRATOR_AGENT).unwrap();
        ensure_dir(&workspace).unwrap();
        fs::write(
            agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap(),
            "# You are {{assistant_name}}\nbody\n",
        )
        .unwrap();
        // No custom config — falls back to DEFAULT_ASSISTANT_NAME.
        let composed = compose_agent_prompt("p", ORCHESTRATOR_AGENT).unwrap();
        assert!(
            composed.contains(&format!("# You are {}", crate::DEFAULT_ASSISTANT_NAME)),
            "placeholder should be substituted: {composed}",
        );
        assert!(
            !composed.contains("{{assistant_name}}"),
            "no raw placeholder should survive: {composed}",
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// Acceptance criterion (c): the migration hint fires exactly once
    /// per orchestrator session when a legacy CLAUDE.md is present.
    /// Reset clears the latch so the next session re-emits.
    #[test]
    fn maybe_emit_claude_md_migration_hint_is_one_shot_per_session() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Seed a legacy CLAUDE.md at the project workdir.
        let path = legacy_claude_md_path("p").unwrap();
        ensure_dir(path.parent().unwrap()).unwrap();
        fs::write(&path, "old orchestrator prompt\n").unwrap();

        // First emission flips the latch.
        maybe_emit_claude_md_migration_hint("p").unwrap();
        assert!(read_state("p").unwrap().claude_md_migration_hinted);

        // Subsequent calls in the same session are no-ops — flag stays
        // set, no extra writes.
        maybe_emit_claude_md_migration_hint("p").unwrap();
        assert!(read_state("p").unwrap().claude_md_migration_hinted);

        // Reset (called at orch_start) re-arms emission for the new
        // session.
        reset_claude_md_migration_hint("p").unwrap();
        assert!(!read_state("p").unwrap().claude_md_migration_hinted);

        // After reset, the next emission re-flips the latch.
        maybe_emit_claude_md_migration_hint("p").unwrap();
        assert!(read_state("p").unwrap().claude_md_migration_hinted);

        std::env::remove_var("SHELBI_HOME");
    }

    /// Acceptance criterion (d): the migration hint does NOT fire when
    /// no legacy CLAUDE.md is present at the project root.
    #[test]
    fn maybe_emit_claude_md_migration_hint_does_not_fire_when_file_absent() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // No CLAUDE.md at the project workdir — the only place we look.
        let path = legacy_claude_md_path("p").unwrap();
        assert!(!path.exists());

        maybe_emit_claude_md_migration_hint("p").unwrap();
        // Latch stays unset so a CLAUDE.md that appears later (e.g.
        // user pulls a teammate's branch) is still detected.
        assert!(!read_state("p").unwrap().claude_md_migration_hinted);

        std::env::remove_var("SHELBI_HOME");
    }

    /// Reset is a no-op when the latch is already clear — important so
    /// orch_start doesn't dirty `state.json` (and the resulting write
    /// can't race) on every cold start.
    #[test]
    fn reset_claude_md_migration_hint_is_noop_when_already_clear() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // No prior emission, so the flag is clear.
        assert!(!read_state("p").unwrap().claude_md_migration_hinted);
        // Reset must not panic, must not write, must not flip the flag.
        reset_claude_md_migration_hint("p").unwrap();
        assert!(!read_state("p").unwrap().claude_md_migration_hinted);
        // state.json was never created since nothing was dirty.
        assert!(!crate::state_path("p").unwrap().exists());

        std::env::remove_var("SHELBI_HOME");
    }
}
