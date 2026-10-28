//! The editing model behind Settings ▸ Agents: every agent directory under a
//! project's `agents/` (`agent.yaml`, `instructions.md`, `skills/`, and the
//! runner hook wiring in `settings.json`, which these screens carry along but
//! never edit) plus the shared instructions in `agents/_shared/preamble.md`,
//! loaded into one value that edits, validates and saves them. Views never
//! touch the files. The Workflows counterpart is
//! [`workflow_config`](crate::workflow_config); this model follows its shape.
//!
//! [`AgentsConfig::load`] reads everything. Each edit operation takes `&self`
//! and returns a validated new model or an [`AgentError`] in the user's
//! terms, so a view can hold the last good model and show the error. Nothing
//! is written until [`AgentsConfig::save`], which writes only the files whose
//! content changed: `agent.yaml` through [`yaml_edit`](crate::yaml_edit), so
//! its comments, unknown keys and formatting survive; `instructions.md`, the
//! preamble and skills as the raw text. Each file is written atomically. A
//! save first re-reads every agent directory (and the preamble) it is about
//! to change and refuses, writing nothing, if any changed on disk since the
//! load.
//!
//! ## The API, by screen
//!
//! - **Index.** [`summaries`](AgentsConfig::summaries): name, description,
//!   preferred runner and whether it's the shipped recommendation, the
//!   workflow statuses assigned to the agent, its skill count, and its
//!   [`Provenance`] (the Customized badge). Agents whose `agent.yaml` can't
//!   be parsed are in [`unreadable`](AgentsConfig::unreadable), never
//!   written. New agent / ⋮ menu: `create_agent` / `duplicate_agent` /
//!   `rename_agent` / `delete_agent`, with
//!   [`check_rename`](AgentsConfig::check_rename) and
//!   [`check_delete`](AgentsConfig::check_delete) to disable the last two up
//!   front. Shared instructions:
//!   [`shared_instructions`](AgentsConfig::shared_instructions) /
//!   [`set_shared_instructions`](AgentsConfig::set_shared_instructions).
//! - **Details.** [`agent`](AgentsConfig::agent) reads one [`Agent`];
//!   `set_description`, `set_instructions`, `set_preferred_runner`,
//!   `set_model`, `set_reasoning_effort`, `set_permissions_mode`,
//!   `add_skill` / `import_skill` / `remove_skill`, and
//!   `reset_to_default` (with [`can_reset`](AgentsConfig::can_reset)).
//!   [`assignments`](AgentsConfig::assignments) is the read-only "Workflow
//!   assignments" list; [`Agent::skill_files`] / [`Agent::skill_text`]
//!   inspect a skill as the draft holds it. What the project sets over an
//!   agent (its runner pin, a model or effort for every agent on a runner,
//!   the permission ceiling) is read with
//!   [`project_runner_pin`](AgentsConfig::project_runner_pin) and its
//!   siblings, so a details view can say what applies.
//! - **Pickers.** [`runner_options`] (each runner with its model
//!   suggestions and reasoning efforts) and [`PERMISSION_MODES`]. `None`
//!   from a getter (and to a setter) is "Use runner default" / "Use project
//!   settings": the key is absent from `agent.yaml`.
//! - **Save / Discard.** [`is_dirty`](AgentsConfig::is_dirty), then
//!   [`save`](AgentsConfig::save) (which returns the reloaded model and any
//!   comments `agent.yaml` lost); discarding is dropping the edited value.
//!
//! ## When changes apply
//!
//! Nothing caches agent files: dispatch reads `agent.yaml`
//! ([`crate::load_agent_manifest`]), composes the preamble and
//! `instructions.md` ([`crate::compose_agent_prompt`]) and mounts `skills/`
//! each time it launches an agent session. So a save reaches every **new**
//! session, exactly like a hand edit, and a running session keeps the prompt,
//! model and skills it launched with until it is restarted.
//!
//! ## Decisions
//!
//! - **Customized** ([`Provenance`]) compares what these screens edit with
//!   the shipped default for that agent name: `instructions.md` through the
//!   self-heal's provenance check ([`crate::classify_agent_divergence`], so
//!   a file a newer Shelbi would upgrade in place is not "customized"),
//!   `agent.yaml` by meaning (comments and key order don't count), and the
//!   skill set and the shipped skills' text. `settings.json` doesn't count:
//!   these screens don't edit it and reset doesn't touch it. An agent whose
//!   name no shipped default has is [`Provenance::UserCreated`]: there is
//!   nothing to compare with, so the UI shows no badge (like
//!   `shelbi agent list`'s `-`).
//! - **Recommended** under the runner means the agent's `preferred_runner`
//!   is the one its shipped default sets. User-created agents have no
//!   recommendation.
//! - The agent's name is its directory name, which is how workflows, the
//!   project's `agents.<name>.runner` pin and dispatch address it. Rename
//!   moves the directory, and updates the manifest's `name:` only when it
//!   matched the old directory (a different `name:` is a package identity
//!   the user chose). Rename and delete are refused while a workflow status
//!   or the project's runner pin names the agent ([`AgentError::InUse`]
//!   lists them), and always for a shipped agent: the self-heal recreates a
//!   missing shipped agent on the next reload. Reset is the way back.
//! - A new agent gets an `agent.yaml` with its `name:` (and description), a
//!   placeholder `instructions.md` ([`placeholder_instructions`], the one
//!   `shelbi agent new` writes) and an empty `skills/`; with no
//!   `settings.json` it uses the project-wide template, as dispatch already
//!   does. A duplicate copies every file of the source as loaded (a symlink
//!   is copied as the file it points to), with the manifest's `name:` set
//!   to the new name.
//! - A skill is what the runner mounts from `skills/`: a directory
//!   `skills/<name>/SKILL.md` (the layout Claude Code loads, and the one
//!   `add_skill` writes) or a legacy flat `skills/<name>.md`. Its
//!   description is the frontmatter `description:`, which both Add skill
//!   and an import require (an import of a missing path, or of a file whose
//!   frontmatter is absent, broken or has no description, is refused).
//! - Reset rewrites `instructions.md` and `agent.yaml` (comments included)
//!   to the shipped text and makes the skills exactly the shipped ones.
//! - `model` is free text (it's passed to the runner's `--model`), so the
//!   picker's list is [`shelbi_core::model_suggestions`]; a value not in it
//!   stays as written. A generic runner takes no model or effort Shelbi can
//!   drive, so setting one is refused.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_yaml::Value;
use shelbi_core::{
    AgentManifest, ProjectStatuses, ReasoningEffort, RunnerKind, RunnerManifestConfig,
    StatusCategory,
};

pub use shelbi_core::PERMISSION_MODES;

use crate::agent_workspaces::{
    bundled_agent_manifest_yaml, BundledAgent, DEFAULT_AGENTS, SHARED_AGENT_DIR,
    SHARED_PREAMBLE_FILE,
};
use crate::{yaml_edit, AgentDivergence};

const MANIFEST_FILE: &str = "agent.yaml";
const INSTRUCTIONS_FILE: &str = "instructions.md";
const SKILLS_DIR: &str = "skills";
const SKILL_FILE: &str = "SKILL.md";
/// An agent directory's files, by `/`-separated path relative to it.
type Files = BTreeMap<String, Vec<u8>>;

/// Deeper than any real skill tree; stops a symlink loop.
const MAX_DEPTH: usize = 16;

/// Why an edit, load or save was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// The edit would leave invalid config.
    #[error("{0}")]
    Invalid(String),
    #[error("no agent `{0}`")]
    UnknownAgent(String),
    #[error("an agent named `{0}` already exists")]
    AgentExists(String),
    /// Workflow statuses (or the project's runner pin) name the agent, so it
    /// can't be renamed or deleted.
    #[error(
        "agent `{agent}` is used by {}; assign those to another agent first",
        .references.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    )]
    InUse {
        agent: String,
        references: Vec<AgentReference>,
    },
    /// Shipped agents come back on the next reload, so they can't be renamed
    /// or deleted; reset one instead.
    #[error("`{0}` ships with Shelbi and can't be renamed or deleted; reset it instead")]
    Shipped(String),
    #[error("`{0}` isn't a shipped agent, so there's no default to reset to")]
    NotShipped(String),
    #[error("agent `{agent}` has no skill `{skill}`")]
    UnknownSkill { agent: String, skill: String },
    #[error("agent `{agent}` already has a skill `{skill}`")]
    SkillExists { agent: String, skill: String },
    #[error("{} runners take no {setting} Shelbi can set", .runner.as_str())]
    NotForRunner {
        runner: RunnerKind,
        setting: &'static str,
    },
    #[error("`{0}` isn't a permission mode (use one of: {})", PERMISSION_MODES.join(", "))]
    UnknownPermissionMode(String),
    /// Files changed on disk since the model was loaded. Nothing was
    /// written; reload and redo the edit.
    #[error("changed on disk since it was loaded: {}", display_paths(.0))]
    Conflict(Vec<PathBuf>),
    /// A file couldn't be read, parsed or written.
    #[error("{path}: {message}")]
    File { path: PathBuf, message: String },
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub type Result<T, E = AgentError> = std::result::Result<T, E>;

/// A workflow status assigned to an agent (`agent:` on the status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The workflow's file stem.
    pub workflow: String,
    pub status: String,
    /// The catalog's name for the status (its id when the catalog lacks it).
    pub status_name: String,
    /// `None` when the catalog lacks the status.
    pub category: Option<StatusCategory>,
}

/// Config that names an agent, blocking its rename or delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentReference {
    Status(Assignment),
    /// The project YAML's `agents.<name>.runner` pin.
    ProjectRunnerPin,
}

impl std::fmt::Display for AgentReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentReference::Status(a) => {
                write!(f, "workflow `{}` status `{}`", a.workflow, a.status)
            }
            AgentReference::ProjectRunnerPin => f.write_str("the project's runner pin"),
        }
    }
}

/// Where an agent's files come from, for the Customized badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A shipped agent whose files are still a shipped default.
    Shipped,
    /// A shipped agent the user edited.
    Customized,
    /// No shipped agent has this name.
    UserCreated,
}

impl Provenance {
    pub fn is_customized(self) -> bool {
        self == Provenance::Customized
    }
}

/// One row of the index's agent table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSummary {
    pub name: String,
    pub description: Option<String>,
    pub preferred_runner: Option<RunnerKind>,
    /// The preferred runner is the one the shipped default recommends.
    pub runner_recommended: bool,
    pub assignments: Vec<Assignment>,
    pub skills: usize,
    pub provenance: Provenance,
}

/// An agent directory the model can't edit (`agent.yaml` or
/// `instructions.md` doesn't parse). Shown so the user can fix it by hand;
/// never written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableAgent {
    pub name: String,
    pub path: PathBuf,
    pub error: String,
}

/// How a skill sits under `skills/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillLayout {
    /// `skills/<name>/SKILL.md` (plus whatever else the directory holds).
    Directory,
    /// A legacy flat `skills/<name>.md`.
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    /// The frontmatter `description:`.
    pub description: Option<String>,
    pub layout: SkillLayout,
}

/// One runner a picker offers, with what it can be set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerOptions {
    pub kind: RunnerKind,
    /// Suggested models; any other text is valid too. Empty: the runner
    /// takes no model.
    pub models: &'static [&'static str],
    /// Empty: the runner takes no reasoning effort.
    pub efforts: &'static [ReasoningEffort],
}

/// Every runner kind Shelbi supports, in picker order.
pub fn runner_options() -> Vec<RunnerOptions> {
    [RunnerKind::Claude, RunnerKind::Codex, RunnerKind::Generic]
        .into_iter()
        .map(|kind| RunnerOptions {
            kind,
            models: shelbi_core::model_suggestions(kind),
            efforts: shelbi_core::reasoning_efforts(kind),
        })
        .collect()
}

/// The placeholder `instructions.md` a new agent starts with (also
/// `shelbi agent new`'s).
pub fn placeholder_instructions(name: &str) -> String {
    format!(
        "# {name} agent\n\
         \n\
         <!-- Replace this file with the agent's system prompt. It will be loaded as\n\
         --system-prompt when shelbi dispatches a task to a workspace running this\n\
         agent. Project-wide context (the \"you're in this monorepo, here's the\n\
         layout\" intro) belongs in agents/_shared/preamble.md instead — that gets\n\
         prepended automatically. -->\n"
    )
}

/// The shipped `agent.yaml` for a shipped agent name, parsed.
pub fn shipped_manifest(name: &str) -> Option<AgentManifest> {
    let bundled = bundled(name)?;
    AgentManifest::from_yaml_str(&bundled_agent_manifest_yaml(bundled)).ok()
}

fn bundled(name: &str) -> Option<&'static BundledAgent> {
    DEFAULT_AGENTS.iter().find(|a| a.name == name)
}

/// One editable agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    name: String,
    manifest: AgentManifest,
    instructions: String,
    /// Every other file in the directory (`settings.json`, `skills/…`), by
    /// `/`-separated path relative to it.
    files: Files,
    origin: Origin,
    /// Since a reset, `agent.yaml` is patched from the shipped text, not the
    /// file's.
    shipped_text: bool,
}

impl Agent {
    /// The directory name: how workflows and dispatch address the agent.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The parsed `agent.yaml` (synthesized from the name when absent).
    pub fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    pub fn description(&self) -> Option<&str> {
        self.manifest.description.as_deref()
    }

    pub fn preferred_runner(&self) -> Option<RunnerKind> {
        self.manifest.preferred_runner
    }

    /// `None`: use the runner's default model.
    pub fn model(&self, runner: RunnerKind) -> Option<&str> {
        self.manifest.runners.get(&runner)?.model.as_deref()
    }

    /// `None`: use the runner's default effort.
    pub fn reasoning_effort(&self, runner: RunnerKind) -> Option<ReasoningEffort> {
        self.manifest.runners.get(&runner)?.reasoning_effort
    }

    /// `None`: use the project's settings.
    pub fn permissions_mode(&self) -> Option<&str> {
        self.manifest.permissions_mode.as_deref()
    }

    /// `instructions.md`, raw markdown.
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// The skills, by name.
    pub fn skills(&self) -> Vec<Skill> {
        let mut out: BTreeMap<String, Skill> = BTreeMap::new();
        for (path, bytes) in &self.files {
            let Some(rest) = path.strip_prefix("skills/") else {
                continue;
            };
            let (name, layout) = match rest.split_once('/') {
                Some((dir, SKILL_FILE)) => (dir, SkillLayout::Directory),
                None => match rest.strip_suffix(".md") {
                    Some(stem) => (stem, SkillLayout::File),
                    None => continue,
                },
                Some(_) => continue,
            };
            if name.starts_with('.') || name.is_empty() {
                continue;
            }
            out.insert(
                name.to_string(),
                Skill {
                    name: name.to_string(),
                    description: skill_description(&String::from_utf8_lossy(bytes)),
                    layout,
                },
            );
        }
        out.into_values().collect()
    }

    /// Skill `skill`'s files (`SKILL.md` and whatever else its directory
    /// holds, or the flat `.md`), by path relative to the agent's
    /// directory, as the draft holds them.
    pub fn skill_files(&self, skill: &str) -> Vec<String> {
        self.skill_paths(skill)
    }

    /// Skill `skill`'s `SKILL.md` (or flat `.md`) as the draft holds it.
    pub fn skill_text(&self, skill: &str) -> Option<String> {
        let path = self.skill_file(skill)?;
        self.files.get(&path).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// Where skill `skill`'s `SKILL.md` (or flat `.md`) sits, relative to
    /// the agent's directory.
    pub fn skill_file(&self, skill: &str) -> Option<String> {
        let skill = self.skills().into_iter().find(|s| s.name == skill)?;
        Some(match skill.layout {
            SkillLayout::Directory => format!("{SKILLS_DIR}/{}/{SKILL_FILE}", skill.name),
            SkillLayout::File => format!("{SKILLS_DIR}/{}.md", skill.name),
        })
    }

    fn skill_paths(&self, skill: &str) -> Vec<String> {
        let dir = format!("skills/{skill}/");
        let flat = format!("skills/{skill}.md");
        self.files
            .keys()
            .filter(|p| p.starts_with(&dir) || **p == flat)
            .cloned()
            .collect()
    }
}

/// Where an agent's files come from on save.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// Loaded from `agents/<dir>/` (and saved there, or under its new name
    /// after a rename).
    Dir(String),
    /// Created here (including duplicates, which carry their files).
    New,
}

/// What a save did.
#[derive(Debug, Clone)]
pub struct Saved {
    /// The model as now on disk, ready for the next edit.
    pub config: AgentsConfig,
    pub written: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Comments an `agent.yaml` lost: lines whose entry the edit removed, or
    /// the whole file's when its layout couldn't be followed.
    pub dropped_comments: Vec<(PathBuf, Vec<String>)>,
}

/// The editable agents and shared instructions of one project.
#[derive(Debug, Clone)]
pub struct AgentsConfig {
    project: String,
    agents: Vec<Agent>,
    shared_instructions: String,
    unreadable: Vec<UnreadableAgent>,
    disk: Arc<Disk>,
}

/// The files as loaded.
#[derive(Debug)]
struct Disk {
    dir: PathBuf,
    /// Every file of each editable agent directory, by name.
    agents: BTreeMap<String, Loaded>,
    preamble_path: PathBuf,
    /// `None` when the file doesn't exist.
    preamble: Option<String>,
    /// Workflow statuses naming each agent.
    assignments: BTreeMap<String, Vec<Assignment>>,
    /// Agents the project YAML pins a runner for.
    pinned: Vec<String>,
    /// What the project YAML sets over every agent's own preferences.
    overrides: ProjectOverrides,
    /// `deployed_agent_defaults` from the project state: the hash of the
    /// shipped `instructions.md` each agent was deployed with.
    deployed: BTreeMap<String, String>,
}

#[derive(Debug)]
struct Loaded {
    files: Files,
}

impl Loaded {
    fn text(&self, file: &str) -> Option<String> {
        self.files
            .get(file)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }
}

impl AgentsConfig {
    /// Load `project`'s agents, shared instructions and the workflow
    /// statuses that name each agent.
    pub fn load(project: &str) -> Result<Self> {
        let dir = crate::agents_dir(project).map_err(other)?;
        let mut loaded = BTreeMap::new();
        let mut agents = Vec::new();
        let mut unreadable = Vec::new();
        for name in crate::list_agents(project).map_err(other)? {
            let path = dir.join(&name);
            let files = snapshot(&path)?;
            match parse_agent(&name, &files) {
                Ok((manifest, instructions)) => {
                    let mut rest = files.clone();
                    rest.remove(MANIFEST_FILE);
                    rest.remove(INSTRUCTIONS_FILE);
                    agents.push(Agent {
                        name: name.clone(),
                        manifest,
                        instructions,
                        files: rest,
                        origin: Origin::Dir(name.clone()),
                        shipped_text: false,
                    });
                    loaded.insert(name, Loaded { files });
                }
                Err(error) => unreadable.push(UnreadableAgent { name, path, error }),
            }
        }
        // Shipped agents first, in their shipped order, then the user's.
        agents.sort_by_key(|a| {
            let rank = DEFAULT_AGENTS.iter().position(|b| b.name == a.name);
            (rank.unwrap_or(usize::MAX), a.name.clone())
        });

        let preamble_path = dir.join(SHARED_AGENT_DIR).join(SHARED_PREAMBLE_FILE);
        let preamble = read_optional(&preamble_path)?;
        let deployed = crate::read_state(project)
            .map(|s| s.deployed_agent_defaults.into_iter().collect())
            .unwrap_or_default();
        Ok(AgentsConfig {
            project: project.to_string(),
            agents,
            shared_instructions: preamble.clone().unwrap_or_default(),
            unreadable,
            disk: Arc::new(Disk {
                assignments: load_assignments(project)?,
                pinned: load_pins(project),
                overrides: load_overrides(project),
                dir,
                agents: loaded,
                preamble_path,
                preamble,
                deployed,
            }),
        })
    }

    // -----------------------------------------------------------------------
    // Reading

    pub fn project(&self) -> &str {
        &self.project
    }

    /// The editable agents, in index order: shipped ones in shipped order,
    /// then the user's by name, then ones created since the load.
    pub fn agents(&self) -> impl Iterator<Item = &Agent> {
        self.agents.iter()
    }

    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Agent directories that couldn't be loaded for editing.
    pub fn unreadable(&self) -> &[UnreadableAgent] {
        &self.unreadable
    }

    /// The index's agent rows.
    pub fn summaries(&self) -> Vec<AgentSummary> {
        self.agents
            .iter()
            .map(|a| AgentSummary {
                name: a.name.clone(),
                description: a.manifest.description.clone(),
                preferred_runner: a.manifest.preferred_runner,
                runner_recommended: self.runner_recommended(&a.name),
                assignments: self.assignments(&a.name),
                skills: a.skills().len(),
                provenance: self.provenance(&a.name),
            })
            .collect()
    }

    /// The workflow statuses assigned to `agent`, by workflow name then
    /// status order. An agent renamed since the load has the old name's
    /// (the workflows still name the old one until it saves, and a rename
    /// is refused while any do).
    pub fn assignments(&self, agent: &str) -> Vec<Assignment> {
        let loaded_as = match self.agent(agent).map(|a| &a.origin) {
            Some(Origin::Dir(dir)) => dir.as_str(),
            _ => agent,
        };
        self.disk
            .assignments
            .get(loaded_as)
            .cloned()
            .unwrap_or_default()
    }

    /// Everything that names `agent` and so blocks its rename or delete.
    pub fn references(&self, agent: &str) -> Vec<AgentReference> {
        let mut out: Vec<AgentReference> = self
            .assignments(agent)
            .into_iter()
            .map(AgentReference::Status)
            .collect();
        if self.disk.pinned.iter().any(|p| p == agent) {
            out.push(AgentReference::ProjectRunnerPin);
        }
        out
    }

    /// Whether `agent`'s files (as edited) are a shipped default.
    pub fn provenance(&self, agent: &str) -> Provenance {
        let (Some(a), Some(shipped)) = (self.agent(agent), bundled(agent)) else {
            return Provenance::UserCreated;
        };
        let instructions = crate::classify_agent_divergence(
            self.disk.deployed.get(agent).map(String::as_str),
            shipped.instructions,
            &a.instructions,
        );
        let pristine = instructions != AgentDivergence::Customized
            && Some(&a.manifest) == shipped_manifest(agent).as_ref()
            && skills_are_shipped(a, shipped);
        if pristine {
            Provenance::Shipped
        } else {
            Provenance::Customized
        }
    }

    /// The agent's preferred runner is its shipped default's.
    pub fn runner_recommended(&self, agent: &str) -> bool {
        match (self.agent(agent), shipped_manifest(agent)) {
            (Some(a), Some(shipped)) => {
                shipped.preferred_runner.is_some()
                    && a.manifest.preferred_runner == shipped.preferred_runner
            }
            _ => false,
        }
    }

    /// The runner kind the project YAML pins `agent` onto
    /// (`agents.<name>.runner`): it wins over the agent's preferred runner.
    pub fn project_runner_pin(&self, agent: &str) -> Option<RunnerKind> {
        self.disk.overrides.pins.get(agent).copied()
    }

    /// The model the project YAML sets for every agent on `runner`
    /// (`runners.<kind>.model`): it wins over the agent's own.
    pub fn project_model(&self, runner: RunnerKind) -> Option<&str> {
        self.disk.overrides.runners.get(&runner)?.0.as_deref()
    }

    /// The reasoning effort the project YAML sets for every agent on
    /// `runner` (`runners.<kind>.reasoning_effort`): it wins over the
    /// agent's own.
    pub fn project_reasoning_effort(&self, runner: RunnerKind) -> Option<ReasoningEffort> {
        self.disk.overrides.runners.get(&runner)?.1
    }

    /// The project's permission ceiling (`workspace_permissions_mode`): an
    /// agent's request above it is lowered to it, and an agent without one
    /// gets it.
    pub fn project_permissions_ceiling(&self) -> Option<&str> {
        self.disk.overrides.permissions_ceiling.as_deref()
    }

    /// The shared instructions (`agents/_shared/preamble.md`), empty when
    /// the file doesn't exist.
    pub fn shared_instructions(&self) -> &str {
        &self.shared_instructions
    }

    /// `agents/_shared/preamble.md`, for an Edit in editor button.
    pub fn shared_instructions_path(&self) -> &Path {
        &self.disk.preamble_path
    }

    /// `agents/<name>/`.
    pub fn agent_dir(&self, name: &str) -> PathBuf {
        self.disk.dir.join(name)
    }

    /// Whether a save would write anything.
    pub fn is_dirty(&self) -> bool {
        self.plan().map_or(true, |plan| !plan.is_empty())
    }

    // -----------------------------------------------------------------------
    // Agents

    /// Why `agent` can't be renamed, if it can't.
    pub fn check_rename(&self, agent: &str) -> Result<()> {
        self.check_removable(agent)
    }

    /// Why `agent` can't be deleted, if it can't.
    pub fn check_delete(&self, agent: &str) -> Result<()> {
        self.check_removable(agent)
    }

    fn check_removable(&self, agent: &str) -> Result<()> {
        self.index(agent)?;
        if bundled(agent).is_some() {
            return Err(AgentError::Shipped(agent.to_string()));
        }
        let references = self.references(agent);
        if !references.is_empty() {
            return Err(AgentError::InUse {
                agent: agent.to_string(),
                references,
            });
        }
        Ok(())
    }

    /// A new agent named `name`, with a placeholder `instructions.md`.
    pub fn create_agent(&self, name: &str, description: Option<&str>) -> Result<Self> {
        self.check_new_name(name)?;
        let mut next = self.clone();
        next.agents.push(Agent {
            name: name.to_string(),
            manifest: AgentManifest {
                name: name.to_string(),
                version: None,
                description: non_empty(description),
                preferred_runner: None,
                runners: BTreeMap::new(),
                permissions_mode: None,
                requires: None,
            },
            instructions: placeholder_instructions(name),
            files: BTreeMap::new(),
            origin: Origin::New,
            shipped_text: false,
        });
        Ok(next)
    }

    /// A copy of `source` (every file, as edited) named `name`.
    pub fn duplicate_agent(&self, source: &str, name: &str) -> Result<Self> {
        let at = self.index(source)?;
        self.check_new_name(name)?;
        let mut next = self.clone();
        let mut copy = next.agents[at].clone();
        copy.name = name.to_string();
        copy.manifest.name = name.to_string();
        // The copy's `agent.yaml` starts from the source's text.
        if let Origin::Dir(dir) = &copy.origin {
            if let Some(text) = self.disk.agents[dir].text(MANIFEST_FILE) {
                copy.files
                    .insert(MANIFEST_FILE.to_string(), text.into_bytes());
            }
        }
        copy.origin = Origin::New;
        next.agents.push(copy);
        Ok(next)
    }

    /// Rename `from` to `to`: its directory moves on save.
    pub fn rename_agent(&self, from: &str, to: &str) -> Result<Self> {
        if from == to {
            return Ok(self.clone());
        }
        self.check_rename(from)?;
        self.check_new_name(to)?;
        let mut next = self.clone();
        let at = next.index(from)?;
        let agent = &mut next.agents[at];
        if agent.manifest.name == from {
            agent.manifest.name = to.to_string();
        }
        agent.name = to.to_string();
        Ok(next)
    }

    /// Delete an agent nothing references.
    pub fn delete_agent(&self, name: &str) -> Result<Self> {
        self.check_delete(name)?;
        let mut next = self.clone();
        let at = next.index(name)?;
        next.agents.remove(at);
        Ok(next)
    }

    /// Set (or, with `None` or blank, remove) the description.
    pub fn set_description(&self, agent: &str, description: Option<&str>) -> Result<Self> {
        self.edit_manifest(agent, |m| {
            m.description = non_empty(description);
            Ok(())
        })
    }

    /// Replace `instructions.md` (raw markdown).
    pub fn set_instructions(&self, agent: &str, text: &str) -> Result<Self> {
        let mut next = self.clone();
        let at = next.index(agent)?;
        next.agents[at].instructions = text.to_string();
        Ok(next)
    }

    /// Set the preferred runner; `None` removes `preferred_runner:`, so the
    /// project's default runner applies.
    pub fn set_preferred_runner(&self, agent: &str, runner: Option<RunnerKind>) -> Result<Self> {
        self.edit_manifest(agent, |m| {
            m.preferred_runner = runner;
            Ok(())
        })
    }

    /// Set the model `agent` recommends on `runner`; `None` (or blank) is
    /// the runner's default (the key is removed).
    pub fn set_model(&self, agent: &str, runner: RunnerKind, model: Option<&str>) -> Result<Self> {
        let model = non_empty(model);
        if model.is_some() && shelbi_core::model_suggestions(runner).is_empty() {
            return Err(AgentError::NotForRunner {
                runner,
                setting: "model",
            });
        }
        self.edit_runner(agent, runner, |c| c.model = model)
    }

    /// Set the reasoning effort `agent` recommends on `runner`; `None` is
    /// the runner's default (the key is removed).
    pub fn set_reasoning_effort(
        &self,
        agent: &str,
        runner: RunnerKind,
        effort: Option<ReasoningEffort>,
    ) -> Result<Self> {
        if let Some(effort) = effort {
            if !shelbi_core::reasoning_efforts(runner).contains(&effort) {
                return Err(AgentError::NotForRunner {
                    runner,
                    setting: "reasoning effort",
                });
            }
        }
        self.edit_runner(agent, runner, |c| c.reasoning_effort = effort)
    }

    /// Set the permission mode `agent` requests; `None` is the project's
    /// settings (the key is removed).
    pub fn set_permissions_mode(&self, agent: &str, mode: Option<&str>) -> Result<Self> {
        let mode = non_empty(mode);
        if let Some(mode) = &mode {
            if !shelbi_core::is_known_permission_mode(mode) {
                return Err(AgentError::UnknownPermissionMode(mode.clone()));
            }
        }
        self.edit_manifest(agent, |m| {
            m.permissions_mode = mode;
            Ok(())
        })
    }

    /// Add a skill: `skills/<name>/SKILL.md` with `name:` and `description:`
    /// frontmatter, then `body`.
    pub fn add_skill(
        &self,
        agent: &str,
        name: &str,
        description: &str,
        body: &str,
    ) -> Result<Self> {
        validate_skill_name(name)?;
        let description = description.trim();
        if description.is_empty() {
            return Err(AgentError::Invalid(
                "a skill needs a description: the runner uses it to decide when to load the skill"
                    .to_string(),
            ));
        }
        let mut front = serde_yaml::Mapping::new();
        front.insert("name".into(), name.into());
        front.insert("description".into(), description.into());
        let front =
            serde_yaml::to_string(&front).map_err(|e| AgentError::Invalid(e.to_string()))?;
        let mut text = format!("---\n{front}---\n");
        if !body.trim().is_empty() {
            text.push('\n');
            text.push_str(body);
            if !body.ends_with('\n') {
                text.push('\n');
            }
        }
        self.insert_skill(
            agent,
            name,
            [(format!("skills/{name}/{SKILL_FILE}"), text.into_bytes())],
        )
    }

    /// Add a copy of an existing skill: a directory holding `SKILL.md`
    /// (copied whole) or a `.md` file (copied to `skills/<stem>/SKILL.md`).
    /// The skill is named after the directory or file stem.
    pub fn import_skill(&self, agent: &str, source: &Path) -> Result<Self> {
        if !source.exists() {
            return Err(file_err(source, "doesn't exist"));
        }
        let stem = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let name = if source.is_dir() {
            source
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
        } else {
            stem
        };
        validate_skill_name(name)?;
        let files: Vec<(String, Vec<u8>)> = if source.is_dir() {
            let files = snapshot(source)?;
            if !files.contains_key(SKILL_FILE) {
                return Err(file_err(source, format!("has no {SKILL_FILE}")));
            }
            files
                .into_iter()
                .map(|(rel, bytes)| (format!("skills/{name}/{rel}"), bytes))
                .collect()
        } else if source.extension().and_then(|e| e.to_str()) == Some("md") {
            let bytes = fs::read(source).map_err(|e| file_err(source, e))?;
            vec![(format!("skills/{name}/{SKILL_FILE}"), bytes)]
        } else {
            return Err(file_err(
                source,
                format!("isn't a skill (a directory with {SKILL_FILE}, or a .md file)"),
            ));
        };
        let main = format!("skills/{name}/{SKILL_FILE}");
        let manifest = files
            .iter()
            .find(|(rel, _)| *rel == main)
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        let manifest_path = if source.is_dir() { source.join(SKILL_FILE) } else { source.to_path_buf() };
        check_skill_manifest(&manifest).map_err(|why| file_err(&manifest_path, why))?;
        self.insert_skill(agent, name, files)
    }

    pub fn remove_skill(&self, agent: &str, skill: &str) -> Result<Self> {
        let mut next = self.clone();
        let at = next.index(agent)?;
        let a = &mut next.agents[at];
        let paths = a.skill_paths(skill);
        if !a.skills().iter().any(|s| s.name == skill) {
            return Err(AgentError::UnknownSkill {
                agent: agent.to_string(),
                skill: skill.to_string(),
            });
        }
        for path in paths {
            a.files.remove(&path);
        }
        Ok(next)
    }

    /// Whether `agent` has a shipped default to reset to.
    pub fn can_reset(&self, agent: &str) -> bool {
        self.agent(agent).is_some() && bundled(agent).is_some()
    }

    /// Put `agent`'s `instructions.md`, `agent.yaml` and skills back to the
    /// shipped default. `settings.json` is left alone.
    pub fn reset_to_default(&self, agent: &str) -> Result<Self> {
        let at = self.index(agent)?;
        let shipped = bundled(agent).ok_or_else(|| AgentError::NotShipped(agent.to_string()))?;
        let manifest = shipped_manifest(agent).ok_or_else(|| {
            AgentError::Invalid(format!("the shipped `{agent}` manifest is invalid"))
        })?;
        let mut next = self.clone();
        let a = &mut next.agents[at];
        a.instructions = shipped.instructions.to_string();
        a.manifest = manifest;
        a.shipped_text = true;
        a.files.retain(|path, _| !path.starts_with("skills/"));
        for skill in shipped.skills {
            a.files.insert(
                format!("skills/{}", skill.rel_path),
                skill.content.as_bytes().to_vec(),
            );
        }
        Ok(next)
    }

    /// Replace the shared instructions (`agents/_shared/preamble.md`).
    pub fn set_shared_instructions(&self, text: &str) -> Result<Self> {
        let mut next = self.clone();
        next.shared_instructions = text.to_string();
        Ok(next)
    }

    // -----------------------------------------------------------------------
    // Saving

    /// Write every changed file and return the model as now on disk.
    /// Refuses, writing nothing, if an agent directory to be changed (or the
    /// preamble) changed on disk since the load, or a new agent's directory
    /// appeared.
    pub fn save(&self) -> Result<Saved> {
        let plan = self.plan()?;
        let mut conflicts = Vec::new();
        for op in &plan {
            match op {
                Op::Remove { dir, .. } | Op::Rename { from: dir, .. } | Op::Edit { dir, .. } => {
                    let name = file_name(dir);
                    if snapshot(dir).ok().as_ref() != Some(&self.disk.agents[&name].files) {
                        conflicts.push(dir.clone());
                    }
                }
                Op::Create { .. } | Op::Preamble { .. } => {}
            }
        }
        // A new or renamed-to directory must not exist, unless the save
        // first moves or removes what's there.
        let freed: Vec<&PathBuf> = plan
            .iter()
            .filter_map(|op| match op {
                Op::Remove { dir, .. } | Op::Rename { from: dir, .. } => Some(dir),
                _ => None,
            })
            .collect();
        for op in &plan {
            let target = match op {
                Op::Create { dir, .. } | Op::Rename { to: dir, .. } => dir,
                _ => continue,
            };
            if target.exists() && !freed.contains(&target) {
                conflicts.push(target.clone());
            }
        }
        if plan.iter().any(|op| matches!(op, Op::Preamble { .. }))
            && read_optional(&self.disk.preamble_path)? != self.disk.preamble
        {
            conflicts.push(self.disk.preamble_path.clone());
        }
        conflicts.dedup();
        if !conflicts.is_empty() {
            return Err(AgentError::Conflict(conflicts));
        }

        let mut written = Vec::new();
        let mut removed = Vec::new();
        let mut dropped_comments = Vec::new();
        // Removals, then renames through a parking name (so a swap or chain
        // works), then new directories, then edits.
        for op in &plan {
            if let Op::Remove { dir } = op {
                fs::remove_dir_all(dir).map_err(|e| file_err(dir, e))?;
                removed.push(dir.clone());
            }
        }
        let mut parked = Vec::new();
        for op in &plan {
            if let Op::Rename { from, to } = op {
                let park = self.disk.dir.join(format!(".renaming-{}", file_name(from)));
                fs::rename(from, &park).map_err(|e| file_err(from, e))?;
                parked.push((park, to.clone()));
            }
        }
        for (park, to) in parked {
            fs::rename(&park, &to).map_err(|e| file_err(&to, e))?;
        }
        for op in &plan {
            match op {
                Op::Create {
                    dir,
                    files,
                    comments,
                }
                | Op::Edit {
                    target: dir,
                    files,
                    comments,
                    ..
                } => {
                    crate::ensure_dir(&dir.join(SKILLS_DIR)).map_err(|e| file_err(dir, e))?;
                    for (rel, change) in files {
                        let path = dir.join(rel);
                        match change {
                            Some(bytes) => {
                                if let Some(parent) = path.parent() {
                                    crate::ensure_dir(parent).map_err(|e| file_err(parent, e))?;
                                }
                                crate::atomic_write(&path, bytes)
                                    .map_err(|e| file_err(&path, e))?;
                                written.push(path);
                            }
                            None => {
                                remove_file_and_empty_parents(&path, dir)?;
                                removed.push(path);
                            }
                        }
                    }
                    if !comments.is_empty() {
                        dropped_comments.push((dir.join(MANIFEST_FILE), comments.clone()));
                    }
                }
                Op::Preamble { text } => {
                    let path = &self.disk.preamble_path;
                    if let Some(parent) = path.parent() {
                        crate::ensure_dir(parent).map_err(|e| file_err(parent, e))?;
                    }
                    crate::atomic_write(path, text.as_bytes()).map_err(|e| file_err(path, e))?;
                    written.push(path.clone());
                }
                Op::Remove { .. } | Op::Rename { .. } => {}
            }
        }
        Ok(Saved {
            config: AgentsConfig::load(&self.project)?,
            written,
            removed,
            dropped_comments,
        })
    }

    /// What a save does.
    fn plan(&self) -> Result<Vec<Op>> {
        let disk = &self.disk;
        let mut plan = Vec::new();
        for name in disk.agents.keys() {
            let kept = self
                .agents
                .iter()
                .any(|a| a.origin == Origin::Dir(name.clone()));
            if !kept {
                plan.push(Op::Remove {
                    dir: disk.dir.join(name),
                });
            }
        }
        for agent in &self.agents {
            let target = disk.dir.join(&agent.name);
            match &agent.origin {
                Origin::Dir(dir) => {
                    let loaded = &disk.agents[dir];
                    let (files, comments) = self.agent_files(agent, Some(loaded))?;
                    let changes = diff(&loaded.files, &files);
                    let from = disk.dir.join(dir);
                    if *dir != agent.name {
                        plan.push(Op::Rename {
                            from: from.clone(),
                            to: target.clone(),
                        });
                    }
                    if !changes.is_empty() {
                        plan.push(Op::Edit {
                            dir: from,
                            target,
                            files: changes,
                            comments,
                        });
                    }
                }
                Origin::New => {
                    let (files, comments) = self.agent_files(agent, None)?;
                    plan.push(Op::Create {
                        dir: target,
                        files: files.into_iter().map(|(k, v)| (k, Some(v))).collect(),
                        comments,
                    });
                }
            }
        }
        if self.shared_instructions != disk.preamble.clone().unwrap_or_default() {
            plan.push(Op::Preamble {
                text: self.shared_instructions.clone(),
            });
        }
        Ok(plan)
    }

    /// Every file `agent` should have, and the comments its `agent.yaml`
    /// loses.
    fn agent_files(&self, agent: &Agent, loaded: Option<&Loaded>) -> Result<(Files, Vec<String>)> {
        let mut files = agent.files.clone();
        let path = self.agent_dir(&agent.name).join(MANIFEST_FILE);
        // The text the manifest is patched from: the shipped one after a
        // reset, else the copied one a duplicate carries in `files`, else
        // the loaded file.
        let carried = files
            .remove(MANIFEST_FILE)
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let base_text = if agent.shipped_text {
            bundled(&agent.name).map(bundled_agent_manifest_yaml)
        } else {
            carried.or_else(|| loaded.and_then(|l| l.text(MANIFEST_FILE)))
        };
        let base = match &base_text {
            Some(text) => Some(AgentManifest::from_yaml_str(text).map_err(|e| file_err(&path, e))?),
            None => None,
        };
        let mut comments = Vec::new();
        let manifest_text = match (&base_text, &base) {
            (Some(text), Some(base)) => {
                let patched = patch_manifest(text, base, &agent.manifest, &path)?;
                comments = patched.dropped_comments;
                Some(patched.text)
            }
            // No file: write one only once there's more than the name the
            // loader would synthesize.
            _ if agent.manifest == synthesized_manifest(&agent.name) && loaded.is_some() => None,
            _ => Some(serde_yaml::to_string(&agent.manifest).map_err(|e| file_err(&path, e))?),
        };
        if let Some(text) = manifest_text {
            files.insert(MANIFEST_FILE.to_string(), text.into_bytes());
        }
        let had_instructions = loaded.is_some_and(|l| l.files.contains_key(INSTRUCTIONS_FILE));
        if had_instructions || !agent.instructions.is_empty() {
            files.insert(
                INSTRUCTIONS_FILE.to_string(),
                agent.instructions.clone().into_bytes(),
            );
        }
        Ok((files, comments))
    }

    // -----------------------------------------------------------------------
    // Helpers

    fn index(&self, name: &str) -> Result<usize> {
        self.agents
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| AgentError::UnknownAgent(name.to_string()))
    }

    /// `name` is free and valid for a new or renamed agent.
    fn check_new_name(&self, name: &str) -> Result<()> {
        if shelbi_core::validate_agent_id(name).is_err() {
            return Err(AgentError::Invalid(format!(
                "`{name}` isn't a valid agent name: use lowercase letters, digits, `-` and `_`, \
                 starting with a letter or digit"
            )));
        }
        if self.agent(name).is_some() || self.unreadable.iter().any(|u| u.name == name) {
            return Err(AgentError::AgentExists(name.to_string()));
        }
        Ok(())
    }

    fn edit_manifest(
        &self,
        agent: &str,
        edit: impl FnOnce(&mut AgentManifest) -> Result<()>,
    ) -> Result<Self> {
        let mut next = self.clone();
        let at = next.index(agent)?;
        let a = &mut next.agents[at];
        edit(&mut a.manifest)?;
        a.manifest.validate().map_err(invalid)?;
        Ok(next)
    }

    fn edit_runner(
        &self,
        agent: &str,
        runner: RunnerKind,
        edit: impl FnOnce(&mut RunnerManifestConfig),
    ) -> Result<Self> {
        self.edit_manifest(agent, |m| {
            let config = m.runners.entry(runner).or_default();
            edit(config);
            // An emptied block goes, and `runners:` with it once empty.
            if *config == RunnerManifestConfig::default() {
                m.runners.remove(&runner);
            }
            Ok(())
        })
    }

    fn insert_skill(
        &self,
        agent: &str,
        name: &str,
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self> {
        let mut next = self.clone();
        let at = next.index(agent)?;
        let a = &mut next.agents[at];
        if a.skills().iter().any(|s| s.name == name) || !a.skill_paths(name).is_empty() {
            return Err(AgentError::SkillExists {
                agent: agent.to_string(),
                skill: name.to_string(),
            });
        }
        a.files.extend(files);
        Ok(next)
    }
}

/// One step of a save.
#[derive(Debug)]
enum Op {
    /// Remove an agent directory.
    Remove {
        dir: PathBuf,
    },
    /// Move an agent directory (before its edits apply).
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Create an agent directory holding `files`.
    Create {
        dir: PathBuf,
        files: BTreeMap<String, Option<Vec<u8>>>,
        comments: Vec<String>,
    },
    /// Write (`Some`) or remove (`None`) files of the agent loaded from
    /// `dir`, which lives at `target` once renames are done.
    Edit {
        dir: PathBuf,
        target: PathBuf,
        files: BTreeMap<String, Option<Vec<u8>>>,
        comments: Vec<String>,
    },
    Preamble {
        text: String,
    },
}

/// The files to write (`Some`) or remove (`None`) to turn `old` into `new`.
fn diff(old: &Files, new: &Files) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for (path, bytes) in new {
        if old.get(path) != Some(bytes) {
            out.insert(path.clone(), Some(bytes.clone()));
        }
    }
    for path in old.keys() {
        if !new.contains_key(path) {
            out.insert(path.clone(), None);
        }
    }
    out
}

/// The manifest the loader assumes for an agent with no `agent.yaml`.
fn synthesized_manifest(name: &str) -> AgentManifest {
    AgentManifest {
        name: name.to_string(),
        version: None,
        description: None,
        preferred_runner: None,
        runners: BTreeMap::new(),
        permissions_mode: None,
        requires: None,
    }
}

/// Parse an agent directory's manifest and instructions.
fn parse_agent(name: &str, files: &Files) -> Result<(AgentManifest, String), String> {
    let manifest = match files.get(MANIFEST_FILE) {
        Some(bytes) => {
            let text = std::str::from_utf8(bytes).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
            AgentManifest::from_yaml_str(text).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?
        }
        None => synthesized_manifest(name),
    };
    let instructions = match files.get(INSTRUCTIONS_FILE) {
        Some(bytes) => {
            String::from_utf8(bytes.clone()).map_err(|e| format!("{INSTRUCTIONS_FILE}: {e}"))?
        }
        None => String::new(),
    };
    Ok((manifest, instructions))
}

/// Whether `agent`'s skills are exactly the shipped ones, unedited.
fn skills_are_shipped(agent: &Agent, shipped: &BundledAgent) -> bool {
    let skills: BTreeMap<&String, &Vec<u8>> = agent
        .files
        .iter()
        .filter(|(path, _)| path.starts_with("skills/"))
        .collect();
    skills.len() == shipped.skills.len()
        && shipped.skills.iter().all(|s| {
            skills
                .get(&format!("skills/{}", s.rel_path))
                .map(|b| b.as_slice())
                == Some(s.content.as_bytes())
        })
}

/// `text` (an `agent.yaml` holding `base`) rewritten to hold `new`,
/// changing only what differs.
fn patch_manifest(
    text: &str,
    base: &AgentManifest,
    new: &AgentManifest,
    path: &Path,
) -> Result<yaml_edit::Patched> {
    let to_value = |m: &AgentManifest| serde_yaml::to_value(m).map_err(|e| file_err(path, e));
    let raw = yaml_edit::parse(text).map_err(|e| file_err(path, e))?;
    let target = yaml_edit::merge(&raw, &to_value(base)?, &to_value(new)?, &[]);
    yaml_edit::patch(text, &target).map_err(|e| file_err(path, e))
}

/// Every workflow status that names an agent, by agent. Read leniently, so a
/// workflow the loader would reject still counts.
fn load_assignments(project: &str) -> Result<BTreeMap<String, Vec<Assignment>>> {
    let statuses_path = crate::workflows::statuses_read_path(project).map_err(other)?;
    let catalog = read_optional(&statuses_path)?
        .and_then(|text| ProjectStatuses::from_yaml_str(&text).ok())
        .unwrap_or_else(shelbi_core::default_project_statuses);
    let dir = crate::workflows_dir(project).map_err(other)?;
    let mut out: BTreeMap<String, Vec<Assignment>> = BTreeMap::new();
    for (stem, path) in
        crate::workflow_config::workflow_files(&dir).map_err(|e| file_err(&dir, e))?
    {
        let Some(text) = read_optional(&path)? else {
            continue;
        };
        let Ok(value) = serde_yaml::from_str::<Value>(&text) else {
            continue;
        };
        let Some(items) = value.get("statuses").and_then(Value::as_sequence) else {
            continue;
        };
        for item in items {
            let (Some(id), Some(agent)) = (
                item.get("id")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str),
                item.get("agent").and_then(Value::as_str),
            ) else {
                continue;
            };
            let known = catalog.get(id);
            out.entry(agent.to_string()).or_default().push(Assignment {
                workflow: stem.clone(),
                status: id.to_string(),
                status_name: known.map_or_else(|| id.to_string(), |s| s.name.clone()),
                category: known.map(|s| s.category),
            });
        }
    }
    Ok(out)
}

/// What a project YAML sets over its agents' own preferences, as dispatch
/// resolves them ([`shelbi_core::resolve_agent_launch`]).
#[derive(Debug, Default)]
struct ProjectOverrides {
    /// `agents.<name>.runner`.
    pins: BTreeMap<String, RunnerKind>,
    /// `runners.<kind>.model` and `.reasoning_effort`.
    runners: BTreeMap<RunnerKind, (Option<String>, Option<ReasoningEffort>)>,
    /// `workspace_permissions_mode`.
    permissions_ceiling: Option<String>,
}

/// The project YAML's overrides, read leniently: a value that doesn't
/// parse is left out (dispatch would refuse the file anyway).
fn load_overrides(project: &str) -> ProjectOverrides {
    let mut out = ProjectOverrides::default();
    let Ok(path) = crate::workflow_config::project_config_file(project) else {
        return out;
    };
    let Ok(Some(text)) = read_optional(&path) else {
        return out;
    };
    let Ok(value) = serde_yaml::from_str::<Value>(&text) else {
        return out;
    };
    fn parse<T: serde::de::DeserializeOwned>(v: Option<&Value>) -> Option<T> {
        v.and_then(|v| serde_yaml::from_value(v.clone()).ok())
    }
    if let Some(agents) = value.get("agents").and_then(Value::as_mapping) {
        for (name, config) in agents {
            if let (Some(name), Some(kind)) = (name.as_str(), parse(config.get("runner"))) {
                out.pins.insert(name.to_string(), kind);
            }
        }
    }
    if let Some(runners) = value.get("runners").and_then(Value::as_mapping) {
        for (kind, config) in runners {
            let Some(kind) = parse(Some(kind)) else {
                continue;
            };
            let model = config.get("model").and_then(Value::as_str).map(str::to_string);
            let effort = parse(config.get("reasoning_effort"));
            if model.is_some() || effort.is_some() {
                out.runners.insert(kind, (model, effort));
            }
        }
    }
    out.permissions_ceiling = value
        .get("workspace_permissions_mode")
        .and_then(Value::as_str)
        .map(str::to_string);
    out
}

/// The agents the project YAML pins a runner for (`agents.<name>:`).
fn load_pins(project: &str) -> Vec<String> {
    let Ok(path) = crate::workflow_config::project_config_file(project) else {
        return Vec::new();
    };
    let Ok(Some(text)) = read_optional(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_yaml::from_str::<Value>(&text) else {
        return Vec::new();
    };
    value
        .get("agents")
        .and_then(Value::as_mapping)
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Every file under `dir`, by `/`-separated relative path, reading through
/// symlinks. Empty when `dir` doesn't exist.
fn snapshot(dir: &Path) -> Result<Files> {
    let mut out = BTreeMap::new();
    if dir.is_dir() {
        walk(dir, "", 0, &mut out)?;
    }
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Files) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(file_err(dir, "is nested too deeply (a symlink loop?)"));
    }
    for entry in fs::read_dir(dir).map_err(|e| file_err(dir, e))? {
        let entry = entry.map_err(|e| file_err(dir, e))?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let rel = format!("{prefix}{name}");
        // A dangling symlink has nothing to read; skip it like the mount
        // would fail to.
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, &format!("{rel}/"), depth + 1, out)?;
        } else {
            out.insert(rel, fs::read(&path).map_err(|e| file_err(&path, e))?);
        }
    }
    Ok(())
}

/// Remove `path`, then any directories it leaves empty, up to `root`.
fn remove_file_and_empty_parents(path: &Path, root: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(file_err(path, e)),
    }
    let skills = root.join(SKILLS_DIR);
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == root || d == skills || !d.starts_with(root) {
            break;
        }
        if fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// A skill name the runner accepts: lowercase letters, digits and `-`,
/// starting with a letter or digit, at most 64 characters.
fn validate_skill_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(AgentError::Invalid(format!(
            "`{name}` isn't a valid skill name: use lowercase letters, digits and `-` \
             (at most 64), starting with a letter or digit"
        )))
    }
}

/// The `description:` in a skill file's YAML frontmatter.
fn skill_description(text: &str) -> Option<String> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let close = rest.find("\n---")?;
    let value: Value = serde_yaml::from_str(&rest[..close]).ok()?;
    Some(value.get("description")?.as_str()?.trim().to_string())
}

/// Why a skill file's frontmatter won't do, if it won't: the runner reads
/// its `description:` to decide when to load the skill.
fn check_skill_manifest(text: &str) -> std::result::Result<(), String> {
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Err("has no frontmatter: it must open with `---`, `description: …`, `---`".into());
    };
    let Some(close) = rest.find("\n---") else {
        return Err("has no closing `---` after its frontmatter".into());
    };
    let value: Value = serde_yaml::from_str(&rest[..close])
        .map_err(|e| format!("has frontmatter that isn't valid YAML: {e}"))?;
    match value.get("description").and_then(Value::as_str) {
        Some(d) if !d.trim().is_empty() => Ok(()),
        _ => Err("has no `description:` in its frontmatter: the runner uses it to decide when to load the skill".into()),
    }
}

fn file_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(file_err(path, e)),
    }
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn invalid(e: shelbi_core::Error) -> AgentError {
    AgentError::Invalid(match e {
        shelbi_core::Error::Other(m) => m,
        e => e.to_string(),
    })
}

fn other(e: shelbi_core::Error) -> AgentError {
    AgentError::Invalid(e.to_string())
}

fn file_err(path: &Path, e: impl std::fmt::Display) -> AgentError {
    AgentError::File {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests;
