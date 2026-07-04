//! Per-project agent workspaces — the `agents/<name>/` layout that
//! ships with every shelbi project.
//!
//! Each agent gets a stable directory name (the *agent name*) that later
//! integrations — the workflow YAML's `agent:` field, the
//! `shelbi agent` CLI, event-log lines — reference. The directory holds
//! the agent's `instructions.md` system prompt plus a `skills/` subdir
//! that the task-dispatch path mounts into `.claude/skills/`.
//!
//! Three agents ship with the binary:
//!
//! - **orchestrator** — the coordinator agent that runs in the
//!   dashboard's right pane. Its bundled prompt is the content
//!   previously embedded as `default_orchestrator.md.template`.
//! - **developer** — the worker agent handed individual tasks. Bundled
//!   prompt lives in `default_developer.md.template`.
//! - **review** — the loader agent for a review workspace: it installs,
//!   builds, and serves a branch so a human can run it, and does not
//!   modify code. Bundled prompt lives in `default_review.md.template`
//!   and it ships a `load-run-detection` skill.
//!
//! Both are materialized on first [`materialize_default_agents`] (called
//! from `shelbi init`) and self-healed by [`self_heal_default_agents`]
//! (called from `shelbi reload`). User edits to `instructions.md` are
//! preserved on self-heal — a byte-compare against the bundled default
//! decides whether to fire the "you've customized this agent" notice.

use std::fs;
use std::path::PathBuf;

use shelbi_core::{Error, Result};

use crate::{
    agents_dir, atomic_write, ensure_dir, project_dir, read_state,
    update_state, DEFAULT_WORKSPACE_SETTINGS_TEMPLATE,
};

/// Stable identifier of the default orchestrator agent.
pub const ORCHESTRATOR_AGENT: &str = "orchestrator";

/// Stable identifier of the default developer agent.
pub const DEVELOPER_AGENT: &str = "developer";

/// Stable identifier of the default review agent — the loader that
/// prepares a branch on a review workspace so a human can run it. Unlike
/// the developer, it loads-and-serves and does not modify code.
pub const REVIEW_AGENT: &str = "review";

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

/// File name of the orchestrator's one-shot state-transfer file. Lives
/// inside `agents/orchestrator/` and is written by the outgoing
/// orchestrator (on `shelbi reload` / `shelbi quit`) and ingested by the
/// next instance (on startup / post-reload respawn), then deleted. Not
/// persistent state — durable orchestrator state lives in `state.json`.
pub const HANDOFF_FILE: &str = "handoff.md";

/// Relative path (from the orchestrator's workdir) where its handoff
/// file lives. The orchestrator's workdir IS `~/.shelbi/projects/<name>/`
/// (see `ensure_dashboard`), and its agent dir is `agents/orchestrator/`,
/// so a CWD-relative `agents/orchestrator/handoff.md` is the path the
/// running orchestrator sees in its filesystem.
pub const ORCHESTRATOR_HANDOFF_REL: &str = "agents/orchestrator/handoff.md";

/// Bundled orchestrator `instructions.md` content. Source of truth for
/// both the agent workspace materialize/self-heal path and the legacy
/// `shelbi_orchestrator::DEFAULT_SYSTEM_PROMPT` re-export.
pub const DEFAULT_ORCHESTRATOR_INSTRUCTIONS: &str =
    include_str!("default_orchestrator.md.template");

/// Bundled developer `instructions.md` content.
pub const DEFAULT_DEVELOPER_INSTRUCTIONS: &str =
    include_str!("default_developer.md.template");

/// Bundled review `instructions.md` content — the loader charter.
pub const DEFAULT_REVIEW_INSTRUCTIONS: &str = include_str!("default_review.md.template");

/// Bundled body of the review agent's `load-run-detection` skill: the
/// auto-detect heuristics for booting an unknown project on `$PORT`.
/// Kept in the agent (Decision A in the review-workspaces plan) rather
/// than in Rust so per-repo detection stays flexible without schema churn.
pub const DEFAULT_REVIEW_LOAD_RUN_SKILL: &str =
    include_str!("skills/load_run_detection.SKILL.md");

/// One bundled file under a default agent's `skills/` directory. `rel_path`
/// is relative to `<workspace>/skills/` and may include subdirectories
/// (Claude Code skills live in `<skill-name>/SKILL.md`); intermediate
/// directories are created on write.
pub struct BundledSkill {
    pub rel_path: &'static str,
    pub content: &'static str,
}

/// Skills shipped with the review agent. The load/run detection heuristics
/// live here (Decision A) so the auto-detect logic is the agent's, not
/// Rust's.
const REVIEW_SKILLS: &[BundledSkill] = &[BundledSkill {
    rel_path: "load-run-detection/SKILL.md",
    content: DEFAULT_REVIEW_LOAD_RUN_SKILL,
}];

/// One entry in [`DEFAULT_AGENTS`]: the agent name, its bundled
/// `instructions.md`, — for Claude-Code-based roles — the bundled
/// `settings.json` template that ships the message-tail hooks plus the
/// pane-title hooks, and any bundled `skills/` files the role ships with.
/// `settings_template = None` means the role doesn't scaffold a per-role
/// settings.json (e.g. a future codex role); `skills = &[]` (the common
/// case) means the role ships an empty `skills/` directory.
pub struct BundledAgent {
    pub name: &'static str,
    pub instructions: &'static str,
    pub settings_template: Option<&'static str>,
    pub skills: &'static [BundledSkill],
}

/// Defaults shipped with the binary, in declaration order. Iteration
/// order matches the order outcomes appear in
/// [`materialize_default_agents`] / [`self_heal_default_agents`] reports.
///
/// Both default agents currently run on Claude Code, so both scaffold an
/// `agents/<role>/settings.json` from the shared workspace-settings
/// template. The settings file is what Claude Code reads on session
/// start — its hooks tail the per-task message log
/// (`.shelbi/messages/<task>.log`) and inject any unread lines as a
/// system reminder on the agent's next turn (Phase 7 of the
/// worker↔orchestrator communication design).
pub const DEFAULT_AGENTS: &[BundledAgent] = &[
    BundledAgent {
        name: ORCHESTRATOR_AGENT,
        instructions: DEFAULT_ORCHESTRATOR_INSTRUCTIONS,
        settings_template: Some(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE),
        skills: &[],
    },
    BundledAgent {
        name: DEVELOPER_AGENT,
        instructions: DEFAULT_DEVELOPER_INSTRUCTIONS,
        settings_template: Some(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE),
        skills: &[],
    },
    BundledAgent {
        name: REVIEW_AGENT,
        instructions: DEFAULT_REVIEW_INSTRUCTIONS,
        settings_template: Some(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE),
        skills: REVIEW_SKILLS,
    },
];

/// `~/.shelbi/projects/<project>/agents/<agent>/`. The directory name
/// IS the agent's stable identifier — that's what downstream callers
/// (workflow YAML, CLI subcommands, event log lines) reference.
pub fn agent_workspace_dir(project: &str, agent: &str) -> Result<PathBuf> {
    // `agents_dir` validates the project name; guard the agent name here so a
    // `..`/absolute/`a/b` agent can't escape the project's `agents/` dir
    // (state-runtime F14).
    crate::ensure_flat_path_component("agent", agent)?;
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

/// `<workspace>/settings.json` — the Claude-Code settings template that
/// gets deployed to `<worktree>/.claude/settings.json` on each task
/// dispatch when an agent name is provided. Optional: roles that don't
/// ship a settings.json (e.g. future codex/aider roles) fall back to the
/// project-wide template.
pub fn agent_settings_path(project: &str, agent: &str) -> Result<PathBuf> {
    Ok(agent_workspace_dir(project, agent)?.join("settings.json"))
}

/// Read the per-role `agents/<role>/settings.json` if it exists. Returns
/// `None` for roles that don't ship a settings file (codex, aider) or
/// when a user has deleted the file — the caller falls back to the
/// project-wide workspace-settings template in that case.
pub fn load_agent_settings(project: &str, agent: &str) -> Result<Option<String>> {
    let path = agent_settings_path(project, agent)?;
    match fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// `~/.shelbi/projects/<project>/agents/_shared/preamble.md` — the
/// optional, per-project shared preamble that gets prepended to every
/// dispatched agent's `instructions.md`.
pub fn agent_shared_preamble_path(project: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?
        .join(SHARED_AGENT_DIR)
        .join(SHARED_PREAMBLE_FILE))
}

/// `~/.shelbi/projects/<project>/agents/orchestrator/handoff.md` — the
/// orchestrator's one-shot state-transfer file. Written by the outgoing
/// orchestrator on `shelbi reload` / `shelbi quit` and ingested
/// (then deleted) by the next instance on startup. Same path the
/// orchestrator's instructions reference, so the directory layout the
/// running orchestrator sees agrees with the path callers compute.
pub fn orchestrator_handoff_path(project: &str) -> Result<PathBuf> {
    // State path, NOT the mode-aware `agent_workspace_dir`: the handoff is
    // a transient state-transfer artifact the running orchestrator writes
    // CWD-relative to its workdir — which is the project's *state* root
    // (`project_dir`, see `ensure_dashboard`), not its config root. Keying
    // it off `project_dir` (via the same [`ORCHESTRATOR_HANDOFF_REL`] the
    // orchestrator is told to write) keeps the reader and the writer in
    // agreement in both config modes; in global mode this is byte-identical
    // to the old `agents/orchestrator/handoff.md` under the project dir.
    Ok(project_dir(project)?.join(ORCHESTRATOR_HANDOFF_REL))
}

/// Read and delete the orchestrator's handoff file. Returns `Ok(None)`
/// when the file isn't there — the normal case on a clean start.
///
/// "Take" semantics (read-then-delete) keep the handoff one-shot: even
/// if a downstream caller crashes between the read and the next
/// orchestrator launch, the stale file is already gone so the next
/// instance won't re-ingest it. A failed delete is logged but not
/// surfaced as an error — leaving the file behind degrades to a
/// (re-)ingest on the next start, which is recoverable, whereas
/// failing the launch on a stuck `rm` would be worse.
///
/// Best-effort: malformed UTF-8 or partial writes are returned to the
/// caller as-is; the [`compose_agent_prompt`] splice path treats the
/// content as opaque text, and the orchestrator can sanity-check what
/// it received when ingesting.
pub fn take_orchestrator_handoff(project: &str) -> Result<Option<String>> {
    let path = orchestrator_handoff_path(project)?;
    match fs::read_to_string(&path) {
        Ok(s) => {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!(
                    project,
                    handoff = %path.display(),
                    error = %e,
                    "failed to delete handoff.md after ingestion (will re-ingest on next start)",
                );
            }
            Ok(Some(s))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
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
    Ok(composed)
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
/// Best-effort — IO failures from the state update are returned to the
/// caller so the spawn path can decide whether to bail or proceed. In
/// practice every caller treats the hint as advisory and uses `let _ = …`
/// to ignore the result.
pub fn maybe_emit_claude_md_migration_hint(project: &str) -> Result<()> {
    let path = legacy_claude_md_path(project)?;
    update_state(project, |state| {
        if state.claude_md_migration_hinted {
            return Ok(());
        }
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
        Ok(())
    })
}

/// Clear the per-project "migration hint already fired" flag so the
/// next [`maybe_emit_claude_md_migration_hint`] call (in this
/// orchestrator session) re-checks the disk and emits if applicable.
/// Called from `__zen-orch-start` so each new orchestrator session
/// starts with a clean slate — the v1 deprecation guidepost should
/// surface once per session regardless of where the first dispatch
/// originates.
pub fn reset_claude_md_migration_hint(project: &str) -> Result<()> {
    update_state(project, |state| {
        state.claude_md_migration_hinted = false;
        Ok(())
    })
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
        .find(|a| a.name == name)
        .map(|a| a.instructions)
}

/// Bundled `settings.json` template for the named default agent, or
/// `None` if the role doesn't ship a settings file (or `name` isn't a
/// shipped default). Used by the deploy path's per-role-prefers-then-
/// project-wide fallback chain.
pub fn default_agent_settings(name: &str) -> Option<&'static str> {
    DEFAULT_AGENTS
        .iter()
        .find(|a| a.name == name)
        .and_then(|a| a.settings_template)
}

/// True iff `name` is a shipped default agent (currently `orchestrator`
/// or `developer`). Convenience over [`default_agent_body`] for callers
/// that only care about presence, not the bundled body.
pub fn is_default_agent(name: &str) -> bool {
    default_agent_body(name).is_some()
}

/// Stable content hash of a default agent body, used as the *provenance*
/// fingerprint recorded in [`State::deployed_agent_defaults`] at deploy
/// time and compared against on-disk content later.
///
/// FNV-1a, 64-bit. Dependency-free and — being pure integer arithmetic —
/// stable across platforms and binary versions, which is all provenance
/// needs: the same bytes always fingerprint to the same digest, so a
/// later read can tell whether a file still equals the default that was
/// deployed. It is *not* cryptographic; a hostile actor could craft a
/// collision, but the failure mode (a crafted custom prompt reported as
/// "pristine", or — in self-heal — auto-upgraded) is cosmetic, and agent
/// prompts on a user's own machine aren't an adversarial input surface.
pub fn content_hash(body: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for b in body.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    format!("{h:016x}")
}

/// Provenance-aware classification of a default agent's on-disk
/// `instructions.md`, distinguishing the three states a naive
/// byte-compare against the *currently compiled* default conflates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDivergence {
    /// On-disk content equals the currently-compiled bundled default.
    /// Nothing to do — the agent is running the shipped prompt.
    PristineCurrent,
    /// On-disk content equals the default that was *deployed* (its
    /// recorded provenance hash) but no longer equals the current
    /// compiled default — a newer `shelbi` shipped a new default and the
    /// user never touched this file. Safe to auto-upgrade; must NOT be
    /// reported as a user customization.
    PristineStale,
    /// On-disk content differs from the deployed default's provenance
    /// hash — a genuine user edit. (Absent provenance, any content that
    /// doesn't match the current compiled default also lands here, which
    /// is the conservative pre-provenance byte-compare behavior.)
    Customized,
}

/// Classify `on_disk` against the default that was deployed
/// (`deployed_hash`, from [`State::deployed_agent_defaults`]) and the
/// `current_default` compiled into this binary. This is the shared
/// mechanism both the state-runtime self-heal path and the CLI
/// `shelbi agent list` "customized?" marker route through, so a compiled
/// default bump is interpreted identically on both surfaces.
///
/// The order matters: a file equal to the current default is
/// [`AgentDivergence::PristineCurrent`] regardless of provenance (so a
/// pre-provenance agent still reads correctly and gets its provenance
/// backfilled); only then does a recorded provenance hash distinguish
/// pristine-stale from customized.
pub fn classify_agent_divergence(
    deployed_hash: Option<&str>,
    current_default: &str,
    on_disk: &str,
) -> AgentDivergence {
    if on_disk == current_default {
        return AgentDivergence::PristineCurrent;
    }
    if let Some(deployed) = deployed_hash {
        if content_hash(on_disk) == deployed {
            return AgentDivergence::PristineStale;
        }
    }
    AgentDivergence::Customized
}

/// Read-only provenance classification of `agent`'s on-disk
/// `instructions.md` for `project`. `Ok(None)` when `agent` isn't a
/// shipped default (the "customized?" question doesn't apply). A missing
/// `instructions.md` for a shipped default reads as
/// [`AgentDivergence::Customized`] — it's divergent from the bundled body
/// for reporting purposes (self-heal recreates it on the next reload).
///
/// Consults [`State::deployed_agent_defaults`] so a compiled-default bump
/// doesn't misreport an untouched agent as customized. Read-only — unlike
/// [`self_heal_default_agents`] it neither rewrites files nor backfills
/// provenance; it's the query the CLI list marker uses.
pub fn agent_divergence(project: &str, agent: &str) -> Result<Option<AgentDivergence>> {
    let Some(default_body) = default_agent_body(agent) else {
        return Ok(None);
    };
    let path = agent_instructions_path(project, agent)?;
    let current = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(AgentDivergence::Customized));
        }
        Err(e) => return Err(Error::Io(e)),
    };
    let state = read_state(project)?;
    let deployed = state.deployed_agent_defaults.get(agent).map(String::as_str);
    Ok(Some(classify_agent_divergence(
        deployed,
        default_body,
        &current,
    )))
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
    /// The agent directory exists and `instructions.md` matched the
    /// *deployed* default's provenance hash but not the current compiled
    /// default — an untouched file left stale by a `shelbi` upgrade. It
    /// was auto-upgraded to the current bundled default in place (the
    /// user never customized it, so there's nothing to preserve).
    Upgraded { agent: String },
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
            | Self::Upgraded { agent }
            | Self::Preserved { agent, .. } => agent,
        }
    }
}

/// Create `agents/{orchestrator,developer,review}/` from the bundled
/// defaults for `project`. Each agent's directory is created only if missing —
/// existing directories are left untouched and reported as `Unchanged`
/// (init is conservative; self-heal does the SHA-compare).
///
/// Returns one outcome per default agent, in [`DEFAULT_AGENTS`] order.
pub fn materialize_default_agents(project: &str) -> Result<Vec<AgentMaterializeOutcome>> {
    let mut outcomes = Vec::with_capacity(DEFAULT_AGENTS.len());
    let mut created: Vec<&BundledAgent> = Vec::new();
    for agent in DEFAULT_AGENTS {
        let workspace = agent_workspace_dir(project, agent.name)?;
        if workspace.exists() {
            outcomes.push(AgentMaterializeOutcome::Unchanged {
                agent: agent.name.to_string(),
            });
            continue;
        }
        write_bundled_agent(project, agent)?;
        created.push(agent);
        outcomes.push(AgentMaterializeOutcome::Created {
            agent: agent.name.to_string(),
        });
    }
    // Record provenance for the agents we just deployed so a later
    // compiled-default bump can tell an untouched file (still matching the
    // hash recorded here) from a genuine user edit. Done in one locked
    // state write after the file IO.
    if !created.is_empty() {
        update_state(project, |state| {
            for agent in &created {
                state
                    .deployed_agent_defaults
                    .insert(agent.name.to_string(), content_hash(agent.instructions));
            }
            Ok(())
        })?;
    }
    Ok(outcomes)
}

/// `shelbi reload`'s self-heal pass. For each default agent:
///
/// - Missing directory → recreate from the bundled default (`Created`).
/// - `instructions.md` missing → drop the bundled default back in
///   (`Created`).
/// - `instructions.md` equals the current compiled default → `Unchanged`.
/// - `instructions.md` equals the *deployed* default's provenance hash
///   but not the current compiled default (an untouched file left stale
///   by a `shelbi` upgrade) → auto-upgraded in place to the current
///   default (`Upgraded`), NOT reported as a customization.
/// - `instructions.md` differs from the deployed provenance → a genuine
///   user edit; left alone (`Preserved`). The `first_notice` field is set
///   the first time the current divergent content is seen, tracked in
///   [`State::notified_diverged_agents`] so the user-facing notice fires
///   exactly once per divergence.
///
/// Every deploy / upgrade / pristine observation (re)records the agent's
/// provenance in [`State::deployed_agent_defaults`] so a later
/// compiled-default bump is classified against the default that was
/// actually on disk, not the currently-compiled one. This is what stops
/// an upgrade from flagging every untouched agent as customized.
///
/// Also ensures the `skills/` subdir exists for every default agent.
pub fn self_heal_default_agents(project: &str) -> Result<Vec<AgentMaterializeOutcome>> {
    // The whole pass runs inside one locked `update_state` so the
    // divergence bookkeeping can't lose a concurrent mutator's fields
    // (or vice versa). Self-heal runs once per `shelbi reload`, so
    // holding the lock across the file IO is fine.
    update_state(project, |state| {
        let mut outcomes = Vec::with_capacity(DEFAULT_AGENTS.len());

        for agent in DEFAULT_AGENTS {
            let workspace = agent_workspace_dir(project, agent.name)?;
            if !workspace.exists() {
                write_bundled_agent(project, agent)?;
                state.notified_diverged_agents.remove(agent.name);
                state
                    .deployed_agent_defaults
                    .insert(agent.name.to_string(), content_hash(agent.instructions));
                outcomes.push(AgentMaterializeOutcome::Created {
                    agent: agent.name.to_string(),
                });
                continue;
            }

            // Workspace exists. Ensure `skills/` is there before we judge
            // `instructions.md` — a half-materialized workspace from an
            // older shelbi version shouldn't be left without it.
            ensure_dir(&agent_skills_dir(project, agent.name)?)?;

            // Make sure the per-role `settings.json` is on disk before we
            // judge `instructions.md` divergence — a half-materialized
            // workspace from an older shelbi version (no settings file)
            // shouldn't be left without the message-tail hooks.
            ensure_agent_settings_present(project, agent)?;

            // Same seam for bundled skills: a workspace materialized before
            // this role shipped a skill (or one the user deleted) shouldn't
            // be left without it. Missing → drop the default back in;
            // present-and-edited → left alone (existence-guarded).
            ensure_agent_skills_present(project, agent)?;

            let path = agent_instructions_path(project, agent.name)?;
            let current = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Atomic (tmp + rename): a crash mid-write must never
                    // leave a truncated `instructions.md`, which the next
                    // self-heal pass would misclassify as a user
                    // customization and preserve forever (F8).
                    atomic_write(&path, agent.instructions.as_bytes())?;
                    state.notified_diverged_agents.remove(agent.name);
                    state
                        .deployed_agent_defaults
                        .insert(agent.name.to_string(), content_hash(agent.instructions));
                    outcomes.push(AgentMaterializeOutcome::Created {
                        agent: agent.name.to_string(),
                    });
                    continue;
                }
                Err(e) => return Err(Error::Io(e)),
            };

            let deployed = state.deployed_agent_defaults.get(agent.name).map(String::as_str);
            match classify_agent_divergence(deployed, agent.instructions, &current) {
                AgentDivergence::PristineCurrent => {
                    // Matches the current compiled default. Clear any stale
                    // notice latch and (re)record provenance — this backfills
                    // agents materialized by a pre-provenance binary.
                    state.notified_diverged_agents.remove(agent.name);
                    state
                        .deployed_agent_defaults
                        .insert(agent.name.to_string(), content_hash(agent.instructions));
                    outcomes.push(AgentMaterializeOutcome::Unchanged {
                        agent: agent.name.to_string(),
                    });
                }
                AgentDivergence::PristineStale => {
                    // Untouched since deploy, but the compiled default moved
                    // on. The user never customized it, so auto-upgrade to
                    // the new default in place and re-record provenance.
                    atomic_write(&path, agent.instructions.as_bytes())?;
                    state.notified_diverged_agents.remove(agent.name);
                    state
                        .deployed_agent_defaults
                        .insert(agent.name.to_string(), content_hash(agent.instructions));
                    outcomes.push(AgentMaterializeOutcome::Upgraded {
                        agent: agent.name.to_string(),
                    });
                }
                AgentDivergence::Customized => {
                    // Genuine user edit — leave it untouched and keep the
                    // recorded provenance so reverting to the deployed
                    // default is recognized later. Fire the notice once.
                    let first_notice = state
                        .notified_diverged_agents
                        .insert(agent.name.to_string());
                    outcomes.push(AgentMaterializeOutcome::Preserved {
                        agent: agent.name.to_string(),
                        first_notice,
                    });
                }
            }
        }

        Ok(outcomes)
    })
}

/// Create `<workspace>/` and `<workspace>/skills/`, then write
/// `<workspace>/instructions.md` (and `<workspace>/settings.json` for
/// Claude-Code-based roles) from the bundled defaults. Used by both
/// materialize and self-heal whenever a default agent needs to be
/// (re)dropped onto disk in full.
fn write_bundled_agent(project: &str, agent: &BundledAgent) -> Result<()> {
    let workspace = agent_workspace_dir(project, agent.name)?;
    ensure_dir(&workspace)?;
    ensure_dir(&agent_skills_dir(project, agent.name)?)?;
    // Atomic (tmp + rename) so a crash mid-`shelbi init` can't leave a
    // truncated `instructions.md` that self-heal later preserves as a
    // "customization" (F8) — the agent would then dispatch with half a
    // prompt. `atomic_write` is the same sink `status.yaml`/`keys.yaml` use.
    atomic_write(
        &agent_instructions_path(project, agent.name)?,
        agent.instructions.as_bytes(),
    )?;
    if let Some(settings) = agent.settings_template {
        atomic_write(&agent_settings_path(project, agent.name)?, settings.as_bytes())?;
    }
    for skill in agent.skills {
        write_bundled_skill(project, agent.name, skill)?;
    }
    Ok(())
}

/// Write one bundled skill file under `<workspace>/skills/`, creating any
/// intermediate directories (`<skill-name>/SKILL.md` needs `<skill-name>/`).
/// Overwrites unconditionally — used by [`write_bundled_agent`] when the
/// whole workspace is being (re)dropped; the self-heal path guards on
/// existence before calling this via [`ensure_agent_skills_present`].
fn write_bundled_skill(project: &str, agent: &str, skill: &BundledSkill) -> Result<()> {
    let path = agent_skills_dir(project, agent)?.join(skill.rel_path);
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    // Atomic (tmp + rename) for the same F8 reason as `instructions.md`: a
    // crash mid-write must not leave a truncated skill that self-heal later
    // preserves as a "customization" — the same sink used elsewhere here.
    atomic_write(&path, skill.content.as_bytes())
}

/// Self-heal seam for the per-role `settings.json` — Claude Code reads
/// this file's hooks on every session start, so a stale shipped default
/// is a silent regression. Treat it the same way the project-wide
/// workspace-settings template is treated: missing → drop the bundled
/// default; present-and-divergent → leave the user's customization
/// alone. Returns silently for roles that don't ship a settings file.
fn ensure_agent_settings_present(project: &str, agent: &BundledAgent) -> Result<()> {
    let Some(default) = agent.settings_template else {
        return Ok(());
    };
    let path = agent_settings_path(project, agent.name)?;
    if path.exists() {
        return Ok(());
    }
    // Atomic write — a torn `settings.json` from a mid-write crash would
    // drop the message-tail hooks silently (F8).
    atomic_write(&path, default.as_bytes())
}

/// Self-heal seam for bundled `skills/` files. Claude Code loads whatever
/// is under `.claude/skills/` at session start, so a shipped skill that
/// went missing (older materialize, user deletion) is a silent capability
/// regression for that role. Treat each bundled skill like the settings
/// file: missing → drop the bundled default in; present → leave the user's
/// copy alone. Returns silently for roles that ship no skills.
fn ensure_agent_skills_present(project: &str, agent: &BundledAgent) -> Result<()> {
    for skill in agent.skills {
        let path = agent_skills_dir(project, agent.name)?.join(skill.rel_path);
        if path.exists() {
            continue;
        }
        write_bundled_skill(project, agent.name, skill)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_state;
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
        assert_eq!(outcomes.len(), 3);
        for (i, name) in [ORCHESTRATOR_AGENT, DEVELOPER_AGENT, REVIEW_AGENT]
            .iter()
            .enumerate()
        {
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
        }
        // Orchestrator + developer ship an empty skills/ dir; review ships
        // its load-run-detection skill.
        for name in [ORCHESTRATOR_AGENT, DEVELOPER_AGENT] {
            let skills = agent_skills_dir("p", name).unwrap();
            assert_eq!(
                fs::read_dir(&skills).unwrap().count(),
                0,
                "{name}: skills/ should ship empty"
            );
        }
        assert!(
            agent_skills_dir("p", REVIEW_AGENT)
                .unwrap()
                .join("load-run-detection/SKILL.md")
                .is_file(),
            "review: load-run-detection skill should ship"
        );
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap()).unwrap(),
            DEFAULT_ORCHESTRATOR_INSTRUCTIONS
        );
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", DEVELOPER_AGENT).unwrap()).unwrap(),
            DEFAULT_DEVELOPER_INSTRUCTIONS
        );
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", REVIEW_AGENT).unwrap()).unwrap(),
            DEFAULT_REVIEW_INSTRUCTIONS
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
                AgentMaterializeOutcome::Unchanged {
                    agent: REVIEW_AGENT.to_string()
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

    /// The shared provenance classifier is the whole fix in miniature:
    /// against the default that was *deployed* (not the currently-compiled
    /// one), an untouched-but-stale file reads pristine and a genuine edit
    /// reads customized — including an edit that lands on some *other*
    /// default's bytes.
    #[test]
    fn classify_distinguishes_stale_from_customized() {
        let v1 = "# default v1\n";
        let v2 = "# default v2\n";
        let deployed = content_hash(v1);

        // On-disk equals the current compiled default → pristine-current,
        // regardless of what was deployed.
        assert_eq!(
            classify_agent_divergence(Some(&deployed), v2, v2),
            AgentDivergence::PristineCurrent,
        );
        // Untouched since deploy but the compiled default bumped v1→v2 →
        // pristine-stale (the upgrade false-positive the byte-compare hit).
        assert_eq!(
            classify_agent_divergence(Some(&deployed), v2, v1),
            AgentDivergence::PristineStale,
        );
        // A genuine edit that differs from the deployed default → customized.
        assert_eq!(
            classify_agent_divergence(Some(&deployed), v2, "# my own\n"),
            AgentDivergence::Customized,
        );
        // An edit that happens to match some *older* default (neither the
        // deployed nor the current one) is still a real edit → customized
        // (the byte-compare false-negative).
        assert_eq!(
            classify_agent_divergence(Some(&deployed), v2, "# default v0\n"),
            AgentDivergence::Customized,
        );
        // No provenance recorded yet (pre-provenance binary) → fall back to
        // a byte-compare against the current default.
        assert_eq!(
            classify_agent_divergence(None, v2, v2),
            AgentDivergence::PristineCurrent,
        );
        assert_eq!(
            classify_agent_divergence(None, v2, v1),
            AgentDivergence::Customized,
        );
    }

    /// Materialize records each deployed default's provenance hash so a
    /// later compiled-default bump has a baseline to compare against.
    #[test]
    fn materialize_records_deployed_provenance() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let state = read_state("p").unwrap();
        for agent in DEFAULT_AGENTS {
            assert_eq!(
                state.deployed_agent_defaults.get(agent.name).map(String::as_str),
                Some(content_hash(agent.instructions).as_str()),
                "provenance for {} should be the deployed default's hash",
                agent.name,
            );
        }

        std::env::remove_var("SHELBI_HOME");
    }

    /// Acceptance: bump the *compiled* default under an untouched agent and
    /// self-heal auto-upgrades it in place — it is NOT flagged as a user
    /// customization and fires no "you customized this" notice.
    #[test]
    fn self_heal_upgrades_untouched_agent_when_compiled_default_bumps() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();

        // Simulate "shelbi was upgraded": the previous default (v1) is what
        // sits on disk and what provenance was recorded for, while the
        // current compiled default (DEFAULT_ORCHESTRATOR_INSTRUCTIONS) is v2.
        let v1 = "# previous bundled default\nold guidance\n";
        let path = agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap();
        fs::write(&path, v1).unwrap();
        update_state("p", |s| {
            s.deployed_agent_defaults
                .insert(ORCHESTRATOR_AGENT.to_string(), content_hash(v1));
            Ok(())
        })
        .unwrap();

        let outcomes = self_heal_default_agents("p").unwrap();
        let orch = outcomes
            .iter()
            .find(|o| o.agent() == ORCHESTRATOR_AGENT)
            .unwrap();
        assert_eq!(
            orch,
            &AgentMaterializeOutcome::Upgraded {
                agent: ORCHESTRATOR_AGENT.to_string(),
            },
            "an untouched-but-stale agent should upgrade, not flag as customized",
        );
        // File was rewritten to the current compiled default...
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            DEFAULT_ORCHESTRATOR_INSTRUCTIONS,
        );
        let state = read_state("p").unwrap();
        // ...no customization notice fired...
        assert!(!state.notified_diverged_agents.contains(ORCHESTRATOR_AGENT));
        // ...and provenance was re-recorded to the new default.
        assert_eq!(
            state.deployed_agent_defaults.get(ORCHESTRATOR_AGENT).map(String::as_str),
            Some(content_hash(DEFAULT_ORCHESTRATOR_INSTRUCTIONS).as_str()),
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// A genuine user edit is still preserved + flagged even when the
    /// compiled default has since bumped away from what was deployed.
    #[test]
    fn self_heal_preserves_user_edit_across_a_compiled_default_bump() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        // Provenance says v1 was deployed; the user then edited the file.
        let v1 = "# previous bundled default\n";
        let custom = "# my orchestrator\nlocal rules\n";
        let path = agent_instructions_path("p", ORCHESTRATOR_AGENT).unwrap();
        fs::write(&path, custom).unwrap();
        update_state("p", |s| {
            s.deployed_agent_defaults
                .insert(ORCHESTRATOR_AGENT.to_string(), content_hash(v1));
            Ok(())
        })
        .unwrap();

        let outcomes = self_heal_default_agents("p").unwrap();
        let orch = outcomes
            .iter()
            .find(|o| o.agent() == ORCHESTRATOR_AGENT)
            .unwrap();
        assert_eq!(
            orch,
            &AgentMaterializeOutcome::Preserved {
                agent: ORCHESTRATOR_AGENT.to_string(),
                first_notice: true,
            },
        );
        // Edit untouched, and the deployed-default provenance is retained so
        // reverting to v1 would later be recognized as pristine.
        assert_eq!(fs::read_to_string(&path).unwrap(), custom);
        assert_eq!(
            read_state("p")
                .unwrap()
                .deployed_agent_defaults
                .get(ORCHESTRATOR_AGENT)
                .map(String::as_str),
            Some(content_hash(v1).as_str()),
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// Read-only `agent_divergence` reports pristine-stale (not customized)
    /// for an untouched agent after a compiled-default bump — the query the
    /// CLI list marker uses.
    #[test]
    fn agent_divergence_reports_pristine_stale_after_bump() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let v1 = "# previous bundled default\n";
        let path = agent_instructions_path("p", DEVELOPER_AGENT).unwrap();
        fs::write(&path, v1).unwrap();
        update_state("p", |s| {
            s.deployed_agent_defaults
                .insert(DEVELOPER_AGENT.to_string(), content_hash(v1));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            agent_divergence("p", DEVELOPER_AGENT).unwrap(),
            Some(AgentDivergence::PristineStale),
        );
        // A non-default agent has nothing to compare against.
        assert_eq!(agent_divergence("p", "qa").unwrap(), None);

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
        assert!(DEFAULT_ORCHESTRATOR_INSTRUCTIONS.contains("# You are the Orchestrator"));
    }

    /// Phase 5 (review-workspaces §8/§9/§15/§16): the orchestrator prompt must
    /// carry the auto-load trigger, the pending-load queue vocabulary, and the
    /// Zen carve-out so a copy edit can't quietly drop the flow that makes
    /// review-routed tasks reach a human instead of auto-merging.
    #[test]
    fn orchestrator_template_contains_review_workspace_rules() {
        let t = DEFAULT_ORCHESTRATOR_INSTRUCTIONS;
        // Auto-load trigger: the handoff reaction loads onto a review slot.
        assert!(t.contains("shelbi review <id>"));
        // Scarcity/queue: the pending-load sub-state is the queue signal.
        assert!(t.contains("pending-load"));
        // Free-on-resolve: the review-workspace-free event drains the queue.
        assert!(t.contains("review** workspace"));
        // Dev session closes on completion (§16) — the orchestrator must know
        // the freed dev slot needs no teardown from it.
        assert!(t.contains("closes its own session"));
        // Zen gate: review-routed tasks are the human path, never auto-merged.
        assert!(t.contains("review-workspace gate"));
    }

    /// Sanity-check the developer prompt has the spec-required hooks
    /// (ready marker handoff, agents/_shared/preamble.md reference,
    /// the Phase 5 socket-emit paragraph) so a regression doesn't
    /// quietly ship a half-written prompt.
    #[test]
    fn developer_template_contains_required_hooks() {
        assert!(DEFAULT_DEVELOPER_INSTRUCTIONS.contains("ready marker"));
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

    /// The review charter (§6) must be explicit that the agent loads and
    /// serves and does NOT modify code — plus carry the load/serve
    /// mechanics (setup/serve, $PORT, ready probe, the ready signal) so a
    /// copy edit can't quietly gut the role's contract.
    #[test]
    fn review_template_contains_required_charter_language() {
        // Load-and-serve, not keep-coding — the whole point of the role.
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("do not modify code"));
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("runnable for a human"));
        // The human-requested-tweak carve-out must survive edits — it's the
        // sole case the review agent touches code.
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("tweak"));
        // Load/serve mechanics.
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("review.setup"));
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("review.serve"));
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("$PORT"));
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("diff-only"));
        // The ready signal + its verbs the board/orchestrator watch for.
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("$SHELBI_HUB_SOCK"));
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("review_ready"));
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("review_loaded"));
        // Points the agent at its detection skill.
        assert!(DEFAULT_REVIEW_INSTRUCTIONS.contains("skills"));
    }

    /// The bundled load/run detection skill ships with valid frontmatter
    /// (Claude Code needs `name:` + `description:` to load it) and the
    /// auto-detect precedence the plan (§7) calls out.
    #[test]
    fn review_load_run_skill_has_frontmatter_and_precedence() {
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.starts_with("---"));
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.contains("name: load-run-detection"));
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.contains("description:"));
        // Precedence: declared > framework > generic > diff-only.
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.contains("package.json"));
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.contains("Cargo.toml"));
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.contains("Makefile"));
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.contains("Procfile"));
        assert!(DEFAULT_REVIEW_LOAD_RUN_SKILL.contains("diff-only"));
    }

    /// `shelbi init` materializes the review agent alongside the others,
    /// including its bundled `load-run-detection` skill (mirrors the
    /// developer materialize test).
    #[test]
    fn materialize_ships_review_agent_with_load_run_skill() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        assert_eq!(
            fs::read_to_string(agent_instructions_path("p", REVIEW_AGENT).unwrap()).unwrap(),
            DEFAULT_REVIEW_INSTRUCTIONS
        );
        let skill = agent_skills_dir("p", REVIEW_AGENT)
            .unwrap()
            .join("load-run-detection/SKILL.md");
        assert!(skill.is_file(), "review skill missing");
        assert_eq!(
            fs::read_to_string(&skill).unwrap(),
            DEFAULT_REVIEW_LOAD_RUN_SKILL
        );
        // Review is a Claude-Code role → ships the shared settings.json.
        assert!(agent_settings_path("p", REVIEW_AGENT).unwrap().is_file());

        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload`: a review workspace from a shelbi that predates the
    /// bundled skill (or whose user deleted it) self-heals by dropping the
    /// skill back in. Mirrors the settings-file self-heal seam.
    #[test]
    fn self_heal_recreates_missing_review_skill() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let skill = agent_skills_dir("p", REVIEW_AGENT)
            .unwrap()
            .join("load-run-detection/SKILL.md");
        fs::remove_file(&skill).unwrap();
        assert!(!skill.exists());

        self_heal_default_agents("p").unwrap();
        assert!(skill.is_file(), "review skill should be recreated");
        assert_eq!(
            fs::read_to_string(&skill).unwrap(),
            DEFAULT_REVIEW_LOAD_RUN_SKILL
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload`: a user-edited review skill is preserved — self-heal
    /// only intervenes when the file is missing (same contract as the
    /// per-role settings.json).
    #[test]
    fn self_heal_preserves_user_edited_review_skill() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let skill = agent_skills_dir("p", REVIEW_AGENT)
            .unwrap()
            .join("load-run-detection/SKILL.md");
        let custom = "---\nname: load-run-detection\n---\nmy local heuristics\n";
        fs::write(&skill, custom).unwrap();

        self_heal_default_agents("p").unwrap();
        assert_eq!(fs::read_to_string(&skill).unwrap(), custom);

        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload`: user-edited review `instructions.md` is preserved
    /// byte-for-byte and the divergence notice fires once — same contract
    /// as the orchestrator/developer agents.
    #[test]
    fn self_heal_preserves_user_edited_review_instructions() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let custom = "# my review agent\nlocal serve rules\n";
        let path = agent_instructions_path("p", REVIEW_AGENT).unwrap();
        fs::write(&path, custom).unwrap();

        let outcomes = self_heal_default_agents("p").unwrap();
        let preserved = outcomes.iter().find(|o| o.agent() == REVIEW_AGENT).unwrap();
        assert_eq!(
            preserved,
            &AgentMaterializeOutcome::Preserved {
                agent: REVIEW_AGENT.to_string(),
                first_notice: true,
            }
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), custom);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn orchestrator_handoff_path_lives_inside_orchestrator_workspace() {
        // The path must land at `agents/orchestrator/handoff.md` so the
        // orchestrator (running with cwd = project dir) sees the same
        // path its instructions reference.
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = orchestrator_handoff_path("p").unwrap();
        let expected_tail = "agents/orchestrator/handoff.md";
        let path_str = path.to_string_lossy();
        assert!(
            path_str.ends_with(expected_tail),
            "expected path ending with {expected_tail}, got {path_str}"
        );

        // And the orchestrator-instructions-relative constant exposed
        // for the request message must match the path's tail so the
        // request message points the agent at the correct file.
        assert!(
            path_str.ends_with(ORCHESTRATOR_HANDOFF_REL),
            "ORCHESTRATOR_HANDOFF_REL ({ORCHESTRATOR_HANDOFF_REL}) must match the on-disk path tail",
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn take_orchestrator_handoff_reads_then_deletes_the_file() {
        // Acceptance criterion: handoff is one-shot. Read returns
        // the body and the file is gone afterwards so a second
        // ingestion on the next reload doesn't replay stale state.
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = orchestrator_handoff_path("p").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "in-flight: X\n").unwrap();

        let got = take_orchestrator_handoff("p").unwrap();
        assert_eq!(got.as_deref(), Some("in-flight: X\n"));
        assert!(
            !path.exists(),
            "handoff.md must be deleted after take_orchestrator_handoff"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn take_orchestrator_handoff_returns_none_when_absent() {
        // Cold-start case — no handoff on disk is normal, not an
        // error. Caller treats `None` as "start fresh".
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let got = take_orchestrator_handoff("p").unwrap();
        assert!(got.is_none());

        std::env::remove_var("SHELBI_HOME");
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
        assert!(is_default_agent(REVIEW_AGENT));
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
        assert_eq!(
            default_agent_body(REVIEW_AGENT),
            Some(DEFAULT_REVIEW_INSTRUCTIONS),
        );
        assert_eq!(default_agent_body("qa"), None);
    }

    /// Both default agents currently run on Claude Code, so both ship a
    /// settings template. The deploy path reads this via
    /// [`default_agent_settings`] to decide whether to use the per-role
    /// file or fall back to the project-wide workspace-settings.
    #[test]
    fn default_agent_settings_returns_shared_template_for_claude_roles() {
        assert_eq!(
            default_agent_settings(ORCHESTRATOR_AGENT),
            Some(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE),
        );
        assert_eq!(
            default_agent_settings(DEVELOPER_AGENT),
            Some(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE),
        );
        assert_eq!(
            default_agent_settings(REVIEW_AGENT),
            Some(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE),
        );
        assert_eq!(default_agent_settings("ghost"), None);
    }

    #[test]
    fn agent_workspace_dir_rejects_traversal_names() {
        // Residual chokepoint hardening (state-runtime F14): a `..`/absolute/
        // separator agent name must not escape the project's `agents/` dir.
        for bad in ["..", "../evil", "a/b", "/abs", "nested/../escape", ""] {
            assert!(
                agent_workspace_dir("p", bad).is_err(),
                "agent_workspace_dir should reject `{bad}`"
            );
        }
        // A normal single-component name still resolves.
        assert!(agent_workspace_dir("p", "developer").is_ok());
    }

    /// `shelbi init` happy path for per-role settings.json: both default
    /// Claude-Code agents land with their `settings.json` containing the
    /// SessionStart + Stop message-tail hook scripts. This is the file
    /// the deploy path prefers over the project-wide workspace-settings
    /// template on each task dispatch (Phase 7).
    #[test]
    fn materialize_writes_per_role_settings_json_with_message_hooks() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        for name in [ORCHESTRATOR_AGENT, DEVELOPER_AGENT] {
            let settings = agent_settings_path("p", name).unwrap();
            assert!(settings.is_file(), "{name}: settings.json missing");
            let body = fs::read_to_string(&settings).unwrap();
            assert!(body.contains("SessionStart"), "{name} missing SessionStart");
            assert!(
                body.contains(".shelbi/messages/$TASK_ID.tail.d"),
                "{name} missing tail-lock script"
            );
            assert!(
                body.contains("UNREAD=.shelbi/messages/$TASK_ID.unread.log"),
                "{name} missing Stop message-inject script"
            );
            assert!(
                body.contains("message-ack"),
                "{name} missing message-ack write"
            );
        }

        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload`: a workspace from a pre-Phase-7 shelbi (no
    /// settings.json) self-heals by dropping the bundled default back
    /// in. User edits to `instructions.md` are not what triggers this —
    /// the settings file is a separate self-heal seam.
    #[test]
    fn self_heal_recreates_missing_per_role_settings_json() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        // Simulate a pre-Phase-7 install: the workspace is fully
        // materialized but no settings.json was ever written.
        let settings = agent_settings_path("p", DEVELOPER_AGENT).unwrap();
        fs::remove_file(&settings).unwrap();
        assert!(!settings.exists());

        self_heal_default_agents("p").unwrap();
        assert!(settings.is_file(), "settings.json should be recreated");
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            DEFAULT_WORKSPACE_SETTINGS_TEMPLATE,
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// User-edited `settings.json` (e.g. they added a project-specific
    /// hook) is preserved by `shelbi reload`. Self-heal only intervenes
    /// when the file is missing.
    #[test]
    fn self_heal_preserves_user_edited_settings_json() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        materialize_default_agents("p").unwrap();
        let settings = agent_settings_path("p", DEVELOPER_AGENT).unwrap();
        let custom = "{ \"hooks\": { \"PostToolUse\": [] } }\n";
        fs::write(&settings, custom).unwrap();

        self_heal_default_agents("p").unwrap();
        assert_eq!(fs::read_to_string(&settings).unwrap(), custom);

        std::env::remove_var("SHELBI_HOME");
    }

    /// Acceptance criterion (b): missing `agents/_shared/preamble.md` is
    /// a no-op — the agent's own `instructions.md` flows through
    /// verbatim.
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
