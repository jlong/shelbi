//! Project-root prompt + validators shared by `shelbi init` and the
//! first-run path from `shelbi` (no subcommand).
//!
//! The validator ([`validate_root`]) is a pure function — no prompting,
//! no global state, no `inquire` calls — so it's straightforward to
//! unit-test. The prompt loop ([`resolve_root_for_init`]) is the only
//! part that needs a real terminal; it composes the validator with
//! `inquire` widgets and surfaces the wireframe messaging from the
//! task brief.
//!
//! TTY gating: when stdin is not a terminal we refuse to prompt and
//! ask the caller to supply `--root` instead. The error message is
//! the exact string documented in the task acceptance criteria so
//! scripts can match on it.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use inquire::{Confirm, Text};

/// Outcome of validating a candidate project-root path. Non-OK variants
/// carry only the discriminant; the caller renders the user-facing
/// message because the message differs slightly between the prompt
/// loop (re-prompt with `✗`) and the non-interactive `--root` path
/// (error out with `anyhow!`).
#[derive(Debug, PartialEq, Eq)]
pub enum RootValidation {
    Ok,
    NotExists,
    NotDirectory,
    /// Directory exists but doesn't look like a git repo. Treated as a
    /// warning, not an error — shelbi's workflow assumes git, but
    /// nothing in the scaffold actively rejects a non-git dir.
    NotGitRepo,
}

/// Pure validator: no prompting, no global state. Checks (in order):
/// 1. path exists
/// 2. path is a directory
/// 3. path is a git repo (has `.git`, OR `git rev-parse --git-dir`
///    succeeds inside it — the latter catches working trees whose
///    `.git` is a regular file pointing at the gitdir).
pub fn validate_root(path: &Path) -> RootValidation {
    if !path.exists() {
        return RootValidation::NotExists;
    }
    let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
    if !is_dir {
        return RootValidation::NotDirectory;
    }
    if !is_git_repo(path) {
        return RootValidation::NotGitRepo;
    }
    RootValidation::Ok
}

fn is_git_repo(path: &Path) -> bool {
    if path.join(".git").exists() {
        return true;
    }
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Validate a project name before it's used as a filesystem path
/// component (`~/.shelbi/projects/<name>.yaml`, `~/.shelbi/projects/<name>/`)
/// and interpolated into on-disk config.
///
/// The name reaches this function from three untrusted-ish sources: a
/// `--project` override, the basename of a chosen root, and — most
/// importantly — the `name:` key of a teammate's *committed*
/// `<repo>/.shelbi/project.yaml` read by `--pick-up`. That last one is
/// attacker-influenced by design (pick-up runs on someone else's repo),
/// so an unvalidated `name: ../../.config/foo` would have `PathBuf::join`
/// traverse out of the projects dir, and an embedded newline would inject
/// keys into the local registry.
///
/// Delegates to [`shelbi_core::validate_project_name`] — the storage-layer
/// chokepoint — so the CLI pre-check and the on-disk invariant enforce one
/// charset and can't drift apart. That means a name must be a single path
/// component of lowercase `[a-z0-9_-]` starting with a letter or digit; the
/// charset also guarantees the name round-trips through YAML unquoted.
/// Onboarding-captured names are normalized into this charset first (see
/// [`normalize_project_name_announced`]); this guard is for the paths that
/// pass a raw name straight through (chiefly the pick-up committed name).
pub fn validate_project_name(name: &str) -> Result<()> {
    shelbi_core::validate_project_name(name).map_err(|_| {
        anyhow!(
            "project name `{name}` is invalid — it must be a single path component of \
             lowercase `[a-z0-9_-]` starting with a letter or digit (no `/`, `..`, spaces, \
             uppercase, or leading `.`). Pass --project NAME with a name like `my-app`."
        )
    })
}

/// Normalize a captured project name into the agent-id charset, printing a
/// one-line notice when the result differs from the input so the change is
/// never silent. Errors (with a suggestion) when the input can't reduce to
/// a valid name — e.g. it was all punctuation.
///
/// This is the single capture-time chokepoint for the `shelbi init` /
/// first-run path: every name (basename default or `--project` override)
/// flows through [`pick_name`] into here before it reaches the on-disk
/// layout, so files are only ever written under the normalized name.
pub fn normalize_project_name_announced(raw: &str) -> Result<String> {
    let normalized = shelbi_core::normalize_project_name(raw).map_err(|_| {
        anyhow!(
            "project name `{raw}` can't be normalized to a valid id — it needs at \
             least one ASCII letter or digit (allowed characters: [a-z0-9_-]). \
             Pass --project NAME with a name like `my-app`."
        )
    })?;
    if normalized != raw {
        println!("using project name '{normalized}' (normalized from '{raw}')");
    }
    Ok(normalized)
}

/// Project name derived from a chosen root: the basename of the path,
/// unchanged. Returns `None` if the path has no usable file component
/// (e.g. `/`).
pub fn project_name_from_root(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Whether `~/.shelbi/projects/<name>.yaml` already exists. The init
/// scaffolder uses this to refuse to silently overwrite a pre-existing
/// project YAML.
pub fn project_name_collides(name: &str) -> Result<bool> {
    let dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    Ok(dir.join(format!("{name}.yaml")).exists())
}

/// Resolved + validated project root, plus the derived project name.
#[derive(Debug, Clone)]
pub struct ResolvedProjectRoot {
    pub path: PathBuf,
    pub name: String,
}

/// End-to-end resolver used by `shelbi init`. Picks `force_root` if
/// given (scripted path), prompts the user otherwise. The resulting
/// `name` is `force_name` if set, otherwise the basename of the
/// resolved path.
///
/// Non-TTY + no `--root` → returns the documented error so scripted
/// callers get a single line they can match on, never a hung prompt.
pub fn resolve_root_for_init(
    cwd: &Path,
    force_root: Option<PathBuf>,
    force_name: Option<&str>,
) -> Result<ResolvedProjectRoot> {
    if let Some(root) = force_root {
        return resolve_scripted(cwd, &root, force_name);
    }
    if !std::io::stdin().is_terminal() {
        bail!("shelbi: not running interactively — pass --root <path> to set the project root.");
    }
    prompt_loop(cwd, force_name)
}

/// Validation + name derivation for the `--root` (scripted) path. No
/// prompting — failures are hard errors so a script can't accidentally
/// scaffold against a bad root.
fn resolve_scripted(
    cwd: &Path,
    root: &Path,
    force_name: Option<&str>,
) -> Result<ResolvedProjectRoot> {
    let path = absolutize(cwd, root);
    match validate_root(&path) {
        RootValidation::Ok => {}
        RootValidation::NotExists => {
            bail!("{} doesn't exist", path.display());
        }
        RootValidation::NotDirectory => {
            bail!("{} is not a directory", path.display());
        }
        RootValidation::NotGitRepo => {
            eprintln!(
                "⚠ {} is not a git repository — shelbi expects a git repo, but \
                 you passed --root explicitly so we'll continue.",
                path.display()
            );
        }
    }
    let name = pick_name(&path, force_name)?;
    if project_name_collides(&name)? {
        bail!(
            "a shelbi project named `{name}` already exists at \
             ~/.shelbi/projects/{name}.yaml — remove the existing YAML or pass \
             --project NAME to pick a different name"
        );
    }
    Ok(ResolvedProjectRoot { path, name })
}

/// Interactive prompt loop. Matches the wireframes in the task brief:
/// re-prompts on `NotExists` / `NotDirectory`, warns + confirms on
/// `NotGitRepo`, warns + confirms on name collision. Exits the loop
/// only when the user has accepted a fully-validated path.
fn prompt_loop(cwd: &Path, force_name: Option<&str>) -> Result<ResolvedProjectRoot> {
    let default_path = cwd.display().to_string();
    loop {
        let raw = Text::new("Project root?")
            .with_default(&default_path)
            .prompt()
            .context("project root prompt")?;
        let candidate = absolutize(cwd, Path::new(raw.trim()));

        match validate_root(&candidate) {
            RootValidation::Ok => {
                println!(
                    "✓ {} exists, is a directory, is a git repo",
                    candidate.display()
                );
            }
            RootValidation::NotExists => {
                println!(
                    "✗ {} doesn't exist — please enter a valid project root.",
                    candidate.display()
                );
                continue;
            }
            RootValidation::NotDirectory => {
                println!(
                    "✗ {} is not a directory — please enter a valid project root.",
                    candidate.display()
                );
                continue;
            }
            RootValidation::NotGitRepo => {
                let proceed = Confirm::new(&format!(
                    "⚠ {} is not a git repository. Shelbi expects a git repo \
                     (worktrees, branch dispatch, PR flow). Continue anyway?",
                    candidate.display()
                ))
                .with_default(false)
                .prompt()
                .context("non-git confirm prompt")?;
                if !proceed {
                    continue;
                }
            }
        }

        let name = match pick_name(&candidate, force_name) {
            Ok(n) => n,
            Err(e) => {
                println!("✗ {e}");
                continue;
            }
        };

        if project_name_collides(&name)? {
            let proceed = Confirm::new(&format!(
                "⚠ a shelbi project named `{name}` already exists at \
                 ~/.shelbi/projects/{name}.yaml. Re-initialize?"
            ))
            .with_default(false)
            .prompt()
            .context("name collision confirm prompt")?;
            if !proceed {
                continue;
            }
        }

        return Ok(ResolvedProjectRoot {
            path: candidate,
            name,
        });
    }
}

fn pick_name(path: &Path, force_name: Option<&str>) -> Result<String> {
    let raw = match force_name {
        Some(n) => n.to_string(),
        None => project_name_from_root(path).ok_or_else(|| {
            anyhow!(
                "can't derive a project name from {} — pass --project NAME on the command line",
                path.display()
            )
        })?,
    };
    // Normalize the captured name (folder basename or `--project` override)
    // into the agent-id charset before it becomes a path component. A folder
    // like `Shaft` becomes project `shaft` instead of erroring at launch.
    normalize_project_name_announced(&raw)
}

/// Expand `~` / `~/...` against `$HOME` and resolve relative paths
/// against `cwd`. Stops short of `canonicalize` because that fails on
/// non-existent paths — we want validation to report the user's typed
/// path verbatim ("`/tmp/nope` doesn't exist"), not a canonicalized
/// form they didn't type.
fn absolutize(cwd: &Path, path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let expanded: PathBuf = if raw == "~" {
        dirs::home_dir().unwrap_or_else(|| path.to_path_buf())
    } else if let Some(rest) = raw.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(h) => h.join(rest),
            None => path.to_path_buf(),
        }
    } else {
        path.to_path_buf()
    };

    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;

    fn fresh_tmp_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-project-root-test-{}-{}",
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
    fn validate_root_reports_missing_path() {
        let tmp = fresh_tmp_dir();
        let missing = tmp.join("nope");
        assert_eq!(validate_root(&missing), RootValidation::NotExists);
    }

    #[test]
    fn validate_root_reports_file() {
        let tmp = fresh_tmp_dir();
        let file = tmp.join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(validate_root(&file), RootValidation::NotDirectory);
    }

    #[test]
    fn validate_root_reports_non_git_dir() {
        let tmp = fresh_tmp_dir();
        let dir = tmp.join("not-a-repo");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(validate_root(&dir), RootValidation::NotGitRepo);
    }

    #[test]
    fn validate_root_accepts_dir_with_dot_git_directory() {
        let tmp = fresh_tmp_dir();
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(validate_root(&repo), RootValidation::Ok);
    }

    #[test]
    fn validate_root_accepts_dir_with_dot_git_file() {
        // Worktrees ship a `.git` *file* whose contents point at the
        // real gitdir. The validator must accept that shape, not only
        // a `.git` directory.
        let tmp = fresh_tmp_dir();
        let repo = tmp.join("worktree");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: /tmp/elsewhere\n").unwrap();
        assert_eq!(validate_root(&repo), RootValidation::Ok);
    }

    #[test]
    fn validate_project_name_accepts_ordinary_names() {
        for ok in ["shelbi", "my-app", "app_2", "web3", "a"] {
            assert!(validate_project_name(ok).is_ok(), "expected `{ok}` to pass");
        }
        // Uppercase is now rejected here (it would crash dashboard setup);
        // onboarding normalizes it upstream instead.
        assert!(validate_project_name("Web3").is_err());
    }

    #[test]
    fn validate_project_name_rejects_traversal_and_injection() {
        // `..` and leading-dot: PathBuf::join traversal / hidden file.
        assert!(validate_project_name("..").is_err());
        assert!(validate_project_name("../../.config/foo").is_err());
        assert!(validate_project_name(".hidden").is_err());
        // Path separators would escape ~/.shelbi/projects/.
        assert!(validate_project_name("a/b").is_err());
        // Embedded newline would inject keys into the registry YAML.
        assert!(validate_project_name("foo\nname: evil").is_err());
        // Spaces / colons / other punctuation break the unquoted YAML round-trip.
        assert!(validate_project_name("has space").is_err());
        assert!(validate_project_name("a: b").is_err());
        // Empty.
        assert!(validate_project_name("").is_err());
    }

    #[test]
    fn pick_name_normalizes_force_name() {
        let cwd = PathBuf::from("/tmp/cwd");
        // A valid name passes through unchanged.
        assert_eq!(pick_name(&cwd, Some("ok-name")).unwrap(), "ok-name");
        // Uppercase / spaces are slugified rather than rejected.
        assert_eq!(pick_name(&cwd, Some("Shaft")).unwrap(), "shaft");
        assert_eq!(pick_name(&cwd, Some("My App")).unwrap(), "my-app");
        // Traversal metacharacters are neutralized by normalization (the
        // `/` and `.` are stripped), so no `../`-style name survives.
        assert_eq!(pick_name(&cwd, Some("../evil")).unwrap(), "evil");
        // An all-punctuation name can't be normalized → clear error.
        assert!(pick_name(&cwd, Some("...")).is_err());
    }

    #[test]
    fn project_name_from_root_uses_basename() {
        assert_eq!(
            project_name_from_root(Path::new("/Users/jlong/Projects/my-thing")),
            Some("my-thing".to_string())
        );
    }

    #[test]
    fn project_name_from_root_returns_none_for_root() {
        assert!(project_name_from_root(Path::new("/")).is_none());
    }

    #[test]
    fn absolutize_makes_relative_absolute() {
        let cwd = PathBuf::from("/tmp/cwd");
        assert_eq!(
            absolutize(&cwd, Path::new("sub")),
            PathBuf::from("/tmp/cwd/sub")
        );
    }

    #[test]
    fn absolutize_passes_absolute_through() {
        let cwd = PathBuf::from("/tmp/cwd");
        assert_eq!(absolutize(&cwd, Path::new("/abs")), PathBuf::from("/abs"));
    }

    #[test]
    fn project_name_collides_detects_existing_yaml() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_tmp_dir();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(home.join("projects/taken.yaml"), "name: taken\n").unwrap();
        assert!(project_name_collides("taken").unwrap());
        assert!(!project_name_collides("free").unwrap());
        std::env::remove_var("SHELBI_HOME");
    }

    /// When stdin is not a terminal and no `--root` was passed, the
    /// resolver must fail with the documented error message rather
    /// than block on `inquire`. `cargo test` runs without a TTY on
    /// stdin so this exercises the real non-interactive branch.
    #[test]
    fn resolve_root_errors_without_tty_when_no_force_root() {
        // No TTY in the test runner.
        assert!(!std::io::stdin().is_terminal());
        let cwd = std::env::current_dir().unwrap();
        let err = resolve_root_for_init(&cwd, None, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not running interactively"),
            "expected TTY error, got: {msg}"
        );
        assert!(msg.contains("--root"), "expected --root hint, got: {msg}");
    }

    /// `--root` skips the prompt entirely. With a valid git repo at the
    /// chosen path and a clean projects dir, the scripted path resolves
    /// successfully and derives the basename as the name.
    #[test]
    fn resolve_root_bypasses_prompt_when_force_root_given() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_tmp_dir();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(home.join("projects")).unwrap();

        let repo = home.join("my-repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let resolved = resolve_root_for_init(&cwd, Some(repo.clone()), None).expect("resolver");
        assert_eq!(resolved.path, repo);
        assert_eq!(resolved.name, "my-repo");
        std::env::remove_var("SHELBI_HOME");
    }

    /// `--root` against a non-existent path errors out cleanly without
    /// touching disk.
    #[test]
    fn resolve_root_rejects_non_existent_root() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_tmp_dir();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(home.join("projects")).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let bogus = home.join("does-not-exist");
        let err = resolve_root_for_init(&cwd, Some(bogus), None).unwrap_err();
        assert!(err.to_string().contains("doesn't exist"));
        std::env::remove_var("SHELBI_HOME");
    }

    /// `--root` against a path that's already a registered shelbi
    /// project surfaces the collision instead of silently overwriting.
    #[test]
    fn resolve_root_detects_name_collision_in_scripted_mode() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_tmp_dir();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(home.join("projects/dupe.yaml"), "name: dupe\n").unwrap();

        let repo = home.join("dupe");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let err = resolve_root_for_init(&cwd, Some(repo), None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "got: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    /// `--project NAME --root /path` lets a script override the
    /// auto-derived name and bypasses the basename collision check.
    /// Collision is then checked against the override name.
    #[test]
    fn resolve_root_uses_force_name_when_given() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_tmp_dir();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(home.join("projects")).unwrap();

        let repo = home.join("repo-dir");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let resolved =
            resolve_root_for_init(&cwd, Some(repo.clone()), Some("custom-name")).expect("resolver");
        assert_eq!(resolved.name, "custom-name");
        std::env::remove_var("SHELBI_HOME");
    }
}
