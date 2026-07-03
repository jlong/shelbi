//! Onboarding wizard.
//!
//! Not a TUI — just a sequence of `inquire` prompts (single-select with
//! arrow keys, free text, y/N). Each phase is idempotent: re-running the
//! wizard skips any phase whose answer is already on disk.
//!
//! Library choice is locked to `inquire`: it shares crossterm with
//! ratatui, has chainable validators, and the `Select` prompt has
//! built-in type-to-filter (used by later phases for the project picker).
//! Do not pull in `dialoguer` alongside.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use inquire::{Confirm, Select, Text};
use shelbi_core::{
    format_bytes_short, recommended_workspace_count, total_memory_bytes, validate_agent_id,
    AgentRunnerSpec, Machine, MachineKind, OrchestratorSpec, Project, WorkspaceNamePreset, WorkspaceSpec,
};

/// Half-block ASCII banner for the shelbi brand. Reproduced verbatim at
/// the top of `README.md` — keep both copies identical so the banner is
/// the same wherever the project surfaces. Written with `concat!` and
/// explicit `\n` because Rust's `\<newline>` continuation elides the
/// leading whitespace of the next line, which would erase the 3-space
/// indent on line 1.
pub const BANNER: &str = concat!(
    "   ▄▀▀▀▀▀▄   ▀▀    ▀▀  ▀▀▀▀▀▀▀   ▀▀   ▀▀▀▀▀▀▀▀▀▀▄   ▀▀▀▀▀\n",
    "  ▀▀        ▀▀    ▀▀  ▀▀        ▀▀        ▀▀    ▀▀   ▀▀\n",
    "  ▀▀▀▀▄    ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀    ▀▀      ▀▀▀▀▀▀▀▀▄    ▀▀\n",
    "▄     ▀▀  ▀▀    ▀▀  ▀▀        ▀▀        ▀▀     ▀▀  ▀▀\n",
    " ▀▀▀▀▀▀  ▀▀    ▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀",
);

/// One-line tagline printed under the banner on first-run.
pub const TAGLINE: &str = "an open-source agent orchestrator for the terminal";

/// Print the banner + tagline + a trailing blank line. Called once at the
/// very top of a first-run wizard; never inside `shelbi project add` or
/// re-entries into `shelbi wizard`.
pub fn print_banner() {
    println!("{BANNER}");
    println!("{TAGLINE}");
    println!();
}

/// Single-select prompt with arrow-key navigation. `options` must be
/// non-empty.
pub fn select<T: Display>(label: &str, options: Vec<T>) -> Result<T> {
    Select::new(label, options)
        .prompt()
        .with_context(|| format!("select prompt `{label}`"))
}

/// Free-text prompt with a default that the user can accept by pressing
/// Enter on an empty line.
pub fn text(label: &str, default: &str) -> Result<String> {
    Text::new(label)
        .with_default(default)
        .prompt()
        .with_context(|| format!("text prompt `{label}`"))
}

/// Free-text prompt with no default — the user must type something.
fn text_required(label: &str) -> Result<String> {
    Text::new(label)
        .prompt()
        .with_context(|| format!("text prompt `{label}`"))
}

/// y/N (or Y/n) prompt. `default` selects which case is upper-cased in
/// the rendered hint.
pub fn confirm(label: &str, default: bool) -> Result<bool> {
    Confirm::new(label)
        .with_default(default)
        .prompt()
        .with_context(|| format!("confirm prompt `{label}`"))
}

/// Project setup: interactively set up one or more projects. Idempotent —
/// skipped entirely if the user already has at least one project on
/// disk. The `shelbi project add` command (separate task) reuses
/// [`setup_one_project`] without the idempotence guard.
pub fn phase_project_setup() -> Result<()> {
    let projects = shelbi_state::list_projects().map_err(|e| anyhow!(e))?;
    if !projects.is_empty() {
        return Ok(());
    }
    loop {
        setup_one_project()?;
        if !confirm("Set up another project?", false)? {
            break;
        }
    }
    Ok(())
}

/// Walk the full project-setup prompt sequence and write the resulting
/// `~/.shelbi/projects/<name>.yaml`. Exposed for `shelbi project add`.
pub fn setup_one_project() -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let git = GitDefaults::probe(&cwd);

    // ---- Project basics --------------------------------------------------
    let default_name = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("my-project")
        .to_string();
    let name = prompt_project_name(&default_name)?;

    let repo_path = text("Path to the repo:", &cwd.display().to_string())?;
    let repo_path = repo_path.trim().to_string();

    let default_branch = text("Default branch:", git.default_branch.as_deref().unwrap_or("main"))?;
    let default_branch = default_branch.trim().to_string();

    let github_url_raw = text(
        "GitHub repo URL (optional):",
        git.remote_url.as_deref().unwrap_or(""),
    )?;
    let github_url = {
        let trimmed = github_url_raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    // ---- Hub machine -----------------------------------------------------
    let hub_name = text("Hub machine name:", "hub")?;
    let hub_name = hub_name.trim().to_string();
    let hub_work_dir = text("Hub work directory:", &repo_path)?;
    let hub_work_dir = hub_work_dir.trim().to_string();

    let mut machines = vec![Machine {
        name: hub_name,
        kind: MachineKind::Local,
        work_dir: PathBuf::from(&hub_work_dir),
        host: None,
    }];

    // ---- Remote machine loop --------------------------------------------
    let mut prompt_label = "Add a remote machine?";
    while confirm(prompt_label, false)? {
        let m_name = text_required("  Remote machine name:")?;
        let m_name = m_name.trim().to_string();
        let m_host = text("  SSH host:", &m_name)?;
        let m_host = m_host.trim().to_string();
        let m_work_dir = text("  Work directory on remote:", &repo_path)?;
        let m_work_dir = m_work_dir.trim().to_string();
        machines.push(Machine {
            name: m_name,
            kind: MachineKind::Ssh,
            work_dir: PathBuf::from(m_work_dir),
            host: Some(m_host),
        });
        prompt_label = "Add another remote machine?";
    }

    // ---- Agent runner ---------------------------------------------------
    let agent_runner = select(
        "Agent runner (used by every workspace):",
        vec![Runner::Claude, Runner::Codex],
    )?;

    // ---- Workspace count ---------------------------------------------------
    let machine_count = machines.len() as u32;
    let (recommended, recommendation_hint) = workspace_count_recommendation(machine_count);
    if let Some(hint) = recommendation_hint {
        println!("  ({hint} — configurable later)");
    }
    let count = prompt_workspace_count(recommended)?;

    // ---- Workspace naming preset -------------------------------------------
    let preset = select(
        "Workspace naming style:",
        vec![
            PresetChoice(WorkspaceNamePreset::Phonetic),
            PresetChoice(WorkspaceNamePreset::Greek),
            PresetChoice(WorkspaceNamePreset::ToyStory),
        ],
    )?
    .0;

    // ---- Orchestrator runner --------------------------------------------
    let orch_default = match agent_runner {
        Runner::Claude => 0,
        Runner::Codex => 1,
    };
    let orch_runner = Select::new(
        "Orchestrator runner:",
        vec![Runner::Claude, Runner::Codex],
    )
    .with_starting_cursor(orch_default)
    .prompt()
    .context("orchestrator runner select")?;

    // ---- Assemble workspaces ----------------------------------------------
    let workspaces = assign_workspace_names(&machines, count, preset)?;

    // ---- Assemble Project struct ---------------------------------------
    let mut agent_runners = BTreeMap::new();
    agent_runners.insert(
        Runner::Claude.id().to_string(),
        AgentRunnerSpec {
            command: Runner::Claude.id().to_string(),
            flags: vec![],
            dialog_signatures: vec![],
        },
    );
    agent_runners.insert(
        Runner::Codex.id().to_string(),
        AgentRunnerSpec {
            command: Runner::Codex.id().to_string(),
            flags: vec![],
            dialog_signatures: vec![],
        },
    );

    let project = Project {
        name: name.clone(),
        repo: repo_path.clone(),
        default_branch,
        config_mode: None,
        machines,
        orchestrator: OrchestratorSpec {
            runner: orch_runner.id().to_string(),
        },
        agent_runners,
        editor: None,
        github_url,
        workspaces,
        workspace_poll_interval_secs: 5,
        workspace_permissions_mode: "auto".into(),
        workspace_settings_template: None,
        zen: shelbi_core::ZenConfig::default(),
        heartbeat: shelbi_core::HeartbeatConfig::default(),
        git: shelbi_core::GitConfig::default(),
        review: Default::default(),
        contextstore_sync: Vec::new(),
        detected_shapes: Vec::new(),
    };
    project.validate_workspaces().map_err(|e| anyhow!(e))?;

    // ---- Persist --------------------------------------------------------
    shelbi_state::save_project(&project).map_err(|e| anyhow!(e))?;

    write_workspace_settings_template(&name)?;
    let _ = shelbi_state::materialize_default_agents(&name)
        .map_err(|e| anyhow!(e))?;

    // ---- Done -----------------------------------------------------------
    let names_csv = project
        .workspaces
        .iter()
        .map(|w| w.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "✓ Project {} created ({} workspaces: {}).",
        name,
        project.workspaces.len(),
        names_csv
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runner {
    Claude,
    Codex,
}

impl Runner {
    fn id(self) -> &'static str {
        match self {
            Runner::Claude => "claude",
            Runner::Codex => "codex",
        }
    }
}

impl Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// Newtype that gives [`WorkspaceNamePreset`] a Select-friendly label
/// without leaking display formatting into the core crate.
struct PresetChoice(WorkspaceNamePreset);

impl Display for PresetChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.label())
    }
}

/// Auto-fillable defaults read from `cwd`'s git checkout. Probing errors
/// are swallowed — every field falls back to a hard-coded default.
struct GitDefaults {
    default_branch: Option<String>,
    remote_url: Option<String>,
}

impl GitDefaults {
    fn probe(cwd: &Path) -> Self {
        let inside_git = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(cwd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !inside_git {
            return Self {
                default_branch: None,
                remote_url: None,
            };
        }
        Self {
            default_branch: probe_default_branch(cwd),
            remote_url: probe_remote_url(cwd),
        }
    }
}

fn probe_default_branch(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Expected form: `refs/remotes/origin/main`. Strip everything up to
    // and including the last `/`.
    s.rsplit('/').next().map(|s| s.to_string())
}

fn probe_remote_url(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn prompt_project_name(default: &str) -> Result<String> {
    use inquire::validator::{ErrorMessage, Validation};
    let validator = |s: &str| -> std::result::Result<Validation, inquire::CustomUserError> {
        if validate_agent_id(s.trim()).is_ok() {
            Ok(Validation::Valid)
        } else {
            Ok(Validation::Invalid(ErrorMessage::Custom(
                "project name must be kebab/snake-case alphanumeric (e.g. `my-app`)".into(),
            )))
        }
    };
    let raw = Text::new("Project name:")
        .with_default(default)
        .with_validator(validator)
        .prompt()
        .context("project name prompt")?;
    Ok(raw.trim().to_string())
}

/// Returns `(recommended_count, optional_hint_to_print_above_prompt)`.
/// Hint is `None` when the platform doesn't expose total memory (so the
/// user gets to pick blind with `1` as the default — same as the
/// system_memory module's clamp floor).
fn workspace_count_recommendation(machine_count: u32) -> (u32, Option<String>) {
    match total_memory_bytes() {
        Ok(bytes) => {
            let count = recommended_workspace_count(bytes, machine_count);
            let hint = format!(
                "memory: {} → recommended {} workspace{} per machine",
                format_bytes_short(bytes),
                count,
                if count == 1 { "" } else { "s" }
            );
            (count, Some(hint))
        }
        Err(_) => (1, None),
    }
}

fn prompt_workspace_count(default: u32) -> Result<u32> {
    loop {
        let raw = text("Workspace count per machine:", &default.to_string())?;
        match raw.trim().parse::<u32>() {
            Ok(n) if n >= 1 => return Ok(n),
            _ => println!("  (expected a positive integer)"),
        }
    }
}

/// Build the per-machine workspace list. Names are taken from `preset` and
/// laid out machine-by-machine, so the first `count` names go to the
/// first machine, the next `count` to the second, etc. Falls back to
/// `<machine>-<index>` once the preset is exhausted.
fn assign_workspace_names(
    machines: &[Machine],
    count: u32,
    preset: WorkspaceNamePreset,
) -> Result<Vec<WorkspaceSpec>> {
    let preset_names = preset.names();
    let total = (count as usize) * machines.len();
    let mut workspaces = Vec::with_capacity(total);
    let mut cursor = 0usize;
    for machine in machines {
        for slot in 0..count {
            let name = if cursor < preset_names.len() {
                let n = preset_names[cursor].to_string();
                cursor += 1;
                n
            } else {
                format!("{}-{}", machine.name, slot + 1)
            };
            workspaces.push(WorkspaceSpec {
                name,
                machine: machine.name.clone(),
                runner: Runner::Claude.id().to_string(),
                role: Default::default(),
            });
        }
    }
    Ok(workspaces)
}

/// Drop the per-project workspace-settings template at
/// `~/.shelbi/projects/<name>/workspace-settings.json` so the workspace deploy
/// step has something to render. Mirrors `shelbi init --project`.
fn write_workspace_settings_template(project: &str) -> Result<()> {
    let path = shelbi_state::project_dir(project)
        .map_err(|e| anyhow!(e))?
        .join("workspace-settings.json");
    if path.exists() {
        return Ok(());
    }
    shelbi_state::ensure_dir(path.parent().unwrap()).map_err(|e| anyhow!(e))?;
    std::fs::write(&path, shelbi_state::DEFAULT_WORKSPACE_SETTINGS_TEMPLATE)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_workspace_names_uses_preset_in_order() {
        let machines = vec![Machine {
            name: "hub".into(),
            kind: MachineKind::Local,
            work_dir: "/tmp".into(),
            host: None,
        }];
        let workspaces = assign_workspace_names(&machines, 3, WorkspaceNamePreset::Phonetic).unwrap();
        let names: Vec<_> = workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn assign_workspace_names_spreads_across_machines() {
        let machines = vec![
            Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp".into(),
                host: None,
            },
            Machine {
                name: "remote".into(),
                kind: MachineKind::Ssh,
                work_dir: "/tmp".into(),
                host: Some("remote".into()),
            },
        ];
        let workspaces = assign_workspace_names(&machines, 2, WorkspaceNamePreset::Phonetic).unwrap();
        assert_eq!(workspaces.len(), 4);
        assert_eq!(workspaces[0].name, "alpha");
        assert_eq!(workspaces[0].machine, "hub");
        assert_eq!(workspaces[1].name, "bravo");
        assert_eq!(workspaces[1].machine, "hub");
        assert_eq!(workspaces[2].name, "charlie");
        assert_eq!(workspaces[2].machine, "remote");
        assert_eq!(workspaces[3].name, "delta");
        assert_eq!(workspaces[3].machine, "remote");
    }

    #[test]
    fn assign_workspace_names_falls_back_when_preset_exhausted() {
        let machines = vec![Machine {
            name: "hub".into(),
            kind: MachineKind::Local,
            work_dir: "/tmp".into(),
            host: None,
        }];
        // Toy Story has 20 names; ask for 22.
        let workspaces = assign_workspace_names(&machines, 22, WorkspaceNamePreset::ToyStory).unwrap();
        assert_eq!(workspaces.len(), 22);
        // First 20 come from the preset; tail falls back to <machine>-<n>.
        assert_eq!(workspaces[0].name, "woody");
        assert_eq!(workspaces[20].name, "hub-21");
        assert_eq!(workspaces[21].name, "hub-22");
    }

    #[test]
    fn workspace_count_recommendation_returns_hint_when_memory_known() {
        // Either branch is acceptable — the test just pins the shape of
        // the contract so callers can rely on it.
        let (count, hint) = workspace_count_recommendation(1);
        assert!(count >= 1);
        if let Some(h) = hint {
            assert!(h.contains("recommended"));
        }
    }
}
