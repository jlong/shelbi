use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args as ClapArgs, ValueEnum};
use inquire::Select;
use shelbi_state::AgentMaterializeOutcome;

use crate::project_root::{
    project_name_collides, resolve_root_for_init, validate_root, ResolvedProjectRoot,
    RootValidation,
};

pub mod heuristic;

use heuristic::{probe_team_signals, recommend_mode};

/// Filename of the committed, in-repo project config. Written under
/// `<repo>/.shelbi/project.yaml` when the user picks `--mode in-repo`;
/// read by `--pick-up` to register a teammate's already-committed
/// project into the local registry.
///
/// The current committed shape is intentionally minimal (`name: <name>`)
/// — that's the one field pick-up needs and the one field that has to
/// stay stable while the shared/local split evolves. Future phases can
/// grow the committed keys without breaking pick-up because the loader
/// is name-anchored.
pub const IN_REPO_CONFIG_REL: &str = ".shelbi/project.yaml";

/// Where the project config should live. Chosen once at `shelbi init`
/// time and rendered into the on-disk layout the loader reads. In
/// interactive mode the picker offers both with a heuristic-driven
/// prefill; in non-interactive mode this flag is required (no silent
/// default — the wrong choice is destructive enough that we won't guess).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InitMode {
    /// Committed to the repo at `<repo>/.shelbi/project.yaml`. Shared
    /// with everyone who clones — appropriate for teams.
    #[value(name = "in-repo")]
    InRepo,
    /// Per-user under `~/.shelbi/projects/<name>.yaml`. Not committed.
    /// Appropriate for solo or scratch work.
    #[value(name = "global")]
    Global,
}

impl InitMode {
    fn short_label(self) -> &'static str {
        match self {
            InitMode::InRepo => "in-repo",
            InitMode::Global => "global",
        }
    }
}

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Override the project name. Defaults to the basename of the
    /// project root (current directory or `--root`).
    #[arg(long)]
    pub project: Option<String>,

    /// Project root directory — the repo `shelbi` will manage. Skips
    /// the interactive "Project root?" prompt. Required when stdin
    /// is not a TTY (CI, piped input).
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Where the project config should live: `in-repo` writes a
    /// committed `<repo>/.shelbi/project.yaml` shared with the team;
    /// `global` keeps everything under `~/.shelbi/projects/`. In
    /// interactive contexts this may be omitted — the wizard asks and
    /// prefills a recommendation. In non-interactive contexts this
    /// flag is REQUIRED: shelbi refuses to silently pick a mode for
    /// scripted callers.
    #[arg(long, value_enum)]
    pub mode: Option<InitMode>,

    /// Register an existing `<repo>/.shelbi/project.yaml` into the
    /// local registry — the flow for cloning a teammate's shelbi
    /// project. Skips mode selection (the mode is already `in-repo`
    /// by virtue of the committed file existing) and prompts only for
    /// the user-local fields. If the canonical name collides with an
    /// existing local project, the local alias is auto-suffixed
    /// (`-2`, `-3`, …) — the committed YAML is never touched.
    #[arg(long)]
    pub pick_up: bool,
}

pub fn run(args: Args) -> Result<()> {
    if args.pick_up {
        let outcome = run_pick_up(args)?;
        println!();
        println!("next:");
        println!(
            "  1. add machines/workspaces to ~/.shelbi/projects/{}.yaml if needed",
            outcome.local_alias
        );
        if outcome.suffixed_from.is_some() {
            println!(
                "  2. pass `-p {}` on the command line to target this project",
                outcome.local_alias
            );
            println!(
                "     (the committed name `{}` was already taken locally — the alias only \
                 affects your machine)",
                outcome.canonical_name
            );
        } else {
            println!("  2. spawn your first agent: shelbi spawn TASK --on hub --runner claude \"…\"");
        }
        return Ok(());
    }

    let resolved = scaffold_with_prompt(args)?;
    println!();
    println!("next:");
    println!(
        "  1. add machines to ~/.shelbi/projects/{}.yaml if you have remote hubs",
        resolved.name
    );
    println!("  2. add the project to ~/.shelbi/sessions/default.yaml's projects: list");
    println!("  3. spawn your first agent: shelbi spawn TASK --on hub --runner claude \"…\"");
    Ok(())
}

/// `shelbi init` entry point factored so the no-subcommand first-run
/// path can share the same scaffolding without printing the trailing
/// `next:` block (that path is about to launch the TUI, so the hints
/// would just scroll off-screen).
///
/// Resolves the project root (prompting interactively, or honoring
/// `--root` when supplied), asks the mode question (or reads it from
/// `--mode`), then writes the project YAML, workspace-settings template,
/// default agent workspaces, and the project-wide statuses catalogue.
/// When mode is `InRepo`, also drops a committed `<repo>/.shelbi/project.yaml`
/// carrying the canonical name. No `.shelbi/project` marker is written
/// — resolution reverse-looks-up the directory against the registered
/// project YAMLs (see [`shelbi_state::resolve_project_for_cwd`]).
pub fn scaffold_with_prompt(args: Args) -> Result<ResolvedProjectRoot> {
    // Hard-fail with a clear, source-tagged error if the shelbi root is
    // unwritable; otherwise materialize the standard layout
    // (projects/, sessions/, agents/, logs/, workspaces/).
    let home = shelbi_state::ensure_root_subdirs().map_err(|e| anyhow!(e))?;

    let sessions_dir = shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?;

    let default_session = sessions_dir.join("default.yaml");
    if !default_session.exists() {
        std::fs::write(
            &default_session,
            "name: default\nprojects: []\nstartup: []\n",
        )?;
    }

    println!("✓ scaffolded {}", home.display());

    let interactive = std::io::stdin().is_terminal() && args.root.is_none();
    if interactive {
        println!();
        println!("shelbi setup — let's get your project configured.");
        println!();
    }

    let cwd = std::env::current_dir()?;
    let resolved = resolve_root_for_init(&cwd, args.root.clone(), args.project.as_deref())?;

    // Half-set-up detection: a bare `shelbi init` (no --mode, no --pick-up)
    // that lands on a repo already carrying a committed
    // `<repo>/.shelbi/project.yaml` but no matching local registry entry
    // is the pick-up scenario in disguise. Surface it and bail rather
    // than silently scaffolding a second (conflicting) local project on
    // top of the committed one.
    let in_repo_config = resolved.path.join(IN_REPO_CONFIG_REL);
    if in_repo_config.is_file() && args.mode.is_none() {
        if let Some(committed_name) = read_in_repo_name(&in_repo_config)? {
            // Only nag when there's no local mirror yet — a re-run of
            // `shelbi init` inside a project where both files exist is
            // handled by the collision check below.
            if !project_name_collides(&committed_name)? {
                bail!(
                    "found {} (committed) but no local registry entry for `{committed_name}` — \
                     this repo looks like a teammate's shelbi project. Run \
                     `shelbi init --pick-up` to register it locally.",
                    in_repo_config.display()
                );
            }
        }
    }

    let mode = resolve_mode(args.mode, interactive, &resolved.path)?;

    scaffold_project(&resolved, mode)?;
    Ok(resolved)
}

/// Decide the [`InitMode`] using the precedence documented on the flag:
/// `--mode` wins when set; interactive mode falls back to the picker
/// (prefilled from the heuristic); non-interactive without `--mode` is
/// a hard error so scripts can't silently pick the wrong side.
fn resolve_mode(
    from_flag: Option<InitMode>,
    interactive: bool,
    root: &Path,
) -> Result<InitMode> {
    if let Some(m) = from_flag {
        return Ok(m);
    }
    if !interactive {
        bail!(
            "shelbi init: pass `--mode in-repo` or `--mode global` — non-interactive callers \
             must choose explicitly (no silent default)."
        );
    }
    prompt_mode(root)
}

/// Interactive mode picker. Prefills the selection cursor with the
/// heuristic's recommendation so a team repo lands on `in-repo` by
/// default (but the user still confirms).
fn prompt_mode(root: &Path) -> Result<InitMode> {
    let signals = probe_team_signals(root);
    let recommended = recommend_mode(&signals);
    let recommendation_hint = match recommended {
        InitMode::InRepo => {
            "  (recommended: in-repo — multiple committers and a remote origin were detected)"
        }
        InitMode::Global => {
            "  (recommended: global — this looks like a solo checkout: no remote or single committer)"
        }
    };
    println!();
    println!("Where should this project's shelbi config live?");
    println!(
        "  in-repo — committed at <repo>/.shelbi/project.yaml, shared with the team."
    );
    println!(
        "  global  — per-user under ~/.shelbi/projects/<name>.yaml, not committed."
    );
    println!("{recommendation_hint}");

    let options = vec![
        ModeChoice(InitMode::InRepo),
        ModeChoice(InitMode::Global),
    ];
    let starting_cursor = match recommended {
        InitMode::InRepo => 0,
        InitMode::Global => 1,
    };
    let picked = Select::new("Config location:", options)
        .with_starting_cursor(starting_cursor)
        .prompt()
        .context("mode prompt")?;
    Ok(picked.0)
}

struct ModeChoice(InitMode);

impl std::fmt::Display for ModeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.short_label())
    }
}

/// Write the project YAML, the workspace-settings template, materialize
/// the default agents, and write the project-wide statuses catalogue.
/// Deliberately does **not** drop a `.shelbi/project` marker: the project
/// tree stays clean and resolution reads the registered YAMLs instead.
///
/// The collision check in [`resolve_root_for_init`] guarantees the YAML
/// path is free at the time we're called. We still guard the write
/// with `exists()` so a race against a concurrent `shelbi init` doesn't
/// blow away another invocation's freshly-written YAML.
///
/// When `mode == InRepo`, also writes a minimal committed
/// `<repo>/.shelbi/project.yaml` carrying just the canonical name.
/// That's the file `shelbi init --pick-up` reads on a teammate's clone.
fn scaffold_project(resolved: &ResolvedProjectRoot, mode: InitMode) -> Result<()> {
    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    let yaml_path = projects_dir.join(format!("{}.yaml", resolved.name));

    if yaml_path.exists() {
        println!("(project YAML already exists at {})", yaml_path.display());
        return Ok(());
    }

    let yaml = format!(
        "name: {name}\n\
         repo: \n\
         default_branch: main\n\
         machines:\n\
         \x20\x20- name: hub\n\
         \x20\x20\x20\x20kind: local\n\
         \x20\x20\x20\x20work_dir: {root}\n\
         orchestrator:\n\
         \x20\x20runner: claude\n\
         agent_runners:\n\
         \x20\x20claude: {{ command: claude, flags: [] }}\n\
         \x20\x20codex:  {{ command: codex,  flags: [] }}\n",
        name = resolved.name,
        root = resolved.path.display(),
    );
    std::fs::write(&yaml_path, yaml)?;
    println!("✓ wrote project: {}", yaml_path.display());

    if mode == InitMode::InRepo {
        write_in_repo_config(&resolved.path, &resolved.name)?;
    }

    write_workspace_settings_template(&resolved.name)?;

    let outcomes = shelbi_state::materialize_default_agents(&resolved.name)
        .map_err(|e| anyhow!(e))?;
    for outcome in outcomes {
        print_agent_materialize_outcome(&outcome);
    }

    // Materialize `workflows/statuses.yml` so a fresh project ships with
    // the project-wide status catalogue alongside its starter
    // `default.yaml`. `load_project` runs the same migration when the
    // project is opened, but writing it here keeps `shelbi init`'s
    // post-condition self-contained.
    let statuses_path =
        shelbi_state::statuses_path(&resolved.name).map_err(|e| anyhow!(e))?;
    if !statuses_path.exists() {
        shelbi_state::save_project_statuses(
            &resolved.name,
            &shelbi_core::default_project_statuses(),
        )
        .map_err(|e| anyhow!(e))?;
        println!("✓ wrote project statuses: {}", statuses_path.display());
    }
    Ok(())
}

/// Write `<repo>/.shelbi/project.yaml` carrying the canonical project
/// name. Idempotent — a pre-existing file is left alone (a previous
/// run, or a teammate committed it).
///
/// The shape stays minimal on purpose: the *committed* config is a
/// contract with every future clone of the repo, so the shared surface
/// is the one field that has to be stable while the rest of the config
/// schema evolves.
fn write_in_repo_config(root: &Path, name: &str) -> Result<()> {
    let dir = root.join(".shelbi");
    let path = dir.join("project.yaml");
    if path.exists() {
        println!("(in-repo config already exists at {})", path.display());
        return Ok(());
    }
    shelbi_state::ensure_dir(&dir).map_err(|e| anyhow!(e))?;
    std::fs::write(&path, format!("name: {name}\n"))?;
    println!("✓ wrote in-repo config: {}", path.display());
    Ok(())
}

/// Best-effort read of the canonical `name:` from a committed
/// `<repo>/.shelbi/project.yaml`. Returns `Ok(None)` when the file
/// isn't a YAML map with a `name` key — we don't want a malformed
/// commit to abort `shelbi init` on every clone.
fn read_in_repo_name(path: &Path) -> Result<Option<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    #[derive(serde::Deserialize)]
    struct Head {
        name: Option<String>,
    }
    let head: Head = match serde_yaml::from_str(&text) {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    Ok(head.name.and_then(|n| {
        let t = n.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    }))
}

/// `shelbi init --pick-up`: register a teammate's committed
/// `<repo>/.shelbi/project.yaml` into the local registry.
///
/// The scoping rule: nothing about the committed file changes; only
/// the local `~/.shelbi/projects/<alias>.yaml` is written. If the
/// canonical name already exists locally (a common case when two
/// teammates share the same project name from different repos), the
/// local alias is auto-suffixed — the user picks the alias out of the
/// success message and passes it to `shelbi -p`.
fn run_pick_up(args: Args) -> Result<PickUpOutcome> {
    // Same root-resolution flow as fresh init — TTY-gated for the
    // prompt-based branch, honors `--root` when scripted. `--project`
    // still works: it lets the user override the local alias even
    // before the auto-suffix kicks in.
    let home = shelbi_state::ensure_root_subdirs().map_err(|e| anyhow!(e))?;
    let sessions_dir = shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?;
    let default_session = sessions_dir.join("default.yaml");
    if !default_session.exists() {
        std::fs::write(
            &default_session,
            "name: default\nprojects: []\nstartup: []\n",
        )?;
    }
    println!("✓ scaffolded {}", home.display());

    let cwd = std::env::current_dir()?;
    let repo_root = resolve_repo_root_for_pick_up(&cwd, args.root.as_deref())?;

    let config_path = repo_root.join(IN_REPO_CONFIG_REL);
    if !config_path.is_file() {
        bail!(
            "no committed config at {} — expected `<repo>/{}`. Did you mean plain \
             `shelbi init` for a fresh project?",
            config_path.display(),
            IN_REPO_CONFIG_REL
        );
    }
    let canonical_name = read_in_repo_name(&config_path)?.ok_or_else(|| {
        anyhow!(
            "the committed {} is missing a `name:` key — refusing to pick up an unnamed project. \
             Ask a teammate to fix the committed file, or scaffold a fresh one with `shelbi init`.",
            config_path.display()
        )
    })?;

    // Honor `--project` as an explicit override (the user picking their
    // own local alias up front); otherwise walk the collision ladder
    // starting from the canonical name.
    let (local_alias, suffixed_from) = if let Some(override_name) = args.project.clone() {
        if project_name_collides(&override_name)? {
            bail!(
                "a shelbi project named `{override_name}` already exists locally — pick a \
                 different `--project` name or omit the flag to let shelbi auto-suffix from \
                 `{canonical_name}`."
            );
        }
        (override_name, None)
    } else {
        next_available_alias(&canonical_name)?
    };

    if let Some(from) = &suffixed_from {
        println!(
            "note: local alias `{}` was already taken — using `{}` on this machine instead \
             (the committed name is unchanged)",
            from, local_alias
        );
    }

    // Write the local registry entry. Repo/work_dir default to the
    // repo root; the user can add remote machines and workspaces after
    // the fact. Same body shape as fresh init so the loader treats
    // both alike.
    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    let yaml_path = projects_dir.join(format!("{}.yaml", local_alias));
    let yaml = format!(
        "name: {name}\n\
         repo: {root}\n\
         default_branch: main\n\
         machines:\n\
         \x20\x20- name: hub\n\
         \x20\x20\x20\x20kind: local\n\
         \x20\x20\x20\x20work_dir: {root}\n\
         orchestrator:\n\
         \x20\x20runner: claude\n\
         agent_runners:\n\
         \x20\x20claude: {{ command: claude, flags: [] }}\n\
         \x20\x20codex:  {{ command: codex,  flags: [] }}\n",
        name = local_alias,
        root = repo_root.display(),
    );
    std::fs::write(&yaml_path, yaml)?;
    println!("✓ registered project: {}", yaml_path.display());

    write_workspace_settings_template(&local_alias)?;
    let outcomes = shelbi_state::materialize_default_agents(&local_alias)
        .map_err(|e| anyhow!(e))?;
    for outcome in outcomes {
        print_agent_materialize_outcome(&outcome);
    }

    let statuses_path =
        shelbi_state::statuses_path(&local_alias).map_err(|e| anyhow!(e))?;
    if !statuses_path.exists() {
        shelbi_state::save_project_statuses(
            &local_alias,
            &shelbi_core::default_project_statuses(),
        )
        .map_err(|e| anyhow!(e))?;
        println!("✓ wrote project statuses: {}", statuses_path.display());
    }

    println!();
    if suffixed_from.is_some() {
        println!(
            "✓ picked up `{canonical_name}` from {} as local alias `{local_alias}`.",
            config_path.display()
        );
        println!(
            "  Tip: `shelbi project rename` can retitle the local alias to something friendlier."
        );
    } else {
        println!(
            "✓ picked up `{canonical_name}` from {}.",
            config_path.display()
        );
    }

    Ok(PickUpOutcome {
        canonical_name,
        local_alias,
        suffixed_from,
    })
}

/// Result of the `--pick-up` flow, threaded through to the top-level
/// `next:` renderer so it knows whether to nudge the user about the
/// suffix or not.
struct PickUpOutcome {
    canonical_name: String,
    local_alias: String,
    /// `Some(canonical_name)` when the local alias diverged from the
    /// canonical due to a collision, `None` when they match.
    suffixed_from: Option<String>,
}

/// Resolve the repo root for `--pick-up`: `--root` wins, otherwise walk
/// up from `cwd` looking for `<parent>/.shelbi/project.yaml`. Doesn't
/// prompt — `--pick-up` is deliberately a low-ceremony flow.
fn resolve_repo_root_for_pick_up(cwd: &Path, force_root: Option<&Path>) -> Result<PathBuf> {
    if let Some(root) = force_root {
        let path = absolutize(cwd, root);
        match validate_root(&path) {
            RootValidation::Ok | RootValidation::NotGitRepo => Ok(path),
            RootValidation::NotExists => bail!("{} doesn't exist", path.display()),
            RootValidation::NotDirectory => bail!("{} is not a directory", path.display()),
        }
    } else {
        find_in_repo_config_ancestor(cwd).ok_or_else(|| {
            anyhow!(
                "no `.shelbi/project.yaml` found in {} or any parent — pass --root <path> to \
                 point shelbi at the checkout, or drop the --pick-up flag for a fresh project.",
                cwd.display()
            )
        })
    }
}

/// Walk from `start` up to the filesystem root looking for a
/// `.shelbi/project.yaml`. Returns the ancestor that contains it (i.e.
/// the repo root), not the config path itself. `--pick-up` is
/// convenience wiring, so we handle nested cwds transparently — the
/// user shouldn't have to `cd` to the repo root before invoking it.
fn find_in_repo_config_ancestor(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(IN_REPO_CONFIG_REL).is_file() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

fn absolutize(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Given a canonical project name, return `(local_alias, suffixed_from)`
/// where `local_alias` is a name that doesn't collide with any existing
/// registered project and `suffixed_from` is `Some(canonical)` when a
/// suffix was applied (`None` on a clean pass-through).
///
/// The suffix ladder is deterministic and starts at `-2` (skipping `-1`
/// so the sequence matches the plan's `shelbi → shelbi-2 → shelbi-3`
/// example). Gives up after a large sentinel — nobody has 1000 clones
/// of the same project and the loop shouldn't spin forever if the
/// scanner is misbehaving.
fn next_available_alias(canonical: &str) -> Result<(String, Option<String>)> {
    if !project_name_collides(canonical)? {
        return Ok((canonical.to_string(), None));
    }
    for n in 2..=999 {
        let candidate = format!("{canonical}-{n}");
        if !project_name_collides(&candidate)? {
            return Ok((candidate, Some(canonical.to_string())));
        }
    }
    bail!(
        "couldn't allocate a local alias for `{canonical}` — every suffix up to `{canonical}-999` \
         is taken. Rename or remove an existing project YAML in ~/.shelbi/projects/ first."
    )
}

fn write_workspace_settings_template(project: &str) -> Result<()> {
    let template_path = shelbi_state::project_dir(project)
        .map_err(|e| anyhow!(e))?
        .join("workspace-settings.json.template");
    if template_path.exists() {
        println!(
            "(workspace settings template already exists at {})",
            template_path.display()
        );
        return Ok(());
    }
    shelbi_state::ensure_dir(template_path.parent().unwrap()).map_err(|e| anyhow!(e))?;
    std::fs::write(&template_path, shelbi_state::DEFAULT_WORKSPACE_SETTINGS_TEMPLATE)?;
    println!(
        "✓ wrote workspace settings template: {}",
        template_path.display()
    );
    Ok(())
}

/// Stringify a [`shelbi_state::AgentMaterializeOutcome`] for the init /
/// reload report. Same renderer used by both commands so the user sees
/// the same wording for the same outcome regardless of which path
/// touched the agent workspace.
pub(super) fn print_agent_materialize_outcome(outcome: &AgentMaterializeOutcome) {
    match outcome {
        AgentMaterializeOutcome::Created { agent } => {
            println!("✓ created agent workspace: agents/{agent}/");
        }
        AgentMaterializeOutcome::Unchanged { agent } => {
            println!("(agent workspace already exists: agents/{agent}/)");
        }
        AgentMaterializeOutcome::Preserved { agent, first_notice } => {
            if *first_notice {
                println!(
                    "(preserved your custom agents/{agent}/instructions.md — \
                     differs from the bundled default; the project owns the override)"
                );
            } else {
                println!("(preserved your custom agents/{agent}/instructions.md)");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;

    fn fresh_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-init-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn scaffold_writes_yaml_but_no_marker_global_mode() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let project_root = fresh_dir("repo");
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::ensure_root_subdirs().unwrap();

        let resolved = ResolvedProjectRoot {
            path: project_root.clone(),
            name: "myapp".to_string(),
        };
        scaffold_project(&resolved, InitMode::Global).unwrap();

        let yaml = home.join("projects/myapp.yaml");
        assert!(yaml.is_file(), "expected project YAML at {}", yaml.display());
        let body = std::fs::read_to_string(&yaml).unwrap();
        assert!(body.contains(&format!("work_dir: {}", project_root.display())));

        // Global mode: no in-repo file, no `.shelbi/project` marker,
        // no `.shelbi` directory in the repo tree.
        assert!(
            !project_root.join(".shelbi/project").exists(),
            "init must not write a .shelbi/project marker"
        );
        assert!(
            !project_root.join(IN_REPO_CONFIG_REL).exists(),
            "global mode must not write the committed <repo>/.shelbi/project.yaml"
        );
        assert!(
            !project_root.join(".shelbi").exists(),
            "global mode must not create a .shelbi directory in the project tree"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn scaffold_writes_in_repo_config_for_in_repo_mode() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let project_root = fresh_dir("repo-in");
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::ensure_root_subdirs().unwrap();

        let resolved = ResolvedProjectRoot {
            path: project_root.clone(),
            name: "team-app".to_string(),
        };
        scaffold_project(&resolved, InitMode::InRepo).unwrap();

        // Global side still exists (same registry mechanism).
        assert!(home.join("projects/team-app.yaml").is_file());
        // In-repo side is what pick-up will detect on a teammate's
        // clone. Shape is intentionally minimal — just the canonical name.
        let committed = project_root.join(IN_REPO_CONFIG_REL);
        assert!(committed.is_file());
        let body = std::fs::read_to_string(&committed).unwrap();
        assert_eq!(body.trim(), "name: team-app");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn read_in_repo_name_returns_none_for_malformed_yaml() {
        let tmp = fresh_dir("malformed");
        let path = tmp.join("project.yaml");
        std::fs::write(&path, "this is not: [valid: yaml").unwrap();
        assert_eq!(read_in_repo_name(&path).unwrap(), None);
    }

    #[test]
    fn read_in_repo_name_returns_none_for_missing_name_key() {
        let tmp = fresh_dir("no-name");
        let path = tmp.join("project.yaml");
        std::fs::write(&path, "repo: /some/path\n").unwrap();
        assert_eq!(read_in_repo_name(&path).unwrap(), None);
    }

    #[test]
    fn read_in_repo_name_returns_trimmed_name() {
        let tmp = fresh_dir("name");
        let path = tmp.join("project.yaml");
        std::fs::write(&path, "name: my-app\nrepo: /x\n").unwrap();
        assert_eq!(
            read_in_repo_name(&path).unwrap(),
            Some("my-app".to_string())
        );
    }

    /// The auto-suffix ladder skips `-1` and starts at `-2`, per the
    /// plan (`shelbi → shelbi-2 → shelbi-3`). The committed name never
    /// changes; only the local alias.
    #[test]
    fn next_available_alias_deterministic_ladder() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_dir("alias");
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(home.join("projects")).unwrap();

        // Clean case: no collision → canonical passes through.
        let (alias, from) = next_available_alias("solo").unwrap();
        assert_eq!(alias, "solo");
        assert!(from.is_none());

        // Single collision → `-2`.
        std::fs::write(home.join("projects/shelbi.yaml"), "name: shelbi\n").unwrap();
        let (alias, from) = next_available_alias("shelbi").unwrap();
        assert_eq!(alias, "shelbi-2");
        assert_eq!(from.as_deref(), Some("shelbi"));

        // Two collisions → `-3`, deterministically.
        std::fs::write(home.join("projects/shelbi-2.yaml"), "name: shelbi\n").unwrap();
        let (alias, from) = next_available_alias("shelbi").unwrap();
        assert_eq!(alias, "shelbi-3");
        assert_eq!(from.as_deref(), Some("shelbi"));

        std::env::remove_var("SHELBI_HOME");
    }

    /// The picker's ancestor walk locates a committed config from a
    /// nested cwd. `--pick-up` should be usable without having to `cd`
    /// up to the repo root first.
    #[test]
    fn find_in_repo_config_ancestor_walks_up() {
        let root = fresh_dir("ancestor");
        std::fs::create_dir_all(root.join(".shelbi")).unwrap();
        std::fs::write(root.join(IN_REPO_CONFIG_REL), "name: nested\n").unwrap();
        let deep = root.join("src/mod/leaf");
        std::fs::create_dir_all(&deep).unwrap();

        let found = find_in_repo_config_ancestor(&deep).unwrap();
        // Canonicalize both sides so temp-dir symlinks (macOS
        // `/tmp -> /private/tmp`) don't upset the equality check.
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(&root).unwrap()
        );
    }

    #[test]
    fn find_in_repo_config_ancestor_returns_none_when_absent() {
        let root = fresh_dir("no-config");
        assert!(find_in_repo_config_ancestor(&root).is_none());
    }

    /// Non-interactive callers that omit `--mode` get a hard error, not
    /// a silent default. `cargo test` runs without a TTY on stdin, so
    /// this exercises the real non-interactive branch.
    #[test]
    fn resolve_mode_errors_without_tty_and_no_flag() {
        assert!(!std::io::stdin().is_terminal());
        let tmp = fresh_dir("no-flag");
        let err = resolve_mode(None, false, &tmp).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--mode"), "expected --mode hint, got: {msg}");
        assert!(
            msg.contains("no silent default"),
            "expected the 'no silent default' rationale in the message, got: {msg}"
        );
    }

    #[test]
    fn resolve_mode_passes_through_explicit_flag() {
        let tmp = fresh_dir("explicit");
        assert_eq!(
            resolve_mode(Some(InitMode::InRepo), false, &tmp).unwrap(),
            InitMode::InRepo
        );
        assert_eq!(
            resolve_mode(Some(InitMode::Global), false, &tmp).unwrap(),
            InitMode::Global
        );
    }
}
