//! Best-effort cross-machine sync for the user's ContextStore spaces.
//!
//! ## Why this exists
//!
//! Each machine has its own local ContextStore git repo (typically under
//! `~/Documents/ContextStore/<space>`). They are independent repos, not
//! clones of a shared remote — so when a devbox-workspace writes
//! `Shelbi/Research/<slug>.md` via `cstore new`, the file lives ONLY on
//! devbox until something pulls it back to the user's primary machine
//! (hub). The user browses ContextStore from hub. Without sync, every
//! research note / plan authored by a remote workspace is effectively
//! invisible.
//!
//! ## What this does (Plan B from the original task)
//!
//! After the hub-side poller promotes a workspace's task to the review
//! column, we check the task body against a small heuristic (was
//! `cstore` mentioned, or a `<configured-space>/` path referenced?) and,
//! if matched, rsync each matching space dir from the remote workspace's
//! machine back to hub. Local workspaces skip — the files are already on
//! hub. Failures log to `events.log` and surface in the activity feed,
//! but never block the promotion: the worst case is the user notices
//! the missing file and re-runs sync, which beats silently failing to
//! advance the task.
//!
//! ## What this does NOT do
//!
//! - Push hub edits *out* to remote workspaces. The next workspace dispatch
//!   on that machine reuses the workspace's stable worktree, which is
//!   independent of ContextStore; staleness on remote is acceptable for
//!   v1. A future Plan A (real shared git remote) or Plan C (`cstore
//!   sync` primitives) replaces this.
//! - Resolve write/write conflicts. We trust the workspace as the source
//!   of truth at handoff time — `rsync --delete` is NOT used so hub-only
//!   files survive, but a hub-and-remote concurrent edit of the same
//!   file resolves to whichever wrote last on the remote side. In
//!   practice the workspace is the only writer during its turn.

use shelbi_core::{ContextStoreSyncSpec, Host, Machine, Project};

/// Outcome of trying to sync one space from one remote machine. Returned
/// to callers (and surfaced via `events.log`) so the orchestrator can
/// see — and tell the user — when a remote-workspace write didn't make it
/// back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub space: String,
    pub machine: String,
    pub status: SyncStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    /// rsync completed; remote files are now mirrored on hub.
    Ok,
    /// rsync exited non-zero or couldn't be invoked. `detail` is the
    /// short reason (stderr first line, or the io::Error message).
    Failed { detail: String },
    /// Local hub-side workspace — sync is a no-op. Returned so callers can
    /// still log the decision uniformly without special-casing the
    /// caller side.
    SkippedLocal,
}

impl SyncStatus {
    pub fn label(&self) -> &'static str {
        match self {
            SyncStatus::Ok => "ok",
            SyncStatus::Failed { .. } => "failed",
            SyncStatus::SkippedLocal => "skipped-local",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            SyncStatus::Failed { detail } => detail.clone(),
            _ => String::new(),
        }
    }
}

/// Decide which configured spaces a task's body claims to have touched.
///
/// Heuristic:
/// - The literal token `cstore` anywhere in the body → ALL configured
///   spaces are candidates (the agent invoked the CLI; we don't know
///   which space, so sync them all to be safe).
/// - `<SpaceName>/` substring (e.g. `Shelbi/`) → that specific space.
///
/// We're deliberately generous: the cost of a false positive (one
/// extra rsync that finds no changes) is much smaller than the cost
/// of a false negative (workspace write stranded on the remote).
pub fn body_matches<'a>(
    body: &str,
    specs: &'a [ContextStoreSyncSpec],
) -> Vec<&'a ContextStoreSyncSpec> {
    if specs.is_empty() {
        return Vec::new();
    }
    let mentions_cstore = body.contains("cstore");
    specs
        .iter()
        .filter(|s| mentions_cstore || body.contains(&format!("{}/", s.space)))
        .collect()
}

/// Pull every matched ContextStore space from `machine` back to hub.
///
/// - Local machines short-circuit to `SkippedLocal` for every spec —
///   no rsync needed since the files already live on hub.
/// - Remote machines run `rsync -az` over SSH (no `--delete`, so a
///   file the user has only on hub doesn't get clobbered). Failures
///   are captured into the outcome and don't propagate; the caller is
///   the poller and we never want to abort review promotion over
///   sync.
///
/// Returns one `SyncOutcome` per matched spec so the caller can log
/// them all to `events.log` and surface them in the activity feed.
pub fn sync_after_review(
    project: &Project,
    machine: &Machine,
    task_body: &str,
) -> Vec<SyncOutcome> {
    let matches = body_matches(task_body, &project.contextstore_sync);
    if matches.is_empty() {
        return Vec::new();
    }
    let host = machine.host();
    matches
        .into_iter()
        .map(|spec| SyncOutcome {
            space: spec.space.clone(),
            machine: machine.name.clone(),
            status: sync_one(&host, spec),
        })
        .collect()
}

fn sync_one(host: &Host, spec: &ContextStoreSyncSpec) -> SyncStatus {
    let Host::Ssh { host: ssh_host } = host else {
        return SyncStatus::SkippedLocal;
    };
    let path_str = trim_trailing_slash(&spec.path.to_string_lossy());
    // Trailing slash on src AND dst is intentional: rsync semantics
    // are "copy contents of src into dst" only when src ends with `/`,
    // which is what we want (we already have the parent dir on both
    // sides — we want the contents to overlay). Without it rsync
    // creates `<dst>/<basename(src)>` and the layout drifts.
    let src = format!("{ssh_host}:{path_str}/");
    let dst = format!("{path_str}/");

    // Make sure the destination directory exists. The mac default
    // `rsync` is openrsync, which doesn't support `--mkpath`, so we
    // can't rely on rsync to create it. `mkdir -p` is a no-op when the
    // dir already exists and surfaces real errors (permissions, etc.)
    // via stderr — exactly the behavior we want.
    if let Err(e) = ensure_local_dir(&path_str) {
        return SyncStatus::Failed {
            detail: format!("mkdir -p failed: {e}"),
        };
    }

    let argv = rsync_argv(&src, &dst);
    let mut cmd = std::process::Command::new(&argv[0]);
    for a in &argv[1..] {
        cmd.arg(a);
    }
    tracing::debug!(?cmd, "contextstore sync_one");
    match cmd.output() {
        Ok(o) if o.status.success() => SyncStatus::Ok,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let first_line = stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("rsync failed (no stderr)")
                .to_string();
            SyncStatus::Failed { detail: first_line }
        }
        Err(e) => SyncStatus::Failed {
            detail: format!("rsync invocation failed: {e}"),
        },
    }
}

/// Build the rsync argv (broken out so tests can pin the exact flags
/// without running rsync).
///
/// Flags:
/// - `-a`  archive (recursive, preserve perms/times/symlinks) — matches
///   a working-tree mirror, which is what ContextStore directories are.
/// - `-z`  compress over the wire — cheap and these are markdown files.
/// - `--rsh "ssh -o ControlMaster=auto ..."`  reuse the same SSH
///   ControlMaster the hub already opens for `shelbi-ssh`, so this
///   sync doesn't trigger a separate auth handshake.
///
/// The `--rsh` string mirrors `shelbi-ssh`'s control opts (ControlPath
/// under `$SHELBI_HOME/ssh/`) so this rsync attaches to the same
/// long-lived master rather than opening a fresh one — important for
/// hosts where the daemon's reverse forward is already pinned.
///
/// No `--delete`: hub-only files (e.g. notes the user wrote locally
/// between the workspace's start and finish) must survive. We're catching
/// up, not mirroring. No `--mkpath` either — macOS's bundled openrsync
/// rejects the flag; the caller `mkdir -p`s the destination instead.
fn rsync_argv(src: &str, dst: &str) -> Vec<String> {
    let control_path = shelbi_state::ssh_control_path_template()
        .unwrap_or_else(|_| "~/.shelbi/ssh/%C".to_string());
    let rsh = format!(
        "ssh -o ControlMaster=auto -o ControlPath={control_path} \
         -o ControlPersist=600 -o ConnectTimeout=5 -o BatchMode=yes \
         -o LogLevel=ERROR"
    );
    vec![
        "rsync".to_string(),
        "-az".to_string(),
        "--rsh".to_string(),
        rsh,
        src.to_string(),
        dst.to_string(),
    ]
}

/// Create the local destination directory if missing, expanding a
/// leading `~/` against `$HOME`. Anything more exotic (`$VAR`,
/// embedded `~user/`) is treated literally — the YAML field is
/// typically a fully-resolvable home-relative path.
fn ensure_local_dir(path: &str) -> std::io::Result<()> {
    let expanded = expand_tilde(path);
    std::fs::create_dir_all(&expanded)
}

fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

fn trim_trailing_slash(s: &str) -> String {
    let mut out = s.to_string();
    while out.ends_with('/') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn specs() -> Vec<ContextStoreSyncSpec> {
        vec![
            ContextStoreSyncSpec {
                space: "Shelbi".into(),
                path: PathBuf::from("~/Documents/ContextStore/shelbi"),
            },
            ContextStoreSyncSpec {
                space: "Memories".into(),
                path: PathBuf::from("~/Documents/ContextStore/memories"),
            },
        ]
    }

    #[test]
    fn empty_specs_match_nothing_regardless_of_body() {
        let m = body_matches("cstore new --space Shelbi Research/foo.md", &[]);
        assert!(m.is_empty());
    }

    #[test]
    fn cstore_mention_matches_every_configured_space() {
        // The agent invoked the CLI — we don't know which space, so be
        // generous. Both Shelbi and Memories get a sync attempt.
        let s = specs();
        let m = body_matches("# Task\n\nFinish writing to ContextStore via cstore.\n", &s);
        let names: Vec<&str> = m.iter().map(|s| s.space.as_str()).collect();
        assert_eq!(names, vec!["Shelbi", "Memories"]);
    }

    #[test]
    fn space_slug_mention_matches_only_that_space() {
        let s = specs();
        let m = body_matches("Write notes to Shelbi/Research/foo.md", &s);
        let names: Vec<&str> = m.iter().map(|s| s.space.as_str()).collect();
        // Only the Shelbi spec — Memories isn't named anywhere.
        assert_eq!(names, vec!["Shelbi"]);
    }

    #[test]
    fn space_slug_match_is_case_sensitive_to_avoid_false_positives() {
        // `shelbi/` (lowercase) is the on-disk dir name, NOT the space
        // name. The heuristic targets the user-facing space name, so we
        // expect no match here. Avoids drag from random file paths.
        let s = specs();
        let m = body_matches("see shelbi/.shelbi/wt/alpha for the worktree", &s);
        assert!(m.is_empty());
    }

    #[test]
    fn body_with_no_signals_matches_nothing() {
        let s = specs();
        let m = body_matches("Fix the Safari SSO bug.", &s);
        assert!(m.is_empty());
    }

    #[test]
    fn sync_one_skips_local_host_without_invoking_rsync() {
        // Local hub-side workspaces don't need a sync — their writes
        // already land on the hub's filesystem. Returning SkippedLocal
        // surfaces the decision so callers log it uniformly.
        let status = sync_one(
            &Host::Local,
            &ContextStoreSyncSpec {
                space: "Shelbi".into(),
                path: PathBuf::from("~/Documents/ContextStore/shelbi"),
            },
        );
        assert_eq!(status, SyncStatus::SkippedLocal);
    }

    #[test]
    fn rsync_argv_uses_archive_compress_and_trailing_slashes() {
        let argv = rsync_argv(
            "devbox:~/Documents/ContextStore/shelbi/",
            "~/Documents/ContextStore/shelbi/",
        );
        // Sanity check the flags we depend on for correctness, not the
        // exact order of every token.
        assert_eq!(argv[0], "rsync");
        assert!(argv.contains(&"-az".to_string()));
        // No `--mkpath`: openrsync (macOS default) doesn't support it.
        // ensure_local_dir handles the parent dir; rsync stays portable.
        assert!(!argv.contains(&"--mkpath".to_string()));
        // Last two args are src then dst. Trailing slash on both — see
        // the comment on `rsync_argv` for why.
        assert!(argv[argv.len() - 2].ends_with("shelbi/"));
        assert!(argv[argv.len() - 1].ends_with("shelbi/"));
    }

    #[test]
    fn expand_tilde_resolves_home_prefix_and_passes_through_others() {
        // Save and restore the original HOME so we don't perturb other tests.
        let original = std::env::var_os("HOME");
        std::env::set_var("HOME", "/Users/test");
        assert_eq!(
            expand_tilde("~/Documents/ContextStore/shelbi"),
            std::path::PathBuf::from("/Users/test/Documents/ContextStore/shelbi"),
        );
        // Absolute paths pass through unmodified.
        assert_eq!(
            expand_tilde("/tmp/cs"),
            std::path::PathBuf::from("/tmp/cs"),
        );
        // Bare `~` (no slash) isn't expanded — keeps the function from
        // accidentally interpreting `~user/` style paths.
        assert_eq!(expand_tilde("~root/x"), std::path::PathBuf::from("~root/x"));
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn trim_trailing_slash_collapses_repeats_to_no_slash() {
        assert_eq!(trim_trailing_slash("/a/b///"), "/a/b");
        assert_eq!(trim_trailing_slash("/a/b"), "/a/b");
        assert_eq!(trim_trailing_slash("~/x/"), "~/x");
    }
}
