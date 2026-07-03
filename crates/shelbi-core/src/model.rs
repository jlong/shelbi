use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
//
// Fields are grouped into three buckets that determine which YAML file they
// belong to under [`ConfigMode::InRepo`]:
//
// * **Shared** — safe to commit to the project repo. In global mode these
//   sit alongside everything else in `~/.shelbi/projects/<name>.yaml`; in
//   in-repo mode they live in `<repo>/.shelbi/project.yaml`.
// * **User-local** — per-machine or per-developer state that must never be
//   committed. In global mode they share the same YAML as the shared
//   fields; in in-repo mode they move to `~/.shelbi/projects/<name>/local.yaml`.
// * **Runtime** — populated after the YAML is loaded (never serialized).
//
// The bucket lists in [`SHARED_PROJECT_FIELDS`] and [`LOCAL_PROJECT_FIELDS`]
// (below) drive the split-mode parse/serialize helpers on `Project`. Keep
// them in sync with the field order here.
//
// See `Plans/in-repo-vs-global-project-config.md`.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    // --- shared -----------------------------------------------------------
    pub name: String,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    /// Which YAML layout this project uses on disk. See [`ConfigMode`].
    /// `None` (the default) means [`ConfigMode::Global`] — the historical
    /// single-YAML shape — and is elided from the wire form so existing
    /// projects don't grow an extra key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_mode: Option<ConfigMode>,
    pub orchestrator: OrchestratorSpec,
    pub agent_runners: std::collections::BTreeMap<String, AgentRunnerSpec>,
    /// Optional GitHub repo URL (e.g. `git@github.com:owner/repo.git`)
    /// recorded by the project-setup wizard. Informational for now — the
    /// merge `--pr` flow still resolves the remote via local git config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_url: Option<String>,
    /// How often the orchestrator polls each workspace pane for state changes.
    #[serde(default = "default_workspace_poll_interval_secs")]
    pub workspace_poll_interval_secs: u64,
    /// Permissions posture rendered into the workspace settings template
    /// (see [`Project::workspace_settings_template`]). The default `auto`
    /// is mapped to claude's `acceptEdits` at render time.
    #[serde(default = "default_workspace_permissions_mode")]
    pub workspace_permissions_mode: String,
    /// Optional override for the path to the per-project workspace settings
    /// template. When `None`, the default at
    /// `~/.shelbi/projects/<name>/workspace-settings.json.template` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_settings_template: Option<PathBuf>,
    /// Zen Mode configuration: which checks to run, how long to wait on
    /// CI, and which paths require human review even in Zen Mode.
    #[serde(default)]
    pub zen: ZenConfig,
    /// Periodic heartbeat the hub-side poller writes into
    /// `~/.shelbi/events.log` so the orchestrator's `events tail --follow`
    /// watch has a guaranteed recurring trigger when the board is quiet.
    /// Default `3m`; set to `off` to disable. See [`HeartbeatConfig`].
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    /// Project-level git config: where workspace branches are based and how
    /// `shelbi merge` (and Zen Mode's auto-merge path) integrates them
    /// back. `base_branch` falls back to [`Project::default_branch`] when
    /// unset, so existing project YAMLs keep working without a `git:`
    /// block. See [`GitConfig`] and [`Project::base_branch`] /
    /// [`Project::merge_strategy`].
    #[serde(default)]
    pub git: GitConfig,
    /// Optional `review:` block configuring how *review workspaces* load and
    /// serve a task's branch for human inspection (ports, setup/serve
    /// commands, ready probe). Absent block ⇒ all defaults (base port 3000,
    /// stride 10, auto-detected setup/serve). Rides the shared/repo config
    /// half — it describes the project, not the machine. See
    /// [`ReviewConfig`] and `Plans/review-workspaces.md` §5.2. Elided from
    /// the wire form when fully default so existing project YAMLs — which
    /// carry no `review:` key — round-trip unchanged.
    #[serde(default, skip_serializing_if = "is_default")]
    pub review: ReviewConfig,

    // --- user-local -------------------------------------------------------
    pub repo: String,
    pub machines: Vec<Machine>,
    #[serde(default)]
    pub editor: Option<String>,
    /// Fixed pool of workspace agents available to this project. Each owns a
    /// stable worktree on its machine; the orchestrator routes tasks to
    /// workspaces by name. See [`WorkspaceSpec`].
    ///
    /// Accepts the legacy `workers:` key as an alias for one release; new
    /// projects materialized by the wizard / `shelbi init` emit
    /// `workspaces:`. See `shelbi_state::load_project` for the
    /// one-shot deprecation warning that fires when the legacy key is read.
    #[serde(default, alias = "workers")]
    pub workspaces: Vec<WorkspaceSpec>,
    /// ContextStore spaces that should be rsynced from a remote workspace's
    /// machine back to hub after the workspace hands off for review. Each
    /// space's path is interpreted on both hub and remote — leading `~`
    /// is expanded by rsync against the respective `$HOME`. See
    /// `shelbi_orchestrator::contextstore` for the sync path. Default
    /// empty: no sync runs unless the project opts in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contextstore_sync: Vec<ContextStoreSyncSpec>,

    // --- runtime ----------------------------------------------------------
    /// Project-shape signals discovered at load time (Cargo workspace,
    /// Next.js, Docker, …). Populated by [`Project::detect_shapes`] when
    /// the project YAML is loaded; serialization is skipped so the on-disk
    /// form stays declarative. Drives the auto-extended danger-paths list
    /// in [`danger_paths_for_project`].
    #[serde(skip)]
    pub detected_shapes: Vec<ProjectShape>,
}

/// How this project's configuration is laid out on disk.
///
/// * [`ConfigMode::Global`] (the default and current behavior): everything
///   lives under `~/.shelbi/projects/<name>/`, with the project YAML at
///   `~/.shelbi/projects/<name>.yaml`.
/// * [`ConfigMode::InRepo`]: shared fields live in
///   `<repo>/.shelbi/project.yaml` (committed to git); user-local fields
///   live in `~/.shelbi/projects/<name>/local.yaml` (never committed).
///
/// The variant lives in whichever YAML the discovery code finds first, so
/// the value on `Project::config_mode` is really "the mode implied by the
/// file we loaded from" — see `Plans/in-repo-vs-global-project-config.md`
/// §Resolved decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigMode {
    #[default]
    Global,
    InRepo,
}

/// YAML keys that belong in the *shared* half of a split project config.
/// Matches the order of the shared fields on [`Project`].
pub const SHARED_PROJECT_FIELDS: &[&str] = &[
    "name",
    "default_branch",
    "config_mode",
    "orchestrator",
    "agent_runners",
    "github_url",
    "workspace_poll_interval_secs",
    "workspace_permissions_mode",
    "workspace_settings_template",
    "zen",
    "heartbeat",
    "git",
    "review",
];

/// YAML keys that belong in the *user-local* half of a split project
/// config. Includes the legacy `workers` alias so a misplaced legacy key
/// still routes to the right side of the split.
pub const LOCAL_PROJECT_FIELDS: &[&str] = &[
    "repo",
    "machines",
    "editor",
    "workspaces",
    "workers",
    "contextstore_sync",
];

/// One ContextStore space that shelbi keeps in sync between hub and
/// remote workspaces. The `space` field is matched against the body
/// heuristic (`"<space>/"` substring) when deciding whether a finished
/// task touched ContextStore; `path` is fed to rsync on both sides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextStoreSyncSpec {
    pub space: String,
    pub path: PathBuf,
}

/// One blocking-dialog signature: a substring that, when present in a
/// workspace pane's captured text, means the runner is frozen on an
/// interactive prompt (usage-limit, workspace-trust, permission-confirm, …)
/// that no hook or pane-title marker will ever clear on its own. The hub
/// poller matches these against its `tmux capture-pane` sample so a stall
/// surfaces as an event instead of sitting invisible behind a stale
/// `shelbi:working` title.
///
/// `kind` is the short token that lands in the emitted event
/// (`reason=dialog:<kind>`); `pattern` is matched case-insensitively as a
/// plain substring. Several signatures may share a `kind` (e.g. two
/// wordings of the same trust prompt).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DialogSignature {
    pub kind: String,
    pub pattern: String,
}

impl DialogSignature {
    pub fn new(kind: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            pattern: pattern.into(),
        }
    }
}

/// Built-in blocking-dialog signatures for a runner, keyed on the runner's
/// executable basename. Used when the runner declares no `dialog_signatures`
/// of its own, so the common cases work with zero config. Returns an empty
/// list for unknown runners — an unrecognized runner simply gets no dialog
/// detection until the user adds signatures in project.yaml.
///
/// The `claude` set covers the dialogs seen to freeze a whole board in
/// practice: the usage-limit modal ("Stop and wait for limit to reset"),
/// the first-run workspace-trust prompt, and a tool-permission confirm.
pub fn default_dialog_signatures(command: &str) -> Vec<DialogSignature> {
    let base = Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(command);
    match base {
        "claude" => vec![
            DialogSignature::new("usage-limit", "Stop and wait for limit to reset"),
            DialogSignature::new("trust", "Do you trust the files"),
            DialogSignature::new("trust", "trust this folder"),
            DialogSignature::new("permission", "Enter to confirm"),
        ],
        _ => Vec::new(),
    }
}

/// How often the hub poller emits a `project=<name> heartbeat` line into
/// `~/.shelbi/events.log`. The heartbeat is the orchestrator's fallback
/// trigger — `events tail --follow` may sit silent for hours on a quiet
/// board, and a recurring line guarantees the watch wakes up to check
/// active tasks even when no real transition has fired.
///
/// On disk the value is a duration string (`45s`, `3m`, `1h`) or the
/// literal `off`. Bare integers are rejected — there's no implicit unit.
/// See `HEARTBEAT_DEFAULT` for the default interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum HeartbeatConfig {
    Off,
    Every(Duration),
}

/// Default heartbeat cadence: 3 minutes. Tuned to be frequent enough that
/// a stuck orchestrator wakes up within a couple of intervals, but rare
/// enough that an idle hub doesn't bloat `events.log` with thousands of
/// lines a day.
pub const HEARTBEAT_DEFAULT: Duration = Duration::from_secs(180);

impl Default for HeartbeatConfig {
    fn default() -> Self {
        HeartbeatConfig::Every(HEARTBEAT_DEFAULT)
    }
}

impl HeartbeatConfig {
    /// The cadence, or `None` if heartbeats are turned off.
    pub fn interval(&self) -> Option<Duration> {
        match self {
            HeartbeatConfig::Off => None,
            HeartbeatConfig::Every(d) => Some(*d),
        }
    }
}

impl std::fmt::Display for HeartbeatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeartbeatConfig::Off => f.write_str("off"),
            HeartbeatConfig::Every(d) => {
                let secs = d.as_secs();
                if secs == 0 {
                    return f.write_str("0s");
                }
                if secs % 3600 == 0 {
                    write!(f, "{}h", secs / 3600)
                } else if secs % 60 == 0 {
                    write!(f, "{}m", secs / 60)
                } else {
                    write!(f, "{secs}s")
                }
            }
        }
    }
}

impl From<HeartbeatConfig> for String {
    fn from(h: HeartbeatConfig) -> Self {
        h.to_string()
    }
}

impl TryFrom<String> for HeartbeatConfig {
    type Error = String;
    fn try_from(s: String) -> std::result::Result<Self, Self::Error> {
        s.parse()
    }
}

impl std::str::FromStr for HeartbeatConfig {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(
                "heartbeat: empty string — use a duration like `3m` or `off`".to_string(),
            );
        }
        if trimmed.eq_ignore_ascii_case("off") {
            return Ok(HeartbeatConfig::Off);
        }
        // Require an explicit unit suffix. Without one we'd have to guess
        // (seconds? minutes?) and a bug like `heartbeat: 3` silently
        // becoming three-of-the-wrong-unit is exactly the foot-gun this
        // type is meant to avoid.
        let last = trimmed.chars().last().unwrap();
        let (num_part, mult) = match last {
            's' | 'S' => (&trimmed[..trimmed.len() - last.len_utf8()], 1u64),
            'm' | 'M' => (&trimmed[..trimmed.len() - last.len_utf8()], 60u64),
            'h' | 'H' => (&trimmed[..trimmed.len() - last.len_utf8()], 3_600u64),
            _ => {
                return Err(format!(
                    "heartbeat `{s}`: missing unit — use `s`, `m`, `h` (e.g. `45s`, `3m`, `1h`) or `off`"
                ));
            }
        };
        let n: u64 = num_part.trim().parse().map_err(|_| {
            format!("heartbeat `{s}`: not a number followed by `s`/`m`/`h`")
        })?;
        if n == 0 {
            return Err(format!("heartbeat `{s}`: zero interval — use `off` to disable"));
        }
        let secs = n
            .checked_mul(mult)
            .ok_or_else(|| format!("heartbeat `{s}`: duration overflows"))?;
        Ok(HeartbeatConfig::Every(Duration::from_secs(secs)))
    }
}

fn default_branch() -> String {
    "main".to_string()
}

// ---------------------------------------------------------------------------
// Git config (base branch + merge strategy)

/// Project-level git config: which branch to base feature branches on
/// and how to integrate them back. Stored under the `git:` key in the
/// project YAML; absent altogether on existing projects, in which case
/// every field falls back to its default.
///
/// `base_branch` is intentionally `Option` so old YAMLs that only carry
/// the top-level `default_branch:` keep working — the accessor
/// [`Project::base_branch`] resolves the effective value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitConfig {
    /// Branch to base workspace branches on and target when merging. When
    /// `None`, callers fall back to [`Project::default_branch`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// How `shelbi merge` (and Zen Mode's auto-merge path) integrates a
    /// workspace branch back into [`Project::base_branch`]. Default
    /// [`MergeStrategy::Squash`] preserves the historical behavior.
    #[serde(default)]
    pub merge_strategy: MergeStrategy,
}

/// How a workspace branch is integrated back into the base branch. Maps
/// 1:1 onto `gh pr merge --{squash,merge,rebase}` and the equivalent
/// local `git merge` / `git rebase` invocations.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Collapse the branch into a single commit on the base branch.
    /// Default — matches the original hardcoded behavior of `shelbi
    /// merge` and Zen Mode's auto-merge.
    #[default]
    Squash,
    /// Standard three-way merge — preserves every commit on the branch
    /// plus a merge commit on top.
    Merge,
    /// Replay the branch's commits on top of the base branch (no merge
    /// commit).
    Rebase,
}

impl MergeStrategy {
    /// The `gh pr merge` flag corresponding to this strategy: `--squash`,
    /// `--merge`, or `--rebase`. The hyphen-prefixed form matches what
    /// the existing call sites pass to `gh`.
    pub fn gh_flag(self) -> &'static str {
        match self {
            MergeStrategy::Squash => "--squash",
            MergeStrategy::Merge => "--merge",
            MergeStrategy::Rebase => "--rebase",
        }
    }

    /// Short label for diagnostics — matches the YAML wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            MergeStrategy::Squash => "squash",
            MergeStrategy::Merge => "merge",
            MergeStrategy::Rebase => "rebase",
        }
    }
}

impl std::fmt::Display for MergeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Review config (how review workspaces load & serve a branch)

/// Per-project configuration for *review workspaces* — the pool slots that
/// load a task's branch and serve it for human inspection. Stored under the
/// `review:` key in the project YAML; an absent block means "auto-detect
/// everything, base port 3000, stride 10" — every field falls back to its
/// default. Belongs in the shared (repo) config half. See
/// `Plans/review-workspaces.md` §5.2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewConfig {
    /// Base TCP port for the first review workspace's dev server. Each
    /// review workspace gets a deterministic slot `base_port + n *
    /// port_stride` (see [`Project::review_workspaces`]). Default 3000.
    #[serde(default = "default_base_port")]
    pub base_port: u16,
    /// Port spacing between consecutive review workspaces on a machine, so
    /// concurrent servers never collide. Default 10 (review-1→3000,
    /// review-2→3010).
    #[serde(default = "default_port_stride")]
    pub port_stride: u16,
    /// Explicit setup commands run before the server starts (e.g. `npm
    /// install`). Empty ⇒ the Review agent auto-detects from the project
    /// shape. Declared commands always win over auto-detection.
    #[serde(default)]
    pub setup: Vec<String>,
    /// Explicit command that starts the dev server (e.g. `npm run dev --
    /// --port $PORT`). `None` ⇒ auto-detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serve: Option<String>,
    /// How the Review agent decides the server is up and ready for a human.
    /// `None` ⇒ a default settle/probe chosen by the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_probe: Option<ReadyProbe>,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            base_port: default_base_port(),
            port_stride: default_port_stride(),
            setup: Vec::new(),
            serve: None,
            ready_probe: None,
        }
    }
}

/// Default base port for review dev servers: 3000.
fn default_base_port() -> u16 {
    3000
}

/// Default port stride between review workspaces: 10.
fn default_port_stride() -> u16 {
    10
}

/// How the Review agent probes a freshly-started dev server for readiness
/// before handing off to the human. Both fields are optional; a bare
/// `ready_probe:` with neither set just means "wait `timeout`, then assume
/// ready."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadyProbe {
    /// URL to poll for an HTTP 200 (e.g. `http://localhost:$PORT`). `None`
    /// ⇒ no HTTP probe; the agent falls back to a fixed settle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<String>,
    /// How long to wait for the probe to succeed before giving up.
    /// Serialized as a number of seconds. Default 90s.
    #[serde(default = "default_ready_probe_timeout", with = "duration_secs")]
    pub timeout: Duration,
}

impl Default for ReadyProbe {
    fn default() -> Self {
        Self { http: None, timeout: default_ready_probe_timeout() }
    }
}

/// Default ready-probe timeout: 90 seconds.
fn default_ready_probe_timeout() -> Duration {
    Duration::from_secs(90)
}

fn default_workspace_poll_interval_secs() -> u64 {
    5
}

fn default_workspace_permissions_mode() -> String {
    "auto".to_string()
}

impl Project {
    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.name == name)
    }

    /// Effective base branch — `git.base_branch` when set, otherwise the
    /// top-level `default_branch`. Call this instead of reading
    /// `default_branch` directly so a project that adopts the `git:`
    /// block transparently overrides the older field.
    pub fn base_branch(&self) -> &str {
        self.git
            .base_branch
            .as_deref()
            .unwrap_or(&self.default_branch)
    }

    /// Configured merge strategy, defaulting to [`MergeStrategy::Squash`].
    pub fn merge_strategy(&self) -> MergeStrategy {
        self.git.merge_strategy
    }

    pub fn runner(&self, name: &str) -> Option<&AgentRunnerSpec> {
        self.agent_runners.get(name)
    }

    pub fn workspace(&self, name: &str) -> Option<&WorkspaceSpec> {
        self.workspaces.iter().find(|w| w.name == name)
    }

    /// Review-role workspaces declared on `machine`, in declaration order.
    /// The slot index into this list drives the deterministic port
    /// assignment (`review.base_port + i * review.port_stride`). See
    /// [`ReviewConfig`].
    pub fn review_workspaces(&self, machine: &str) -> Vec<&WorkspaceSpec> {
        self.workspaces
            .iter()
            .filter(|w| w.machine == machine && w.is_review())
            .collect()
    }

    /// All dev-role workspaces across every machine, in declaration order.
    /// These are the slots that pick up tasks for autonomous development.
    pub fn dev_workspaces(&self) -> Vec<&WorkspaceSpec> {
        self.workspaces.iter().filter(|w| !w.is_review()).collect()
    }

    /// Inspect the filesystem at `root` (typically `self.repo`) and cache
    /// the recognized [`ProjectShape`]s on `self.detected_shapes`. Safe
    /// to call from `load_project`: any I/O error is treated as "no
    /// signal" rather than fatal.
    pub fn detect_shapes(&mut self, root: impl AsRef<Path>) {
        self.detected_shapes = detect_project_shapes(root.as_ref());
    }

    /// Cross-check workspaces reference declared machines and runners, and
    /// enforce the review-workspace scarcity invariant.
    ///
    /// Hard errors: an unknown machine or runner, or **more than
    /// [`MAX_REVIEW_WORKSPACES_PER_MACHINE`] review workspaces on a single
    /// machine** — review slots may each pin a running server and a port, so
    /// over-provisioning them is almost always a config mistake worth
    /// surfacing loudly. Other conditions (e.g. a machine with zero review
    /// workspaces) are soft and left to callers to warn about rather than
    /// fail the load. See `Plans/review-workspaces.md` §5.1.
    pub fn validate_workspaces(&self) -> crate::Result<()> {
        let mut review_per_machine: BTreeMap<&str, usize> = BTreeMap::new();
        for w in &self.workspaces {
            if self.machine(&w.machine).is_none() {
                return Err(crate::Error::UnknownMachine(w.machine.clone()));
            }
            if self.runner(&w.runner).is_none() {
                return Err(crate::Error::UnknownRunner(w.runner.clone()));
            }
            if w.is_review() {
                *review_per_machine.entry(w.machine.as_str()).or_default() += 1;
            }
        }
        for (machine, count) in review_per_machine {
            if count > MAX_REVIEW_WORKSPACES_PER_MACHINE {
                return Err(crate::Error::Other(format!(
                    "machine `{machine}` declares {count} review workspaces, but at \
                     most {MAX_REVIEW_WORKSPACES_PER_MACHINE} are allowed per machine \
                     (each review workspace may hold a running server and a port)"
                )));
            }
        }
        Ok(())
    }

    /// Parse a `Project` from a single YAML — the [`ConfigMode::Global`]
    /// on-disk shape, matching the historical behavior of
    /// `serde_yaml::from_str::<Project>`.
    ///
    /// The split-mode counterpart is [`Project::from_split_yaml_str`].
    pub fn from_yaml_str(text: &str) -> crate::Result<Self> {
        Ok(serde_yaml::from_str(text)?)
    }

    /// Parse a `Project` from the two YAML halves of
    /// [`ConfigMode::InRepo`]: `shared_yaml` is the committed
    /// `<repo>/.shelbi/project.yaml`, `local_yaml` is the user-local
    /// `~/.shelbi/projects/<name>/local.yaml`. The halves are validated
    /// for correct key placement (a misplaced field produces
    /// [`crate::Error::MisplacedProjectField`]) and then merged into the
    /// same flat wire form the global-mode parser consumes.
    ///
    /// Duplicate keys — the same field name appearing in both halves —
    /// are rejected: which side wins is not a decision this layer should
    /// make, so the merge refuses instead of silently dropping one.
    pub fn from_split_yaml_str(
        shared_yaml: &str,
        local_yaml: &str,
    ) -> crate::Result<Self> {
        let shared_val: serde_yaml::Value = serde_yaml::from_str(shared_yaml)?;
        let local_val: serde_yaml::Value = serde_yaml::from_str(local_yaml)?;

        let shared_map = require_mapping(&shared_val, "shared")?;
        let local_map = require_mapping(&local_val, "user-local")?;

        check_field_placement(
            shared_map,
            "shared",
            SHARED_PROJECT_FIELDS,
            LOCAL_PROJECT_FIELDS,
            "user-local",
        )?;
        check_field_placement(
            local_map,
            "user-local",
            LOCAL_PROJECT_FIELDS,
            SHARED_PROJECT_FIELDS,
            "shared",
        )?;

        let mut merged = serde_yaml::Mapping::new();
        for (k, v) in shared_map {
            merged.insert(k.clone(), v.clone());
        }
        for (k, v) in local_map {
            if merged.contains_key(k) {
                let name = k.as_str().unwrap_or("<non-string key>").to_string();
                return Err(crate::Error::Other(format!(
                    "project YAML key `{name}` appears in both the shared \
                     and user-local files; each key must live in exactly one"
                )));
            }
            merged.insert(k.clone(), v.clone());
        }
        Ok(serde_yaml::from_value(serde_yaml::Value::Mapping(merged))?)
    }

    /// Serialize the shared half of this project — the committed
    /// `<repo>/.shelbi/project.yaml` under [`ConfigMode::InRepo`].
    /// User-local and runtime fields are omitted.
    pub fn to_shared_yaml_string(&self) -> crate::Result<String> {
        let mut value = serde_yaml::to_value(self)?;
        retain_fields(&mut value, SHARED_PROJECT_FIELDS);
        Ok(serde_yaml::to_string(&value)?)
    }

    /// Serialize the user-local half of this project — the gitignored
    /// `~/.shelbi/projects/<name>/local.yaml` under
    /// [`ConfigMode::InRepo`]. Shared and runtime fields are omitted.
    pub fn to_local_yaml_string(&self) -> crate::Result<String> {
        let mut value = serde_yaml::to_value(self)?;
        retain_fields(&mut value, LOCAL_PROJECT_FIELDS);
        Ok(serde_yaml::to_string(&value)?)
    }
}

fn require_mapping<'a>(
    value: &'a serde_yaml::Value,
    file_kind: &'static str,
) -> crate::Result<&'a serde_yaml::Mapping> {
    value.as_mapping().ok_or_else(|| {
        crate::Error::Other(format!(
            "{file_kind} project YAML must be a mapping at the top level"
        ))
    })
}

fn check_field_placement(
    map: &serde_yaml::Mapping,
    found_in: &'static str,
    valid_here: &[&str],
    valid_elsewhere: &[&str],
    other_file: &'static str,
) -> crate::Result<()> {
    for (k, _) in map {
        let Some(name) = k.as_str() else { continue };
        if valid_here.contains(&name) {
            continue;
        }
        if valid_elsewhere.contains(&name) {
            return Err(crate::Error::MisplacedProjectField {
                field: name.to_string(),
                found_in,
                expected_in: other_file,
            });
        }
        // Not recognized in either bucket — leave it to the flat Project
        // Deserialize to accept or reject, matching global-mode behavior.
    }
    Ok(())
}

fn retain_fields(value: &mut serde_yaml::Value, keep: &[&str]) {
    if let Some(map) = value.as_mapping_mut() {
        map.retain(|k, _| k.as_str().map(|s| keep.contains(&s)).unwrap_or(false));
    }
}

// ---------------------------------------------------------------------------
// Workspace (declared agent in project YAML)

/// Scarcity invariant for review workspaces: at most this many per machine.
/// A review workspace may pin a long-running dev server and a port, so
/// declaring more than this on one machine is treated as a config error by
/// [`Project::validate_workspaces`]. See `Plans/review-workspaces.md` §5.1.
pub const MAX_REVIEW_WORKSPACES_PER_MACHINE: usize = 2;

/// Whether a workspace is an autonomous development slot or a slot
/// designated for human review. See `Plans/review-workspaces.md` §5.
///
/// The wire form is `dev` / `review` (lowercase). `Dev` is the default so
/// existing project YAMLs — which carry no `role:` key — deserialize
/// unchanged, and the field is elided on serialize (see
/// [`WorkspaceSpec::role`]) so they round-trip byte-identically.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceRole {
    #[default]
    Dev,
    Review,
}

/// A workspace is a long-lived slot on a machine: one stable worktree, one
/// runner. Workspaces pick up tasks from the board and switch branches between
/// assignments (with cleared context). The worktree path is derived as
/// `<machine.work_dir>/.shelbi/wt/<workspace-name>` — not configurable yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSpec {
    pub name: String,
    pub machine: String,
    pub runner: String,
    /// Whether this slot does autonomous development ([`WorkspaceRole::Dev`],
    /// the default) or is reserved for loading & serving a task's branch for
    /// human review ([`WorkspaceRole::Review`]). Rides the user-local config
    /// half along with the rest of `workspaces:`. Elided from the wire form
    /// when `Dev` so existing YAMLs round-trip unchanged.
    #[serde(default, skip_serializing_if = "is_default")]
    pub role: WorkspaceRole,
}

/// `skip_serializing_if` helper: true when a value equals its `Default`.
/// Used to keep default-valued optional fields off the wire so existing
/// configs round-trip byte-identically.
fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    *t == T::default()
}

impl WorkspaceSpec {
    /// Whether this workspace is reserved for human review (as opposed to
    /// autonomous development). See [`WorkspaceRole`].
    pub fn is_review(&self) -> bool {
        self.role == WorkspaceRole::Review
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

    pub fn is_ssh(&self) -> bool {
        matches!(self, Host::Ssh { .. })
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
    /// Blocking-dialog signatures for this runner. When empty, the poller
    /// falls back to [`default_dialog_signatures`] keyed on `command`, so
    /// the built-in per-runner set applies with no config. Populate this in
    /// project.yaml to teach the heartbeat about a new runner dialog without
    /// a rebuild.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dialog_signatures: Vec<DialogSignature>,
}

impl AgentRunnerSpec {
    /// The blocking-dialog signatures to match this runner's pane against:
    /// the explicit `dialog_signatures` list when non-empty, otherwise the
    /// built-in per-runner defaults for `command`.
    pub fn effective_dialog_signatures(&self) -> Vec<DialogSignature> {
        if self.dialog_signatures.is_empty() {
            default_dialog_signatures(&self.command)
        } else {
            self.dialog_signatures.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSpec {
    /// Name of an agent runner declared in `agent_runners`.
    pub runner: String,
}

// ---------------------------------------------------------------------------
// Workspace / Agent state

/// Persistent state for a single workspace agent.
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

/// A tmux address — `session:window` (we keep pane implicit; one pane per workspace).
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
// Tasks (Kanban board)

/// Where on the board a task lives.
///
/// Lifecycle:
///
/// - `Backlog`: orchestrator-created, awaiting user triage.
/// - `Todo`: user-curated, ready for a workspace to pick up.
/// - `InProgress`: assigned and active on a workspace.
/// - `Review`: workspace reports done; user inspects via the review dir.
/// - `Done`: accepted by user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Backlog,
    Todo,
    InProgress,
    Review,
    Done,
}

impl Column {
    pub const ALL: [Column; 5] = [
        Column::Backlog,
        Column::Todo,
        Column::InProgress,
        Column::Review,
        Column::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Column::Backlog => "backlog",
            Column::Todo => "todo",
            Column::InProgress => "in_progress",
            Column::Review => "review",
            Column::Done => "done",
        }
    }

    /// The PascalCase status *display name* this column maps to under
    /// the canonical default workflow (see `default_workflow()`). Used
    /// when rendering labels for tasks whose only known position is the
    /// legacy [`Column`]. Generic code that needs to ask "is this task
    /// in the merge-bar trigger status?" should use
    /// [`Column::default_status_id`] instead — workflow lookups are
    /// keyed by the stable `id`, not the renamable display label.
    pub fn default_status_name(self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::Todo => "Todo",
            Column::InProgress => "InProgress",
            Column::Review => "Review",
            Column::Done => "Done",
        }
    }

    /// The stable status *id* this column maps to under the canonical
    /// default workflow (see `default_workflow()`). Matches the `id:`
    /// fields the workflow YAML's `from:` / `to:` strings reference, so
    /// `workflow.status(col.default_status_id())` is the right lookup
    /// for "find the canonical status for this task's legacy column."
    pub fn default_status_id(self) -> &'static str {
        match self {
            Column::Backlog => "backlog",
            Column::Todo => "todo",
            Column::InProgress => "in-progress",
            Column::Review => "review",
            Column::Done => "done",
        }
    }

    /// The semantic [`crate::StatusCategory`] this column maps to under
    /// the canonical default workflow. Used by the events log writer to
    /// fill the `from_category=`/`to_category=` fields on the new line
    /// shape, and by the back-compat parser to derive a category for
    /// pre-workflow lines that don't carry one. See `Plans/workflows.md`
    /// §1 for the category table.
    pub fn category(self) -> crate::StatusCategory {
        match self {
            Column::Backlog => crate::StatusCategory::Backlog,
            Column::Todo => crate::StatusCategory::Ready,
            Column::InProgress => crate::StatusCategory::Active,
            Column::Review => crate::StatusCategory::Handoff,
            Column::Done => crate::StatusCategory::Done,
        }
    }
}

impl std::fmt::Display for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Column {
    type Err = crate::Error;
    fn from_str(s: &str) -> crate::Result<Self> {
        // Accept both the canonical form and a few friendly aliases users
        // are likely to type at the CLI ("in-progress", "wip", "todo").
        match s.trim().to_ascii_lowercase().as_str() {
            "backlog" => Ok(Column::Backlog),
            "todo" | "to_do" | "to-do" => Ok(Column::Todo),
            "in_progress" | "in-progress" | "wip" => Ok(Column::InProgress),
            "review" | "ready_for_review" | "ready-for-review" => Ok(Column::Review),
            "done" | "complete" | "completed" => Ok(Column::Done),
            other => Err(crate::Error::Other(format!("unknown column: {other}"))),
        }
    }
}

/// One Kanban card. Position within a column is given by `priority`
/// (0 = top); the storage layer keeps these contiguous within each column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub column: Column,
    pub priority: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// Name of the workflow this task runs under, matching the filename
    /// (`workflows/<name>.yaml`) minus the extension. Absent means the
    /// task uses the project's default workflow — see
    /// [`Task::workflow_or_default`] and [`DEFAULT_WORKFLOW_NAME`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The git branch this task operates on. Two modes (`Plans/workflows.md`
    /// §12): omitted at creation means the orchestrator will cut
    /// `shelbi/<task-id>` off the resolved base branch when the task moves
    /// to `InProgress` and write the name back here; pre-filled at creation
    /// means use that branch as-is (the *release task* pattern).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Other task ids this task depends on. A task is **blocked** (see
    /// [`Task::is_blocked`]) when any of these are not yet in
    /// [`Column::Done`]. Stored as a list rather than a reverse `blocks`
    /// field so cycle detection and dep editing only touch one file.
    /// Cycles are rejected at save time by
    /// [`shelbi_state::validate_depends_on`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Optional hint to the orchestrator: prefer assigning this task to a
    /// workspace on the named machine. Persisted only; enforcement (or
    /// override) is the orchestrator's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefers_machine: Option<String>,
    /// Per-task overrides for Zen Mode: opt-in/out of auto-promote and
    /// adjust the check set against the project default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zen: Option<TaskZenConfig>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Free-form parameters carried at the top level of the task's
    /// frontmatter. Used by a workflow's `git:` block to resolve `{{var}}`
    /// placeholders at task-load time (`Plans/workflows.md` §12). Captured
    /// via `#[serde(flatten)]` so a task can declare `feature: auth-rewrite`
    /// directly alongside the structured fields, which is what the workflow
    /// docs and `feature-task` example assume.
    ///
    /// Anything that is not a typed Task field lands here. Values are
    /// constrained to YAML strings — workflow git fields are always strings,
    /// and forcing string-typed params keeps `{{feature}}` substitution from
    /// silently coercing numeric or boolean YAML values.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

impl Task {
    /// True iff any id in `depends_on` is not yet `Done` in `columns`.
    /// IDs missing from `columns` are treated as not-done (which matches
    /// the project-wide validation that rejects unknown ids at save time
    /// — if a dep id is unknown here, the safer answer is still blocked).
    pub fn is_blocked(&self, columns: &std::collections::HashMap<String, Column>) -> bool {
        self.depends_on
            .iter()
            .any(|id| columns.get(id).copied() != Some(Column::Done))
    }

    /// The workflow name this task runs under: the explicit
    /// [`Task::workflow`] field if set, otherwise [`DEFAULT_WORKFLOW_NAME`].
    /// Callers that need to look up the YAML definition should hit this
    /// instead of the raw `workflow` field so absence routes to the
    /// default uniformly.
    pub fn workflow_or_default(&self) -> &str {
        self.workflow.as_deref().unwrap_or(DEFAULT_WORKFLOW_NAME)
    }
}

/// Name of the workflow used when a task's `workflow:` frontmatter field
/// is absent. Matches the filename of the workflow YAML that ships with
/// every new project (`workflows/default.yaml`).
pub const DEFAULT_WORKFLOW_NAME: &str = "default";

/// Max byte length of a task id. The workspace branch is `shelbi/<id>` (7-byte
/// prefix) and GitHub caps ref names at 255 bytes; we leave a 15-byte buffer
/// so refs derived from the id stay at most 240 bytes.
pub const MAX_TASK_ID_LEN: usize = 233;

/// Same character set as agent ids (kebab/snake alphanumeric), plus a length
/// cap so the derived `shelbi/<id>` branch stays pushable to GitHub.
pub fn validate_task_id(s: &str) -> crate::Result<()> {
    validate_agent_id(s)?;
    if s.len() > MAX_TASK_ID_LEN {
        return Err(crate::Error::TaskIdTooLong {
            id: s.to_string(),
            len: s.len(),
            max: MAX_TASK_ID_LEN,
        });
    }
    Ok(())
}

/// Validate the `workflow:` value on a task frontmatter. The string is
/// used as a filename component (`workflows/<name>.yaml`) so it follows
/// the same character set as agent and task ids.
pub fn validate_workflow_name(s: &str) -> crate::Result<()> {
    validate_agent_id(s)
}

/// Validate a task's `branch:` override. The value flows into `git checkout`
/// / `git worktree add` on a possibly-remote worker; the SSH transport
/// shell-escapes it (so it survives as one word), but escaping alone doesn't
/// stop git from reading a leading `-` as a flag (argument injection). So we
/// pin the value to the task-id character set plus `/` (branch names are
/// conventionally slash-namespaced, e.g. `shelbi/<id>` or `feature/foo`):
/// ASCII alphanumerics, `-`, `_`, `/`, and a required alphanumeric first
/// character. That rejects `-`-leading flags, `..`, and every shell/dash
/// metacharacter before the value can reach git. Length is bounded by
/// [`MAX_TASK_ID_LEN`] plus a small slack for the namespace prefix so the
/// derived ref stays under GitHub's 255-byte cap.
pub fn validate_branch(s: &str) -> crate::Result<()> {
    let first_ok = s
        .chars()
        .next()
        .map(|c| c.is_ascii_alphanumeric())
        .unwrap_or(false);
    let chars_ok = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'));
    if !first_ok || !chars_ok || s.len() > 255 {
        return Err(crate::Error::InvalidBranch(s.to_string()));
    }
    Ok(())
}

/// Validate a project name used as a directory component under
/// `~/.shelbi/projects/<name>/`. Unlike task/agent ids, project names
/// default to a repo basename and historically carry a looser character
/// set (dots, mixed case), so this only enforces the security-critical
/// invariant: the name must resolve to exactly one *normal* path
/// component — never empty, never containing a separator, never `.`/`..`.
/// That closes the `../`-traversal hole at the storage chokepoint
/// (`shelbi_state::project_dir`) without rejecting existing names.
pub fn validate_project_name(s: &str) -> crate::Result<()> {
    use std::path::{Component, Path};
    let mut comps = Path::new(s).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(c)), None) if c.to_str() == Some(s) => Ok(()),
        _ => Err(crate::Error::InvalidProjectName(s.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Zen Mode

/// Project-level Zen Mode configuration. Stored under the `zen:` key in
/// the project YAML. Defaults are tuned for a small repo with a sane CI
/// pipeline — `checks.local` empty, 15-minute CI timeout, no extra
/// danger paths beyond the built-in list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZenConfig {
    /// Local checks run before promotion (e.g. `cargo test`, `npm test`).
    #[serde(default)]
    pub checks: ZenChecks,
    /// How long Zen Mode will wait for CI to report a verdict before
    /// timing out the promotion. Serialized as a number of seconds.
    #[serde(default = "default_ci_timeout", with = "duration_secs")]
    pub ci_timeout: Duration,
    /// Glob patterns considered too sensitive to auto-promote. By default
    /// this *extends* the built-in list (see [`Project::builtin_danger_paths`]);
    /// projects can opt into a full override via the `override` variant.
    #[serde(default)]
    pub danger_paths: ZenDangerPaths,
}

impl Default for ZenConfig {
    fn default() -> Self {
        Self {
            checks: ZenChecks::default(),
            ci_timeout: default_ci_timeout(),
            danger_paths: ZenDangerPaths::default(),
        }
    }
}

/// Default CI wait: 15 minutes.
fn default_ci_timeout() -> Duration {
    Duration::from_secs(15 * 60)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZenChecks {
    /// Shell commands run locally before the workspace hands off to CI.
    /// Each entry is a single command line, executed in the worktree
    /// root.
    #[serde(default)]
    pub local: Vec<String>,
}

/// How the project's `danger_paths` list interacts with the built-in
/// list. `Extend` (default) keeps the built-in patterns and adds the
/// user's; `Override` replaces them entirely. The wire format is a map
/// with a single `extend:` or `override:` key — we hand-roll it via
/// [`ZenDangerPathsRepr`] because serde_yaml's externally-tagged
/// default uses YAML tags (`!extend`), which are awkward to write in a
/// hand-edited project file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ZenDangerPathsRepr", into = "ZenDangerPathsRepr")]
pub enum ZenDangerPaths {
    Extend(Vec<String>),
    Override(Vec<String>),
}

impl Default for ZenDangerPaths {
    fn default() -> Self {
        ZenDangerPaths::Extend(Vec::new())
    }
}

/// Wire form for [`ZenDangerPaths`]: exactly one of `extend` / `override`
/// is set. Both being set, or neither, is a deserialization error.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ZenDangerPathsRepr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extend: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "override")]
    override_: Option<Vec<String>>,
}

impl TryFrom<ZenDangerPathsRepr> for ZenDangerPaths {
    type Error = &'static str;
    fn try_from(r: ZenDangerPathsRepr) -> Result<Self, Self::Error> {
        match (r.extend, r.override_) {
            (Some(_), Some(_)) => {
                Err("zen.danger_paths: set either `extend:` or `override:`, not both")
            }
            (Some(v), None) => Ok(ZenDangerPaths::Extend(v)),
            (None, Some(v)) => Ok(ZenDangerPaths::Override(v)),
            (None, None) => Ok(ZenDangerPaths::default()),
        }
    }
}

impl From<ZenDangerPaths> for ZenDangerPathsRepr {
    fn from(p: ZenDangerPaths) -> Self {
        match p {
            ZenDangerPaths::Extend(v) => Self { extend: Some(v), override_: None },
            ZenDangerPaths::Override(v) => Self { extend: None, override_: Some(v) },
        }
    }
}

/// Built-in danger paths: always part of the resolved list when the
/// project uses the `extend` variant (the default). Patterns are glob
/// strings; matching is the caller's job.
pub const BUILTIN_DANGER_PATHS: &[&str] = &[
    ".github/workflows/**",
    "scripts/install.sh",
    "*.yaml",
    "*.yml",
    "LICENSE",
    "package-lock.json",
    "Cargo.lock",
];

/// Recognized project shapes. Each shape contributes a fixed set of
/// danger paths that get merged on top of [`BUILTIN_DANGER_PATHS`] when
/// the project uses the `extend` variant.
///
/// Detection is intentionally coarse: presence of a single sentinel
/// file. The goal is "good defaults for the common case", not exhaustive
/// classification — a project that wants finer control sets
/// `zen.danger_paths.override:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectShape {
    /// `Cargo.toml` with a `[workspace]` section at the repo root.
    CargoWorkspace,
    /// `package.json` at the repo root (covers Next.js, plain Node,
    /// monorepos that hoist a root manifest).
    Node,
    /// `.github/` directory at the repo root.
    GitHub,
    /// `Dockerfile` or `compose.yaml` at the repo root.
    Docker,
    /// `shelbi.yaml` or `.shelbi/` directory at the repo root.
    Shelbi,
}

impl ProjectShape {
    /// Short human label, used by `shelbi zen status`.
    pub fn label(self) -> &'static str {
        match self {
            ProjectShape::CargoWorkspace => "cargo workspace",
            ProjectShape::Node => "node / next.js",
            ProjectShape::GitHub => "github",
            ProjectShape::Docker => "docker",
            ProjectShape::Shelbi => "shelbi",
        }
    }

    /// Danger paths contributed by this shape. Order is stable so the
    /// resolved list is deterministic.
    pub fn danger_paths(self) -> &'static [&'static str] {
        match self {
            ProjectShape::CargoWorkspace => &[
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
                ".cargo/config.toml",
            ],
            ProjectShape::Node => &[
                "package.json",
                "package-lock.json",
                "next.config.*",
                "vercel.json",
                ".npmrc",
            ],
            ProjectShape::GitHub => &[
                ".github/CODEOWNERS",
                ".github/dependabot.yml",
            ],
            ProjectShape::Docker => &["Dockerfile", "compose.yaml"],
            ProjectShape::Shelbi => &[".shelbi/**", "shelbi.yaml"],
        }
    }
}

/// Scan `root` for the project-shape signals listed on [`ProjectShape`].
/// Returns shapes in a stable order (Cargo → Node → GitHub → Docker →
/// Shelbi); duplicate shapes never appear. Missing or unreadable files
/// produce no signal — the function never errors.
pub fn detect_project_shapes(root: &Path) -> Vec<ProjectShape> {
    let mut out = Vec::new();

    if let Ok(s) = std::fs::read_to_string(root.join("Cargo.toml")) {
        if s.contains("[workspace]") {
            out.push(ProjectShape::CargoWorkspace);
        }
    }

    if root.join("package.json").is_file() {
        out.push(ProjectShape::Node);
    }

    if root.join(".github").is_dir() {
        out.push(ProjectShape::GitHub);
    }

    if root.join("Dockerfile").is_file() || root.join("compose.yaml").is_file() {
        out.push(ProjectShape::Docker);
    }

    if root.join("shelbi.yaml").is_file() || root.join(".shelbi").is_dir() {
        out.push(ProjectShape::Shelbi);
    }

    out
}

/// Per-task Zen Mode overrides. Lives under `zen:` in the task
/// frontmatter. Every field is optional so a task can adjust just one
/// dimension.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskZenConfig {
    /// Explicit opt-in/out of Zen Mode promotion for this task. `None`
    /// means "follow project default"; `Some(false)` is the canonical
    /// way to keep a sensitive task on the manual-review path even when
    /// the project is otherwise in Zen Mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Checks to run *in addition to* the project's `zen.checks.local`.
    /// Ignored when `checks_only` is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks_additional: Vec<String>,
    /// Checks to run *instead of* the project's `zen.checks.local`.
    /// Takes precedence over `checks_additional`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks_only: Vec<String>,
}

/// Resolve the effective check list for a task: `checks_only` replaces
/// the project list, `checks_additional` extends it, and absent both the
/// project's `zen.checks.local` is returned verbatim.
///
/// This is the legacy (workflow-unaware) form. New call sites should
/// prefer [`checks_for_task_in_workflow`], which threads the task's
/// workflow into the resolution chain so a per-workflow `zen.checks`
/// override wins over the project-level default.
pub fn checks_for_task(project: &Project, task: &Task) -> Vec<String> {
    checks_for_task_in_workflow(project, None, task)
}

/// Resolve the effective check list for `task` against `project`, with
/// an optional `workflow` whose per-workflow `zen.checks` override (if
/// set) supersedes `project.zen.checks`. Per-task overrides on the task
/// frontmatter (`zen.checks_only` / `checks_additional`) still win
/// against whichever layer supplied the workflow-level base list.
///
/// Resolution order from base to override:
///
/// 1. Project-level `zen.checks.local`.
/// 2. Per-workflow `WorkflowZenConfig::checks.local` (if set), replacing 1.
/// 3. Per-task `TaskZenConfig::checks_only` (if set), replacing 2.
/// 4. Otherwise per-task `TaskZenConfig::checks_additional` extending 2.
///
/// Pass `workflow: None` to skip the workflow layer entirely — the
/// helper still applies the project + per-task rules and matches the
/// legacy [`checks_for_task`] behavior.
pub fn checks_for_task_in_workflow(
    project: &Project,
    workflow: Option<&crate::Workflow>,
    task: &Task,
) -> Vec<String> {
    let base = workflow
        .and_then(|w| w.zen.as_ref())
        .and_then(|z| z.checks.as_ref())
        .map(|c| c.local.clone())
        .unwrap_or_else(|| project.zen.checks.local.clone());
    match task.zen.as_ref() {
        Some(z) if !z.checks_only.is_empty() => z.checks_only.clone(),
        Some(z) if !z.checks_additional.is_empty() => {
            let mut out = base;
            out.extend(z.checks_additional.iter().cloned());
            out
        }
        _ => base,
    }
}

/// Resolve the effective Zen `ci_timeout` for `project` + an optional
/// `workflow`. The workflow's `zen.ci_timeout` override wins when set;
/// otherwise the project default applies.
pub fn ci_timeout_for_workflow(
    project: &Project,
    workflow: Option<&crate::Workflow>,
) -> Duration {
    workflow
        .and_then(|w| w.zen.as_ref())
        .and_then(|z| z.ci_timeout)
        .unwrap_or(project.zen.ci_timeout)
}

/// Resolve the effective danger-glob list for `project` + an optional
/// `workflow`. When the workflow declares `zen.danger_paths`, *that*
/// list owns the `extend` vs `override` decision — the workflow's
/// `extend` extends the project's resolved list, and `override`
/// replaces it outright. When the workflow has no `danger_paths`
/// override, falls back to [`danger_paths_for_project`].
///
/// This mirrors the project-level resolution rule one level out: the
/// per-workflow override is structurally the same shape as the
/// project-level config, so a workflow can shadow `extend`/`override`
/// independently.
pub fn danger_paths_for_workflow(
    project: &Project,
    workflow: Option<&crate::Workflow>,
) -> Vec<String> {
    let Some(wz) = workflow.and_then(|w| w.zen.as_ref()) else {
        return danger_paths_for_project(project);
    };
    let Some(custom) = wz.danger_paths.as_ref() else {
        return danger_paths_for_project(project);
    };
    match custom {
        ZenDangerPaths::Extend(extra) => {
            let mut out = danger_paths_for_project(project);
            out.extend(extra.iter().cloned());
            dedupe_preserve_order(out)
        }
        ZenDangerPaths::Override(custom) => custom.clone(),
    }
}

/// Resolve the effective danger-path list for a project.
///
/// In `Extend` mode (the default) the result is `builtin ++ detected ++
/// user-extend`, deduplicated while preserving first occurrence. The
/// `detected` segment comes from [`Project::detect_shapes`] and is empty
/// for any project whose YAML hasn't been loaded through `load_project`
/// (e.g. fixtures constructed inline in tests).
///
/// In `Override` mode the user's list wins outright — no builtins, no
/// detected paths. This is the escape hatch for projects with a
/// non-standard layout that the shape detector would mis-classify.
pub fn danger_paths_for_project(project: &Project) -> Vec<String> {
    match &project.zen.danger_paths {
        ZenDangerPaths::Extend(extra) => {
            let mut out: Vec<String> =
                BUILTIN_DANGER_PATHS.iter().map(|s| s.to_string()).collect();
            for shape in &project.detected_shapes {
                for p in shape.danger_paths() {
                    out.push((*p).to_string());
                }
            }
            out.extend(extra.iter().cloned());
            dedupe_preserve_order(out)
        }
        ZenDangerPaths::Override(custom) => custom.clone(),
    }
}

fn dedupe_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut out = Vec::with_capacity(items.len());
    for s in items {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

/// Serde adapter that stores a `Duration` as an integer number of
/// seconds in YAML/JSON. Keeps the project YAML readable while letting
/// the in-memory type stay a `Duration`.
mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
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
    use crate::WorkflowZenConfig;

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
    fn branch_validation() {
        // Namespaced branches — the common shape — pass.
        assert!(validate_branch("shelbi/fix-login-bug").is_ok());
        assert!(validate_branch("feature/auth-rewrite").is_ok());
        assert!(validate_branch("main").is_ok());
        assert!(validate_branch("release_2").is_ok());

        // Empty and leading-`-` (git flag injection) are rejected.
        assert!(validate_branch("").is_err());
        assert!(validate_branch("-b").is_err());
        assert!(validate_branch("--upload-pack=touch /tmp/x").is_err());
        assert!(validate_branch("-leading").is_err());

        // A leading slash (no alphanumeric first char) is rejected too.
        assert!(validate_branch("/etc/passwd").is_err());

        // Shell metacharacters that would re-tokenize on the SSH wire.
        assert!(validate_branch("a b").is_err());
        assert!(validate_branch("a;rm -rf /").is_err());
        assert!(validate_branch("$(touch x)").is_err());
        assert!(validate_branch("a`id`").is_err());
        assert!(validate_branch("a&&b").is_err());
        // `.` is outside the charset, so `..` traversal-style refs are out.
        assert!(validate_branch("a..b").is_err());

        // Over the 255-byte ref cap.
        assert!(validate_branch(&"a".repeat(256)).is_err());
        assert!(validate_branch(&"a".repeat(255)).is_ok());
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

    #[test]
    fn column_serde_roundtrip() {
        for c in Column::ALL {
            let y = serde_yaml::to_string(&c).unwrap();
            let back: Column = serde_yaml::from_str(&y).unwrap();
            assert_eq!(c, back);
        }
        // Wire format is the snake_case form.
        assert_eq!(serde_yaml::to_string(&Column::InProgress).unwrap().trim(), "in_progress");
    }

    #[test]
    fn column_from_str_friendly_aliases() {
        use std::str::FromStr;
        assert_eq!(Column::from_str("backlog").unwrap(), Column::Backlog);
        assert_eq!(Column::from_str("to-do").unwrap(), Column::Todo);
        assert_eq!(Column::from_str("WIP").unwrap(), Column::InProgress);
        assert_eq!(Column::from_str("in-progress").unwrap(), Column::InProgress);
        assert_eq!(Column::from_str("ready-for-review").unwrap(), Column::Review);
        assert_eq!(Column::from_str("complete").unwrap(), Column::Done);
        assert!(Column::from_str("garbage").is_err());
    }

    #[test]
    fn task_depends_on_defaults_to_empty_and_omits_in_serialization() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.depends_on.is_empty());
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("depends_on"));
    }

    #[test]
    fn task_workflow_branch_depends_on_round_trip_together() {
        // The three frontmatter fields added in
        // `tasks-add-workflow-branch-depends-on-frontmatter-fields...`
        // round-trip together with their expected wire shape: a string,
        // a string, and a sequence of strings.
        let yaml = r#"
id: build-login
title: Build the login form
column: in_progress
priority: 0
workflow: feature-task
branch: feature/auth-rewrite
depends_on:
  - scaffold-auth
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(t.workflow.as_deref(), Some("feature-task"));
        assert_eq!(t.branch.as_deref(), Some("feature/auth-rewrite"));
        assert_eq!(t.depends_on, vec!["scaffold-auth".to_string()]);

        let back = serde_yaml::to_string(&t).unwrap();
        assert!(back.contains("workflow: feature-task"));
        assert!(back.contains("branch: feature/auth-rewrite"));
        assert!(back.contains("depends_on"));
        assert!(back.contains("- scaffold-auth"));

        // Stable second round-trip — no spurious normalization.
        let t2: Task = serde_yaml::from_str(&back).unwrap();
        assert_eq!(serde_yaml::to_string(&t2).unwrap(), back);
    }

    #[test]
    fn task_workflow_defaults_to_none_and_omits_in_serialization() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.workflow.is_none());
        // Absent `workflow:` routes to the canonical default name; no
        // caller has to special-case `Option::None`.
        assert_eq!(t.workflow_or_default(), DEFAULT_WORKFLOW_NAME);
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("workflow"));
    }

    #[test]
    fn task_workflow_or_default_prefers_explicit() {
        let mut t: Task = serde_yaml::from_str(
            r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#,
        )
        .unwrap();
        assert_eq!(t.workflow_or_default(), DEFAULT_WORKFLOW_NAME);
        t.workflow = Some("design-review".into());
        assert_eq!(t.workflow_or_default(), "design-review");
    }

    #[test]
    fn task_branch_defaults_to_none_and_omits_in_serialization() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.branch.is_none());
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("branch"));
    }

    #[test]
    fn workflow_name_validation_matches_id_rules() {
        // Same character class as task ids — workflow names are filename
        // components (`workflows/<name>.yaml`), so spaces and slashes are
        // rejected for the same reason.
        assert!(validate_workflow_name("default").is_ok());
        assert!(validate_workflow_name("feature-task").is_ok());
        assert!(validate_workflow_name("design_review").is_ok());
        assert!(validate_workflow_name("").is_err());
        assert!(validate_workflow_name("has spaces").is_err());
        assert!(validate_workflow_name("slash/in/name").is_err());
        assert!(validate_workflow_name("-leading-hyphen").is_err());
    }

    #[test]
    fn project_name_rejects_path_traversal_but_allows_looser_names() {
        // Security-critical: a name must be exactly one *normal* path
        // component so it can't escape `~/.shelbi/projects/`.
        assert!(validate_project_name("shelbi").is_ok());
        assert!(validate_project_name("my-app").is_ok());
        assert!(validate_project_name("my_app").is_ok());
        // Looser than task/agent ids on purpose (repo-basename defaults):
        assert!(validate_project_name("My.App").is_ok());
        assert!(validate_project_name("app2").is_ok());
        // Traversal / separators / specials are rejected.
        assert!(validate_project_name("").is_err());
        assert!(validate_project_name(".").is_err());
        assert!(validate_project_name("..").is_err());
        assert!(validate_project_name("../other").is_err());
        assert!(validate_project_name("a/b").is_err());
        assert!(validate_project_name("/abs").is_err());
        assert!(validate_project_name("nested/../escape").is_err());
    }

    #[test]
    fn task_is_blocked_when_any_dep_not_done() {
        let now = chrono::Utc::now();
        let task = Task {
            id: "a".into(),
            title: "A".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: vec!["b".into(), "c".into()],
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        };
        let mut cols = std::collections::HashMap::new();
        cols.insert("b".to_string(), Column::Done);
        cols.insert("c".to_string(), Column::InProgress);
        assert!(task.is_blocked(&cols));

        cols.insert("c".to_string(), Column::Done);
        assert!(!task.is_blocked(&cols));

        // Unknown dep id is treated as not-done.
        cols.remove("c");
        assert!(task.is_blocked(&cols));
    }

    #[test]
    fn task_prefers_machine_round_trips() {
        let now = chrono::Utc::now();
        let task = Task {
            id: "linux-probe".into(),
            title: "Tune the readiness probe".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: vec![],
            prefers_machine: Some("devbox".into()),
            zen: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        };
        let y = serde_yaml::to_string(&task).unwrap();
        assert!(y.contains("prefers_machine: devbox"));
        let back: Task = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.prefers_machine.as_deref(), Some("devbox"));
        // YAML representation is stable across a second round trip.
        let y2 = serde_yaml::to_string(&back).unwrap();
        assert_eq!(y, y2);
    }

    #[test]
    fn task_prefers_machine_defaults_to_none_and_omits_in_serialization() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.prefers_machine.is_none());
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("prefers_machine"));
    }

    #[test]
    fn task_id_uses_same_rules_as_agent_id() {
        assert!(validate_task_id("fix-login").is_ok());
        assert!(validate_task_id("with spaces").is_err());
    }

    #[test]
    fn task_id_rejects_lengths_that_would_overflow_a_git_ref() {
        let at_limit = "a".repeat(MAX_TASK_ID_LEN);
        assert!(validate_task_id(&at_limit).is_ok());

        let one_over = "a".repeat(MAX_TASK_ID_LEN + 1);
        match validate_task_id(&one_over) {
            Err(crate::Error::TaskIdTooLong { len, max, .. }) => {
                assert_eq!(len, MAX_TASK_ID_LEN + 1);
                assert_eq!(max, MAX_TASK_ID_LEN);
            }
            other => panic!("expected TaskIdTooLong, got {other:?}"),
        }

        // Agent ids are unaffected — only the task wrapper enforces length.
        assert!(validate_agent_id(&"a".repeat(MAX_TASK_ID_LEN + 1)).is_ok());
    }

    #[test]
    fn task_params_capture_top_level_extra_keys() {
        // The `feature:` and `region:` keys are extra frontmatter fields
        // that the workflow's `git:` block references with `{{feature}}`
        // / `{{region}}`. They round-trip through serde flatten without
        // any special wrapper, matching the worked example in
        // `Plans/workflows.md` §12.
        let yaml = r#"
id: build-login-form
title: Build the login form
column: todo
priority: 0
workflow: feature-task
feature: auth-rewrite
region: us-east
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(t.params.get("feature").map(String::as_str), Some("auth-rewrite"));
        assert_eq!(t.params.get("region").map(String::as_str), Some("us-east"));
        // Structured fields aren't double-counted in params.
        assert!(!t.params.contains_key("id"));
        assert!(!t.params.contains_key("workflow"));

        let back = serde_yaml::to_string(&t).unwrap();
        assert!(back.contains("feature: auth-rewrite"), "out: {back}");
        assert!(back.contains("region: us-east"), "out: {back}");
    }

    #[test]
    fn task_params_default_to_empty_and_serialize_silently() {
        // No extra keys → `params` is empty → nothing extra on the wire.
        // The schema stays identical to existing tasks; only tasks that
        // opt into `{{var}}` parameterization carry extra frontmatter.
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.params.is_empty());
        let back = serde_yaml::to_string(&t).unwrap();
        // There's no good single token to grep for "params:" since it
        // would only appear if we'd nested under that key — instead
        // assert that the round-trip is stable.
        let t2: Task = serde_yaml::from_str(&back).unwrap();
        assert!(t2.params.is_empty());
    }

    #[test]
    fn project_yaml_omits_new_workspace_keys_and_uses_defaults() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.workspace_poll_interval_secs, 5);
        assert_eq!(p.workspace_permissions_mode, "auto");
        assert!(p.workspace_settings_template.is_none());
    }

    #[test]
    fn project_yaml_omits_contextstore_sync_when_not_set() {
        // Older project YAMLs predate the field. `#[serde(default)]` plus
        // a Vec default means absence parses cleanly and serializes back
        // out without leaking the empty list.
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert!(p.contextstore_sync.is_empty());
        let back = serde_yaml::to_string(&p).unwrap();
        assert!(!back.contains("contextstore_sync"));
    }

    #[test]
    fn project_yaml_round_trips_contextstore_sync_block() {
        // Real-world shape: opt in by listing the spaces that need to
        // come back from remote workspaces, with their on-disk dir.
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
contextstore_sync:
  - space: Shelbi
    path: ~/Documents/ContextStore/shelbi
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.contextstore_sync.len(), 1);
        assert_eq!(p.contextstore_sync[0].space, "Shelbi");
        assert_eq!(
            p.contextstore_sync[0].path,
            PathBuf::from("~/Documents/ContextStore/shelbi")
        );
        // Round-trip preserves the structure.
        let back = serde_yaml::to_string(&p).unwrap();
        let p2: Project = serde_yaml::from_str(&back).unwrap();
        assert_eq!(p2.contextstore_sync, p.contextstore_sync);
    }

    #[test]
    fn project_yaml_round_trips_explicit_workspace_keys() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
workspace_poll_interval_secs: 12
workspace_permissions_mode: acceptEdits
workspace_settings_template: /etc/shelbi/p.json
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.workspace_poll_interval_secs, 12);
        assert_eq!(p.workspace_permissions_mode, "acceptEdits");
        assert_eq!(
            p.workspace_settings_template.as_deref(),
            Some(std::path::Path::new("/etc/shelbi/p.json"))
        );
    }

    #[test]
    fn workspaces_validate_against_machines_and_runners() {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert("claude".to_string(), AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] });
        let project = Project {
            name: "p".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp".into(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![
                WorkspaceSpec { name: "alice".into(), machine: "hub".into(), runner: "claude".into(), role: Default::default() },
            ],
            workspace_poll_interval_secs: default_workspace_poll_interval_secs(),
            workspace_permissions_mode: default_workspace_permissions_mode(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            review: ReviewConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        };
        assert!(project.validate_workspaces().is_ok());

        let mut bad = project.clone();
        bad.workspaces.push(WorkspaceSpec { name: "bob".into(), machine: "ghost".into(), runner: "claude".into(), role: Default::default() });
        assert!(matches!(bad.validate_workspaces(), Err(crate::Error::UnknownMachine(_))));

        let mut bad2 = project.clone();
        bad2.workspaces.push(WorkspaceSpec { name: "bob".into(), machine: "hub".into(), runner: "ghost".into(), role: Default::default() });
        assert!(matches!(bad2.validate_workspaces(), Err(crate::Error::UnknownRunner(_))));
    }

    // ---- Review workspaces -------------------------------------------------

    /// Build a project with two machines (`hub`, `devbox`) and the given
    /// workspaces, so review-workspace behavior can be exercised without
    /// spelling out every unrelated field at each call site.
    fn project_with_workspaces(workspaces: Vec<WorkspaceSpec>) -> Project {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                dialog_signatures: vec![],
            },
        );
        let machine = |name: &str| Machine {
            name: name.into(),
            kind: MachineKind::Local,
            work_dir: "/tmp".into(),
            host: None,
        };
        Project {
            name: "p".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![machine("hub"), machine("devbox")],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces,
            workspace_poll_interval_secs: default_workspace_poll_interval_secs(),
            workspace_permissions_mode: default_workspace_permissions_mode(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            review: ReviewConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    fn ws(name: &str, machine: &str, role: WorkspaceRole) -> WorkspaceSpec {
        WorkspaceSpec {
            name: name.into(),
            machine: machine.into(),
            runner: "claude".into(),
            role,
        }
    }

    #[test]
    fn workspace_role_parses_and_defaults_to_dev() {
        // No `role:` key → Dev.
        let dev: WorkspaceSpec =
            serde_yaml::from_str("{ name: alpha, machine: hub, runner: claude }").unwrap();
        assert_eq!(dev.role, WorkspaceRole::Dev);
        assert!(!dev.is_review());

        // Explicit `role: review` (lowercase wire form) → Review.
        let rev: WorkspaceSpec =
            serde_yaml::from_str("{ name: review-1, machine: hub, runner: claude, role: review }")
                .unwrap();
        assert_eq!(rev.role, WorkspaceRole::Review);
        assert!(rev.is_review());
    }

    #[test]
    fn default_role_is_elided_on_the_wire() {
        // A Dev workspace must not grow a `role:` key, so existing YAMLs
        // round-trip byte-identically.
        let dev = ws("alpha", "hub", WorkspaceRole::Dev);
        let y = serde_yaml::to_string(&dev).unwrap();
        assert!(!y.contains("role"), "unexpected role key on the wire: {y}");

        // A Review workspace does serialize its role.
        let rev = ws("review-1", "hub", WorkspaceRole::Review);
        let y = serde_yaml::to_string(&rev).unwrap();
        assert!(y.contains("role: review"), "missing role key: {y}");
    }

    #[test]
    fn review_and_dev_workspace_partitioning() {
        let project = project_with_workspaces(vec![
            ws("alpha", "hub", WorkspaceRole::Dev),
            ws("review-1", "hub", WorkspaceRole::Review),
            ws("beta", "devbox", WorkspaceRole::Dev),
            ws("review-1", "devbox", WorkspaceRole::Review),
        ]);

        let hub_review = project.review_workspaces("hub");
        assert_eq!(hub_review.len(), 1);
        assert_eq!(hub_review[0].name, "review-1");
        assert_eq!(hub_review[0].machine, "hub");

        // Machine scoping: a machine with no review workspaces yields none.
        assert!(project.review_workspaces("nonexistent").is_empty());

        let dev = project.dev_workspaces();
        let dev_names: Vec<&str> = dev.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(dev_names, vec!["alpha", "beta"]);
    }

    #[test]
    fn at_most_two_review_workspaces_per_machine() {
        // Exactly two on one machine is fine.
        let ok = project_with_workspaces(vec![
            ws("review-1", "hub", WorkspaceRole::Review),
            ws("review-2", "hub", WorkspaceRole::Review),
        ]);
        assert!(ok.validate_workspaces().is_ok());

        // A third on the same machine is a hard error with a clear message.
        let over = project_with_workspaces(vec![
            ws("review-1", "hub", WorkspaceRole::Review),
            ws("review-2", "hub", WorkspaceRole::Review),
            ws("review-3", "hub", WorkspaceRole::Review),
        ]);
        match over.validate_workspaces() {
            Err(crate::Error::Other(msg)) => {
                assert!(msg.contains("hub"), "message should name the machine: {msg}");
                assert!(
                    msg.contains("review workspaces"),
                    "message should explain the invariant: {msg}"
                );
            }
            other => panic!("expected a hard error, got {other:?}"),
        }

        // The cap is per-machine: two on hub + two on devbox is fine.
        let split = project_with_workspaces(vec![
            ws("review-1", "hub", WorkspaceRole::Review),
            ws("review-2", "hub", WorkspaceRole::Review),
            ws("review-1", "devbox", WorkspaceRole::Review),
            ws("review-2", "devbox", WorkspaceRole::Review),
        ]);
        assert!(split.validate_workspaces().is_ok());
    }

    #[test]
    fn review_config_defaults_apply_when_block_absent() {
        // A project YAML with no `review:` block gets the documented
        // defaults: base port 3000, stride 10, no setup/serve/probe.
        let rc = ReviewConfig::default();
        assert_eq!(rc.base_port, 3000);
        assert_eq!(rc.port_stride, 10);
        assert!(rc.setup.is_empty());
        assert!(rc.serve.is_none());
        assert!(rc.ready_probe.is_none());

        // Parsing an empty mapping fills every field from its default.
        let parsed: ReviewConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(parsed, rc);

        // Partial blocks override only the named fields.
        let partial: ReviewConfig = serde_yaml::from_str(
            "base_port: 4000\nsetup: [npm install]\nready_probe: { http: http://localhost:4000, timeout: 45 }",
        )
        .unwrap();
        assert_eq!(partial.base_port, 4000);
        assert_eq!(partial.port_stride, 10); // still the default
        assert_eq!(partial.setup, vec!["npm install".to_string()]);
        let probe = partial.ready_probe.expect("probe parsed");
        assert_eq!(probe.http.as_deref(), Some("http://localhost:4000"));
        assert_eq!(probe.timeout, Duration::from_secs(45));
    }

    #[test]
    fn absent_review_block_is_elided_on_the_wire() {
        // A default ReviewConfig on a project must not emit a `review:`
        // key, so existing project YAMLs round-trip byte-identically.
        let project = project_with_workspaces(vec![ws("alpha", "hub", WorkspaceRole::Dev)]);
        let y = serde_yaml::to_string(&project).unwrap();
        assert!(!y.contains("review:"), "unexpected review block: {y}");
        assert!(!y.contains("role:"), "unexpected role key: {y}");
    }

    // ---- Zen Mode ----------------------------------------------------------

    fn project_with_zen(zen: ZenConfig) -> Project {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] },
        );
        Project {
            name: "p".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp".into(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![],
            workspace_poll_interval_secs: default_workspace_poll_interval_secs(),
            workspace_permissions_mode: default_workspace_permissions_mode(),
            workspace_settings_template: None,
            zen,
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            review: ReviewConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    fn task_with_zen(zen: Option<TaskZenConfig>) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: "t".into(),
            title: "T".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: vec![],
            prefers_machine: None,
            zen,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        }
    }

    #[test]
    fn zen_config_defaults_match_spec() {
        let z = ZenConfig::default();
        assert!(z.checks.local.is_empty());
        assert_eq!(z.ci_timeout, Duration::from_secs(15 * 60));
        assert_eq!(z.danger_paths, ZenDangerPaths::Extend(vec![]));
    }

    #[test]
    fn project_yaml_omits_zen_and_uses_defaults() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.zen, ZenConfig::default());
    }

    #[test]
    fn project_yaml_parses_zen_block() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
zen:
  checks:
    local:
      - cargo test
      - cargo clippy
  ci_timeout: 600
  danger_paths:
    extend:
      - migrations/**
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.zen.checks.local, vec!["cargo test", "cargo clippy"]);
        assert_eq!(p.zen.ci_timeout, Duration::from_secs(600));
        assert_eq!(
            p.zen.danger_paths,
            ZenDangerPaths::Extend(vec!["migrations/**".into()])
        );
    }

    #[test]
    fn project_yaml_parses_override_danger_paths() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
zen:
  danger_paths:
    override:
      - "**/*"
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            p.zen.danger_paths,
            ZenDangerPaths::Override(vec!["**/*".into()])
        );
    }

    #[test]
    fn zen_config_yaml_round_trip() {
        let cfg = ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ci_timeout: Duration::from_secs(900),
            danger_paths: ZenDangerPaths::Extend(vec!["docs/**".into()]),
        };
        let y = serde_yaml::to_string(&cfg).unwrap();
        let back: ZenConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(cfg, back);
        // ci_timeout serializes as a plain integer (no struct form).
        assert!(y.contains("ci_timeout: 900"));
    }

    #[test]
    fn task_frontmatter_parses_zen_overrides() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
zen:
  enabled: false
  checks_only:
    - cargo test --doc
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        let z = t.zen.expect("zen block present");
        assert_eq!(z.enabled, Some(false));
        assert_eq!(z.checks_only, vec!["cargo test --doc"]);
        assert!(z.checks_additional.is_empty());
    }

    #[test]
    fn task_zen_round_trips_and_defaults_to_none() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.zen.is_none());
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("zen"));
    }

    #[test]
    fn task_zen_config_round_trip() {
        let cfg = TaskZenConfig {
            enabled: Some(true),
            checks_additional: vec!["npm test".into()],
            checks_only: vec![],
        };
        let y = serde_yaml::to_string(&cfg).unwrap();
        let back: TaskZenConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(cfg, back);
        // Empty lists omitted on the wire.
        assert!(!y.contains("checks_only"));
    }

    #[test]
    fn checks_for_task_falls_back_to_project_when_no_overrides() {
        let p = project_with_zen(ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ..Default::default()
        });
        let t = task_with_zen(None);
        assert_eq!(checks_for_task(&p, &t), vec!["cargo test"]);
    }

    #[test]
    fn checks_for_task_extends_with_additional() {
        let p = project_with_zen(ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ..Default::default()
        });
        let t = task_with_zen(Some(TaskZenConfig {
            checks_additional: vec!["cargo clippy".into()],
            ..Default::default()
        }));
        assert_eq!(checks_for_task(&p, &t), vec!["cargo test", "cargo clippy"]);
    }

    #[test]
    fn checks_for_task_only_replaces_project_checks() {
        let p = project_with_zen(ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ..Default::default()
        });
        let t = task_with_zen(Some(TaskZenConfig {
            checks_only: vec!["cargo test --doc".into()],
            // `checks_only` wins even when both are set.
            checks_additional: vec!["never-run".into()],
            ..Default::default()
        }));
        assert_eq!(checks_for_task(&p, &t), vec!["cargo test --doc"]);
    }

    #[test]
    fn danger_paths_extend_appends_to_builtins() {
        let p = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Extend(vec!["secrets/**".into()]),
            ..Default::default()
        });
        let paths = danger_paths_for_project(&p);
        for builtin in BUILTIN_DANGER_PATHS {
            assert!(paths.iter().any(|p| p == builtin), "missing builtin {builtin}");
        }
        assert!(paths.iter().any(|p| p == "secrets/**"));
    }

    #[test]
    fn danger_paths_override_drops_builtins() {
        let p = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Override(vec!["only/this".into()]),
            ..Default::default()
        });
        let paths = danger_paths_for_project(&p);
        assert_eq!(paths, vec!["only/this".to_string()]);
    }

    #[test]
    fn danger_paths_default_returns_just_builtins() {
        let p = project_with_zen(ZenConfig::default());
        let paths = danger_paths_for_project(&p);
        assert_eq!(paths.len(), BUILTIN_DANGER_PATHS.len());
        for (got, want) in paths.iter().zip(BUILTIN_DANGER_PATHS.iter()) {
            assert_eq!(got, want);
        }
    }

    // ---- ProjectShape detection -------------------------------------------

    fn fresh_tempdir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("shelbi-shape-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detect_cargo_workspace_requires_workspace_marker() {
        let root = fresh_tempdir("cargo-ws");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"a\"]\n").unwrap();
        assert_eq!(
            detect_project_shapes(&root),
            vec![ProjectShape::CargoWorkspace]
        );
    }

    #[test]
    fn detect_cargo_single_crate_is_not_a_workspace() {
        let root = fresh_tempdir("cargo-crate");
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert!(detect_project_shapes(&root).is_empty());
    }

    #[test]
    fn detect_node_package() {
        let root = fresh_tempdir("node");
        std::fs::write(root.join("package.json"), "{}").unwrap();
        assert_eq!(detect_project_shapes(&root), vec![ProjectShape::Node]);
    }

    #[test]
    fn detect_github_dir() {
        let root = fresh_tempdir("gh");
        std::fs::create_dir_all(root.join(".github")).unwrap();
        assert_eq!(detect_project_shapes(&root), vec![ProjectShape::GitHub]);
    }

    #[test]
    fn detect_docker_dockerfile_or_compose() {
        let with_df = fresh_tempdir("df");
        std::fs::write(with_df.join("Dockerfile"), "FROM scratch\n").unwrap();
        assert_eq!(detect_project_shapes(&with_df), vec![ProjectShape::Docker]);

        let with_compose = fresh_tempdir("compose");
        std::fs::write(with_compose.join("compose.yaml"), "services: {}\n").unwrap();
        assert_eq!(detect_project_shapes(&with_compose), vec![ProjectShape::Docker]);
    }

    #[test]
    fn detect_shelbi_via_dir_or_yaml() {
        let with_dir = fresh_tempdir("shelbi-dir");
        std::fs::create_dir_all(with_dir.join(".shelbi")).unwrap();
        assert_eq!(detect_project_shapes(&with_dir), vec![ProjectShape::Shelbi]);

        let with_yaml = fresh_tempdir("shelbi-yaml");
        std::fs::write(with_yaml.join("shelbi.yaml"), "").unwrap();
        assert_eq!(detect_project_shapes(&with_yaml), vec![ProjectShape::Shelbi]);
    }

    #[test]
    fn detect_multiple_shapes_in_stable_order() {
        let root = fresh_tempdir("multi");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
        std::fs::write(root.join("shelbi.yaml"), "").unwrap();
        assert_eq!(
            detect_project_shapes(&root),
            vec![
                ProjectShape::CargoWorkspace,
                ProjectShape::Node,
                ProjectShape::GitHub,
                ProjectShape::Docker,
                ProjectShape::Shelbi,
            ]
        );
    }

    #[test]
    fn detect_missing_root_is_empty_not_error() {
        let nowhere = std::env::temp_dir().join("shelbi-shape-nowhere-does-not-exist-12345");
        // Don't create it.
        assert!(detect_project_shapes(&nowhere).is_empty());
    }

    // ---- danger_paths_for_project + detected_shapes -----------------------

    #[test]
    fn danger_paths_extend_merges_detected_after_builtins() {
        let mut p = project_with_zen(ZenConfig::default());
        p.detected_shapes = vec![ProjectShape::CargoWorkspace];
        let paths = danger_paths_for_project(&p);

        // Builtins come first, in order.
        for (got, want) in paths.iter().zip(BUILTIN_DANGER_PATHS.iter()) {
            assert_eq!(got, want);
        }
        // Detected paths appear after — and Cargo.lock from the shape
        // dedupes against the builtin (which is also Cargo.lock).
        assert!(paths.iter().any(|p| p == "Cargo.toml"));
        assert!(paths.iter().any(|p| p == "rust-toolchain.toml"));
        assert!(paths.iter().any(|p| p == ".cargo/config.toml"));
        let cargo_lock_count = paths.iter().filter(|p| *p == "Cargo.lock").count();
        assert_eq!(cargo_lock_count, 1, "Cargo.lock must be deduplicated");
    }

    #[test]
    fn danger_paths_extend_keeps_builtin_detected_and_user_extras() {
        let mut p = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Extend(vec!["secrets/**".into()]),
            ..Default::default()
        });
        p.detected_shapes = vec![ProjectShape::Node];
        let paths = danger_paths_for_project(&p);
        assert!(paths.iter().any(|p| p == ".github/workflows/**")); // builtin
        assert!(paths.iter().any(|p| p == "vercel.json")); // detected
        assert!(paths.iter().any(|p| p == "secrets/**")); // user
    }

    #[test]
    fn danger_paths_override_drops_detected_shapes() {
        let mut p = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Override(vec!["only/this".into()]),
            ..Default::default()
        });
        p.detected_shapes = vec![ProjectShape::CargoWorkspace, ProjectShape::Node];
        let paths = danger_paths_for_project(&p);
        assert_eq!(paths, vec!["only/this".to_string()]);
    }

    #[test]
    fn project_detect_shapes_populates_field_from_repo() {
        let root = fresh_tempdir("project-detect");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let mut p = project_with_zen(ZenConfig::default());
        p.detect_shapes(&root);
        assert_eq!(p.detected_shapes, vec![ProjectShape::CargoWorkspace]);
    }

    // ---- HeartbeatConfig --------------------------------------------------

    #[test]
    fn heartbeat_config_default_is_three_minutes() {
        assert_eq!(
            HeartbeatConfig::default(),
            HeartbeatConfig::Every(Duration::from_secs(180))
        );
        assert_eq!(
            HeartbeatConfig::default().interval(),
            Some(Duration::from_secs(180))
        );
    }

    #[test]
    fn heartbeat_config_parses_seconds_minutes_hours() {
        use std::str::FromStr;
        assert_eq!(
            HeartbeatConfig::from_str("45s").unwrap(),
            HeartbeatConfig::Every(Duration::from_secs(45))
        );
        assert_eq!(
            HeartbeatConfig::from_str("3m").unwrap(),
            HeartbeatConfig::Every(Duration::from_secs(180))
        );
        assert_eq!(
            HeartbeatConfig::from_str("1h").unwrap(),
            HeartbeatConfig::Every(Duration::from_secs(3_600))
        );
        // Case-insensitive on both the unit and the `off` keyword so
        // hand-edited YAML doesn't surprise on capitalization.
        assert_eq!(
            HeartbeatConfig::from_str("2H").unwrap(),
            HeartbeatConfig::Every(Duration::from_secs(7_200))
        );
        assert_eq!(HeartbeatConfig::from_str("OFF").unwrap(), HeartbeatConfig::Off);
        assert_eq!(HeartbeatConfig::from_str("off").unwrap(), HeartbeatConfig::Off);
    }

    #[test]
    fn heartbeat_config_rejects_bare_integers_and_unknown_units() {
        use std::str::FromStr;
        // No unit suffix — explicitly rejected so `heartbeat: 3`
        // doesn't silently become 3 of some default unit.
        assert!(HeartbeatConfig::from_str("3").is_err());
        assert!(HeartbeatConfig::from_str("180").is_err());
        // Unknown units.
        assert!(HeartbeatConfig::from_str("5x").is_err());
        assert!(HeartbeatConfig::from_str("1d").is_err()); // days unsupported
        // Zero interval is a misuse; ask for `off` instead.
        assert!(HeartbeatConfig::from_str("0s").is_err());
        assert!(HeartbeatConfig::from_str("0m").is_err());
        // Empty.
        assert!(HeartbeatConfig::from_str("").is_err());
        assert!(HeartbeatConfig::from_str("   ").is_err());
    }

    #[test]
    fn heartbeat_config_serializes_as_compact_string() {
        // Round-trips through serde_yaml as a plain string.
        let cfg = HeartbeatConfig::Every(Duration::from_secs(180));
        let y = serde_yaml::to_string(&cfg).unwrap();
        assert!(y.contains("3m"), "got {y:?}");
        let back: HeartbeatConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, cfg);

        let cfg = HeartbeatConfig::Off;
        let y = serde_yaml::to_string(&cfg).unwrap();
        assert!(y.contains("off"), "got {y:?}");
        let back: HeartbeatConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, cfg);

        // Non-round-number seconds stay in seconds.
        let cfg = HeartbeatConfig::Every(Duration::from_secs(45));
        let y = serde_yaml::to_string(&cfg).unwrap();
        assert!(y.contains("45s"), "got {y:?}");
    }

    #[test]
    fn default_dialog_signatures_covers_claude_and_ignores_unknown() {
        // The `claude` runner ships built-in signatures for the dialogs that
        // froze a whole board in the 2026-07-02 incident.
        let sigs = default_dialog_signatures("claude");
        assert!(sigs.iter().any(|s| s.kind == "usage-limit"));
        assert!(sigs
            .iter()
            .any(|s| s.pattern.contains("Stop and wait for limit to reset")));
        assert!(sigs.iter().any(|s| s.kind == "trust"));

        // A basename is used, so an absolute path to the same binary still
        // resolves the built-ins.
        assert_eq!(
            default_dialog_signatures("/usr/local/bin/claude").len(),
            sigs.len()
        );

        // Unknown runner → no built-ins (opt-in via config only).
        assert!(default_dialog_signatures("codex").is_empty());
    }

    #[test]
    fn effective_dialog_signatures_prefers_config_over_builtins() {
        // No explicit list → built-in claude defaults.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            dialog_signatures: vec![],
        };
        assert_eq!(
            spec.effective_dialog_signatures(),
            default_dialog_signatures("claude")
        );

        // Explicit list wins verbatim — this is the "extensible via config"
        // path, letting a project add a new runner dialog without a rebuild.
        let custom = DialogSignature::new("my-modal", "Please respond");
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            dialog_signatures: vec![custom.clone()],
        };
        assert_eq!(spec.effective_dialog_signatures(), vec![custom]);
    }

    #[test]
    fn dialog_signatures_round_trip_through_yaml() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            dialog_signatures: vec![DialogSignature::new("usage-limit", "Stop and wait")],
        };
        let y = serde_yaml::to_string(&spec).unwrap();
        assert!(y.contains("dialog_signatures"), "got {y:?}");
        let back: AgentRunnerSpec = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.dialog_signatures, spec.dialog_signatures);

        // Absent in YAML → empty (and elided on the way back out).
        let spec2: AgentRunnerSpec =
            serde_yaml::from_str("command: claude\nflags: []\n").unwrap();
        assert!(spec2.dialog_signatures.is_empty());
        let y2 = serde_yaml::to_string(&spec2).unwrap();
        assert!(!y2.contains("dialog_signatures"), "should be elided: {y2:?}");
    }

    #[test]
    fn project_yaml_omits_heartbeat_when_default() {
        // Older project YAMLs predate the field — `#[serde(default)]`
        // means parsing fills in the default and serialization should
        // emit the canonical string.
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.heartbeat, HeartbeatConfig::default());
    }

    #[test]
    fn project_yaml_parses_heartbeat_off_and_string_form() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
heartbeat: off
"#;
        // YAML interprets bare `off` as the boolean `false`. Quote it
        // explicitly when handing user-written configs — but the
        // wizard / serializer always writes the quoted form below.
        let yaml_quoted = yaml.replace("heartbeat: off", "heartbeat: \"off\"");
        let p: Project = serde_yaml::from_str(&yaml_quoted).unwrap();
        assert_eq!(p.heartbeat, HeartbeatConfig::Off);

        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
heartbeat: 90s
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            p.heartbeat,
            HeartbeatConfig::Every(Duration::from_secs(90))
        );
    }

    #[test]
    fn project_yaml_rejects_heartbeat_without_unit() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
heartbeat: 180
"#;
        // YAML parses `180` as an integer, not a string — so deserialization
        // fails at the type level (`expected String`). Quote it and we
        // get the explicit "missing unit" error.
        assert!(serde_yaml::from_str::<Project>(yaml).is_err());

        let yaml_quoted = yaml.replace("heartbeat: 180", "heartbeat: \"180\"");
        let err = serde_yaml::from_str::<Project>(&yaml_quoted)
            .expect_err("must reject unitless heartbeat");
        let msg = err.to_string();
        assert!(
            msg.contains("missing unit") || msg.contains("180"),
            "expected error to mention the missing unit, got: {msg}"
        );
    }

    // ---- Git config (base_branch, merge_strategy) -------------------------

    #[test]
    fn git_config_defaults_to_squash_and_no_base_branch_override() {
        let g = GitConfig::default();
        assert!(g.base_branch.is_none());
        assert_eq!(g.merge_strategy, MergeStrategy::Squash);
    }

    #[test]
    fn merge_strategy_yaml_wire_form_is_snake_case() {
        assert_eq!(
            serde_yaml::to_string(&MergeStrategy::Squash).unwrap().trim(),
            "squash"
        );
        assert_eq!(
            serde_yaml::to_string(&MergeStrategy::Merge).unwrap().trim(),
            "merge"
        );
        assert_eq!(
            serde_yaml::to_string(&MergeStrategy::Rebase).unwrap().trim(),
            "rebase"
        );
        for s in [MergeStrategy::Squash, MergeStrategy::Merge, MergeStrategy::Rebase] {
            let y = serde_yaml::to_string(&s).unwrap();
            let back: MergeStrategy = serde_yaml::from_str(&y).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn merge_strategy_gh_flags_match_cli() {
        assert_eq!(MergeStrategy::Squash.gh_flag(), "--squash");
        assert_eq!(MergeStrategy::Merge.gh_flag(), "--merge");
        assert_eq!(MergeStrategy::Rebase.gh_flag(), "--rebase");
    }

    #[test]
    fn project_yaml_omits_git_block_and_uses_defaults() {
        // Pre-existing project YAMLs don't carry a `git:` block; the
        // accessors must fall back to `default_branch` and `Squash`.
        let yaml = r#"
name: p
repo: r
default_branch: develop
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.git, GitConfig::default());
        assert_eq!(p.base_branch(), "develop");
        assert_eq!(p.merge_strategy(), MergeStrategy::Squash);
    }

    #[test]
    fn project_yaml_parses_git_block() {
        let yaml = r#"
name: p
repo: r
default_branch: main
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
git:
  base_branch: trunk
  merge_strategy: rebase
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.git.base_branch.as_deref(), Some("trunk"));
        assert_eq!(p.git.merge_strategy, MergeStrategy::Rebase);
        // base_branch() prefers git.base_branch over default_branch when
        // both are present.
        assert_eq!(p.base_branch(), "trunk");
        assert_eq!(p.merge_strategy(), MergeStrategy::Rebase);
    }

    #[test]
    fn project_yaml_parses_partial_git_block_only_merge_strategy() {
        // A common shape: keep the historical default_branch, just opt
        // into a non-squash merge.
        let yaml = r#"
name: p
repo: r
default_branch: main
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
git:
  merge_strategy: merge
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert!(p.git.base_branch.is_none());
        assert_eq!(p.git.merge_strategy, MergeStrategy::Merge);
        assert_eq!(p.base_branch(), "main");
        assert_eq!(p.merge_strategy(), MergeStrategy::Merge);
    }

    #[test]
    fn git_block_round_trips_and_omits_unset_base_branch() {
        let cfg = GitConfig {
            base_branch: None,
            merge_strategy: MergeStrategy::Merge,
        };
        let y = serde_yaml::to_string(&cfg).unwrap();
        // base_branch is None → must not surface on the wire.
        assert!(!y.contains("base_branch"), "got: {y}");
        assert!(y.contains("merge_strategy: merge"), "got: {y}");
        let back: GitConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(cfg, back);

        let cfg = GitConfig {
            base_branch: Some("trunk".into()),
            merge_strategy: MergeStrategy::Squash,
        };
        let y = serde_yaml::to_string(&cfg).unwrap();
        assert!(y.contains("base_branch: trunk"), "got: {y}");
        let back: GitConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn project_yaml_rejects_unknown_merge_strategy() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
git:
  merge_strategy: ff_only
"#;
        // `ff_only` isn't a known variant — serde must reject so a typo
        // doesn't silently fall back to `Squash`.
        assert!(serde_yaml::from_str::<Project>(yaml).is_err());
    }

    // ---- Per-workflow zen resolution helpers ------------------------------
    //
    // `checks_for_task_in_workflow`, `ci_timeout_for_workflow`,
    // `danger_paths_for_workflow` collapse the three-layer hierarchy
    // (project → workflow → task) into a single resolved value per call
    // site. The contract these tests pin down: workflow override wins
    // over project default; per-task override wins over both for the
    // check list; workflow-omitted resolution is identical to passing
    // `None`.

    fn workflow_with_zen(zen: WorkflowZenConfig) -> crate::Workflow {
        let mut wf = crate::default_workflow();
        wf.zen = Some(zen);
        wf
    }

    #[test]
    fn ci_timeout_for_workflow_uses_workflow_override_when_set() {
        let project = project_with_zen(ZenConfig::default());
        let wf = workflow_with_zen(WorkflowZenConfig {
            ci_timeout: Some(Duration::from_secs(60)),
            ..Default::default()
        });
        assert_eq!(
            ci_timeout_for_workflow(&project, Some(&wf)),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn ci_timeout_for_workflow_falls_back_to_project_when_unset() {
        let project = project_with_zen(ZenConfig {
            ci_timeout: Duration::from_secs(1234),
            ..Default::default()
        });
        let wf = workflow_with_zen(WorkflowZenConfig::default());
        assert_eq!(
            ci_timeout_for_workflow(&project, Some(&wf)),
            Duration::from_secs(1234)
        );
        // None-workflow is the same as a workflow with no zen overrides.
        assert_eq!(
            ci_timeout_for_workflow(&project, None),
            Duration::from_secs(1234)
        );
    }

    #[test]
    fn danger_paths_for_workflow_extend_appends_to_resolved_project_list() {
        let project = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Extend(vec!["site/public/install.sh".into()]),
            ..Default::default()
        });
        let wf = workflow_with_zen(WorkflowZenConfig {
            danger_paths: Some(ZenDangerPaths::Extend(vec!["fixtures/**".into()])),
            ..Default::default()
        });
        let resolved = danger_paths_for_workflow(&project, Some(&wf));
        // Builtins still present + project extend + workflow extend, in
        // that order, with dedupe preserving first occurrence.
        assert!(resolved.iter().any(|p| p == ".github/workflows/**"));
        assert!(resolved.iter().any(|p| p == "site/public/install.sh"));
        assert!(resolved.iter().any(|p| p == "fixtures/**"));
    }

    #[test]
    fn danger_paths_for_workflow_override_replaces_everything() {
        // The project may have a wide list with builtins; an `override:`
        // at the workflow level wins outright.
        let project = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Extend(vec!["site/public/install.sh".into()]),
            ..Default::default()
        });
        let wf = workflow_with_zen(WorkflowZenConfig {
            danger_paths: Some(ZenDangerPaths::Override(vec!["config/**".into()])),
            ..Default::default()
        });
        let resolved = danger_paths_for_workflow(&project, Some(&wf));
        assert_eq!(resolved, vec!["config/**".to_string()]);
    }

    #[test]
    fn danger_paths_for_workflow_falls_back_when_workflow_lacks_override() {
        let project = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Override(vec!["just/this".into()]),
            ..Default::default()
        });
        let wf = workflow_with_zen(WorkflowZenConfig::default());
        // Workflow has no danger_paths override → project's resolved list
        // wins. `Override` mode at the project level means the list IS
        // exactly the user's, no builtins.
        let resolved = danger_paths_for_workflow(&project, Some(&wf));
        assert_eq!(resolved, vec!["just/this".to_string()]);
        // None-workflow matches Some(workflow-with-no-override).
        assert_eq!(danger_paths_for_workflow(&project, None), resolved);
    }

    #[test]
    fn checks_for_task_in_workflow_uses_workflow_checks_when_set() {
        let project = project_with_zen(ZenConfig {
            checks: ZenChecks {
                local: vec!["cargo test".into()],
            },
            ..Default::default()
        });
        let wf = workflow_with_zen(WorkflowZenConfig {
            checks: Some(ZenChecks {
                local: vec!["pytest -q".into()],
            }),
            ..Default::default()
        });
        let task = task_with_zen(None);
        assert_eq!(
            checks_for_task_in_workflow(&project, Some(&wf), &task),
            vec!["pytest -q".to_string()]
        );
    }

    #[test]
    fn checks_for_task_only_replaces_workflow_base() {
        // `checks_only` on the task wins over both project AND workflow
        // base lists.
        let project = project_with_zen(ZenConfig {
            checks: ZenChecks {
                local: vec!["cargo test".into()],
            },
            ..Default::default()
        });
        let wf = workflow_with_zen(WorkflowZenConfig {
            checks: Some(ZenChecks {
                local: vec!["pytest -q".into()],
            }),
            ..Default::default()
        });
        let task = task_with_zen(Some(TaskZenConfig {
            enabled: None,
            checks_only: vec!["just this one".into()],
            checks_additional: vec![],
        }));
        assert_eq!(
            checks_for_task_in_workflow(&project, Some(&wf), &task),
            vec!["just this one".to_string()]
        );
    }

    #[test]
    fn checks_for_task_additional_extends_workflow_base() {
        let project = project_with_zen(ZenConfig {
            checks: ZenChecks {
                local: vec!["cargo test".into()],
            },
            ..Default::default()
        });
        let wf = workflow_with_zen(WorkflowZenConfig {
            checks: Some(ZenChecks {
                local: vec!["pytest -q".into()],
            }),
            ..Default::default()
        });
        let task = task_with_zen(Some(TaskZenConfig {
            enabled: None,
            checks_only: vec![],
            checks_additional: vec!["npm test".into()],
        }));
        assert_eq!(
            checks_for_task_in_workflow(&project, Some(&wf), &task),
            vec!["pytest -q".to_string(), "npm test".to_string()],
            "workflow checks form the base, per-task additional appended"
        );
    }

    #[test]
    fn checks_for_task_no_workflow_matches_legacy_helper() {
        // Passing `None` for the workflow must produce the exact same
        // list as the older `checks_for_task` helper — call sites that
        // haven't migrated yet need that invariant.
        let project = project_with_zen(ZenConfig {
            checks: ZenChecks {
                local: vec!["cargo test".into(), "cargo clippy".into()],
            },
            ..Default::default()
        });
        let task = task_with_zen(Some(TaskZenConfig {
            enabled: None,
            checks_additional: vec!["npm test".into()],
            checks_only: vec![],
        }));
        assert_eq!(
            checks_for_task_in_workflow(&project, None, &task),
            checks_for_task(&project, &task),
        );
    }

    // ---- Shared / user-local YAML split (in-repo config mode) -------------
    //
    // These tests pin down the contract for Phase 1 of the in-repo config
    // work: `Project` gains a set of parse/serialize helpers that split its
    // fields into a shared half (safe to commit) and a user-local half
    // (never committed), while the historical single-YAML shape used by
    // global mode keeps parsing unchanged. See
    // `Plans/in-repo-vs-global-project-config.md`.

    /// Fully-populated project fixture with something in every non-runtime
    /// field, so round-trip tests notice if a bucket loses a field.
    fn fully_populated_project() -> Project {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec!["--verbose".into()],
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "shelbi".into(),
            default_branch: "main".into(),
            config_mode: Some(ConfigMode::InRepo),
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            github_url: Some("git@github.com:example/shelbi.git".into()),
            workspace_poll_interval_secs: 7,
            workspace_permissions_mode: "acceptEdits".into(),
            workspace_settings_template: Some(PathBuf::from(
                "workspace-settings.json.template",
            )),
            zen: ZenConfig {
                checks: ZenChecks {
                    local: vec!["cargo test".into()],
                },
                ci_timeout: Duration::from_secs(900),
                danger_paths: ZenDangerPaths::Extend(vec!["secrets/**".into()]),
            },
            heartbeat: HeartbeatConfig::Every(Duration::from_secs(120)),
            git: GitConfig {
                base_branch: Some("trunk".into()),
                merge_strategy: MergeStrategy::Rebase,
            },
            review: ReviewConfig {
                base_port: 4000,
                port_stride: 20,
                setup: vec!["npm install".into()],
                serve: Some("npm run dev -- --port $PORT".into()),
                ready_probe: Some(ReadyProbe {
                    http: Some("http://localhost:$PORT".into()),
                    timeout: Duration::from_secs(45),
                }),
            },
            repo: "/home/dev/shelbi".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/home/dev/shelbi".into(),
                host: None,
            }],
            editor: Some("nvim".into()),
            workspaces: vec![WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: WorkspaceRole::Review,
            }],
            contextstore_sync: vec![ContextStoreSyncSpec {
                space: "Shelbi".into(),
                path: PathBuf::from("~/Documents/ContextStore/shelbi"),
            }],
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn config_mode_defaults_to_none_and_omits_in_serialization() {
        // Pre-split project YAMLs don't carry `config_mode:` — the flat
        // parser must accept them and re-serialize without leaking a
        // synthetic key. `None` is the on-disk shape for
        // [`ConfigMode::Global`].
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert!(p.config_mode.is_none());
        let back = serde_yaml::to_string(&p).unwrap();
        assert!(!back.contains("config_mode"), "got: {back}");
    }

    #[test]
    fn config_mode_parses_kebab_case_variants() {
        let yaml = r#"
name: p
repo: r
config_mode: in-repo
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.config_mode, Some(ConfigMode::InRepo));
        // Round-trip re-serializes back to the kebab-case form.
        let back = serde_yaml::to_string(&p).unwrap();
        assert!(back.contains("config_mode: in-repo"), "got: {back}");

        // `global` is the other explicit variant; the parser accepts it
        // even though it matches the default.
        let yaml_global = yaml.replace("in-repo", "global");
        let p: Project = serde_yaml::from_str(&yaml_global).unwrap();
        assert_eq!(p.config_mode, Some(ConfigMode::Global));
    }

    #[test]
    fn shared_and_local_field_lists_cover_every_non_runtime_field() {
        // If someone adds a field to `Project` and forgets to place it in
        // one of the two buckets, the split helpers will silently drop it
        // from the emitted YAML. Guard against that by serializing a
        // populated project and asserting every top-level key is either
        // shared, user-local, or the legacy `workers` alias (which is a
        // deserialization alias only — no serialize path emits it).
        let p = fully_populated_project();
        let value = serde_yaml::to_value(&p).unwrap();
        let map = value.as_mapping().expect("Project serializes as a map");
        for (k, _) in map {
            let key = k.as_str().expect("all Project keys are strings");
            let in_shared = SHARED_PROJECT_FIELDS.contains(&key);
            let in_local = LOCAL_PROJECT_FIELDS.contains(&key);
            assert!(
                in_shared || in_local,
                "field `{key}` is in `Project` but not in either bucket list"
            );
            assert!(
                !(in_shared && in_local),
                "field `{key}` is in BOTH bucket lists — pick one"
            );
        }
    }

    #[test]
    fn from_yaml_str_matches_direct_serde_deserialize() {
        // `Project::from_yaml_str` is the global-mode entry point and must
        // stay behavior-identical to the historical
        // `serde_yaml::from_str::<Project>` path.
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let via_helper = Project::from_yaml_str(yaml).unwrap();
        let via_serde: Project = serde_yaml::from_str(yaml).unwrap();
        // No PartialEq on Project — compare via a stable re-serialization.
        assert_eq!(
            serde_yaml::to_string(&via_helper).unwrap(),
            serde_yaml::to_string(&via_serde).unwrap()
        );
    }

    #[test]
    fn split_yaml_round_trips_populated_project() {
        // A populated Project → split YAMLs → re-merged Project must
        // stably re-emit the same split YAMLs (no field drift, no key
        // migration between halves).
        let p = fully_populated_project();
        let shared_1 = p.to_shared_yaml_string().unwrap();
        let local_1 = p.to_local_yaml_string().unwrap();

        let reparsed = Project::from_split_yaml_str(&shared_1, &local_1).unwrap();
        let shared_2 = reparsed.to_shared_yaml_string().unwrap();
        let local_2 = reparsed.to_local_yaml_string().unwrap();

        assert_eq!(shared_1, shared_2);
        assert_eq!(local_1, local_2);
    }

    #[test]
    fn split_yaml_shared_half_contains_only_shared_keys() {
        let p = fully_populated_project();
        let shared_yaml = p.to_shared_yaml_string().unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&shared_yaml).unwrap();
        let map = value.as_mapping().unwrap();
        for (k, _) in map {
            let name = k.as_str().unwrap();
            assert!(
                SHARED_PROJECT_FIELDS.contains(&name),
                "shared YAML leaked `{name}` — should be user-local"
            );
        }
        // Sample assertions: the shared half must carry the fields the
        // task description explicitly enumerates.
        for expected in ["name", "default_branch", "orchestrator", "agent_runners", "zen", "git", "heartbeat", "config_mode"] {
            assert!(
                map.contains_key(serde_yaml::Value::String(expected.into())),
                "shared YAML missing `{expected}`"
            );
        }
    }

    #[test]
    fn split_yaml_local_half_contains_only_user_local_keys() {
        let p = fully_populated_project();
        let local_yaml = p.to_local_yaml_string().unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&local_yaml).unwrap();
        let map = value.as_mapping().unwrap();
        for (k, _) in map {
            let name = k.as_str().unwrap();
            assert!(
                LOCAL_PROJECT_FIELDS.contains(&name),
                "user-local YAML leaked `{name}` — should be shared"
            );
        }
        for expected in ["repo", "machines", "workspaces", "contextstore_sync"] {
            assert!(
                map.contains_key(serde_yaml::Value::String(expected.into())),
                "user-local YAML missing `{expected}`"
            );
        }
    }

    #[test]
    fn split_yaml_matches_global_yaml_after_merge() {
        // Merging the two split halves must produce the same in-memory
        // Project as the equivalent single YAML would in global mode.
        let p = fully_populated_project();
        let global_yaml = serde_yaml::to_string(&p).unwrap();
        let shared_yaml = p.to_shared_yaml_string().unwrap();
        let local_yaml = p.to_local_yaml_string().unwrap();

        let from_global = Project::from_yaml_str(&global_yaml).unwrap();
        let from_split = Project::from_split_yaml_str(&shared_yaml, &local_yaml).unwrap();

        // Compare via a stable re-serialization to sidestep PartialEq.
        assert_eq!(
            serde_yaml::to_string(&from_global).unwrap(),
            serde_yaml::to_string(&from_split).unwrap()
        );
    }

    #[test]
    fn split_yaml_rejects_user_local_field_in_shared_file() {
        // A shared YAML that includes `machines:` (a user-local field)
        // must produce a targeted error pointing at the correct file —
        // not a silent misparse and not the generic "unknown field"
        // message from `deny_unknown_fields`.
        let shared = r#"
name: p
default_branch: main
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
machines:
  - { name: hub, kind: local, work_dir: /tmp }
"#;
        let local = r#"
repo: /tmp
"#;
        match Project::from_split_yaml_str(shared, local) {
            Err(crate::Error::MisplacedProjectField {
                field,
                found_in,
                expected_in,
            }) => {
                assert_eq!(field, "machines");
                assert_eq!(found_in, "shared");
                assert_eq!(expected_in, "user-local");
            }
            other => panic!("expected MisplacedProjectField, got {other:?}"),
        }
    }

    #[test]
    fn split_yaml_rejects_shared_field_in_user_local_file() {
        let shared = r#"
name: p
default_branch: main
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        // `zen:` is a shared field — declaring it in the user-local half
        // must fail with the pointer to the shared file.
        let local = r#"
repo: /tmp
machines:
  - { name: hub, kind: local, work_dir: /tmp }
zen:
  ci_timeout: 60
"#;
        match Project::from_split_yaml_str(shared, local) {
            Err(crate::Error::MisplacedProjectField {
                field,
                found_in,
                expected_in,
            }) => {
                assert_eq!(field, "zen");
                assert_eq!(found_in, "user-local");
                assert_eq!(expected_in, "shared");
            }
            other => panic!("expected MisplacedProjectField, got {other:?}"),
        }
    }

    #[test]
    fn split_yaml_rejects_duplicate_unknown_key_across_files() {
        // A key that appears on both sides is ambiguous — the merge
        // refuses rather than silently letting one side win. Bucket-known
        // fields can't collide (a shared field on the local side is
        // caught by the misplacement check first) so this defensive path
        // fires for unknown keys appearing in both files.
        let shared = r#"
name: p
default_branch: main
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
custom_ext: shared-side
"#;
        let local = r#"
repo: /tmp
machines:
  - { name: hub, kind: local, work_dir: /tmp }
custom_ext: local-side
"#;
        match Project::from_split_yaml_str(shared, local) {
            Err(crate::Error::Other(msg)) => {
                assert!(msg.contains("custom_ext"), "msg was: {msg}");
                assert!(msg.contains("both"), "msg was: {msg}");
            }
            other => panic!("expected the duplicate-key `Other` error, got {other:?}"),
        }
    }

    #[test]
    fn split_yaml_shared_missing_name_bubbles_up_deserialize_error() {
        // Merging still delegates to the flat Project deserializer for
        // the final assembly, so a required field missing from both
        // halves surfaces as the usual yaml error (not a placement
        // error). This just documents the seam.
        let shared = r#"
default_branch: main
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let local = r#"
repo: /tmp
machines:
  - { name: hub, kind: local, work_dir: /tmp }
"#;
        let err = Project::from_split_yaml_str(shared, local)
            .expect_err("`name` is required — merge must fail");
        let msg = err.to_string();
        assert!(msg.contains("name"), "err was: {msg}");
    }
}
