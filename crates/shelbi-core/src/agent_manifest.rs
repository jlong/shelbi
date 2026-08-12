//! The runner-agnostic **agent manifest** (`agents/<name>/agent.yaml`) and the
//! resolution chains that turn it — plus the consuming project's config — into
//! a concrete *(runner kind, model, reasoning effort, permission mode)* for a
//! dispatch.
//!
//! An agent (a role: a system prompt + skill set) declares its *recommended*
//! runner and per-runner model/effort here; the consuming project *overrides*
//! those recommendations. The asymmetry is deliberate and lives in the field
//! names: the agent side says `preferred_runner` (a recommendation), the
//! project side says `runner` (authoritative). See
//! `Plans/agent-manifest-model-configuration` in the Shelbi ContextStore space.
//!
//! This module holds only the *pure* pieces — the manifest shape, the
//! low/medium/high/max effort scale, the permission-mode clamp, and the two
//! precedence chains (runner kind; model/effort). Loading `agent.yaml` off disk
//! is `shelbi_state`'s job; turning the resolved model/effort into launch flags
//! is the runner adapter's (`shelbi_agent::RunnerAdapter`) — the same seam that
//! already owns `--permission-mode` / `--continue`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Project, RunnerKind};

/// A coarse reasoning-effort dial that travels *with* a runner kind inside a
/// [`RunnerManifestConfig`]. Greenfield: there is no shared ordinal scale on the
/// wire — each adapter maps this to its runner's native knob (Claude `--effort`,
/// Codex `model_reasoning_effort`), so `Max` can land on the closest supported
/// tier where a runner has no exact match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Max,
}

impl ReasoningEffort {
    /// The canonical lowercase token — matches the YAML wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Max => "max",
        }
    }
}

/// One runner kind's slice of an [`AgentManifest`]: the `model` and
/// `reasoning_effort` the agent *recommends* when it runs on that kind. Both are
/// optional and resolve field-by-field (a project may override just the model
/// and let effort fall through to this block).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerManifestConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Compatibility hints an install/dispatch flow can enforce. Advisory in this
/// milestone (a uniform fleet is assumed — see the plan's §8); retained on the
/// wire so a future marketplace `shelbi agent add` can reject an incompatible
/// host up front.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequires {
    /// Which runner kinds this agent can actually run on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_kinds: Vec<RunnerKind>,
    /// A semver range the host `shelbi` must satisfy (e.g. `">=0.8"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelbi: Option<String>,
}

/// The runner-agnostic agent manifest parsed from `agents/<name>/agent.yaml`.
///
/// Multi-runner-first: `runners` is a per-kind map and `preferred_runner` names
/// which block is the default. Every value is a *recommendation* the consuming
/// project can override (see [`resolve_agent_launch`]). A missing manifest is
/// non-fatal — dispatch falls back to `Workspace.runner` — so every field but
/// `name` tolerates omission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Package identity (stable across installs), independent of the install
    /// directory. The directory name remains how a *project* addresses the
    /// agent; `name` is how a *marketplace* would.
    pub name: String,
    /// Manifest version (semver). Advisory in this milestone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Human/marketplace-facing description. When present it is the preferred
    /// display label for the agent (falling back to the directory name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Which runner *kind* the agent recommends by default — names the default
    /// entry in `runners`. A recommendation only: the project decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_runner: Option<RunnerKind>,
    /// Per-runner-kind model/effort recommendations. Keyed by the canonical
    /// [`RunnerKind`] (`claude` / `codex` / `generic`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runners: BTreeMap<RunnerKind, RunnerManifestConfig>,
    /// The permission posture the agent *requests*. Clamped to the project
    /// ceiling and never escalated (see [`clamp_permission_mode`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions_mode: Option<String>,
    /// Compatibility hints (advisory this milestone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<AgentRequires>,
}

impl AgentManifest {
    /// Parse a manifest from an `agent.yaml` string. The parsed manifest is
    /// [`validate`](Self::validate)d before it is returned, so a syntactically
    /// valid but semantically broken manifest (empty `name`, a
    /// `preferred_runner` the agent's own `requires.runner_kinds` excludes) is
    /// a hard error with a clear message rather than a silent
    /// wrong-runner/model dispatch.
    pub fn from_yaml_str(s: &str) -> crate::Result<Self> {
        let manifest: Self = serde_yaml::from_str(s)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Semantic validation beyond serde's shape check. Returns a clear,
    /// actionable error (`crate::Error::Other`) for each way a manifest can be
    /// well-formed YAML yet wrong:
    ///
    /// - **empty `name`** — the manifest must carry a package identity; a blank
    ///   one would make the agent unaddressable in a marketplace and reads as a
    ///   copy/paste slip.
    /// - **`preferred_runner` the agent can't run** — when the manifest both
    ///   names a `preferred_runner` and declares `requires.runner_kinds`, the
    ///   preferred kind must appear in that compatibility list; recommending a
    ///   default the agent itself says it can't run is a contradiction the
    ///   resolver would otherwise honor blindly.
    pub fn validate(&self) -> crate::Result<()> {
        if self.name.trim().is_empty() {
            return Err(crate::Error::Other(
                "agent.yaml `name` must not be empty".to_string(),
            ));
        }
        if let (Some(preferred), Some(requires)) = (self.preferred_runner, &self.requires) {
            if !requires.runner_kinds.is_empty() && !requires.runner_kinds.contains(&preferred) {
                let allowed = requires
                    .runner_kinds
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(crate::Error::Other(format!(
                    "agent.yaml `preferred_runner: {}` is not in `requires.runner_kinds` [{}] \
                     for agent `{}`",
                    preferred.as_str(),
                    allowed,
                    self.name,
                )));
            }
        }
        Ok(())
    }

    /// The preferred display label: the manifest `description` when set. The
    /// caller falls back to the directory name when this is `None` (the
    /// manifest is optional under the additive migration).
    pub fn display_label(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// A permission mode expressed as a rank on a single tightest→loosest axis, so
/// two vocabularies compare cleanly: the plan's generic names (`read-only`,
/// `auto-edit`, `full-access`) and the Claude/shelbi names shelbi already emits
/// (`plan`, `default`, `auto`/`acceptEdits`, `bypassPermissions`). Higher rank =
/// looser (more capability). An unrecognized string ranks as `default` so a
/// typo can never silently *widen* the ceiling.
fn permission_rank(mode: &str) -> u8 {
    match mode.trim() {
        "read-only" | "readonly" | "plan" => 0,
        "default" => 1,
        "auto" | "auto-edit" | "acceptEdits" | "acceptedits" => 2,
        "full-access" | "full" | "bypassPermissions" | "bypass" => 3,
        _ => 1,
    }
}

/// Translate a resolved (canonical) permission mode into the value shelbi hands
/// to the runner adapter. The two generic aliases the plan uses map onto the
/// Claude modes shelbi actually launches with; everything shelbi already emits
/// (`auto`, `default`, `plan`, `acceptEdits`, `bypassPermissions`) passes
/// through untouched so existing behavior is byte-for-byte preserved.
fn permission_flag_value(mode: &str) -> String {
    match mode.trim() {
        "read-only" | "readonly" => "plan".to_string(),
        "full-access" | "full" => "bypassPermissions".to_string(),
        other => other.to_string(),
    }
}

/// Resolve the effective permission mode as a **ceiling clamp**: unlike model
/// (taste), permissions is a security boundary, so the project always wins the
/// *loosen* direction. The agent may request equal-or-tighter and gets it; a
/// request looser than the ceiling is clamped down to the ceiling, never
/// granted. `ceiling` is the project cap (its `workspace_permissions_mode`);
/// `request` is the agent manifest's `permissions_mode` (absent → the ceiling
/// applies unchanged). The returned string is already the runner-flag value.
pub fn clamp_permission_mode(ceiling: &str, request: Option<&str>) -> String {
    match request {
        None => permission_flag_value(ceiling),
        Some(req) => {
            if permission_rank(req) <= permission_rank(ceiling) {
                permission_flag_value(req)
            } else {
                permission_flag_value(ceiling)
            }
        }
    }
}

/// The outcome of the two resolution chains for one dispatch: which runner
/// *kind* the agent runs on, the `model`/`effort` values for that kind, and the
/// clamped permission mode. Turning `kind` into a concrete [`AgentRunnerSpec`]
/// and `model`/`effort` into launch flags is the caller's job (it needs the
/// runner fleet and the adapter) — this struct is the pure decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentLaunch {
    pub kind: RunnerKind,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub permission_mode: String,
}

/// Resolve runner kind + model + effort + permission mode for `agent_name`,
/// honoring the full precedence chain from the plan (§4). `manifest` is the
/// agent's parsed `agent.yaml`, or `None` when it ships none.
///
/// **Runner kind** (highest→lowest): project `agents.<name>.runner` →
/// `agent.yaml` `preferred_runner` → built-in default (Claude). (A task/status
/// override sits above all of these; it has no config surface yet and is passed
/// as `None` today.)
///
/// **Model / effort** (highest→lowest, each field independent): project
/// top-level `runners.<kind>.{model,reasoning_effort}` → `agent.yaml`
/// `runners.<kind>.{model,reasoning_effort}` → built-in default (none).
///
/// **Permission mode**: the project ceiling (`workspace_permissions_mode`)
/// clamps the agent's request downward (see [`clamp_permission_mode`]).
pub fn resolve_agent_launch(
    project: &Project,
    agent_name: &str,
    manifest: Option<&AgentManifest>,
) -> ResolvedAgentLaunch {
    // --- runner kind chain -------------------------------------------------
    let kind = project
        .agents
        .get(agent_name)
        .and_then(|a| a.runner)
        .or_else(|| manifest.and_then(|m| m.preferred_runner))
        .unwrap_or(RunnerKind::Claude);

    // --- model / effort chains (field-independent) -------------------------
    let project_runner = project.runners.get(&kind);
    let manifest_runner = manifest.and_then(|m| m.runners.get(&kind));

    let model = project_runner
        .and_then(|r| r.model.clone())
        .or_else(|| manifest_runner.and_then(|r| r.model.clone()));
    let effort = project_runner
        .and_then(|r| r.reasoning_effort)
        .or_else(|| manifest_runner.and_then(|r| r.reasoning_effort));

    // --- permission mode: project ceiling clamps the request down ----------
    let permission_mode = clamp_permission_mode(
        &project.workspace_permissions_mode,
        manifest.and_then(|m| m.permissions_mode.as_deref()),
    );

    ResolvedAgentLaunch {
        kind,
        model,
        effort,
        permission_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentRunnerSpec, ProjectAgentConfig, ProjectRunnerConfig};

    fn manifest_yaml() -> &'static str {
        r#"
name: adversarial-reviewer
version: 1.2.0
description: Adversarial code review before a human sees the branch
preferred_runner: claude
runners:
  claude:
    model: claude-opus-4-8
    reasoning_effort: high
  codex:
    model: gpt-5
    reasoning_effort: high
permissions_mode: read-only
requires:
  runner_kinds: [claude, codex]
  shelbi: ">=0.8"
"#
    }

    #[test]
    fn parses_multi_runner_manifest() {
        let m = AgentManifest::from_yaml_str(manifest_yaml()).unwrap();
        assert_eq!(m.name, "adversarial-reviewer");
        assert_eq!(m.version.as_deref(), Some("1.2.0"));
        assert_eq!(m.preferred_runner, Some(RunnerKind::Claude));
        assert_eq!(
            m.runners.get(&RunnerKind::Claude).unwrap().model.as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            m.runners.get(&RunnerKind::Codex).unwrap().reasoning_effort,
            Some(ReasoningEffort::High)
        );
        assert_eq!(m.permissions_mode.as_deref(), Some("read-only"));
        let req = m.requires.unwrap();
        assert_eq!(req.runner_kinds, vec![RunnerKind::Claude, RunnerKind::Codex]);
        assert_eq!(req.shelbi.as_deref(), Some(">=0.8"));
    }

    #[test]
    fn manifest_roundtrips_through_yaml() {
        let m = AgentManifest::from_yaml_str(manifest_yaml()).unwrap();
        let s = serde_yaml::to_string(&m).unwrap();
        let back = AgentManifest::from_yaml_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn minimal_manifest_only_needs_name() {
        let m = AgentManifest::from_yaml_str("name: bare\n").unwrap();
        assert_eq!(m.name, "bare");
        assert!(m.preferred_runner.is_none());
        assert!(m.runners.is_empty());
        assert!(m.permissions_mode.is_none());
    }

    #[test]
    fn validate_rejects_empty_name_and_incompatible_preferred_runner() {
        // Empty name → rejected at parse time.
        let err = AgentManifest::from_yaml_str("name: \"\"\n").unwrap_err();
        assert!(err.to_string().contains("`name` must not be empty"), "{err}");

        // preferred_runner outside requires.runner_kinds → rejected, and the
        // message names the offending kind and the allowed set.
        let err = AgentManifest::from_yaml_str(
            "name: r\npreferred_runner: codex\nrequires:\n  runner_kinds: [claude]\n",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("preferred_runner: codex"), "{msg}");
        assert!(msg.contains("claude"), "{msg}");

        // preferred_runner IN requires.runner_kinds → accepted.
        assert!(AgentManifest::from_yaml_str(
            "name: r\npreferred_runner: claude\nrequires:\n  runner_kinds: [claude, codex]\n",
        )
        .is_ok());

        // No requires block → preferred_runner is unconstrained.
        assert!(AgentManifest::from_yaml_str("name: r\npreferred_runner: codex\n").is_ok());
    }

    fn base_project() -> Project {
        Project::from_yaml_str(
            r#"
name: proj
repo: /tmp/proj
workspace_permissions_mode: auto
machines:
  - name: local
    kind: local
    work_dir: /tmp/proj
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
workspaces: []
"#,
        )
        .unwrap()
    }

    #[test]
    fn kind_falls_through_manifest_preferred_then_default() {
        let project = base_project();
        // No manifest, no project override → built-in Claude default.
        let r = resolve_agent_launch(&project, "review", None);
        assert_eq!(r.kind, RunnerKind::Claude);

        // Manifest recommends codex → honored when the project is silent.
        let m = AgentManifest::from_yaml_str(
            "name: r\npreferred_runner: codex\nrunners:\n  codex:\n    model: gpt-5\n",
        )
        .unwrap();
        let r = resolve_agent_launch(&project, "review", Some(&m));
        assert_eq!(r.kind, RunnerKind::Codex);
        assert_eq!(r.model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn project_agent_runner_overrides_manifest_preferred() {
        let mut project = base_project();
        project
            .agents
            .insert("review".into(), ProjectAgentConfig { runner: Some(RunnerKind::Codex) });
        let m = AgentManifest::from_yaml_str(
            "name: r\npreferred_runner: claude\nrunners:\n  codex:\n    model: gpt-5\n  claude:\n    model: claude-opus-4-8\n",
        )
        .unwrap();
        let r = resolve_agent_launch(&project, "review", Some(&m));
        // Project pins codex over the manifest's claude preference; the codex
        // block's model + effort travel together with the chosen kind.
        assert_eq!(r.kind, RunnerKind::Codex);
        assert_eq!(r.model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn project_runner_model_is_field_level_authoritative() {
        let mut project = base_project();
        // Project sets a house model on the claude kind but leaves effort unset.
        project.runners.insert(
            RunnerKind::Claude,
            ProjectRunnerConfig {
                spec: AgentRunnerSpec {
                    command: "claude".into(),
                    flags: vec![],
                    prompt_injection: None,
                    dialog_signatures: vec![],
                    integration: None,
                },
                model: Some("claude-sonnet-5".into()),
                reasoning_effort: None,
            },
        );
        let m = AgentManifest::from_yaml_str(
            "name: r\npreferred_runner: claude\nrunners:\n  claude:\n    model: claude-opus-4-8\n    reasoning_effort: high\n",
        )
        .unwrap();
        let r = resolve_agent_launch(&project, "review", Some(&m));
        // Model comes from the project (authoritative); effort falls through to
        // the manifest since the project omitted it.
        assert_eq!(r.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(r.effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn permission_mode_clamps_down_never_up() {
        // Ceiling `auto` (rank 2). An agent requesting the tighter `read-only`
        // gets it (mapped to claude's `plan`).
        assert_eq!(clamp_permission_mode("auto", Some("read-only")), "plan");
        // An agent requesting the looser `full-access` is clamped to the
        // ceiling, not granted.
        assert_eq!(clamp_permission_mode("read-only", Some("full-access")), "plan");
        // Equal request is honored.
        assert_eq!(clamp_permission_mode("auto", Some("auto")), "auto");
        // No request → the ceiling applies unchanged.
        assert_eq!(clamp_permission_mode("auto", None), "auto");
        // Claude-vocabulary ceiling passes through untouched.
        assert_eq!(
            clamp_permission_mode("acceptEdits", Some("bypassPermissions")),
            "acceptEdits"
        );
    }
}
