//! The one detection behind every new-project front end: what a chosen
//! folder is ([`check_folder`]) and the defaults each step starts on
//! ([`shelbi_app::new_project::Facts`]): the default branch and the origin
//! remote (credentials removed), the GitHub repo it names, a committed
//! config to pick up, the recommended settings location, and the agent
//! runners installed on this machine.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use shelbi_app::new_project::{
    recommend_mode, Facts, FolderCheck, PickUp, Problem, Repo, TeamSignals, DEFAULT_BRANCH, NOT_GIT_REASON,
};
use shelbi_core::{ConfigMode, RunnerKind};

use super::{absolutize, project_name_collides, project_name_from_root, validate_root, RootValidation, IN_REPO_CONFIG_REL};

/// The folder check for the path field's text `root` (resolved against
/// `cwd`, `~` expanded) and, after a clash, the typed `name`: what the
/// folder is (a repo, a worktree, a plain folder), the project name and id
/// it gives (the folder's own name unless `name` is given), whether that id
/// is already registered, where each config mode would keep its settings,
/// and every default the flow's steps start on ([`Facts`]). The same checks
/// [`super::create_project`] runs, so no front end can continue with a
/// folder that would fail them. Runs git and reads the Shelbi home (and,
/// once per process, each runner's `--version`): keep it off any UI thread.
pub fn check_folder(cwd: &Path, root: &str, name: Option<&str>) -> FolderCheck {
    let trimmed = root.trim();
    if trimmed.is_empty() {
        return FolderCheck::failed(PathBuf::new(), Problem::Empty);
    }
    let path = absolutize(cwd, Path::new(trimmed));
    let repo = match validate_root(&path) {
        RootValidation::NotExists => return FolderCheck::failed(path, Problem::NotFound),
        RootValidation::NotDirectory => return FolderCheck::failed(path, Problem::NotDirectory),
        RootValidation::NotGitRepo => Repo::NotGit,
        // A worktree's `.git` is a file pointing at the main repo's gitdir.
        RootValidation::Ok if path.join(".git").is_file() => Repo::Worktree,
        RootValidation::Ok => Repo::Git,
    };
    let facts = detect_facts(&path, repo);
    // A pick-up registers under the committed name (suffixed locally when
    // it's taken), so the folder's name is only the default otherwise.
    let name = match (name.map(str::trim), &facts.pick_up) {
        (Some(n), _) => Some(n.to_string()),
        (None, Some(pick_up)) => Some(pick_up.name.clone()),
        (None, None) => project_name_from_root(&path),
    };
    let slug = name
        .as_deref()
        .and_then(|n| shelbi_core::normalize_project_name(n.trim()).ok());
    let problem = match &slug {
        None => Some(Problem::NoName),
        // A pick-up finds a free alias for itself.
        Some(_) if facts.pick_up.is_some() => None,
        Some(slug) => match project_name_collides(slug) {
            Ok(true) => Some(Problem::Taken { slug: slug.clone() }),
            Ok(false) => None,
            Err(e) => Some(Problem::Unreadable(e.to_string())),
        },
    };
    let global_dir = slug
        .as_deref()
        .and_then(|s| shelbi_state::projects_dir().ok().map(|d| d.join(s)));
    FolderCheck {
        in_repo_dir: path.join(".shelbi"),
        path,
        name,
        slug,
        repo: Some(repo),
        problem,
        global_dir,
        facts,
    }
}

/// Every default the flow's steps start on, for the folder `path`.
pub fn detect_facts(path: &Path, repo: Repo) -> Facts {
    let in_git = repo != Repo::NotGit;
    let remote = in_git
        .then(|| git_line(path, &["remote", "get-url", "origin"]))
        .flatten()
        .as_deref()
        .and_then(remote_url_for_storage);
    let default_branch = in_git
        .then(|| detect_default_branch(path))
        .flatten()
        .unwrap_or_else(|| DEFAULT_BRANCH.to_string());
    let (recommended_mode, mode_reason) = if in_git {
        recommend_mode(&probe_team_signals(path, remote.is_some()))
    } else {
        (ConfigMode::Global, NOT_GIT_REASON.to_string())
    };
    Facts {
        github_repo: super::issues::detect_github_repo(remote.as_deref()),
        recommended_mode,
        mode_reason,
        pick_up: detect_pick_up(path),
        runners: installed_runners().iter().map(|r| r.kind).collect(),
        default_branch,
        remote,
    }
}

/// The default branch of the repo at `path`: origin's `HEAD`, else the
/// checked-out branch.
fn detect_default_branch(path: &Path) -> Option<String> {
    git_line(path, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|reference| {
            let branch = reference.strip_prefix("origin/").unwrap_or(&reference);
            (!branch.is_empty()).then(|| branch.to_string())
        })
        .or_else(|| git_line(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]))
        .filter(|b| shelbi_core::validate_branch(b).is_ok())
}

fn git_line(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).current_dir(cwd).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(sanitize)
}

fn sanitize(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

/// A remote safe to keep in project YAML: HTTP(S) userinfo and URL
/// query/fragment data (where access tokens are commonly embedded) dropped,
/// the conventional SSH `git@host:path` form kept. `None` for a blank one.
pub fn remote_url_for_storage(remote: &str) -> Option<String> {
    let sanitized = sanitize(remote);
    let remote = sanitized.trim();
    if remote.is_empty() {
        return None;
    }
    let Some((scheme, rest)) = remote.split_once("://") else {
        return Some(remote.to_string());
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let scheme_lower = scheme.to_ascii_lowercase();
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let safe_authority = match authority.rsplit_once('@') {
        Some((userinfo, host)) if matches!(scheme_lower.as_str(), "ssh" | "git+ssh") => {
            let username = userinfo.split_once(':').map(|(user, _)| user).unwrap_or(userinfo);
            if username.is_empty() {
                host.to_string()
            } else {
                format!("{username}@{host}")
            }
        }
        Some((_, host)) => host.to_string(),
        None => authority.to_string(),
    };
    if safe_authority.is_empty() {
        return None;
    }
    let path = &rest[authority_end..];
    Some(format!("{scheme}://{safe_authority}{path}"))
}

/// The committed config in `path` this machine could pick up: a
/// `.shelbi/project.yaml` with a usable `name:`, in a folder no registered
/// project already resolves to.
pub fn detect_pick_up(path: &Path) -> Option<PickUp> {
    let config = path.join(IN_REPO_CONFIG_REL);
    if !config.is_file() {
        return None;
    }
    let name = read_in_repo_name(&config)?;
    shelbi_core::validate_project_name(&name).ok()?;
    match shelbi_state::resolve_project_for_cwd(path) {
        // Already registered here: nothing to pick up.
        Ok(Some(_)) => None,
        _ => Some(PickUp { name, config }),
    }
}

/// Best-effort read of the canonical `name:` from a committed
/// `<repo>/.shelbi/project.yaml`. `None` when the file isn't a YAML map
/// with a non-blank `name` key, so a malformed commit never blocks setup.
pub fn read_in_repo_name(path: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Head {
        name: Option<String>,
    }
    let text = std::fs::read_to_string(path).ok()?;
    let head: Head = serde_yaml::from_str(&text).ok()?;
    head.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}

/// How far back the committer scan walks: two distinct emails is all
/// [`recommend_mode`] needs, so a huge history is never loaded.
const COMMITTER_SCAN_DEPTH: &str = "2000";

fn probe_team_signals(cwd: &Path, has_remote_origin: bool) -> TeamSignals {
    let emails = Command::new("git")
        .args(["log", "-n", COMMITTER_SCAN_DEPTH, "--format=%ae"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect::<HashSet<_>>()
                .len()
        })
        .unwrap_or(0);
    TeamSignals { has_remote_origin, distinct_committer_emails: emails }
}

/// An agent runner found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRunner {
    pub kind: RunnerKind,
    /// Its `--version`, without the product name.
    pub version: String,
}

/// The runners a new project can default to, in preference order.
pub const SUPPORTED_RUNNERS: [RunnerKind; 2] = [RunnerKind::Claude, RunnerKind::Codex];

/// The supported runners installed on this machine (Claude Code first),
/// probed once per process: each probe runs the runner's `--version` with a
/// deadline, and installs don't come and go while a flow is open.
pub fn installed_runners() -> &'static [InstalledRunner] {
    static RUNNERS: OnceLock<Vec<InstalledRunner>> = OnceLock::new();
    RUNNERS.get_or_init(|| {
        if shelbi_state::in_test_binary() {
            // A test never probes the machine's real installs: it sees both.
            return SUPPORTED_RUNNERS
                .into_iter()
                .map(|kind| InstalledRunner { kind, version: "test".into() })
                .collect();
        }
        SUPPORTED_RUNNERS
            .into_iter()
            .filter_map(|kind| runner_version(kind).map(|version| InstalledRunner { kind, version }))
            .collect()
    })
}

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn runner_version(kind: RunnerKind) -> Option<String> {
    let command = kind.as_str();
    let argv = [command, "--version"];
    let output =
        shelbi_ssh::run_with_deadline(&shelbi_core::Host::Local, argv, VERSION_PROBE_TIMEOUT).ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(sanitize)
        .unwrap_or_else(|| "version unknown".to_string());
    Some(normalize_version(command, &line))
}

/// A `--version` line without the product name (`claude code 1.2` → `1.2`).
pub fn normalize_version(command: &str, line: &str) -> String {
    let sanitized = sanitize(line);
    let trimmed = sanitized.trim();
    let prefixes: &[&str] = match command {
        "claude" => &["claude code ", "claude "],
        "codex" => &["codex-cli ", "codex "],
        _ => &[],
    };
    let lower = trimmed.to_ascii_lowercase();
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            return trimmed[prefix.len()..].trim().to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_remotes_drop_credentials() {
        assert_eq!(
            remote_url_for_storage("https://user:token@github.com/acme/app.git?x=1#y").as_deref(),
            Some("https://github.com/acme/app.git")
        );
        assert_eq!(
            remote_url_for_storage("ssh://git:secret@github.com/acme/app.git").as_deref(),
            Some("ssh://git@github.com/acme/app.git")
        );
        assert_eq!(
            remote_url_for_storage("git@github.com:acme/app.git").as_deref(),
            Some("git@github.com:acme/app.git")
        );
        assert_eq!(remote_url_for_storage("  "), None);
    }

    #[test]
    fn versions_lose_the_product_name() {
        assert_eq!(normalize_version("claude", "Claude Code 2.1.0"), "2.1.0");
        assert_eq!(normalize_version("codex", "codex-cli 0.9"), "0.9");
        assert_eq!(normalize_version("codex", "\u{1b}weird"), "weird");
    }

    #[test]
    fn read_in_repo_name_reads_a_trimmed_name_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("project.yaml");
        std::fs::write(&p, "name: \"  acme  \"\n").unwrap();
        assert_eq!(read_in_repo_name(&p).as_deref(), Some("acme"));
        std::fs::write(&p, "default_branch: main\n").unwrap();
        assert_eq!(read_in_repo_name(&p), None);
        std::fs::write(&p, ": : not yaml [").unwrap();
        assert_eq!(read_in_repo_name(&p), None);
    }
}
