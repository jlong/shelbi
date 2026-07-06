//! Project resolution — first by walking up from cwd looking for an
//! in-repo `<dir>/.shelbi/project.yaml` (like git's `.git` walk), then
//! falling back to a reverse-lookup against the global registry of
//! `~/.shelbi/projects/*.yaml`.
//!
//! **Walk-up (in-repo mode).** When a project's config is committed to
//! its own repo, the shared half lives at `<repo>/.shelbi/project.yaml`
//! and the per-user half at `~/.shelbi/projects/<name>/local.yaml`.
//! Walking up from cwd looking for the shared half lets in-repo mode
//! "just work" without env vars once the user cds into the repo.
//! Walk-up matches take precedence over global-registry matches for the
//! same project name.
//!
//! **Global fallback.** For projects that keep their config at
//! `~/.shelbi/projects/<name>.yaml` (the pre-split layout), we scan
//! those YAMLs once, collect each project's local `work_dir`(s), and
//! match cwd (or an ancestor) against them — deepest match wins. No
//! `.shelbi/project` sentinel is needed; the project metadata on disk
//! is the single source of truth.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use shelbi_core::{Error, Machine, MachineKind, Project, Result};

use crate::projects_dir;

/// A candidate project root used for cwd-based resolution: one entry per
/// local `work_dir` declared by a registered project.
#[derive(Debug, Clone)]
pub struct ProjectRoot {
    /// The project name (from the YAML).
    pub name: String,
    /// The `work_dir` exactly as written in the YAML.
    pub work_dir: PathBuf,
    /// `work_dir` run through [`fs::canonicalize`], resolving symlinks.
    /// `None` when the path no longer exists on disk — such a project is
    /// excluded from resolution and surfaced as a warning by `shelbi
    /// reload`.
    pub canonical: Option<PathBuf>,
}

/// Scan `<shelbi-root>/projects/*.yaml` and collect every local machine's
/// `work_dir` as a [`ProjectRoot`]. Files that fail to read or parse are
/// skipped with a once-per-process warning — a single malformed YAML
/// shouldn't break resolution for every other project, but a hand-broken
/// registration silently vanishing from cwd resolution is undebuggable.
/// Only `kind: local` machines contribute: a remote `work_dir` is a path
/// on another host and matching it against the local cwd would be a
/// false positive.
pub fn project_roots() -> Result<Vec<ProjectRoot>> {
    let dir = projects_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                warn_skipped_project_yaml_once(&path, &e.to_string());
                continue;
            }
        };
        let project: Project = match serde_yaml::from_str(&text) {
            Ok(p) => p,
            Err(e) => {
                warn_skipped_project_yaml_once(&path, &e.to_string());
                continue;
            }
        };
        for machine in &project.machines {
            if machine.kind != MachineKind::Local {
                continue;
            }
            let canonical = fs::canonicalize(&machine.work_dir).ok();
            out.push(ProjectRoot {
                name: project.name.clone(),
                work_dir: machine.work_dir.clone(),
                canonical,
            });
        }
    }
    Ok(out)
}

/// One-time-per-process warning (keyed by file path) when a registered
/// project YAML can't be read or parsed during root collection. Routed
/// through `tracing::warn!` — not `eprintln!` — for the same reason as
/// `warn_legacy_workers_key`: TUI subcommands log to a file and must not
/// paint onto the alt-screen pane.
fn warn_skipped_project_yaml_once(path: &Path, err: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static WARNED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

    let mut guard = match WARNED.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let seen = guard.get_or_insert_with(HashSet::new);
    if !seen.insert(path.to_path_buf()) {
        return;
    }
    drop(guard);
    tracing::warn!(
        file = %path.display(),
        "shelbi: skipping unreadable project registration {}: {err} — \
         this project is excluded from cwd resolution until the file is fixed",
        path.display(),
    );
}

/// Resolve the project owning `cwd`.
///
/// 1. **Walk up** from `cwd` (canonicalized so symlinks resolve) looking
///    for `<dir>/.shelbi/project.yaml`. If found, the project name comes
///    from that file and its per-user companion `local.yaml` under
///    `~/.shelbi/projects/<name>/` must also be present — otherwise this
///    is a freshly-cloned repo and we return
///    [`Error::ProjectNotPickedUp`] so the caller can prompt for
///    `shelbi init --pick-up`. Corrupt in-repo YAML is *not* fatal: the
///    walk-up logs a warning, skips that file, and keeps walking so the
///    registry fallback can still resolve the subtree.
///
///    **Trust boundary.** The `name:` field is committed to a repo and so
///    is attacker-controlled in any cloned third-party checkout. Before
///    honoring it we cross-check that the discovered repo root actually
///    belongs to the registered project — it must be the registered local
///    `work_dir` itself or a shelbi worktree beneath it at
///    `<work_dir>/.shelbi/wt/<name>` (git grew `safe.directory` for this
///    same class of problem). A repo that ships `name: shelbi` but lives
///    somewhere else on disk fails the check; we log a warning and fall
///    through to the registry rather than hijacking the user's real board.
/// 2. **Fall back** to reverse-lookup against the global registry: match
///    `cwd` (or an ancestor) against each registered project's local
///    `work_dir`. When two registrations nest (`~/foo` and `~/foo/sub`),
///    a cwd inside `sub` resolves to the sub-project. Projects whose
///    `work_dir` no longer exists are excluded.
/// 3. Both miss → `Ok(None)`. The caller renders the "no project
///    specified" message.
///
/// Walk-up takes precedence: if `<repo>/.shelbi/project.yaml` names
/// project `X` *and* `~/.shelbi/projects/X.yaml` also exists, the walk-up
/// wins. Same for name collisions where both point at overlapping trees.
pub fn resolve_project_for_cwd(cwd: &Path) -> Result<Option<String>> {
    if let Some(hit) = walk_up_for_in_repo(cwd)? {
        let expected_local = projects_dir()?.join(&hit.name).join("local.yaml");
        if !expected_local.is_file() {
            // The in-repo config names a project this machine has never
            // registered. Might be a genuine fresh clone the user wants to
            // pick up — surface the actionable error rather than silently
            // swallowing it. (No board exists to hijack in this case.)
            return Err(Error::ProjectNotPickedUp {
                name: hit.name,
                config_path: hit.config_path,
                expected_local,
            });
        }
        // The project IS registered locally, so honoring the committed
        // `name:` here could redirect this command at the user's real board.
        // Only trust it when the discovered repo root is genuinely a checkout
        // of that registered project; otherwise fall through to the registry.
        if in_repo_root_is_trusted(&hit.repo_root, &hit.name)? {
            return Ok(Some(hit.name));
        }
        tracing::warn!(
            project = %hit.name,
            config = %hit.config_path.display(),
            "shelbi: ignoring in-repo project config at {} — its repo root {} is \
             not the registered work_dir (nor a worktree beneath it) for project \
             `{}`; resolving via the global registry instead",
            hit.config_path.display(),
            hit.repo_root.display(),
            hit.name,
        );
    }
    Ok(match_root(cwd, &project_roots()?))
}

/// True when `repo_root` (canonical) is genuinely a checkout of the registered
/// project `name` on this machine: either the registered local `work_dir`
/// itself or a shelbi worktree beneath it at `<work_dir>/.shelbi/wt/<name>`
/// (see [`shelbi_orchestrator::workspace::workspace_worktree`]). This is the
/// trust cross-check that stops a cloned third-party repo shipping a
/// `name: <yours>` config from redirecting resolution at your board.
fn in_repo_root_is_trusted(repo_root: &Path, name: &str) -> Result<bool> {
    for work_dir in registered_local_work_dirs(name)? {
        let worktrees = work_dir.join(".shelbi").join("wt");
        if repo_root == work_dir || repo_root.strip_prefix(&worktrees).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Minimal projection of the in-repo split's per-user `local.yaml`: we only
/// need the `machines` list to recover this machine's registered `work_dir`s.
#[derive(Deserialize)]
struct LocalHalf {
    #[serde(default)]
    machines: Vec<Machine>,
}

/// Collect the canonical local `work_dir`s registered for project `name` on
/// this machine, reading whichever registry layout is present:
///
/// * the global single-YAML at `~/.shelbi/projects/<name>.yaml`, and
/// * the in-repo split's per-user half at
///   `~/.shelbi/projects/<name>/local.yaml`.
///
/// Only `kind: local` machines contribute (a remote `work_dir` is a path on
/// another host). Paths that no longer canonicalize are dropped, and read/parse
/// failures are swallowed: a broken registration simply yields no trusted roots
/// — the caller then falls through to the registry rather than exploding.
fn registered_local_work_dirs(name: &str) -> Result<Vec<PathBuf>> {
    let projects = projects_dir()?;
    let mut out = Vec::new();

    let mut push_local = |machines: &[Machine]| {
        for m in machines {
            if m.kind == MachineKind::Local {
                if let Ok(canonical) = fs::canonicalize(&m.work_dir) {
                    out.push(canonical);
                }
            }
        }
    };

    // Global single-YAML layout.
    let global = projects.join(format!("{name}.yaml"));
    if let Ok(text) = fs::read_to_string(&global) {
        if let Ok(project) = serde_yaml::from_str::<Project>(&text) {
            push_local(&project.machines);
        }
    }

    // In-repo split layout — per-user local half.
    let local = projects.join(name).join("local.yaml");
    if let Ok(text) = fs::read_to_string(&local) {
        if let Ok(half) = serde_yaml::from_str::<LocalHalf>(&text) {
            push_local(&half.machines);
        }
    }

    Ok(out)
}

/// Minimal projection of `<repo>/.shelbi/project.yaml` used by discovery:
/// we only need the project `name` to reach the per-user `local.yaml`.
/// Extra fields (workflows, danger paths, etc.) are ignored here — the
/// two-file merge parser handles the full load once discovery has picked
/// the config path.
#[derive(Deserialize)]
struct InRepoProjectHeader {
    name: String,
}

/// What the walk-up found: the exact path of the discovered
/// `.shelbi/project.yaml` and the project name inside it. The path is
/// carried through so error messages can point the user at the specific
/// file that was inspected (there may be more than one in a nested
/// checkout).
struct InRepoHit {
    name: String,
    config_path: PathBuf,
    /// The repo root the config was found under (the directory containing
    /// `.shelbi/`), canonicalized by the walk. Used to cross-check the
    /// discovered project against its registered `work_dir`.
    repo_root: PathBuf,
}

/// Walk from `cwd` up to filesystem root looking for `<dir>/.shelbi/project.yaml`.
///
/// The starting cwd is canonicalized so symlinked working directories
/// resolve to their real path before the walk — matching git's `.git`
/// discovery behavior. If canonicalization fails (cwd was deleted from
/// under us, permission denied, etc.), we walk the literal path so we
/// still degrade gracefully.
///
/// A found file is read and parsed for its `name` field. A parse failure is
/// *not* fatal: a single corrupt in-repo config used to hard-fail resolution
/// for the entire subtree (even when the global registry would have matched).
/// Instead we log a warning naming the exact file and keep walking, so a valid
/// ancestor config — or the registry fallback in [`resolve_project_for_cwd`] —
/// can still resolve the project.
fn walk_up_for_in_repo(cwd: &Path) -> Result<Option<InRepoHit>> {
    let start = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut cur: Option<&Path> = Some(&start);
    while let Some(dir) = cur {
        let candidate = dir.join(".shelbi").join("project.yaml");
        if candidate.is_file() {
            let text = fs::read_to_string(&candidate)?;
            match serde_yaml::from_str::<InRepoProjectHeader>(&text) {
                Ok(header) => {
                    return Ok(Some(InRepoHit {
                        name: header.name,
                        config_path: candidate,
                        repo_root: dir.to_path_buf(),
                    }));
                }
                Err(source) => {
                    tracing::warn!(
                        config = %candidate.display(),
                        "shelbi: ignoring unparseable in-repo project config at {}: \
                         {source}; continuing project resolution",
                        candidate.display(),
                    );
                }
            }
        }
        cur = dir.parent();
    }
    Ok(None)
}

/// Pure matching core, factored out for tests: canonicalize `cwd`, walk up
/// its ancestors, and return the name of the first (hence deepest) root
/// whose canonical `work_dir` equals an ancestor.
fn match_root(cwd: &Path, roots: &[ProjectRoot]) -> Option<String> {
    // Canonicalize the start so the comparison is symlink-stable on both
    // sides. If cwd itself can't be canonicalized (e.g. it was deleted out
    // from under us) fall back to the literal path.
    let start = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    // Walking up from cwd visits the deepest directory first, so the first
    // ancestor that equals any root's canonical work_dir is the deepest
    // match — no separate length sort needed.
    let mut cur: Option<&Path> = Some(&start);
    while let Some(dir) = cur {
        if let Some(root) = roots.iter().find(|r| r.canonical.as_deref() == Some(dir)) {
            return Some(root.name.clone());
        }
        cur = dir.parent();
    }
    None
}

/// What `shelbi reload` did (or noticed) for one registered project root
/// while cleaning up legacy state.
#[derive(Debug, Clone)]
pub struct MarkerCleanup {
    pub name: String,
    pub work_dir: PathBuf,
    /// A legacy `<work_dir>/.shelbi/project` marker existed and was removed.
    pub marker_removed: bool,
    /// The `work_dir` no longer exists on disk — the project is excluded
    /// from resolution until the user re-points or removes it.
    pub work_dir_missing: bool,
}

/// Sweep every registered project's local `work_dir`: delete any leftover
/// `.shelbi/project` marker (now redundant — resolution reads the YAMLs)
/// and flag work_dirs that have gone missing. Idempotent and best-effort;
/// a project whose tree is gone is reported, not deleted.
pub fn cleanup_legacy_markers() -> Result<Vec<MarkerCleanup>> {
    let mut out = Vec::new();
    for root in project_roots()? {
        let mut cleanup = MarkerCleanup {
            name: root.name,
            work_dir: root.work_dir.clone(),
            marker_removed: false,
            work_dir_missing: root.canonical.is_none(),
        };
        if !cleanup.work_dir_missing {
            let marker = root.work_dir.join(".shelbi").join("project");
            if marker.is_file() && fs::remove_file(&marker).is_ok() {
                cleanup.marker_removed = true;
            }
        }
        out.push(cleanup);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;

    /// A unique temp home for one test. The nanosecond suffix keeps
    /// parallel tests from colliding even though they share `SHELBI_HOME`.
    fn fresh_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-resolve-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// Write a minimal project YAML registering `name` with a single local
    /// hub whose `work_dir` is `work_dir`.
    fn write_project(home: &Path, name: &str, work_dir: &Path) {
        let dir = home.join("projects");
        fs::create_dir_all(&dir).unwrap();
        let yaml = format!(
            "name: {name}\n\
             repo: {wd}\n\
             machines:\n\
             \x20\x20- {{ name: hub, kind: local, work_dir: {wd} }}\n\
             orchestrator: {{ runner: claude }}\n\
             agent_runners:\n\
             \x20\x20claude: {{ command: claude, flags: [] }}\n",
            wd = work_dir.display(),
        );
        fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
    }

    #[test]
    fn cwd_at_project_root_matches() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("foo");
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "foo", &root);

        let found = resolve_project_for_cwd(&root).unwrap();
        assert_eq!(found.as_deref(), Some("foo"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cwd_below_project_root_matches() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("foo");
        let deep = root.join("src/bar");
        fs::create_dir_all(&deep).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "foo", &root);

        let found = resolve_project_for_cwd(&deep).unwrap();
        assert_eq!(found.as_deref(), Some("foo"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn deepest_overlapping_work_dir_wins() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let outer = fresh_dir("foo");
        let inner = outer.join("sub-project");
        fs::create_dir_all(&inner).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "outer", &outer);
        write_project(&home, "inner", &inner);

        // cwd inside the inner project resolves to it, not the outer one.
        let nested = inner.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            resolve_project_for_cwd(&nested).unwrap().as_deref(),
            Some("inner")
        );
        // cwd between the two (in outer but above inner) resolves to outer.
        assert_eq!(
            resolve_project_for_cwd(&outer).unwrap().as_deref(),
            Some("outer")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn corrupt_registration_is_skipped_but_others_still_resolve() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("foo");
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "foo", &root);
        // A hand-broken registration alongside it must not take down
        // resolution for the healthy project (it's skipped with a
        // warning — see `warn_skipped_project_yaml_once`).
        fs::write(
            home.join("projects/broken.yaml"),
            "name: [this is not a scalar\n",
        )
        .unwrap();

        let roots = project_roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "foo");
        assert_eq!(
            resolve_project_for_cwd(&root).unwrap().as_deref(),
            Some("foo")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn missing_work_dir_is_excluded() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let ghost = std::env::temp_dir().join(format!(
            "shelbi-resolve-ghost-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Note: `ghost` is never created on disk.
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "ghost", &ghost);

        let roots = project_roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert!(roots[0].canonical.is_none());
        // Even querying from inside the (nonexistent) path resolves to nothing.
        assert!(resolve_project_for_cwd(&ghost).unwrap().is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn symlinked_project_root_resolves() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let real = fresh_dir("real");
        let link_parent = fresh_dir("links");
        let link = link_parent.join("link-to-real");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        // Register the project by its REAL path; query through the symlink.
        write_project(&home, "foo", &real);

        assert_eq!(
            resolve_project_for_cwd(&link).unwrap().as_deref(),
            Some("foo")
        );

        // And the reverse: register through a symlink, query the real path.
        let home2 = fresh_dir("home2");
        std::env::set_var("SHELBI_HOME", &home2);
        write_project(&home2, "bar", &link);
        assert_eq!(
            resolve_project_for_cwd(&real).unwrap().as_deref(),
            Some("bar")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cleanup_removes_legacy_marker_and_flags_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("foo");
        let ghost = std::env::temp_dir().join(format!(
            "shelbi-resolve-cleanup-ghost-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "foo", &root);
        write_project(&home, "ghost", &ghost);

        // Drop a legacy marker into the live project tree.
        let marker = root.join(".shelbi/project");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "foo\n").unwrap();

        let report = cleanup_legacy_markers().unwrap();
        let foo = report.iter().find(|c| c.name == "foo").unwrap();
        assert!(foo.marker_removed);
        assert!(!foo.work_dir_missing);
        assert!(!marker.exists(), "marker should be gone");

        let ghost_c = report.iter().find(|c| c.name == "ghost").unwrap();
        assert!(ghost_c.work_dir_missing);
        assert!(!ghost_c.marker_removed);

        // Idempotent: a second sweep finds nothing to remove.
        let report2 = cleanup_legacy_markers().unwrap();
        assert!(!report2.iter().any(|c| c.marker_removed));
        std::env::remove_var("SHELBI_HOME");
    }

    // ---------------------------------------------------------------
    // In-repo discovery walk-up (Phase 2 of in-repo vs global config).

    /// Drop an in-repo shared-half YAML at `<repo>/.shelbi/project.yaml`
    /// containing just enough for discovery — the walk-up parser only
    /// reads `name`.
    fn write_in_repo_config(repo: &Path, name: &str) {
        let dir = repo.join(".shelbi");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("project.yaml"), format!("name: {name}\n")).unwrap();
    }

    /// Create the per-user `local.yaml` companion under
    /// `<home>/projects/<name>/`, registering a single local hub whose
    /// `work_dir` is `work_dir`. The walk-up checks existence; the trust
    /// cross-check reads the `machines` list to confirm the discovered repo
    /// root belongs to the registered project.
    fn touch_local_half(home: &Path, name: &str, work_dir: &Path) {
        let dir = home.join("projects").join(name);
        fs::create_dir_all(&dir).unwrap();
        let yaml = format!(
            "machines:\n\
             \x20\x20- {{ name: hub, kind: local, work_dir: {wd} }}\n",
            wd = work_dir.display(),
        );
        fs::write(dir.join("local.yaml"), yaml).unwrap();
    }

    #[test]
    fn walkup_finds_in_repo_project_when_local_half_present() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let repo = fresh_dir("in-repo");
        std::env::set_var("SHELBI_HOME", &home);
        write_in_repo_config(&repo, "shelbi");
        touch_local_half(&home, "shelbi", &repo);

        // At the repo root and from a subdir the walk-up hits the same
        // config file.
        let deep = repo.join("crates/shelbi-state");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            resolve_project_for_cwd(&repo).unwrap().as_deref(),
            Some("shelbi")
        );
        assert_eq!(
            resolve_project_for_cwd(&deep).unwrap().as_deref(),
            Some("shelbi")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_missing_local_half_returns_project_not_picked_up() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let repo = fresh_dir("fresh-clone");
        std::env::set_var("SHELBI_HOME", &home);
        // In-repo config exists, but this machine has never registered
        // a `local.yaml` for it — a freshly-cloned repo the user hasn't
        // yet run `shelbi init --pick-up` against.
        write_in_repo_config(&repo, "shelbi");

        let err = resolve_project_for_cwd(&repo).unwrap_err();
        match err {
            shelbi_core::Error::ProjectNotPickedUp {
                name,
                config_path,
                expected_local,
            } => {
                assert_eq!(name, "shelbi");
                assert_eq!(
                    config_path,
                    fs::canonicalize(&repo)
                        .unwrap()
                        .join(".shelbi/project.yaml"),
                );
                assert_eq!(expected_local, home.join("projects/shelbi/local.yaml"),);
            }
            other => panic!("expected ProjectNotPickedUp, got {other:?}"),
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_takes_precedence_over_global_registry_for_same_name() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let in_repo = fresh_dir("in-repo");
        let global_root = fresh_dir("global-root");
        std::env::set_var("SHELBI_HOME", &home);

        // Same project name registered in two places: an in-repo
        // config at `<in_repo>/.shelbi/project.yaml` and a global YAML
        // pointing at an entirely different work_dir.
        write_in_repo_config(&in_repo, "shared");
        touch_local_half(&home, "shared", &in_repo);
        write_project(&home, "shared", &global_root);

        // Query from inside the in-repo tree — walk-up wins.
        let deep = in_repo.join("sub/dir");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            resolve_project_for_cwd(&deep).unwrap().as_deref(),
            Some("shared")
        );

        // Query from inside the global-registered tree — no walk-up
        // hit (no `.shelbi/project.yaml` there), fallback resolves via
        // work_dir.
        assert_eq!(
            resolve_project_for_cwd(&global_root).unwrap().as_deref(),
            Some("shared")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_misses_falls_back_to_global_registry() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("plain-global");
        std::env::set_var("SHELBI_HOME", &home);
        // Global-mode project only: no `.shelbi/project.yaml` under
        // the work_dir, so the walk-up walks off the top and we land
        // in the reverse-lookup path.
        write_project(&home, "plain", &root);

        assert_eq!(
            resolve_project_for_cwd(&root).unwrap().as_deref(),
            Some("plain")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_returns_none_when_nothing_matches() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let stray = fresh_dir("stray");
        std::env::set_var("SHELBI_HOME", &home);
        // No global registrations, no in-repo config — pure miss.
        assert!(resolve_project_for_cwd(&stray).unwrap().is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_through_symlink_resolves_to_in_repo_project() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let real = fresh_dir("real-repo");
        let link_parent = fresh_dir("links");
        let link = link_parent.join("link-to-real");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        std::env::set_var("SHELBI_HOME", &home);
        write_in_repo_config(&real, "shelbi");
        touch_local_half(&home, "shelbi", &real);

        // Query through the symlink — canonicalization makes the
        // walk-up see the real path and find the config.
        assert_eq!(
            resolve_project_for_cwd(&link).unwrap().as_deref(),
            Some("shelbi")
        );
        // Nested query through the symlink resolves identically.
        let via_link_deep = link.join("crates");
        fs::create_dir_all(real.join("crates")).unwrap();
        assert_eq!(
            resolve_project_for_cwd(&via_link_deep).unwrap().as_deref(),
            Some("shelbi")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_corrupt_yaml_warns_and_falls_through_to_registry() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let repo = fresh_dir("broken-repo");
        std::env::set_var("SHELBI_HOME", &home);
        let cfg_dir = repo.join(".shelbi");
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::write(
            cfg_dir.join("project.yaml"),
            "name: [this is not a scalar\n",
        )
        .unwrap();

        // A corrupt in-repo config is no longer fatal: with no registry
        // entry it's skipped (warned, not propagated) and resolution finds
        // nothing rather than hard-erroring for the whole subtree.
        assert!(resolve_project_for_cwd(&repo).unwrap().is_none());

        // And when the global registry *would* match, the corrupt in-repo
        // config no longer blocks it — we fall through and resolve.
        write_project(&home, "plain", &repo);
        assert_eq!(
            resolve_project_for_cwd(&repo).unwrap().as_deref(),
            Some("plain")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_untrusted_repo_does_not_hijack_registered_project() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let trusted = fresh_dir("trusted-repo");
        let evil = fresh_dir("evil-clone");
        std::env::set_var("SHELBI_HOME", &home);

        // `shelbi` is legitimately picked up, rooted at `trusted`.
        write_in_repo_config(&trusted, "shelbi");
        touch_local_half(&home, "shelbi", &trusted);

        // A third-party repo elsewhere on disk ships the same committed
        // `name: shelbi`. Running inside it must NOT redirect to the real
        // board — the repo root doesn't match the registered work_dir, so
        // we fall through to the registry (which owns nothing here → None).
        write_in_repo_config(&evil, "shelbi");
        assert_eq!(resolve_project_for_cwd(&evil).unwrap(), None);

        // The genuine checkout still resolves.
        assert_eq!(
            resolve_project_for_cwd(&trusted).unwrap().as_deref(),
            Some("shelbi")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn walkup_trusts_worktree_beneath_registered_work_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let repo = fresh_dir("hub-repo");
        std::env::set_var("SHELBI_HOME", &home);

        // Register `shelbi` with its hub work_dir at `repo`.
        write_in_repo_config(&repo, "shelbi");
        touch_local_half(&home, "shelbi", &repo);

        // A shelbi worktree lives at `<work_dir>/.shelbi/wt/<name>` and
        // carries the same committed in-repo config. Resolution from inside
        // the worktree (and a subdir) is trusted via the work_dir prefix.
        let worktree = repo.join(".shelbi/wt/bravo");
        fs::create_dir_all(&worktree).unwrap();
        write_in_repo_config(&worktree, "shelbi");
        assert_eq!(
            resolve_project_for_cwd(&worktree).unwrap().as_deref(),
            Some("shelbi")
        );
        let deep = worktree.join("crates/shelbi-state");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            resolve_project_for_cwd(&deep).unwrap().as_deref(),
            Some("shelbi")
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
