//! `shelbi init`: the shared new-project flow ([`crate::wizard`]) with its
//! steps pre-answered by flags. Interactive at a terminal; `-y` (or no
//! terminal) takes every detected default instead of asking.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args as ClapArgs, ValueEnum};
use shelbi_core::ConfigMode;
use shelbi_orchestrator::project_create::{self, IN_REPO_CONFIG_REL};
use shelbi_state::AgentMaterializeOutcome;

use crate::issue_tracker_setup::RealGhProbe;
use crate::wizard::{self, Defaults, IssuesPreset, Preset, Prompts, Runner, SetupOutcome};

/// Where the project config should live (`--mode`).
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
    fn config_mode(self) -> ConfigMode {
        match self {
            InitMode::InRepo => ConfigMode::InRepo,
            InitMode::Global => ConfigMode::Global,
        }
    }
}

/// The `--issue-tracker` backend choice. Only the two live backends are
/// selectable; jira/linear parse but have no store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IssueTrackerArg {
    /// Store issues as GitHub issues in `--github-repo` (or the detected repo).
    #[value(name = "github")]
    Github,
    /// Keep issues as local markdown under `~/.shelbi/projects/<name>/tasks/`.
    #[value(name = "file_system", alias = "file-system")]
    FileSystem,
}

#[derive(Debug, ClapArgs)]
#[command(after_long_help = "Examples:\n  shelbi init -y\n  shelbi init -y --runner codex")]
pub struct Args {
    /// Override the project name. Defaults to the basename of the
    /// project root (current directory or `--root`).
    #[arg(long)]
    pub project: Option<String>,

    /// Project root directory — the repo `shelbi` will manage. Defaults to
    /// the current directory with `-y`; otherwise skips the interactive
    /// "Project root?" prompt and is required when stdin is not a TTY.
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// The agents' default runner (the Agents step). Defaults to the
    /// installed runner (Claude Code when both are), which `-y` must find on
    /// PATH.
    #[arg(long, value_enum, value_name = "RUNNER")]
    pub(crate) runner: Option<Runner>,

    /// The agents' default model (the Agents step). Defaults to the
    /// runner's own default.
    #[arg(long, value_name = "MODEL")]
    pub(crate) model: Option<String>,

    /// Override the detected default Git branch (also the branch a `git
    /// init` creates).
    #[arg(long, alias = "branch", value_name = "BRANCH")]
    pub(crate) default_branch: Option<String>,

    /// Override the detected origin URL. Credentials are removed before the
    /// URL is written. Pass an empty value to omit the URL.
    #[arg(long, alias = "remote", value_name = "URL")]
    pub(crate) github_url: Option<String>,

    /// Deprecated: the orchestrator runs on `--runner`, the agents' default.
    /// Accepted as `--runner` when that flag is absent.
    #[arg(long, value_enum, value_name = "RUNNER", hide = true)]
    pub(crate) orchestrator_runner: Option<Runner>,

    /// Where the project's issues live (the Issues step): `github` stores
    /// them as GitHub issues (preflighted before anything is written);
    /// `file_system` keeps them as local markdown. Defaults to GitHub when
    /// the origin is a GitHub repo that passes the preflight, else
    /// `file_system`.
    #[arg(long, value_enum, value_name = "BACKEND")]
    pub(crate) issue_tracker: Option<IssueTrackerArg>,

    /// The `owner/repo` for `--issue-tracker github`. Defaults to the repo
    /// detected from the origin remote; required when that detection finds no
    /// GitHub remote.
    #[arg(long, value_name = "OWNER/REPO")]
    pub(crate) github_repo: Option<String>,

    /// Where the project's settings live (the Settings step): `in-repo`
    /// writes a committed `<repo>/.shelbi/project.yaml` shared with the
    /// team; `global` keeps everything under `~/.shelbi/projects/`. Defaults
    /// to the folder's recommendation: in-repo when it has an origin and
    /// more than one committer, else global.
    #[arg(long, value_enum)]
    pub mode: Option<InitMode>,

    /// Register an existing `<repo>/.shelbi/project.yaml` (a teammate's
    /// committed project) on this machine. The Folder step offers this on
    /// its own when it finds one; the flag also looks in parent folders and
    /// takes the offer without asking. If the committed name is taken
    /// locally, the local alias is suffixed (`-2`, `-3`, …). See
    /// `site/content/docs/concepts/config-modes.mdx`.
    #[arg(long)]
    pub pick_up: bool,
}

pub fn run(args: Args, assume_yes: bool) -> Result<()> {
    let interactive = !assume_yes && std::io::stdin().is_terminal();
    let preset = preset(args)?;
    let outcome = if interactive {
        wizard::setup(preset, &mut Prompts)?
    } else {
        wizard::setup(preset, &mut Defaults { gh: &mut RealGhProbe })?
    };
    if let SetupOutcome::Created(project) = outcome {
        if interactive {
            println!();
            println!("next: run `shelbi` here to open {project}; its orchestrator sets up your workspaces.");
        }
    }
    Ok(())
}

/// The steps `args` pre-answer.
fn preset(args: Args) -> Result<Preset> {
    let runner = match (args.runner, args.orchestrator_runner) {
        (Some(runner), Some(orchestrator)) if runner != orchestrator => bail!(
            "`--orchestrator-runner` is retired: the orchestrator runs on the agents' default              runner. Pass only `--runner {orchestrator}`."
        ),
        (runner, orchestrator) => runner.or(orchestrator),
    };
    let issues = match (args.issue_tracker, args.github_repo) {
        (None | Some(IssueTrackerArg::FileSystem), Some(_)) => {
            bail!("--github-repo requires `--issue-tracker github`")
        }
        (None, None) => None,
        (Some(IssueTrackerArg::FileSystem), None) => Some(IssuesPreset::FileSystem),
        (Some(IssueTrackerArg::Github), repo) => Some(IssuesPreset::Github { repo }),
    };
    let cwd = std::env::current_dir().context("reading current directory")?;
    let root = match args.root {
        Some(root) => Some(canonical_root(&cwd, &root)?),
        // `--pick-up` looks for the committed config from here up.
        None if args.pick_up => Some(find_in_repo_config_ancestor(&cwd).ok_or_else(|| {
            anyhow!(
                "no `.shelbi/project.yaml` found in {} or any parent — pass --root <path> to \
                 point shelbi at the checkout, or drop the --pick-up flag for a fresh project.",
                cwd.display()
            )
        })?),
        None => None,
    };
    Ok(Preset {
        root,
        project: args.project,
        pick_up: args.pick_up,
        runner,
        model: args.model,
        issues,
        mode: args.mode.map(InitMode::config_mode),
        default_branch: args.default_branch,
        remote: args.github_url,
    })
}

/// `--root`, resolved against `cwd`, which must name an existing folder.
fn canonical_root(cwd: &Path, root: &Path) -> Result<PathBuf> {
    let root = project_create::absolutize(cwd, root);
    if !root.exists() {
        bail!("project root {} does not exist", root.display());
    }
    if !root.is_dir() {
        bail!("project root {} is not a directory", root.display());
    }
    std::fs::canonicalize(&root).with_context(|| format!("resolving project root {}", root.display()))
}

/// Walk from `start` up to the filesystem root looking for a
/// `.shelbi/project.yaml`: the ancestor holding it (the repo root).
fn find_in_repo_config_ancestor(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|dir| dir.join(IN_REPO_CONFIG_REL).is_file()).map(Path::to_path_buf)
}

/// Print a [`shelbi_state::AgentMaterializeOutcome`] for the reload report,
/// in the engine's wording ([`project_create::render_agent_materialize_outcome`]).
pub(super) fn print_agent_materialize_outcome(outcome: &AgentMaterializeOutcome) {
    println!("{}", project_create::render_agent_materialize_outcome(outcome));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            project: None,
            root: None,
            runner: None,
            model: None,
            default_branch: None,
            github_url: None,
            orchestrator_runner: None,
            issue_tracker: None,
            github_repo: None,
            mode: None,
            pick_up: false,
        }
    }

    #[test]
    fn flags_map_onto_the_steps_they_answer() {
        let p = preset(Args {
            runner: Some(Runner::Codex),
            model: Some("gpt-5".into()),
            issue_tracker: Some(IssueTrackerArg::Github),
            github_repo: Some("acme/app".into()),
            mode: Some(InitMode::InRepo),
            ..args()
        })
        .unwrap();
        assert_eq!(p.runner, Some(Runner::Codex));
        assert_eq!(p.model.as_deref(), Some("gpt-5"));
        assert_eq!(p.issues, Some(IssuesPreset::Github { repo: Some("acme/app".into()) }));
        assert_eq!(p.mode, Some(ConfigMode::InRepo));

        let p = preset(Args { orchestrator_runner: Some(Runner::Codex), ..args() }).unwrap();
        assert_eq!(p.runner, Some(Runner::Codex), "the retired flag stands in for --runner");
        assert!(preset(Args {
            runner: Some(Runner::Claude),
            orchestrator_runner: Some(Runner::Codex),
            ..args()
        })
        .is_err());
        assert!(preset(Args { github_repo: Some("a/b".into()), ..args() }).is_err());
        assert_eq!(
            preset(Args { issue_tracker: Some(IssueTrackerArg::FileSystem), ..args() }).unwrap().issues,
            Some(IssuesPreset::FileSystem)
        );
    }

    #[test]
    fn find_in_repo_config_ancestor_walks_up() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".shelbi")).unwrap();
        std::fs::create_dir_all(repo.join("a/b")).unwrap();
        std::fs::write(repo.join(IN_REPO_CONFIG_REL), "name: x\n").unwrap();
        assert_eq!(find_in_repo_config_ancestor(&repo.join("a/b")), Some(repo.clone()));
        assert_eq!(find_in_repo_config_ancestor(temp.path()), None);
    }
}
